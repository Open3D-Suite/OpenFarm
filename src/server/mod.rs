// SPDX-License-Identifier: GPL-3.0-or-later
//! The farm server: HTTP API, the agents' WebSocket, the scene store and the LAN discovery answer.
//!
//! Access: requests from this computer are trusted; from the network they need the key of an
//! approved machine (`X-OpenFarm-Key`), which every agent adds when it relays its user's requests.

pub mod db;
pub mod farm;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use openfarm_core::model::{JobSpec, SceneRef};
use openfarm_core::proto::{AgentMsg, ServerMsg, PROTOCOL};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::config::{Config, VERSION};
use farm::{Farm, Hello};

#[derive(Clone)]
pub struct ServerState {
    pub farm: Arc<Mutex<Farm>>,
    pub version: watch::Receiver<u64>,
    pub data: PathBuf,
    conns: Arc<std::sync::atomic::AtomicU64>,
}

pub type ApiError = (StatusCode, String);

fn bad(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, e.to_string())
}

/// Opens the farm and serves it until the process ends.
pub async fn run(cfg: &Config, data: &Path) -> Result<()> {
    let state = open(cfg, data)?;
    if cfg.discovery {
        tokio::spawn(discovery(cfg.farm_name.clone(), cfg.server_port));
    }
    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.server_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("farm server on {addr}");
    axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

pub fn open(cfg: &Config, data: &Path) -> Result<ServerState> {
    let (tx, rx) = watch::channel(1u64);
    let dir = data.join("server");
    let farm = Farm::open(cfg.farm_name.clone(), dir.clone(), tx)?;
    let state = ServerState { farm: Arc::new(Mutex::new(farm)), version: rx, data: dir, conns: Default::default() };
    // Machines that stop talking are dropped from the farm by their WebSocket closing; this also
    // catches connections that hang without closing.
    let s = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let mut farm = s.farm.lock().unwrap();
            let stale: Vec<(String, u64)> = farm
                .machines
                .values()
                .filter(|m| m.online() && crate::config::now_ms().saturating_sub(m.last_seen) > 45_000)
                .map(|m| (m.id.clone(), m.conn))
                .collect();
            for (id, conn) in stale {
                farm.disconnected(&id, conn);
            }
        }
    });
    Ok(state)
}

pub fn router(state: ServerState) -> Router {
    let api = Router::new()
        .route("/api/v1/state", get(api_state))
        .route("/api/v1/jobs", post(api_submit))
        .route("/api/v1/jobs/{id}", get(api_job))
        .route("/api/v1/jobs/{id}/action", post(api_job_action))
        .route("/api/v1/jobs/{id}/thumbs/{frame}", get(api_thumb))
        .route("/api/v1/tasks/{id}/log", get(api_task_log))
        .route("/api/v1/machines/{id}/action", post(api_machine_action))
        .route("/api/v1/scenes", post(api_scene_upload))
        .route("/api/v1/scenes/from-path", post(api_scene_from_path))
        .route("/api/v1/scenes/{hash}", get(api_scene_download))
        .layer(middleware::from_fn_with_state(state.clone(), require_trust));
    Router::new()
        .merge(api)
        .route("/api/v1/agent", get(agent_ws))
        .merge(crate::ui::routes())
        .with_state(state)
}

async fn require_trust(
    State(s): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if addr.ip().is_loopback() {
        return next.run(req).await;
    }
    let key = req.headers().get("x-openfarm-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if s.farm.lock().unwrap().trusts_key(&key) {
        return next.run(req).await;
    }
    (StatusCode::UNAUTHORIZED, "Open OpenFarm on a machine that belongs to this farm.").into_response()
}

// ---- state and jobs --------------------------------------------------------------------------

#[derive(Deserialize)]
struct Since {
    since: Option<u64>,
}

/// The whole farm for the UI. With `since`, waits up to 25 s for something to change (long poll).
async fn api_state(State(s): State<ServerState>, Query(q): Query<Since>) -> Response {
    let mut rx = s.version.clone();
    if let Some(since) = q.since {
        if *rx.borrow() == since {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(25), rx.changed()).await;
        }
    }
    let v = *rx.borrow();
    let view = s.farm.lock().unwrap().state_view();
    let mut value = serde_json::to_value(view).unwrap_or_default();
    value["v"] = json!(v);
    Json(value).into_response()
}

