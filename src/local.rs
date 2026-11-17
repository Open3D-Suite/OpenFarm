// SPDX-License-Identifier: GPL-3.0-or-later
//! The local app: a small web server on 127.0.0.1 that this machine's window and the 3ds Max and
//! Blender submitters talk to. It serves the interface, the first-run setup and this machine's
//! status, and relays `/api/v1/*` to the farm server, signed with this machine's key.

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

#[derive(Clone)]
pub struct LocalState {
    pub cfg: Arc<Mutex<Config>>,
    pub data: PathBuf,
    pub agent: SharedStatus,
    /// This machine's key pair: it signs what is relayed to the farm server.
    pub identity: Arc<Identity>,
    pub http: reqwest::Client,
    /// Asks the window to show itself / the process to end.
    pub control: tokio::sync::mpsc::UnboundedSender<Control>,
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
        cfg.save(&s.data).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    let (content, body) = if let Some(hasher) = sent.clone() {
        let stream = req.into_body().into_data_stream().inspect(move |chunk| {
            if let Ok(chunk) = chunk {
                hasher.lock().unwrap().update(chunk);
            }
        });
        (auth::UNSIGNED_PAYLOAD.to_string(), reqwest::Body::wrap_stream(stream))
    } else {
        match axum::body::to_bytes(req.into_body(), auth::MAX_SIGNED_BODY).await {
            Ok(bytes) => (auth::sha256_hex(&bytes), reqwest::Body::from(bytes)),
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large to send to the farm").into_response(),
        }
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

    /// A local app on a port of its own, and what it asks the window to do.
    async fn local_app(dir: &TestDir) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Control>) {
        let listener = bind(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (control, asked) = tokio::sync::mpsc::unbounded_channel();
        let state = LocalState {
            cfg: Arc::new(Mutex::new(Config::new())),
            data: dir.path().to_path_buf(),
            agent: Default::default(),
            identity: Arc::new(Identity::generate().unwrap()),
            http: reqwest::Client::new(),
            control,
        };
        tokio::spawn(serve(listener, state));
        (port, asked)
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
        // None of it reached a handler: the window was asked nothing and the settings are as they were.
        assert!(asked.try_recv().is_err());
        assert!(send(port, "GET /local/info", &[host], "").await.1.contains("\"role\":\"unset\""));
        assert!(!dir.path().join("config.json").exists());
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
