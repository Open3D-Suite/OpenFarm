// SPDX-License-Identifier: GPL-3.0-or-later
//! How a machine proves to the farm server who it is.
//!
//! Every machine has an Ed25519 key pair. The private key never leaves the machine (it is kept
//! protected, see `crate::dpapi`); the server only remembers the fingerprint (SHA-256, in hex) of
//! the public key of each machine. Every request a machine sends over the network carries its
//! public key and a signature over these six lines, joined with `\n` and without a final newline:
//!
//! ```text
//! OPENFARM-ED25519-V1
//! <method>                                   GET, POST
//! <path and query, exactly as sent>          /api/v1/state?since=12
//! <time: milliseconds since 1970, UTC>       1794000000000
//! <nonce: 32 lowercase hex digits>           random for every request
//! <SHA-256 of the body, lowercase hex>       or UNSIGNED-PAYLOAD
//! ```
//!
//! in the headers `x-openfarm-public-key` and `x-openfarm-signature` (base64), with the last three
//! lines in `x-openfarm-timestamp`, `x-openfarm-nonce` and `x-openfarm-content-sha256`.
//!
//! The server accepts a request when the signature is right, the time is within `MAX_SKEW_MS` of
//! its own clock, the nonce was not seen before (`ReplayGuard`), the body is the one that was signed
//! and the key's fingerprint is that of an approved machine. So nothing that travels, and nothing
//! the server stores, lets anybody else sign as that machine.
//!
//! `UNSIGNED-PAYLOAD` is only for the scene upload, whose body is sent as it is read and can be many
//! gigabytes: see `crate::local` for how that one is checked.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use anyhow::{anyhow, Result};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::config::now_ms;

/// First line of what is signed. A change of the format changes it.
pub const SCHEME: &str = "OPENFARM-ED25519-V1";
pub const PUBLIC_KEY: &str = "x-openfarm-public-key";
pub const TIMESTAMP: &str = "x-openfarm-timestamp";
pub const NONCE: &str = "x-openfarm-nonce";
pub const CONTENT_SHA256: &str = "x-openfarm-content-sha256";
pub const SIGNATURE: &str = "x-openfarm-signature";
/// What OpenFarm 0.1.1 sent: the machine's secret itself. Never accepted.
pub const LEGACY_KEY: &str = "x-openfarm-key";
/// In place of the body's hash when the body is not covered by the signature.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
/// How far the time of a request may be from the server's clock, either way. Windows keeps the
/// clocks of a network within seconds of each other; five minutes forgives one that was never set.
pub const MAX_SKEW_MS: u64 = 5 * 60 * 1000;
/// A body that is signed is read whole before it is sent or accepted. Job descriptions are a few
/// kilobytes.
pub const MAX_SIGNED_BODY: usize = 2 << 20;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("the system gave no random numbers: {e}"))?;
    Ok(bytes)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(Sha256::digest(data).as_slice())
}

/// What the server keeps of a machine's key: enough to recognise it, useless to sign with.
fn fingerprint(key: &VerifyingKey) -> String {
    sha256_hex(key.as_bytes())
}

fn canonical(method: &str, target: &str, timestamp: &str, nonce: &str, content: &str) -> String {
    [SCHEME, method, target, timestamp, nonce, content].join("\n")
}

/// The path and query of a URL as they go on the request line: what both sides sign.
pub fn target(url: &reqwest::Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}

/// This machine's key pair.
pub struct Identity {
    key: SigningKey,
}

impl Identity {
    pub fn generate() -> Result<Identity> {
        let mut secret = random::<32>()?;
        let identity = Identity { key: SigningKey::from_bytes(&secret) };
        secret.fill(0);
        Ok(identity)
    }

    pub fn from_secret(secret: &[u8]) -> Result<Identity> {
        let secret: &[u8; 32] = secret.try_into().map_err(|_| anyhow!("a private key is 32 bytes long"))?;
        Ok(Identity { key: SigningKey::from_bytes(secret) })
    }