async fn api_submit(State(s): State<ServerState>, Json(spec): Json<JobSpec>) -> Result<Json<serde_json::Value>, ApiError> {
    let id = s.farm.lock().unwrap().submit(spec).map_err(bad)?;
    Ok(Json(json!({ "id": id })))
}

async fn api_job(State(s): State<ServerState>, UrlPath(id): UrlPath<u64>) -> Result<Response, ApiError> {
    let detail = s.farm.lock().unwrap().job_detail(id).ok_or((StatusCode::NOT_FOUND, "no such job".into()))?;
    Ok(Json(detail).into_response())
}

#[derive(Deserialize)]
struct Action {
    action: String,
    #[serde(default)]
    priority: Option<u8>,
}

async fn api_job_action(
    State(s): State<ServerState>,
    UrlPath(id): UrlPath<u64>,
    Json(a): Json<Action>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.farm.lock().unwrap().job_action(id, &a.action, a.priority).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn api_machine_action(
    State(s): State<ServerState>,
    UrlPath(id): UrlPath<String>,
    Json(a): Json<Action>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.farm.lock().unwrap().machine_action(&id, &a.action).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn api_thumb(State(s): State<ServerState>, UrlPath((id, frame)): UrlPath<(u64, i32)>) -> Response {
    let path = s.data.join("thumbs").join(id.to_string()).join(format!("{frame}.jpg"));
    match tokio::fs::read(path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "max-age=3600")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn api_task_log(State(s): State<ServerState>, UrlPath(id): UrlPath<u64>) -> Response {
    let path = s.data.join("logs").join(format!("{id}.log"));
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            // The last megabyte is plenty for the UI.
            let start = bytes.len().saturating_sub(1 << 20);
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], String::from_utf8_lossy(&bytes[start..]).into_owned())
                .into_response()
        }
        Err(_) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], String::new()).into_response(),
    }
}

// ---- scenes -------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
    #[serde(default)]
    original: String,
}

/// The submitter's snapshot of a scene. Stored by content, so sending the same file twice costs nothing.
async fn api_scene_upload(
    State(s): State<ServerState>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> Result<Json<SceneRef>, ApiError> {
    let name = safe_file_name(&q.name).ok_or_else(|| bad("bad file name"))?;
    let tmp_dir = s.data.join("scenes").join(".incoming");
    tokio::fs::create_dir_all(&tmp_dir).await.map_err(bad)?;
    let tmp = tmp_dir.join(uuid::Uuid::new_v4().to_string());
    let mut file = tokio::fs::File::create(&tmp).await.map_err(bad)?;
    let mut hasher = blake3::Hasher::new();
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(bad)?;
        hasher.update(&chunk);
        size += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(bad)?;
    }
    file.flush().await.map_err(bad)?;
    drop(file);
    let hash = hasher.finalize().to_hex().to_string();
    store_scene(&s.data, &tmp, &hash, &name).await.map_err(bad)?;
    Ok(Json(SceneRef { hash, file_name: name, original_path: q.original, size }))
}

#[derive(Deserialize)]
struct FromPath {
    path: String,
}

/// Snapshot of a scene the server can read itself (a path on a share, from the UI's "Submit a file").
async fn api_scene_from_path(State(s): State<ServerState>, Json(p): Json<FromPath>) -> Result<Json<SceneRef>, ApiError> {
    let src = PathBuf::from(p.path.trim().trim_matches('"'));
    let name = src.file_name().and_then(|n| n.to_str()).and_then(safe_file_name).ok_or_else(|| bad("bad file name"))?;
    let data = s.data.clone();
    let original = src.to_string_lossy().into_owned();
    let (hash, size, tmp) = tokio::task::spawn_blocking(move || -> Result<(String, u64, PathBuf)> {
        let tmp_dir = data.join("scenes").join(".incoming");
        std::fs::create_dir_all(&tmp_dir)?;
        let tmp = tmp_dir.join(uuid::Uuid::new_v4().to_string());
        std::fs::copy(&src, &tmp)?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut std::fs::File::open(&tmp)?, &mut hasher)?;
        let size = std::fs::metadata(&tmp)?.len();
        Ok((hasher.finalize().to_hex().to_string(), size, tmp))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("cannot read the scene: {e}")))?;
    store_scene(&s.data, &tmp, &hash, &name).await.map_err(bad)?;
    Ok(Json(SceneRef { hash, file_name: name, original_path: original, size }))
}

async fn store_scene(data: &Path, tmp: &Path, hash: &str, name: &str) -> Result<()> {
    let dir = data.join("scenes").join(hash);
    let dest = dir.join(name);
    if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(tmp).await;
        return Ok(());
    }
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::rename(tmp, &dest).await?;
    Ok(())
}

