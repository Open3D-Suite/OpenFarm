// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeps web pages away from what OpenFarm serves on this computer.
//!
//! Requests that come from this computer are trusted without any key: that is how the app's window,
//! the 3ds Max and Blender buttons and scripts use OpenFarm. But a page open in a browser on this
//! computer can send requests to 127.0.0.1 too, so "it comes from this computer" is not enough.
//! Every request that arrives from a loopback address, on the local app and on the farm server, is
//! checked before any handler runs:
//!
//! - `Host` must be `127.0.0.1`, `localhost` or `[::1]` with the listener's own port. A page that
//!   makes its own name point at 127.0.0.1 (DNS rebinding) arrives under that name and is refused.
//! - `Origin`, when there is one, must be `http://` followed by one of those same hosts and the
//!   same port: the page OpenFarm itself serves there. Browsers add `Origin` to whatever a page of
//!   another origin can send that changes something (a POST, a WebSocket, a `fetch`); programs and
//!   scripts send none.
//! - `Sec-Fetch-Site`, when there is one, must be `same-origin` or `none` (an address typed by the
//!   user). Browsers mark with it the requests of pages of another site (`cross-site`) or of
//!   another port of this computer (`same-site`), even those that carry no `Origin`.
//!
//! Requests from the network are not concerned: they are signed (see `crate::auth`).

use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{Request, State};
use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::config::now_ms;

/// The names under which a listener on this computer is reached from this computer.
const OWN_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Why a request from this computer is not served.
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    /// Asked for under a name that is not this computer's loopback name and this port.
    Host(String),
    /// Sent by a page of another origin.
    Origin(String),
    /// Marked by the browser as coming from another site or another port.
    Site(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let why = match self {
            Refused::Host(host) => format!("it was addressed to \"{host}\""),
            Refused::Origin(origin) => format!("it was sent by a page of {origin}"),
            Refused::Site(site) => format!("the browser marked it as {site}"),
        };
        write!(f, "OpenFarm on this computer only answers its own window and this computer's programs: this request was refused because {why}.")
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (StatusCode::FORBIDDEN, self.to_string()).into_response()
    }
}

/// Is `authority` (`host` or `host:port`) one of this computer's loopback names with `port`?
fn is_own(authority: &str, port: u16) -> bool {
    let (host, given) = match authority.strip_prefix('[') {
        // An IPv6 address is in brackets and has colons of its own.
        Some(rest) => match rest.find(']') {
            Some(end) => authority.split_at(end + 2),
            None => return false,
        },
        None => authority.split_at(authority.rfind(':').unwrap_or(authority.len())),
    };
    let given = match given.strip_prefix(':') {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => digits.parse::<u16>().ok(),
        // No port means the web's own, 80.
        None if given.is_empty() => Some(80),
        _ => None,
    };
    given == Some(port) && OWN_HOSTS.iter().any(|own| host.eq_ignore_ascii_case(own))
}

/// Checks a request that arrived from a loopback address on the listener at `port`.
pub fn check(headers: &HeaderMap, uri: &Uri, port: u16) -> Result<(), Refused> {
    let text = |value: &axum::http::HeaderValue| value.to_str().unwrap_or("?").to_string();
    // The name the request was sent to: exactly one `Host`, and the address of the request line
    // when it carries one.
    let mut hosts = headers.get_all(HOST).iter();
    match (hosts.next(), hosts.next()) {
        (Some(host), None) if is_own(&text(host), port) => {}
        (Some(host), _) => return Err(Refused::Host(text(host))),
        (None, _) if uri.authority().is_none() => return Err(Refused::Host(String::new())),
        (None, _) => {}
    }
    if let Some(authority) = uri.authority()
        && !is_own(authority.as_str(), port)
    {
        return Err(Refused::Host(authority.to_string()));
    }
    for origin in headers.get_all(ORIGIN) {
        let origin = text(origin);
        if !origin.strip_prefix("http://").map(|authority| is_own(authority, port)).unwrap_or(false) {
            return Err(Refused::Origin(origin));
        }
    }
    for site in headers.get_all("sec-fetch-site") {
        let site = text(site);
        if site != "same-origin" && site != "none" {
            return Err(Refused::Site(site));
        }
    }
    Ok(())
}

/// True at most once a second: for log lines that anybody can cause as often as they like.
pub(crate) fn once_a_second(last: &AtomicU64) -> bool {
    let second = now_ms() / 1000;
    last.swap(second, Ordering::Relaxed) != second
}

/// Answers a refused request and notes it in the log.
pub(crate) fn refuse(refused: Refused) -> Response {
    static LAST: AtomicU64 = AtomicU64::new(0);
    if once_a_second(&LAST) {
        tracing::warn!("a request from this computer was refused: {refused}");
    }
    refused.into_response()
}