    /// The private key itself, to store it protected.
    pub fn secret(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key.verifying_key())
    }

    /// Signs a request made now. `content` is the body's `sha256_hex` or `UNSIGNED_PAYLOAD`.
    pub fn sign(&self, method: &str, target: &str, content: &str) -> Result<Signed> {
        Ok(self.sign_at(method, target, content, now_ms(), &hex(&random::<16>()?)))
    }

    pub fn sign_at(&self, method: &str, target: &str, content: &str, timestamp: u64, nonce: &str) -> Signed {
        let timestamp = timestamp.to_string();
        let text = canonical(method, target, &timestamp, nonce, content);
        Signed {
            public_key: BASE64.encode(self.key.verifying_key().as_bytes()),
            timestamp,
            nonce: nonce.to_string(),
            content: content.to_string(),
            signature: BASE64.encode(self.key.sign(text.as_bytes()).to_bytes()),
        }
    }

    /// A request to the farm server with this machine's signature on it.
    pub fn request(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        url: reqwest::Url,
        content: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let signed = self.sign(method.as_str(), &target(&url), content)?;
        let mut request = http.request(method, url);
        for (name, value) in signed.headers() {
            request = request.header(name, value);
        }
        Ok(request)
    }
}

/// The signature of one request, as it travels in the headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub public_key: String,
    pub timestamp: String,
    pub nonce: String,
    pub content: String,
    pub signature: String,
}

impl Signed {
    pub fn headers(&self) -> [(&'static str, &str); 5] {
        [
            (PUBLIC_KEY, &self.public_key),
            (TIMESTAMP, &self.timestamp),
            (NONCE, &self.nonce),
            (CONTENT_SHA256, &self.content),
            (SIGNATURE, &self.signature),
        ]
    }

    pub fn from_headers(headers: &HeaderMap) -> Result<Signed, AuthError> {
        if headers.contains_key(LEGACY_KEY) {
            return Err(AuthError::Legacy);
        }
        let names = [PUBLIC_KEY, TIMESTAMP, NONCE, CONTENT_SHA256, SIGNATURE];
        if !names.iter().any(|n| headers.contains_key(*n)) {
            return Err(AuthError::Missing);
        }
        let get = |name: &str| -> Result<String, AuthError> {
            let value = headers.get(name).ok_or(AuthError::Malformed("a signature header is missing"))?;
            Ok(value.to_str().map_err(|_| AuthError::Malformed("a signature header is not text"))?.to_string())
        };
        Ok(Signed {
            public_key: get(PUBLIC_KEY)?,
            timestamp: get(TIMESTAMP)?,
            nonce: get(NONCE)?,
            content: get(CONTENT_SHA256)?,
            signature: get(SIGNATURE)?,
        })
    }

    /// Checks the time and the signature of a request received at `now`. Returns the fingerprint
    /// of the key that signed it; whether that machine is approved, whether the nonce is new and
    /// whether the body is the signed one are for the caller.
    pub fn verify(&self, method: &str, target: &str, now: u64) -> Result<String, AuthError> {
        if self.timestamp.is_empty() || self.timestamp.len() > 16 || !self.timestamp.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AuthError::Malformed("the time is not a number"));
        }
        let timestamp: u64 = self.timestamp.parse().map_err(|_| AuthError::Malformed("the time is not a number"))?;
        if !is_hex(&self.nonce, 32) {
            return Err(AuthError::Malformed("the nonce is not 32 hex digits"));
        }
        if self.content != UNSIGNED_PAYLOAD && !is_hex(&self.content, 64) {
            return Err(AuthError::Malformed("the body's hash is not SHA-256 in hex"));
        }
        let skew_ms = now.abs_diff(timestamp);
        if skew_ms > MAX_SKEW_MS {
            return Err(AuthError::Clock { skew_ms });
        }
        let key: [u8; 32] = BASE64
            .decode(&self.public_key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(AuthError::Malformed("the public key is not 32 bytes in base64"))?;
        let key = VerifyingKey::from_bytes(&key).map_err(|_| AuthError::BadSignature)?;
        let signature: [u8; 64] = BASE64
            .decode(&self.signature)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(AuthError::Malformed("the signature is not 64 bytes in base64"))?;
        let text = canonical(method, target, &self.timestamp, &self.nonce, &self.content);
        key.verify_strict(text.as_bytes(), &Signature::from_bytes(&signature)).map_err(|_| AuthError::BadSignature)?;
        Ok(fingerprint(&key))
    }

    fn time(&self) -> u64 {
        self.timestamp.parse().unwrap_or(0)
    }
}