fn safe_file_name(name: &str) -> Option<String> {
    let name = name.rsplit(['/', '\\']).next()?.trim();
    if name.is_empty() || name.starts_with('.') || name.contains("..") || name.contains(':') {
        return None;
    }
    Some(name.to_string())
}

async fn api_scene_download(State(s): State<ServerState>, UrlPath(hash): UrlPath<String>) -> Response {
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) || hash.len() != 64 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let dir = s.data.join("scenes").join(&hash);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(Some(entry)) = entries.next_entry().await else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(file) = tokio::fs::File::open(entry.path()).await else { return StatusCode::NOT_FOUND.into_response() };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 1 << 20);
    ([(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_LENGTH, len.to_string())], Body::from_stream(stream))
        .into_response()
}

// ---- agents ----------------------------------------------------------------------------------------

async fn agent_ws(
    State(s): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let rank: u8 = headers.get("x-openfarm-rank").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(1);
    ws.max_message_size(64 << 20).on_upgrade(move |socket| agent_session(s, socket, addr, rank))
}

async fn agent_session(s: ServerState, socket: WebSocket, addr: SocketAddr, rank: u8) {
    let (mut sink, mut stream) = socket.split();
    // The first message must be Hello.
    let hello = loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<AgentMsg>(&t) {
                Ok(AgentMsg::Hello { machine_id, name, key, version, protocol, caps }) => {
                    if protocol != PROTOCOL {
                        let _ = sink
                            .send(Message::Text(
                                json!({"type":"error","error": format!("protocol {protocol} is not {PROTOCOL}: update OpenFarm")})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        return;
                    }
                    break Hello {
                        machine_id,
                        name,
                        key,
                        version,
                        caps,
                        rank,
                        address: addr.ip().to_string(),
                        loopback: addr.ip().is_loopback(),
                    };
                }
                _ => return,
            },
            Some(Ok(Message::Ping(_))) => continue,
            _ => return,
        }
    };
    let machine = hello.machine_id.clone();
    let conn = s.conns.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let farm_name = s.farm.lock().unwrap().name.clone();
    // Welcome goes first so the agent knows where it stands before any assignment.
    let _ = tx.send(ServerMsg::Welcome { approved: false, server_name: farm_name.clone(), server_version: VERSION.into(), protocol: PROTOCOL });
    let approved = s.farm.lock().unwrap().hello(hello, conn, tx.clone());
    if approved {
        let _ = tx.send(ServerMsg::Welcome { approved: true, server_name: farm_name, server_version: VERSION.into(), protocol: PROTOCOL });
    }
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else { continue };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(msg) = serde_json::from_str::<AgentMsg>(&text) else {
            tracing::warn!(machine = %machine, "unreadable message");
            continue;
        };
        let mut farm = s.farm.lock().unwrap();
        match msg {
            AgentMsg::Hello { .. } => {}
            AgentMsg::Status { telemetry, accepting } => farm.status(&machine, telemetry, accepting),
            AgentMsg::Task { task, event } => {
                if farm.machines.get(&machine).map(|m| m.approved).unwrap_or(false) {
                    farm.task_event(&machine, task, event);
                }
            }
        }
    }
    s.farm.lock().unwrap().disconnected(&machine, conn);
    writer.abort();
}

// ---- discovery ----------------------------------------------------------------------------------------

/// Answers `OPENFARM?` broadcasts on the LAN with this farm's name and port.
async fn discovery(name: String, port: u16) {
    let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", crate::config::DISCOVERY_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("discovery off: {e}");
            return;
        }
    };
    let reply = json!({ "openfarm": VERSION, "name": name, "port": port }).to_string();
    let mut buf = [0u8; 64];
    loop {
        if let Ok((n, from)) = sock.recv_from(&mut buf).await {
            if &buf[..n] == crate::discovery::QUESTION {
                let _ = sock.send_to(reply.as_bytes(), from).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_kept_inside_the_store() {
        assert_eq!(safe_file_name("kitchen.max").as_deref(), Some("kitchen.max"));
        assert_eq!(safe_file_name(r"C:\a\b\shot.blend").as_deref(), Some("shot.blend"));
        assert_eq!(safe_file_name(".."), None);
        assert_eq!(safe_file_name("a..b"), None);
        assert_eq!(safe_file_name(""), None);
    }
}
