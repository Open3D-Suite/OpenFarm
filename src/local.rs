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
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
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
/// everything but the body (`UNSIGNED-PAYLOAD`).
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
    let (content, body) = if upload {
        (auth::UNSIGNED_PAYLOAD.to_string(), reqwest::Body::wrap_stream(req.into_body().into_data_stream()))
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
    let mut out = Response::builder().status(resp.status().as_u16());
    for name in ["content-type", "content-length", "cache-control"] {
        if let Some(v) = resp.headers().get(name) {
            out = out.header(name, v.clone());
        }
    }
    out.body(Body::from_stream(resp.bytes_stream())).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}