/// Why the server does not take a request.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthError {
    /// Not signed at all: a browser or a stranger.
    Missing,
    /// It carries the secret header of OpenFarm 0.1.1.
    Legacy,
    Malformed(&'static str),
    Clock {
        skew_ms: u64,
    },
    BadSignature,
    /// Rightly signed by a machine the farm has not approved (or does not know).
    NotApproved,
    Replayed,
    /// The body is not the one whose hash was signed.
    BodyChanged,
    /// More new requests than can be remembered: something is flooding the server.
    Busy,
}

impl AuthError {
    pub fn status(&self) -> StatusCode {
        match self {
            AuthError::Legacy => StatusCode::UPGRADE_REQUIRED,
            AuthError::Malformed(_) | AuthError::BodyChanged => StatusCode::BAD_REQUEST,
            AuthError::NotApproved => StatusCode::FORBIDDEN,
            AuthError::Busy => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::UNAUTHORIZED,
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Missing => f.write_str("Open OpenFarm on a machine that belongs to this farm."),
            AuthError::Legacy => f.write_str(
                "This farm no longer takes the keys of OpenFarm 0.1.1: update OpenFarm on this machine (every computer of a farm needs the same version).",
            ),
            AuthError::Malformed(what) => write!(f, "The request's signature cannot be read: {what}."),
            AuthError::Clock { skew_ms } => write!(
                f,
                "The clocks of this computer and of the farm server differ by {} (at most {} minutes are accepted): set the date and time on both.",
                span(*skew_ms),
                MAX_SKEW_MS / 60_000
            ),
            AuthError::BadSignature => f.write_str("The request's signature is not valid."),
            AuthError::NotApproved => {
                f.write_str("This machine is not approved on this farm: approve it in Machines on the farm's computer.")
            }
            AuthError::Replayed => f.write_str("This request was already received once: a copy of a request is not accepted."),
            AuthError::BodyChanged => f.write_str("The request's body is not the one that was signed."),
            AuthError::Busy => f.write_str("The farm server is getting too many requests: try again in a few minutes."),
        }
    }
}

impl std::error::Error for AuthError {}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        (self.status(), self.to_string()).into_response()
    }
}

fn span(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0..=119 => format!("about {} minutes", minutes.max(1)),
        120..=2879 => format!("about {} hours", minutes / 60),
        _ => format!("about {} days", minutes / 1440),
    }
}

/// The nonces of the requests accepted lately, so that a copy of a request someone recorded on the
/// network is refused. A nonce is remembered for as long as its request could still pass the time
/// check (and as long again, for a server clock that is set back). In memory only.
pub struct ReplayGuard {
    /// `fingerprint:nonce` and the moment it can be forgotten.
    seen: HashMap<String, u64>,
    capacity: usize,
    swept: u64,
}

impl ReplayGuard {
    pub fn new(capacity: usize) -> ReplayGuard {
        ReplayGuard { seen: HashMap::new(), capacity, swept: 0 }
    }