/// For a listener that only listens on 127.0.0.1: every request is checked. The state is the
/// listener's port.
pub async fn guard(State(port): State<u16>, req: Request, next: Next) -> Response {
    match check(req.headers(), req.uri(), port) {
        Ok(()) => next.run(req).await,
        Err(refused) => refuse(refused),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const PORT: u16 = 47102;

    fn request(headers: &[(&str, &str)]) -> Result<(), Refused> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(), value.parse().unwrap());
        }
        check(&map, &Uri::from_static("/local/quit"), PORT)
    }

    #[test]
    fn programs_and_the_apps_own_page_are_served() {
        // A script, the 3ds Max and Blender buttons, the local app relaying to the server: a `Host`
        // and nothing else.
        for host in ["127.0.0.1:47102", "localhost:47102", "LocalHost:47102", "[::1]:47102"] {
            assert_eq!(request(&[("host", host)]), Ok(()), "{host}");
        }
        // The window: its page is served from here, and the browser says so.
        assert_eq!(request(&[("host", "127.0.0.1:47102"), ("origin", "http://127.0.0.1:47102"), ("sec-fetch-site", "same-origin")]), Ok(()));
        assert_eq!(request(&[("host", "localhost:47102"), ("origin", "http://localhost:47102")]), Ok(()));
        // The page itself, opened by the window or typed in a browser.
        assert_eq!(request(&[("host", "127.0.0.1:47102"), ("sec-fetch-site", "none")]), Ok(()));
        // A listener on the web's own port is addressed without one.
        assert_eq!(check(&HeaderMap::from_iter([(HOST, "localhost".parse().unwrap())]), &Uri::from_static("/"), 80), Ok(()));
    }

    #[test]
    fn a_name_that_is_not_this_computers_is_refused() {
        // DNS rebinding: the page's own name, pointed at 127.0.0.1.
        for host in [
            "evil.example:47102",
            "evil.example",
            "127.0.0.1.evil.example:47102",
            "localhost.evil.example:47102",
            "127.0.0.1:47102.evil.example",
            "user@127.0.0.1:47102",
            "127.0.0.1:47103",
            "127.0.0.1",
            "127.0.0.1:",
            "127.0.0.1:+47102",
            "127.0.0.2:47102",
            "0.0.0.0:47102",
            "192.168.1.10:47102",
            "[::1]",
            "[::1",
            "[::2]:47102",
            "",
        ] {
            assert_eq!(request(&[("host", host)]), Err(Refused::Host(host.into())), "{host}");
        }
        assert_eq!(request(&[]), Err(Refused::Host(String::new())));
        // Two names: which one would the handler believe?
        assert!(matches!(request(&[("host", "127.0.0.1:47102"), ("host", "evil.example:47102")]), Err(Refused::Host(_))));
        assert!(matches!(request(&[("host", "evil.example:47102"), ("host", "127.0.0.1:47102")]), Err(Refused::Host(_))));
        // The address on the request line counts as well.
        let headers = HeaderMap::from_iter([(HOST, "127.0.0.1:47102".parse().unwrap())]);
        let absolute = Uri::from_static("http://evil.example:47102/local/quit");
        assert_eq!(check(&headers, &absolute, PORT), Err(Refused::Host("evil.example:47102".into())));
        assert_eq!(check(&headers, &Uri::from_static("http://127.0.0.1:47102/local/quit"), PORT), Ok(()));
        assert_eq!(check(&HeaderMap::new(), &Uri::from_static("http://localhost:47102/"), PORT), Ok(()));
    }

    #[test]
    fn a_page_of_another_origin_is_refused() {
        let own = ("host", "127.0.0.1:47102");
        for origin in [
            "http://evil.example",
            "https://evil.example",
            "null",
            // Another program's page on this computer is another origin too.
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://127.0.0.1:47103",
            "https://127.0.0.1:47102",
            "http://127.0.0.1:47102.evil.example",
            "http://127.0.0.1:47102/",
            "http://evil.example@127.0.0.1:47102",
            "file://",
            "",
        ] {
            assert_eq!(request(&[own, ("origin", origin)]), Err(Refused::Origin(origin.into())), "{origin}");
        }
        // One good origin does not excuse a bad one.
        assert!(matches!(request(&[own, ("origin", "http://127.0.0.1:47102"), ("origin", "http://evil.example")]), Err(Refused::Origin(_))));
    }

    #[test]
    fn what_the_browser_marks_as_foreign_is_refused() {
        let own = ("host", "127.0.0.1:47102");
        for site in ["cross-site", "same-site", "CROSS-SITE", "anything"] {
            assert_eq!(request(&[own, ("sec-fetch-site", site)]), Err(Refused::Site(site.into())), "{site}");
        }
        assert!(matches!(request(&[own, ("sec-fetch-site", "same-origin"), ("sec-fetch-site", "cross-site")]), Err(Refused::Site(_))));
        // The name is looked at first: a rebound page is refused whatever else it says.
        assert!(matches!(request(&[("host", "evil.example:47102"), ("sec-fetch-site", "same-origin")]), Err(Refused::Host(_))));
    }

    #[test]
    fn the_refusal_says_why() {
        let refused = request(&[("host", "127.0.0.1:47102"), ("origin", "http://evil.example")]).unwrap_err();
        let response = refused.clone().into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(refused.to_string().contains("a page of http://evil.example"), "{refused}");
    }

    /// Sends one HTTP request, exactly as written, to a listener of this computer and returns the
    /// status and the body of the answer. For the tests of the listeners themselves.
    pub(crate) async fn send(port: u16, request_line: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut text = format!("{request_line} HTTP/1.1\r\n");
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty() {
            text.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        text.push_str("\r\n");
        text.push_str(body);
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(text.as_bytes()).await.unwrap();
        // The connection stays open after the answer: read the head and as much body as it announces.
        let mut answer = Vec::new();
        let mut buffer = [0u8; 4096];
        let whole = |answer: &[u8]| {
            let text = String::from_utf8_lossy(answer);
            let Some((head, body)) = text.split_once("\r\n\r\n") else { return false };
            let length =
                head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()));
            body.len() >= length.unwrap_or(0)
        };
        while !whole(&answer) {
            match tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buffer)).await {
                Ok(Ok(n)) if n > 0 => answer.extend_from_slice(&buffer[..n]),
                _ => break,
            }
        }
        let answer = String::from_utf8_lossy(&answer).into_owned();
        let status = answer.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let body = answer.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }
}
