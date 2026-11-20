// SPDX-License-Identifier: GPL-3.0-or-later
//! The local app: a small web server on 127.0.0.1 that this machine's window and the 3ds Max and
//! Blender submitters talk to. It serves the interface, the first-run setup, this machine's status
//! and the language of the interface, and relays `/api/v1/*` to the farm server, signed with this
//! machine's key.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use openfarm_core::model::SceneRef;
use serde::Deserialize;
use serde_json::json;

use crate::agent::SharedStatus;
use crate::auth::{self, Identity};
use crate::config::{Config, MachineKind, Role, VERSION};
use crate::open3d_settings::{self, Language};

#[derive(Clone)]
pub struct LocalState {
    pub cfg: Arc<Mutex<Config>>,
    /// The folder of `config.json`.
    pub settings: PathBuf,
    /// What of an earlier OpenFarm's folder could not be copied when this one started
    /// (`crate::folders`); the page tells the person.
    pub not_copied: Arc<Vec<String>>,
    pub agent: SharedStatus,
    /// This machine's key pair: it signs what is relayed to the farm server.
    pub identity: Arc<Identity>,
    pub http: reqwest::Client,
    /// Asks the window to show itself / the process to end.
    pub control: tokio::sync::mpsc::UnboundedSender<Control>,
    /// The language of the interface.
    pub language: Arc<SharedLanguage>,
}

/// The language of the interface: the one every Open3D app shares, in `settings.json`
/// (`crate::open3d_settings`, D-051). It is read once, when OpenFarm starts, and changes only
/// when the person chooses another one on the page (D-056). The other Open3D apps take it the
/// next time they start, and a language chosen in one of them shows here the next time OpenFarm
/// starts: nothing watches the file (D-057). OpenFarm kept no language of its own before (the page
/// followed the browser's), so there is nothing to carry over.
pub struct SharedLanguage {
    /// The folder of `settings.json`; `None` when neither `OPEN3D_SETTINGS_DIR` nor `APPDATA` is set.
    dir: Option<PathBuf>,
    /// The language shown. The lock is also held while the file is written, so two choices made at
    /// once leave the file and the page with the same one.
    shown: Mutex<Language>,
}

impl SharedLanguage {
    /// The language at start: the preference kept in `dir`, or with none the language of Windows
    /// (Spanish if it is Spanish, otherwise English; D-052). It writes nothing.
    pub fn read(dir: Option<PathBuf>) -> SharedLanguage {
        let shown = open3d_settings::language(dir.as_deref());
        SharedLanguage { dir, shown: Mutex::new(shown) }
    }

    /// The language shown.
    pub fn shown(&self) -> Language {
        *self.shown.lock().unwrap()
    }

    /// The person chose `language`: it shows at once, and it is kept for every Open3D app. A
    /// write that fails still leaves it shown here until OpenFarm ends; the error says why.
    fn choose(&self, language: Language) -> Result<(), String> {
        let mut shown = self.shown.lock().unwrap();
        *shown = language;
        let dir = self.dir.as_deref().ok_or("there is no folder for the settings the Open3D apps share (APPDATA is not set)")?;
        open3d_settings::write_language(dir, Some(language))
    }
}

#[derive(Debug)]
pub enum Control {
    Show,
    Restart,
    Quit,
}

pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await
}