    /// Remembers a verified request. Fails when its nonce was already used by the same key.
    pub fn accept(&mut self, fingerprint: &str, signed: &Signed, now: u64) -> Result<(), AuthError> {
        if now.saturating_sub(self.swept) > 30_000 || self.seen.len() >= self.capacity {
            self.seen.retain(|_, until| *until >= now);
            self.swept = now;
        }
        if self.seen.len() >= self.capacity {
            return Err(AuthError::Busy);
        }
        match self.seen.entry(format!("{fingerprint}:{}", signed.nonce)) {
            Entry::Occupied(_) => Err(AuthError::Replayed),
            Entry::Vacant(slot) => {
                slot.insert(signed.time().saturating_add(2 * MAX_SKEW_MS));
                Ok(())
            }
        }
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_794_000_000_000;
    const NONCE_1: &str = "000102030405060708090a0b0c0d0e0f";

    fn signed(id: &Identity, body: &[u8]) -> Signed {
        id.sign_at("POST", "/api/v1/jobs?x=1", &sha256_hex(body), NOW, NONCE_1)
    }

    #[test]
    fn a_signed_request_verifies() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"{}");
        assert_eq!(s.verify("POST", "/api/v1/jobs?x=1", NOW).unwrap(), id.fingerprint());
        assert_eq!(id.fingerprint().len(), 64);
        // The same key gives the same identity again; another key gives another one.
        let again = Identity::from_secret(&id.secret()).unwrap();
        assert_eq!(again.fingerprint(), id.fingerprint());
        assert_ne!(Identity::generate().unwrap().fingerprint(), id.fingerprint());
        assert!(Identity::from_secret(&[1, 2, 3]).is_err());
    }

    #[test]
    fn a_request_made_now_verifies_and_never_repeats_its_nonce() {
        let id = Identity::generate().unwrap();
        let a = id.sign("GET", "/api/v1/state", &sha256_hex(b"")).unwrap();
        let b = id.sign("GET", "/api/v1/state", &sha256_hex(b"")).unwrap();
        assert_eq!(a.verify("GET", "/api/v1/state", now_ms()).unwrap(), id.fingerprint());
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.signature, b.signature);
    }

    #[test]
    fn the_signed_text_is_the_documented_one() {
        assert_eq!(
            canonical("GET", "/api/v1/state?since=3", "1794000000000", NONCE_1, UNSIGNED_PAYLOAD),
            "OPENFARM-ED25519-V1\nGET\n/api/v1/state?since=3\n1794000000000\n000102030405060708090a0b0c0d0e0f\nUNSIGNED-PAYLOAD"
        );
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let url = reqwest::Url::parse("http://farm:47100/api/v1/scenes?name=a b.max&original=C:\\x").unwrap();
        assert_eq!(target(&url), "/api/v1/scenes?name=a%20b.max&original=C:\\x");
        assert_eq!(target(&reqwest::Url::parse("http://farm:47100/api/v1/state").unwrap()), "/api/v1/state");
    }

    #[test]
    fn any_change_breaks_the_signature() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"{\"a\":1}");
        let bad = Err(AuthError::BadSignature);
        assert_eq!(s.verify("GET", "/api/v1/jobs?x=1", NOW), bad, "method");
        assert_eq!(s.verify("POST", "/api/v1/jobs/7/action?x=1", NOW), bad, "path");
        assert_eq!(s.verify("POST", "/api/v1/jobs?x=2", NOW), bad, "query");
        assert_eq!(s.verify("POST", "/api/v1/jobs", NOW), bad, "no query");
        let mut t = s.clone();
        t.content = sha256_hex(b"{\"a\":2}");
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "body");
        let mut t = s.clone();
        t.content = UNSIGNED_PAYLOAD.into();
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "body left out");
        let mut t = s.clone();
        t.timestamp = (NOW + 1).to_string();
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "time");
        let mut t = s.clone();
        t.nonce = "ff0102030405060708090a0b0c0d0e0f".into();
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "nonce");
        // Somebody else's key with this signature, and this key with somebody else's signature.
        let other = signed(&Identity::generate().unwrap(), b"{\"a\":1}");
        let mut t = s.clone();
        t.public_key = other.public_key.clone();
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "key");
        let mut t = s.clone();
        t.signature = other.signature;
        assert_eq!(t.verify("POST", "/api/v1/jobs?x=1", NOW), bad, "signature");
    }

    #[test]
    fn old_and_future_requests_are_refused_with_the_clock_difference() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"");
        assert!(s.verify("POST", "/api/v1/jobs?x=1", NOW + MAX_SKEW_MS).is_ok());
        assert!(s.verify("POST", "/api/v1/jobs?x=1", NOW - MAX_SKEW_MS).is_ok());
        let late = s.verify("POST", "/api/v1/jobs?x=1", NOW + MAX_SKEW_MS + 1).unwrap_err();
        assert_eq!(late, AuthError::Clock { skew_ms: MAX_SKEW_MS + 1 });
        let early = s.verify("POST", "/api/v1/jobs?x=1", NOW - 3 * 3_600_000).unwrap_err();
        assert_eq!(early, AuthError::Clock { skew_ms: 3 * 3_600_000 });
        assert_eq!(early.status(), StatusCode::UNAUTHORIZED);
        let text = early.to_string();
        assert!(text.contains("clocks") && text.contains("about 3 hours") && text.contains("5 minutes"), "{text}");
    }

    #[test]
    fn unreadable_headers_are_refused() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"");
        let check = |change: fn(&mut Signed)| {
            let mut t = s.clone();
            change(&mut t);
            matches!(t.verify("POST", "/api/v1/jobs?x=1", NOW), Err(AuthError::Malformed(_)))
        };
        assert!(check(|t| t.timestamp = "+1794000000000".into()));
        assert!(check(|t| t.timestamp = "soon".into()));
        assert!(check(|t| t.timestamp = String::new()));
        assert!(check(|t| t.nonce = "abc".into()));
        assert!(check(|t| t.nonce = t.nonce.to_uppercase()));
        assert!(check(|t| t.content = "e3b0".into()));
        assert!(check(|t| t.public_key = "not base64".into()));
        assert!(check(|t| t.public_key = BASE64.encode([7u8; 31])));
        assert!(check(|t| t.signature = BASE64.encode([7u8; 63])));
    }

    #[test]
    fn headers_round_trip_and_the_old_key_is_refused() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"");
        let mut headers = HeaderMap::new();
        assert_eq!(Signed::from_headers(&headers), Err(AuthError::Missing));
        for (name, value) in s.headers() {
            headers.insert(name, value.parse().unwrap());
        }
        assert_eq!(Signed::from_headers(&headers).unwrap(), s);
        headers.remove(NONCE);
        assert!(matches!(Signed::from_headers(&headers), Err(AuthError::Malformed(_))));
        headers.insert(NONCE, s.nonce.parse().unwrap());
        headers.insert(LEGACY_KEY, "0123456789abcdef".parse().unwrap());
        assert_eq!(Signed::from_headers(&headers), Err(AuthError::Legacy));
        assert_eq!(AuthError::Legacy.status(), StatusCode::UPGRADE_REQUIRED);
        assert!(AuthError::Legacy.to_string().contains("update OpenFarm"));
    }

    #[test]
    fn a_copy_of_a_request_is_refused() {
        let id = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let s = signed(&id, b"");
        let mut guard = ReplayGuard::new(100);
        assert_eq!(guard.accept(&id.fingerprint(), &s, NOW), Ok(()));
        assert_eq!(guard.accept(&id.fingerprint(), &s, NOW + 1), Err(AuthError::Replayed));
        assert_eq!(guard.accept(&id.fingerprint(), &s, NOW + MAX_SKEW_MS), Err(AuthError::Replayed));
        // The same nonce from another machine is another request.
        assert_eq!(guard.accept(&other.fingerprint(), &signed(&other, b""), NOW), Ok(()));
        // A new nonce is a new request.
        let next = id.sign_at("POST", "/api/v1/jobs?x=1", &sha256_hex(b""), NOW, "1f0102030405060708090a0b0c0d0e0f");
        assert_eq!(guard.accept(&id.fingerprint(), &next, NOW), Ok(()));
        assert_eq!(guard.len(), 3);
    }

    #[test]
    fn nonces_are_forgotten_only_after_their_request_expires() {
        let id = Identity::generate().unwrap();
        let s = signed(&id, b"");
        let mut guard = ReplayGuard::new(100);
        guard.accept(&id.fingerprint(), &s, NOW).unwrap();
        // Still remembered at the last moment the time check would let the copy in…
        let fresh = id.sign_at("GET", "/", &sha256_hex(b""), NOW + MAX_SKEW_MS, "2f0102030405060708090a0b0c0d0e0f");
        guard.accept(&id.fingerprint(), &fresh, NOW + MAX_SKEW_MS).unwrap();
        assert_eq!(guard.accept(&id.fingerprint(), &s, NOW + MAX_SKEW_MS), Err(AuthError::Replayed));
        // …and gone long after, when the time check refuses it anyway.
        let later = NOW + 3 * MAX_SKEW_MS;
        let last = id.sign_at("GET", "/", &sha256_hex(b""), later, "3f0102030405060708090a0b0c0d0e0f");
        guard.accept(&id.fingerprint(), &last, later).unwrap();
        assert_eq!(guard.len(), 2);
        assert!(matches!(s.verify("POST", "/api/v1/jobs?x=1", later), Err(AuthError::Clock { .. })));
    }

    #[test]
    fn a_full_guard_refuses_instead_of_forgetting() {
        let id = Identity::generate().unwrap();
        let mut guard = ReplayGuard::new(2);
        for (i, expected) in [Ok(()), Ok(()), Err(AuthError::Busy)].into_iter().enumerate() {
            let s = id.sign_at("GET", "/", &sha256_hex(b""), NOW, &format!("{i:032x}"));
            assert_eq!(guard.accept(&id.fingerprint(), &s, NOW), expected);
        }
    }
}