pub async fn serve(listener: tokio::net::TcpListener, state: LocalState) -> Result<()> {
    // Everything here is for this computer's own programs: a page open in a browser is kept out
    // before any of it runs (see `crate::loopback`).
    let port = listener.local_addr()?.port();
    let app = Router::new()
        .route("/local/info", get(info))
        .route("/local/setup", post(setup))
        .route("/local/discover", get(discover))
        .route("/local/accepting", post(accepting))
        .route("/local/language", post(language))
        .route("/local/show", post(show))
        .route("/local/quit", post(quit))
        .route("/local/install/{target}", post(install))
        .merge(crate::ui::routes())
        .fallback(proxy)
        .layer(axum::middleware::from_fn_with_state(port, crate::loopback::guard))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info(State(s): State<LocalState>) -> Json<serde_json::Value> {
    let cfg = s.cfg.lock().unwrap().clone();
    let agent = s.agent.lock().unwrap().clone();
    Json(json!({
        "version": VERSION,
        "role": cfg.role,
        "kind": cfg.kind,
        "name": cfg.name,
        "machine_id": cfg.machine_id,
        "server": cfg.server,
        "server_url": cfg.server_url(),
        "render": cfg.render,
        "farm_name": cfg.farm_name,
        "agent": agent,
        "renders": cfg.role == Role::Agent || (cfg.role == Role::Server && cfg.render),
        "language": s.language.shown().code(),
        "not_copied": *s.not_copied,
    }))
}

#[derive(Deserialize)]
struct Setup {
    role: Role,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    kind: Option<MachineKind>,
    #[serde(default)]
    render: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    farm_name: Option<String>,
}

/// First run (or a change of role): saves the choice and restarts OpenFarm with it.
async fn setup(State(s): State<LocalState>, Json(p): Json<Setup>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let mut cfg = s.cfg.lock().unwrap();
        if p.role == Role::Agent && p.server.as_deref().unwrap_or("").trim().is_empty() {
            return Err((StatusCode::BAD_REQUEST, "choose a farm server".into()));
        }
        cfg.role = p.role;
        if let Some(v) = p.server {
            cfg.server = v.trim().to_string();
        }
        if let Some(v) = p.kind {
            cfg.kind = v;
        }
        if let Some(v) = p.render {
            cfg.render = v;
        }
        if let Some(v) = p.name.filter(|n| !n.trim().is_empty()) {
            cfg.name = v.trim().to_string();
        }
        if let Some(v) = p.farm_name.filter(|n| !n.trim().is_empty()) {
            cfg.farm_name = v.trim().to_string();
        }
        cfg.save(&s.settings).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let _ = s.control.send(Control::Restart);
    Ok(Json(json!({ "ok": true })))
}

async fn discover() -> Json<serde_json::Value> {
    Json(json!(crate::discovery::discover(Duration::from_millis(1500)).await))
}

#[derive(Deserialize)]
struct Accepting {
    accepting: bool,
}

async fn accepting(State(s): State<LocalState>, Json(a): Json<Accepting>) -> Json<serde_json::Value> {
    s.agent.lock().unwrap().accepting = a.accepting;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct Choice {
    language: String,
}

/// The language chosen on the page (*This machine*, or the first-run page): `en` or `es`. It shows
/// at once and is written to the settings every Open3D app shares. When the write fails the
/// language still changes here, and the answer says why it was not kept.
async fn language(State(s): State<LocalState>, Json(c): Json<Choice>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let language =
        Language::from_code(&c.language).ok_or_else(|| (StatusCode::BAD_REQUEST, format!("there is no language called {:?}", c.language)))?;
    let shared = s.language.clone();
    tokio::task::spawn_blocking(move || shared.choose(language))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({ "ok": true, "language": language.code() })))
}

async fn show(State(s): State<LocalState>) -> Json<serde_json::Value> {
    let _ = s.control.send(Control::Show);
    Json(json!({ "ok": true }))
}

async fn quit(State(s): State<LocalState>) -> Json<serde_json::Value> {
    let _ = s.control.send(Control::Quit);
    Json(json!({ "ok": true }))
}

async fn install(
    State(s): State<LocalState>,
    axum::extract::Path(target): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = s.cfg.lock().unwrap().clone();
    let result = tokio::task::spawn_blocking(move || match target.as_str() {
        "max" => crate::integrations::install_max(),
        "blender" => crate::integrations::install_blender(&cfg),
        other => Err(anyhow::anyhow!("nothing to install called {other}")),
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match result {
        Ok(path) => Ok(Json(json!({ "ok": true, "path": path }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, format!("{e:#}"))),
    }
}

/// Relays a farm API call to the server, signed by this machine.
///
/// The signature covers the body, so the body is read whole first. The exception is a scene
/// upload, which can be many gigabytes and is passed on as it arrives: its signature covers
/// everything but the body (`UNSIGNED-PAYLOAD`), and the body is checked afterwards against the
/// hash the farm stored it under (`stored_upload`).
async fn proxy(State(s): State<LocalState>, req: Request) -> Response {
    let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    if !path.starts_with("/api/v1/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (url, role) = {
        let cfg = s.cfg.lock().unwrap();
        (format!("{}{}", cfg.server_url(), path), cfg.role)
    };
    if role == Role::Unset {
        return (StatusCode::SERVICE_UNAVAILABLE, "OpenFarm is not set up yet").into_response();
    }
    // What is signed is the path and query of the address as it will really be sent.
    let url = match reqwest::Url::parse(&url) {
        Ok(url) => url,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("the farm server's address is not valid: {e}")).into_response(),
    };
    let method = req.method().clone();
    let upload = method == reqwest::Method::POST && url.path() == "/api/v1/scenes";
    let mut headers = HeaderMap::new();
    for name in ["content-type", "content-length"] {
        // The length of a body read whole is set from the body itself.
        if name == "content-length" && !upload {
            continue;
        }
        if let Some(v) = req.headers().get(name) {
            headers.insert(HeaderName::from_static(name), v.clone());
        }
    }
    // The hash of an upload is worked out while it goes by, to check the server's answer with.
    let sent = upload.then(|| Arc::new(Mutex::new(blake3::Hasher::new())));
    let (content, body) = match sent.clone() {
        Some(hasher) => {
            let stream = req.into_body().into_data_stream().inspect(move |chunk| {
                if let Ok(chunk) = chunk {
                    hasher.lock().unwrap().update(chunk);
                }
            });
            (auth::UNSIGNED_PAYLOAD.to_string(), reqwest::Body::wrap_stream(stream))
        }
        _ => match axum::body::to_bytes(req.into_body(), auth::MAX_SIGNED_BODY).await {
            Ok(bytes) => (auth::sha256_hex(&bytes), reqwest::Body::from(bytes)),
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large to send to the farm").into_response(),
        },
    };
    let request = match s.identity.request(&s.http, method, url, &content) {
        Ok(request) => request,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    };
    let resp = match request.headers(headers).body(body).send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("cannot reach the farm server: {e}")).into_response(),
    };
    if let Some(hasher) = sent.filter(|_| resp.status().is_success()) {
        let sent = hasher.lock().unwrap().finalize();
        return stored_upload(resp, sent).await;
    }
    let mut out = Response::builder().status(resp.status().as_u16());
    for name in ["content-type", "content-length", "cache-control"] {
        if let Some(v) = resp.headers().get(name) {
            out = out.header(name, v.clone());
        }
    }
    out.body(Body::from_stream(resp.bytes_stream())).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// The server's answer to a scene upload, passed on only if the farm stored the file that was sent.
///
/// The body of an upload is not signed, so somebody between this machine and the server could
/// change it on the way. The farm names every scene by its BLAKE3 hash, and the job that follows,
/// which is signed, asks for the scene by that hash. Checking the hash here closes the gap: a job
/// can only render the very file its sender meant, or fail because the farm does not have it.
async fn stored_upload(mut resp: reqwest::Response, sent: blake3::Hash) -> Response {
    let mut answer = Vec::new();
    loop {
        match resp.chunk().await {
            // The answer is one small JSON object.
            Ok(Some(chunk)) if answer.len() + chunk.len() <= 64 * 1024 => answer.extend_from_slice(&chunk),
            Ok(None) => break,
            _ => return (StatusCode::BAD_GATEWAY, "the farm server's answer to the upload could not be read").into_response(),
        }
    }
    if !is_stored(&answer, &sent) {
        tracing::warn!("a scene upload was stored with another hash than the one sent");
        return (
            StatusCode::BAD_GATEWAY,
            "The farm did not store the scene as it was sent: it changed on the way to the farm server. Send it again.",
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, "application/json")], answer).into_response()
}

/// Does the server's answer to an upload name the hash of what was sent?
fn is_stored(answer: &[u8], sent: &blake3::Hash) -> bool {
    serde_json::from_slice::<SceneRef>(answer).map(|stored| stored.hash == sent.to_hex().as_str()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestDir;
    use crate::loopback::tests::send;

    /// The folder of the settings every Open3D app shares, for a test: inside its own folder, never
    /// the real `%APPDATA%\Open3D`.
    fn shared_dir(dir: &TestDir) -> PathBuf {
        dir.path().join("Open3D")
    }

    /// A local app on a port of its own, and what it asks the window to do. It reads the shared
    /// language from [`shared_dir`] as it starts.
    async fn local_app(dir: &TestDir) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Control>) {
        local_app_with(dir, Vec::new()).await
    }

    /// [`local_app`], started after an earlier OpenFarm's folder could not all be copied.
    async fn local_app_with(dir: &TestDir, not_copied: Vec<String>) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Control>) {
        let listener = bind(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (control, asked) = tokio::sync::mpsc::unbounded_channel();
        let state = LocalState {
            cfg: Arc::new(Mutex::new(Config::new())),
            settings: dir.path().to_path_buf(),
            not_copied: Arc::new(not_copied),
            agent: Default::default(),
            identity: Arc::new(Identity::generate().unwrap()),
            http: reqwest::Client::new(),
            control,
            language: Arc::new(SharedLanguage::read(Some(shared_dir(dir)))),
        };
        tokio::spawn(serve(listener, state));
        (port, asked)
    }

    /// The language `/local/info` says the page shows.
    async fn shown(port: u16) -> String {
        let own = format!("127.0.0.1:{port}");
        let (status, info) = send(port, "GET /local/info", &[("Host", own.as_str())], "").await;
        assert_eq!(status, 200, "{info}");
        serde_json::from_str::<serde_json::Value>(&info).unwrap()["language"].as_str().unwrap().to_string()
    }

    /// The window chooses a language: the answer's status and body.
    async fn choose(port: u16, body: &str) -> (u16, String) {
        let own = format!("127.0.0.1:{port}");
        let origin = format!("http://127.0.0.1:{port}");
        let window = [("Host", own.as_str()), ("Origin", origin.as_str()), ("Sec-Fetch-Site", "same-origin"), ("Content-Type", "application/json")];
        send(port, "POST /local/language", &window, body).await
    }

    fn shared_file(dir: &TestDir) -> String {
        std::fs::read_to_string(shared_dir(dir).join(open3d_settings::FILE_NAME)).unwrap()
    }

    #[tokio::test]
    async fn the_page_shows_the_shared_language_and_its_choice_is_kept_for_every_open3d_app() {
        let dir = TestDir::new();
        std::fs::create_dir_all(shared_dir(&dir)).unwrap();
        let before = r#"{"theme":"dark","language":"es"}"#;
        std::fs::write(shared_dir(&dir).join(open3d_settings::FILE_NAME), before).unwrap();
        let (port, _asked) = local_app(&dir).await;
        // The preference wins over the language of Windows, and reading it writes nothing.
        assert_eq!(shown(port).await, "es");
        assert_eq!(shared_file(&dir), before);
        // The choice on the page: it shows at once and goes into the file, with its other keys.
        let (status, answer) = choose(port, r#"{"language":"en"}"#).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(shown(port).await, "en");
        let written: serde_json::Value = serde_json::from_str(&shared_file(&dir)).unwrap();
        assert_eq!(written, json!({ "theme": "dark", "language": "en" }));
        assert_eq!(choose(port, r#"{"language":"es"}"#).await.0, 200);
        assert_eq!(shown(port).await, "es");
        assert_eq!(open3d_settings::read_language(&shared_dir(&dir)), Some(Language::Spanish));
        // Only English and Spanish; anything else changes nothing.
        for body in [r#"{"language":"fr"}"#, r#"{"language":"ES"}"#, r#"{"language":null}"#, r#"{"language":""}"#, "{}", "es"] {
            assert!(choose(port, body).await.0 >= 400, "{body}");
            assert_eq!(shown(port).await, "es", "{body}");
        }
        assert_eq!(open3d_settings::read_language(&shared_dir(&dir)), Some(Language::Spanish));
    }

    #[tokio::test]
    async fn with_no_preference_windows_decides_and_nothing_is_written_at_start() {
        let windows = open3d_settings::windows_language().code();
        // No folder, an empty folder, and files that hold no preference.
        for content in [None, Some(""), Some("{}"), Some(r#"{"language":"fr"}"#), Some("not json")] {
            let dir = TestDir::new();
            if let Some(content) = content {
                std::fs::create_dir_all(shared_dir(&dir)).unwrap();
                std::fs::write(shared_dir(&dir).join(open3d_settings::FILE_NAME), content).unwrap();
            }
            let (port, _asked) = local_app(&dir).await;
            assert_eq!(shown(port).await, windows, "{content:?}");
            match content {
                None => assert!(!shared_dir(&dir).exists(), "nothing is written"),
                Some(content) => assert_eq!(shared_file(&dir), content, "nothing is written"),
            }
        }
    }

    #[tokio::test]
    async fn the_page_is_told_what_of_the_earlier_folder_could_not_be_copied() {
        let info = |port: u16| async move {
            let own = format!("127.0.0.1:{port}");
            let (status, info) = send(port, "GET /local/info", &[("Host", own.as_str())], "").await;
            assert_eq!(status, 200, "{info}");
            serde_json::from_str::<serde_json::Value>(&info).unwrap()["not_copied"].clone()
        };
        let dir = TestDir::new();
        let (port, _asked) = local_app(&dir).await;
        assert_eq!(info(port).await, json!([]));
        let problem = r"couldn't copy the farm: C:\old\server\farm.sqlite is in use by another program".to_string();
        let (port, _asked) = local_app_with(&dir, vec![problem.clone()]).await;
        assert_eq!(info(port).await, json!([problem]));
    }

    #[tokio::test]
    async fn a_choice_that_cannot_be_kept_still_shows_and_says_why() {
        let dir = TestDir::new();
        // The folder of the shared settings is a file: nothing can be written in it.
        std::fs::write(shared_dir(&dir), b"in the way").unwrap();
        let (port, _asked) = local_app(&dir).await;
        let other = if shown(port).await == "es" { "en" } else { "es" };
        let (status, answer) = choose(port, &format!(r#"{{"language":"{other}"}}"#)).await;
        assert_eq!(status, 500, "{answer}");
        assert!(answer.contains("couldn't"), "{answer}");
        assert_eq!(shown(port).await, other);
        assert_eq!(std::fs::read(shared_dir(&dir)).unwrap(), b"in the way");
        // Without any folder for the shared settings, the same.
        let language = SharedLanguage::read(None);
        assert!(language.choose(Language::Spanish).unwrap_err().contains("APPDATA"));
        assert_eq!(language.shown(), Language::Spanish);
    }

    #[tokio::test]
    async fn the_local_app_answers_programs_and_its_window_but_no_web_page() {
        let dir = TestDir::new();
        let (port, mut asked) = local_app(&dir).await;
        let own = format!("127.0.0.1:{port}");
        let host = ("Host", own.as_str());
        let origin = format!("http://127.0.0.1:{port}");

        // A script, the 3ds Max and Blender buttons, a second start of OpenFarm: no `Origin`.
        let (status, info) = send(port, "GET /local/info", &[host], "").await;
        assert_eq!(status, 200, "{info}");
        assert!(info.contains("\"role\":\"unset\""), "{info}");
        assert_eq!(send(port, "POST /local/show", &[host], "").await.0, 200);
        assert!(matches!(asked.try_recv(), Ok(Control::Show)));
        // A farm request goes on to the relay (which has no farm to send it to in this test).
        assert_eq!(send(port, "GET /api/v1/state", &[host], "").await.0, 503);
        // The window: the page comes from this very address, and the browser says so.
        let (status, page) = send(port, "GET /", &[host, ("Sec-Fetch-Site", "none")], "").await;
        assert!(status == 200 && page.contains("<html"), "{status}");
        let window = [host, ("Origin", origin.as_str()), ("Sec-Fetch-Site", "same-origin")];
        assert_eq!(send(port, "GET /local/info", &window, "").await.0, 200);
        assert_eq!(send(port, "POST /local/show", &window, "").await.0, 200);
        assert!(matches!(asked.try_recv(), Ok(Control::Show)));
        let by_name = format!("localhost:{port}");
        assert_eq!(send(port, "GET /local/info", &[("Host", by_name.as_str())], "").await.0, 200);

        // A page open in a browser on this computer, in every way it can show up.
        let rebound = format!("evil.example:{port}");
        let pages: [&[(&str, &str)]; 8] = [
            &[host, ("Origin", "http://evil.example")],
            &[host, ("Origin", "null")],
            &[host, ("Origin", "http://127.0.0.1:8080")],
            &[host, ("Sec-Fetch-Site", "cross-site")],
            &[host, ("Sec-Fetch-Site", "same-site")],
            &[host, ("Origin", "http://evil.example"), ("Sec-Fetch-Site", "cross-site")],
            // DNS rebinding: for the browser this is the page's own origin.
            &[("Host", rebound.as_str()), ("Sec-Fetch-Site", "same-origin")],
            &[("Host", "evil.example")],
        ];
        let setup = r#"{"role":"server","farm_name":"Taken over"}"#;
        for page in pages {
            for (request, body) in [
                ("POST /local/quit", ""),
                ("POST /local/show", ""),
                ("POST /local/accepting", r#"{"accepting":false}"#),
                ("POST /local/language", r#"{"language":"es"}"#),
                ("POST /local/language", r#"{"language":"en"}"#),
                ("POST /local/setup", setup),
                ("GET /local/info", ""),
                ("GET /local/discover", ""),
                ("POST /api/v1/scenes?name=junk.max", "junk"),
                ("POST /api/v1/jobs", "{}"),
                ("GET /api/v1/state", ""),
                ("OPTIONS /local/quit", ""),
                ("GET /", ""),
                ("GET /app.js", ""),
                ("GET /nothing-here", ""),
            ] {
                let mut headers = page.to_vec();
                if !body.is_empty() {
                    headers.push(("Content-Type", "application/json"));
                }
                let (status, text) = send(port, request, &headers, body).await;
                assert_eq!(status, 403, "{request} with {page:?}: {text}");
                assert!(text.contains("only answers its own window"), "{text}");
            }
        }
        // None of it reached a handler: the window was asked nothing and the settings are as they
        // were, the language every Open3D app shares too.
        assert!(asked.try_recv().is_err());
        assert!(send(port, "GET /local/info", &[host], "").await.1.contains("\"role\":\"unset\""));
        assert!(!dir.path().join("config.json").exists());
        assert!(!shared_dir(&dir).exists());
    }

    #[test]
    fn an_upload_counts_only_if_the_farm_stored_what_was_sent() {
        let sent = blake3::hash(b"the scene");
        let answer = |hash: &str| {
            serde_json::to_vec(&SceneRef { hash: hash.into(), file_name: "shot.max".into(), original_path: String::new(), size: 9 }).unwrap()
        };
        assert!(is_stored(&answer(sent.to_hex().as_str()), &sent));
        assert!(!is_stored(&answer(blake3::hash(b"another scene").to_hex().as_str()), &sent));
        assert!(!is_stored(&answer(""), &sent));
        assert!(!is_stored(b"{}", &sent));
        assert!(!is_stored(b"not json", &sent));
    }
}
