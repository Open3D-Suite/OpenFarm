// SPDX-License-Identifier: GPL-3.0-or-later
//! The farm server: HTTP API, the agents' WebSocket, the scene store and the LAN discovery answer.
//!
//! Access: requests from this computer are trusted; from the network they must be signed by an
//! approved machine (see `crate::auth`), which every machine's local app does when it relays its
//! user's requests. An agent signs the request that opens its WebSocket in the same way.

pub mod db;
pub mod farm;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
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

use crate::auth::{self, AuthError, ReplayGuard, Signed};
use crate::config::{now_ms, Config, VERSION};
use farm::{Farm, Hello};

/// The one request whose body is not covered by its signature: a scene upload is passed on while
/// it is read and can be many gigabytes (see `crate::local`).
const UPLOAD_PATH: &str = "/api/v1/scenes";

#[derive(Clone)]
pub struct ServerState {
    pub farm: Arc<Mutex<Farm>>,
    pub version: watch::Receiver<u64>,
    pub data: PathBuf,
    conns: Arc<std::sync::atomic::AtomicU64>,
    /// Nonces of the API requests of approved machines.
    replay: Arc<Mutex<ReplayGuard>>,
    /// Nonces of the agents' connections. Apart, and small: these come from machines that are not
    /// approved yet too, and must not be able to fill the memory or push the others out.
    agent_replay: Arc<Mutex<ReplayGuard>>,
    /// The folders other machines may send scenes from by path (`scene_folders` in the settings).
    scene_folders: Arc<Vec<PathBuf>>,
    /// The port the server listens on: requests from this computer must be addressed to it.
    port: u16,
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

impl ServerState {
    fn new(cfg: &Config, data: &Path) -> Result<ServerState> {
        let (tx, rx) = watch::channel(1u64);
        let dir = data.join("server");
        let farm = Farm::open(cfg.farm_name.clone(), dir.clone(), tx)?;
        let mut scene_folders = Vec::new();
        for folder in cfg.scene_folders.iter().map(|f| PathBuf::from(f.trim())) {
            if folder.is_absolute() {
                scene_folders.push(folder);
            } else {
                tracing::warn!("scene_folders: {} is not a full path and is ignored", folder.display());
            }
        }
        Ok(ServerState {
            farm: Arc::new(Mutex::new(farm)),
            version: rx,
            data: dir,
            conns: Default::default(),
            replay: Arc::new(Mutex::new(ReplayGuard::new(500_000))),
            agent_replay: Arc::new(Mutex::new(ReplayGuard::new(20_000))),
            scene_folders: Arc::new(scene_folders),
            port: cfg.server_port,
        })
    }
}

pub fn open(cfg: &Config, data: &Path) -> Result<ServerState> {
    let state = ServerState::new(cfg, data)?;
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
        .layer(middleware::from_fn_with_state(state.clone(), keep_pages_out))
        .with_state(state)
}

/// Requests from this computer are trusted, so the ones a web page open in a browser on this
/// computer could send are turned away first, whatever they ask for (see `crate::loopback`).
/// Requests from the network go on to be checked for their signature.
async fn keep_pages_out(
    State(s): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if addr.ip().is_loopback() {
        if let Err(refused) = crate::loopback::check(req.headers(), req.uri(), s.port) {
            return crate::loopback::refuse(refused);
        }
    }
    next.run(req).await
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
    match authenticate(&s, req).await {
        Ok(req) => next.run(req).await,
        Err(e) => refuse(addr, e),
    }
}

/// Answers a request that was not let in, and writes it in the log: at most one line a second, so
/// nobody can fill the disk by knocking.
fn refuse(addr: SocketAddr, e: AuthError) -> Response {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // A browser that is not part of the farm asks all the time: not worth a line.
    if e != AuthError::Missing && crate::loopback::once_a_second(&LAST) {
        tracing::warn!(from = %addr.ip(), "refused: {e}");
    }
    e.into_response()
}

/// Lets a request from the network through only if an approved machine signed exactly this
/// request, now, and it was not received before.
async fn authenticate(s: &ServerState, req: Request) -> Result<Request, AuthError> {
    let signed = Signed::from_headers(req.headers())?;
    let now = now_ms();
    let target = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or_default();
    let fingerprint = signed.verify(req.method().as_str(), target, now)?;
    if !s.farm.lock().unwrap().trusts_fingerprint(&fingerprint) {
        return Err(AuthError::NotApproved);
    }
    let upload = req.method() == Method::POST && req.uri().path() == UPLOAD_PATH;
    let req = match (upload, signed.content == auth::UNSIGNED_PAYLOAD) {
        (true, true) => req,
        (true, false) => return Err(AuthError::Malformed("a scene upload is sent with UNSIGNED-PAYLOAD")),
        (false, true) => return Err(AuthError::Malformed("this request has to sign its body")),
        (false, false) => {
            let (parts, body) = req.into_parts();
            let bytes = axum::body::to_bytes(body, auth::MAX_SIGNED_BODY)
                .await
                .map_err(|_| AuthError::Malformed("the body is too large or did not arrive whole"))?;
            if auth::sha256_hex(&bytes) != signed.content {
                return Err(AuthError::BodyChanged);
            }
            Request::from_parts(parts, Body::from(bytes))
        }
    };
    s.replay.lock().unwrap().accept(&fingerprint, &signed, now)?;
    Ok(req)
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
///
/// The server reads the file with its own rights, so who may ask matters: from this computer any
/// path is taken, as always; from another machine of the farm only a file inside one of the
/// folders listed in the farm's settings (`scene_folders`), and none while that list is empty.
async fn api_scene_from_path(
    State(s): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(p): Json<FromPath>,
) -> Result<Json<SceneRef>, ApiError> {
    let src = PathBuf::from(p.path.trim().trim_matches('"'));
    let name = src.file_name().and_then(|n| n.to_str()).and_then(safe_file_name).ok_or_else(|| bad("bad file name"))?;
    let data = s.data.clone();
    let original = src.to_string_lossy().into_owned();
    let src = if addr.ip().is_loopback() {
        src
    } else {
        let folders = s.scene_folders.clone();
        // One answer for "not there" and "not allowed": the farm does not say which files exist.
        let refusal = if folders.is_empty() {
            "This farm does not take scenes by their path from other computers. Send the scene from 3ds Max or Blender, or use Submit on the farm's computer."
        } else {
            "The farm cannot take that scene: it is not a file in one of the folders this farm accepts scenes from. Send it from 3ds Max or Blender, or have its folder added to the farm's settings (scene_folders)."
        };
        tokio::task::spawn_blocking(move || scene_in_folders(&src, &folders))
            .await
            .map_err(bad)?
            .ok_or((StatusCode::FORBIDDEN, refusal.to_string()))?
    };
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

/// Where a scene asked for by its path really is, if it is a regular file inside one of `folders`.
///
/// Both the file and the folders are resolved by Windows first: `..` is worked out, links and
/// junctions are followed, a mapped drive becomes its share and every path comes back in one
/// spelling (`\\?\C:\…`, `\\?\UNC\server\share\…`). What is compared is where the file really is,
/// so a link inside an allowed folder that points out of it does not count.
fn scene_in_folders(requested: &Path, folders: &[PathBuf]) -> Option<PathBuf> {
    // `scenes\a.max` or `\a.max` would depend on the folder and drive the server happens to run in.
    if !requested.is_absolute() {
        return None;
    }
    let real = std::fs::canonicalize(requested).ok()?;
    if !std::fs::metadata(&real).ok()?.is_file() {
        return None;
    }
    folders.iter().filter_map(|folder| std::fs::canonicalize(folder).ok()).any(|folder| is_inside(&real, &folder)).then_some(real)
}

/// Is the resolved path `file` below the resolved path `folder`? Compared part by part (drive or
/// server and share, then each folder), so `C:\scenes-evil` is not inside `C:\scenes`, and without
/// regard to case, as Windows names are.
fn is_inside(file: &Path, folder: &Path) -> bool {
    let parts = |path: &Path| path.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
    let (file, folder) = (parts(file), parts(folder));
    file.len() > folder.len() && file[..folder.len()] == folder[..]
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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let rank: u8 = headers.get("x-openfarm-rank").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(1);
    // Every agent, the server's own too, proves which key it holds before the connection opens.
    // Whether that key is approved is decided after `Hello`: a new machine connects to ask for it.
    let fingerprint = match agent_fingerprint(&s, &method, &uri, &headers) {
        Ok(fingerprint) => fingerprint,
        // OpenFarm 0.1.1 does not sign. Its window shows the status of the answer, so the status
        // itself says what to do: "426 Upgrade Required".
        Err(AuthError::Missing) => return refuse(addr, AuthError::Legacy),
        Err(e) => return refuse(addr, e),
    };
    ws.max_message_size(64 << 20).on_upgrade(move |socket| agent_session(s, socket, addr, rank, fingerprint))
}

/// The fingerprint of the key that signed an agent's request to connect.
fn agent_fingerprint(s: &ServerState, method: &Method, uri: &Uri, headers: &HeaderMap) -> Result<String, AuthError> {
    let signed = Signed::from_headers(headers)?;
    let now = now_ms();
    let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or_default();
    let fingerprint = signed.verify(method.as_str(), target, now)?;
    if signed.content != auth::sha256_hex(b"") {
        return Err(AuthError::Malformed("the request that opens the connection has no body to sign"));
    }
    s.agent_replay.lock().unwrap().accept(&fingerprint, &signed, now)?;
    Ok(fingerprint)
}

async fn agent_session(s: ServerState, socket: WebSocket, addr: SocketAddr, rank: u8, fingerprint: String) {
    let (mut sink, mut stream) = socket.split();
    // The first message must be Hello.
    let hello = loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<AgentMsg>(&t) {
                Ok(AgentMsg::Hello { machine_id, name, version, protocol, caps }) => {
                    if protocol != PROTOCOL {
                        let refusal = ServerMsg::Error {
                            error: format!(
                                "This farm runs OpenFarm {VERSION} (protocol {PROTOCOL}) and this machine speaks protocol {protocol}: every computer of a farm needs the same version."
                            ),
                        };
                        if let Ok(text) = serde_json::to_string(&refusal) {
                            let _ = sink.send(Message::Text(text.into())).await;
                        }
                        return;
                    }
                    break Hello {
                        machine_id,
                        name,
                        fingerprint,
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
    let taken = s.farm.lock().unwrap().hello(hello, conn, tx.clone());
    let approved = match taken {
        Ok(approved) => approved,
        Err(error) => {
            // The id of a machine the farm knows, with another key: it is told why and what to
            // do, and the machine the farm knows is left as it is.
            static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if crate::loopback::once_a_second(&LAST) {
                tracing::warn!(from = %addr.ip(), machine = %machine, "a machine came with the id of a known machine and another key: turned away");
            }
            if let Ok(text) = serde_json::to_string(&ServerMsg::Error { error }) {
                let _ = sink.send(Message::Text(text.into())).await;
            }
            return;
        }
    };
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
        // Only the machine's current connection speaks for it. One that was replaced, or that
        // somebody else opened under the same machine id with another key, is not heard: the
        // approval is for the key of the current one.
        if farm.machines.get(&machine).map(|m| m.conn) != Some(conn) {
            continue;
        }
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
    use crate::auth::Identity;
    use crate::TestDir;

    const EMPTY: &[u8] = b"";

    /// A farm that knows two machines: `a`, approved, and `p`, waiting for approval.
    fn farm(dir: &TestDir) -> (ServerState, Identity, Identity) {
        let state = ServerState::new(&Config::new(), dir.path()).unwrap();
        let (approved, pending) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        {
            let mut farm = state.farm.lock().unwrap();
            assert_eq!(connect(&mut farm, "a", &approved, 1), Ok(false));
            assert_eq!(connect(&mut farm, "p", &pending, 2), Ok(false));
            farm.machine_action("a", "approve").unwrap();
        }
        (state, approved, pending)
    }

    /// What a machine that connects from the network presents: its id, its name, the key it proved.
    fn hello(id: &str, name: &str, identity: &Identity) -> Hello {
        Hello {
            machine_id: id.into(),
            name: name.into(),
            fingerprint: identity.fingerprint(),
            version: VERSION.into(),
            caps: Default::default(),
            rank: 1,
            address: "192.168.2.20".into(),
            loopback: false,
        }
    }

    fn connect(farm: &mut Farm, id: &str, identity: &Identity, conn: u64) -> Result<bool, String> {
        farm.hello(hello(id, &id.to_uppercase(), identity), conn, mpsc::unbounded_channel().0)
    }

    /// Everything about a machine that somebody else's connection must leave alone.
    fn seen(farm: &Farm, id: &str) -> (String, Option<String>, bool, u64, bool, String, String) {
        let m = &farm.machines[id];
        (m.name.clone(), m.fingerprint.clone(), m.approved, m.conn, m.online(), m.address.clone(), m.version.clone())
    }

    fn request(method: &str, target: &str, body: &'static [u8], signed: Option<&Signed>) -> Request {
        let mut builder = Request::builder().method(method).uri(target);
        for (name, value) in signed.map(|s| s.headers()).into_iter().flatten() {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(body)).unwrap()
    }

    async fn check(state: &ServerState, method: &str, target: &str, body: &'static [u8], signed: &Signed) -> Result<(), AuthError> {
        authenticate(state, request(method, target, body, Some(signed))).await.map(|_| ())
    }

    #[tokio::test]
    async fn a_request_signed_by_an_approved_machine_is_let_in_with_its_body() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let body = br#"{"action":"pause"}"#;
        let signed = a.sign("POST", "/api/v1/jobs/3/action", &auth::sha256_hex(body)).unwrap();
        let req = authenticate(&state, request("POST", "/api/v1/jobs/3/action", body, Some(&signed))).await.unwrap();
        assert_eq!(req.uri().path(), "/api/v1/jobs/3/action");
        assert_eq!(axum::body::to_bytes(req.into_body(), 1024).await.unwrap().as_ref(), body);
        let signed = a.sign("GET", "/api/v1/state?since=4", &auth::sha256_hex(EMPTY)).unwrap();
        assert_eq!(check(&state, "GET", "/api/v1/state?since=4", EMPTY, &signed).await, Ok(()));
    }

    #[tokio::test]
    async fn unsigned_requests_and_the_key_of_0_1_1_are_refused() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let unsigned = authenticate(&state, request("GET", "/api/v1/state", EMPTY, None)).await;
        assert_eq!(unsigned.err(), Some(AuthError::Missing));
        // The old header is refused by itself, and also next to a signature that is right.
        let old = Request::builder().uri("/api/v1/state").header(auth::LEGACY_KEY, "5f1d0c6a3b2e4f70").body(Body::empty()).unwrap();
        assert_eq!(authenticate(&state, old).await.err(), Some(AuthError::Legacy));
        let signed = a.sign("GET", "/api/v1/state", &auth::sha256_hex(EMPTY)).unwrap();
        let mut both = request("GET", "/api/v1/state", EMPTY, Some(&signed));
        both.headers_mut().insert(auth::LEGACY_KEY, "5f1d0c6a3b2e4f70".parse().unwrap());
        assert_eq!(authenticate(&state, both).await.err(), Some(AuthError::Legacy));
    }

    #[tokio::test]
    async fn only_approved_machines_are_let_in() {
        let dir = TestDir::new();
        let (state, a, p) = farm(&dir);
        let stranger = Identity::generate().unwrap();
        let empty = auth::sha256_hex(EMPTY);
        for (identity, expected) in [(&a, Ok(())), (&p, Err(AuthError::NotApproved)), (&stranger, Err(AuthError::NotApproved))] {
            let signed = identity.sign("GET", "/api/v1/state", &empty).unwrap();
            assert_eq!(check(&state, "GET", "/api/v1/state", EMPTY, &signed).await, expected);
        }
        // Approving the waiting machine lets it in. Somebody who comes with an approved machine's
        // id and another key gets nothing and takes nothing from that machine. A machine the farm
        // forgot is out.
        state.farm.lock().unwrap().machine_action("p", "approve").unwrap();
        let signed = p.sign("GET", "/api/v1/state", &empty).unwrap();
        assert_eq!(check(&state, "GET", "/api/v1/state", EMPTY, &signed).await, Ok(()));
        assert!(connect(&mut state.farm.lock().unwrap(), "a", &stranger, 3).is_err());
        for (identity, expected) in [(&a, Ok(())), (&stranger, Err(AuthError::NotApproved))] {
            let signed = identity.sign("GET", "/api/v1/state", &empty).unwrap();
            assert_eq!(check(&state, "GET", "/api/v1/state", EMPTY, &signed).await, expected);
        }
        {
            let mut farm = state.farm.lock().unwrap();
            farm.disconnected("p", 2);
            farm.machine_action("p", "forget").unwrap();
        }
        let signed = p.sign("GET", "/api/v1/state", &empty).unwrap();
        assert_eq!(check(&state, "GET", "/api/v1/state", EMPTY, &signed).await, Err(AuthError::NotApproved));
    }

    #[tokio::test]
    async fn a_copy_of_a_request_is_refused() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let body = br#"{"action":"cancel"}"#;
        let signed = a.sign("POST", "/api/v1/jobs/3/action", &auth::sha256_hex(body)).unwrap();
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action", body, &signed).await, Ok(()));
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action", body, &signed).await, Err(AuthError::Replayed));
    }

    #[tokio::test]
    async fn a_signature_is_good_for_one_request_only() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let body = br#"{"action":"pause"}"#;
        let signed = a.sign("POST", "/api/v1/jobs/3/action", &auth::sha256_hex(body)).unwrap();
        let bad = Err(AuthError::BadSignature);
        assert_eq!(check(&state, "POST", "/api/v1/jobs/4/action", body, &signed).await, bad, "path");
        assert_eq!(check(&state, "POST", "/api/v1/machines/3/action", body, &signed).await, bad, "path");
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action?x=1", body, &signed).await, bad, "query");
        assert_eq!(check(&state, "GET", "/api/v1/jobs/3/action", body, &signed).await, bad, "method");
        let other = br#"{"action":"cancel"}"#;
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action", other, &signed).await, Err(AuthError::BodyChanged), "body");
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action", EMPTY, &signed).await, Err(AuthError::BodyChanged), "no body");
        // None of the failed tries used the signature up: the real request still goes through.
        assert_eq!(check(&state, "POST", "/api/v1/jobs/3/action", body, &signed).await, Ok(()));
    }

    #[tokio::test]
    async fn requests_from_another_time_are_refused() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let empty = auth::sha256_hex(EMPTY);
        for (i, timestamp) in [now_ms() - auth::MAX_SKEW_MS - 60_000, now_ms() + auth::MAX_SKEW_MS + 60_000].into_iter().enumerate() {
            let signed = a.sign_at("GET", "/api/v1/state", &empty, timestamp, &format!("{i:032x}"));
            let refused = check(&state, "GET", "/api/v1/state", EMPTY, &signed).await.unwrap_err();
            assert!(matches!(refused, AuthError::Clock { .. }), "{refused:?}");
            assert!(refused.to_string().contains("clocks"));
        }
    }

    #[tokio::test]
    async fn only_a_scene_upload_may_leave_its_body_unsigned() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let scene = b"a scene of many gigabytes";
        let upload = "/api/v1/scenes?name=shot.max";
        let signed = a.sign("POST", upload, auth::UNSIGNED_PAYLOAD).unwrap();
        assert_eq!(check(&state, "POST", upload, scene, &signed).await, Ok(()));
        assert_eq!(check(&state, "POST", upload, scene, &signed).await, Err(AuthError::Replayed));
        // Its name, in the query, is signed.
        let signed = a.sign("POST", upload, auth::UNSIGNED_PAYLOAD).unwrap();
        assert_eq!(check(&state, "POST", "/api/v1/scenes?name=other.max", scene, &signed).await, Err(AuthError::BadSignature));
        // Everything else signs its body, and an upload does not pretend to.
        for target in ["/api/v1/jobs", "/api/v1/scenes/from-path", "/api/v1/machines/a/action"] {
            let signed = a.sign("POST", target, auth::UNSIGNED_PAYLOAD).unwrap();
            assert!(matches!(check(&state, "POST", target, b"{}", &signed).await, Err(AuthError::Malformed(_))), "{target}");
        }
        let signed = a.sign("POST", upload, &auth::sha256_hex(scene)).unwrap();
        assert!(matches!(check(&state, "POST", upload, scene, &signed).await, Err(AuthError::Malformed(_))));
    }

    #[tokio::test]
    async fn an_agent_proves_its_key_to_connect() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let newcomer = Identity::generate().unwrap();
        let uri: Uri = "/api/v1/agent".parse().unwrap();
        let headers = |signed: &Signed| {
            let mut headers = HeaderMap::new();
            for (name, value) in signed.headers() {
                headers.insert(name, value.parse().unwrap());
            }
            headers
        };
        let empty = auth::sha256_hex(EMPTY);
        // A machine the farm has never seen may connect (to wait for approval): what counts is
        // that the key is really its own.
        for identity in [&a, &newcomer] {
            let signed = identity.sign("GET", "/api/v1/agent", &empty).unwrap();
            assert_eq!(agent_fingerprint(&state, &Method::GET, &uri, &headers(&signed)), Ok(identity.fingerprint()));
            // The same request again is somebody else sending a copy.
            assert_eq!(agent_fingerprint(&state, &Method::GET, &uri, &headers(&signed)), Err(AuthError::Replayed));
        }
        assert_eq!(agent_fingerprint(&state, &Method::GET, &uri, &HeaderMap::new()), Err(AuthError::Missing));
        // A signature made for an API request does not open a connection.
        let signed = a.sign("GET", "/api/v1/state", &empty).unwrap();
        assert_eq!(agent_fingerprint(&state, &Method::GET, &uri, &headers(&signed)), Err(AuthError::BadSignature));
        let old = a.sign_at("GET", "/api/v1/agent", &empty, now_ms() - 3_600_000, "000102030405060708090a0b0c0d0e0f");
        assert!(matches!(agent_fingerprint(&state, &Method::GET, &uri, &headers(&old)), Err(AuthError::Clock { .. })));
    }

    #[tokio::test]
    async fn a_machine_is_approved_by_the_key_it_proved() {
        let dir = TestDir::new();
        let (state, a, p) = farm(&dir);
        let mut farm = state.farm.lock().unwrap();
        assert!(farm.trusts_fingerprint(&a.fingerprint()));
        assert!(!farm.trusts_fingerprint(&p.fingerprint()));
        assert!(!farm.trusts_fingerprint(""));
        // The server's own agent (loopback) needs no approval.
        let own = Identity::generate().unwrap();
        let from_here = |identity: &Identity| Hello { address: "127.0.0.1".into(), loopback: true, rank: 2, ..hello("own", "SERVER", identity) };
        assert_eq!(farm.hello(from_here(&own), 9, mpsc::unbounded_channel().0), Ok(true));
        assert!(farm.trusts_fingerprint(&own.fingerprint()));
        // Not even from this computer does another key take a known machine's id.
        let other = Identity::generate().unwrap();
        assert!(farm.hello(from_here(&other), 10, mpsc::unbounded_channel().0).is_err());
        assert!(farm.trusts_fingerprint(&own.fingerprint()) && !farm.trusts_fingerprint(&other.fingerprint()));
        drop(farm);
        // What the farm keeps on disk is the fingerprint: after a restart the same machines are
        // trusted, and the one that was waiting is still waiting, with its key.
        drop(state);
        let state = ServerState::new(&Config::new(), dir.path()).unwrap();
        let farm = state.farm.lock().unwrap();
        assert!(farm.trusts_fingerprint(&a.fingerprint()) && farm.trusts_fingerprint(&own.fingerprint()));
        assert!(!farm.trusts_fingerprint(&p.fingerprint()));
        assert_eq!(farm.machines["p"].fingerprint, Some(p.fingerprint()));
    }

    #[test]
    fn a_machine_id_with_another_key_changes_nothing() {
        let dir = TestDir::new();
        let (state, a, p) = farm(&dir);
        let impostor = Identity::generate().unwrap();
        let mut farm = state.farm.lock().unwrap();

        // The id of the approved machine, which is connected, with somebody else's key and name.
        let before = seen(&farm, "a");
        let refused = farm.hello(hello("a", "IMPOSTOR", &impostor), 7, mpsc::unbounded_channel().0).unwrap_err();
        assert!(refused.contains("another key") && refused.contains("forget the machine in Machines"), "{refused}");
        assert_eq!(seen(&farm, "a"), before);
        assert_eq!(before, ("A".to_string(), Some(a.fingerprint()), true, 1, true, "192.168.2.20".to_string(), VERSION.to_string()));
        assert!(farm.trusts_fingerprint(&a.fingerprint()) && !farm.trusts_fingerprint(&impostor.fingerprint()));
        // The owner pressing Approve after that approves nothing new.
        farm.machine_action("a", "approve").unwrap();
        assert_eq!(seen(&farm, "a"), before);
        assert!(!farm.trusts_fingerprint(&impostor.fingerprint()));

        // The same for a machine that is still waiting: its entry keeps the key it came with, and
        // that is the only key Approve can approve.
        let before = seen(&farm, "p");
        assert!(farm.hello(hello("p", "P", &impostor), 8, mpsc::unbounded_channel().0).is_err());
        assert_eq!(seen(&farm, "p"), before);
        farm.machine_action("p", "approve").unwrap();
        assert_eq!(farm.machines["p"].fingerprint, Some(p.fingerprint()));
        assert!(farm.trusts_fingerprint(&p.fingerprint()) && !farm.trusts_fingerprint(&impostor.fingerprint()));

        // The machine itself comes back with its own key as always.
        assert_eq!(connect(&mut farm, "a", &a, 9), Ok(true));
        assert_eq!(farm.machines["a"].conn, 9);
        // And the impostor is as welcome as anybody under an id of its own: waiting for approval.
        assert_eq!(connect(&mut farm, "i", &impostor, 10), Ok(false));
        assert!(!farm.trusts_fingerprint(&impostor.fingerprint()));
    }

    #[test]
    fn a_machine_that_lost_its_key_joins_again_once_it_is_forgotten() {
        let dir = TestDir::new();
        let (state, a, _) = farm(&dir);
        let new_key = Identity::generate().unwrap();
        let mut farm = state.farm.lock().unwrap();
        // The computer went offline and comes back with a new key: turned away.
        farm.disconnected("a", 1);
        assert_eq!(connect(&mut farm, "a", &new_key, 2), Err(farm::ANOTHER_KEY.to_string()));
        assert!(!farm.machines["a"].online());
        // The owner forgets the machine; now it enrols like a new one and waits for approval.
        farm.machine_action("a", "forget").unwrap();
        assert_eq!(connect(&mut farm, "a", &new_key, 3), Ok(false));
        assert!(!farm.trusts_fingerprint(&new_key.fingerprint()) && !farm.trusts_fingerprint(&a.fingerprint()));
        farm.machine_action("a", "approve").unwrap();
        assert!(farm.trusts_fingerprint(&new_key.fingerprint()) && !farm.trusts_fingerprint(&a.fingerprint()));
        // From now on the id belongs to the new key, and the old one is the one turned away.
        assert!(connect(&mut farm, "a", &a, 4).is_err());
        assert_eq!(farm.machines["a"].conn, 3);
    }

    #[test]
    fn the_key_of_a_waiting_machine_is_kept_across_a_restart() {
        let dir = TestDir::new();
        let (state, _, p) = farm(&dir);
        let impostor = Identity::generate().unwrap();
        drop(state);
        // After a restart nobody is connected. Whoever comes first under the waiting machine's id
        // must still be the machine that was waiting.
        let state = ServerState::new(&Config::new(), dir.path()).unwrap();
        let mut farm = state.farm.lock().unwrap();
        assert!(connect(&mut farm, "p", &impostor, 1).is_err());
        assert!(!farm.machines["p"].online());
        farm.machine_action("p", "approve").unwrap();
        assert!(farm.trusts_fingerprint(&p.fingerprint()) && !farm.trusts_fingerprint(&impostor.fingerprint()));
        assert_eq!(connect(&mut farm, "p", &p, 2), Ok(true));
    }

    #[test]
    fn a_machine_the_farm_has_no_key_for_is_bound_by_its_first_connection() {
        let dir = TestDir::new();
        // What a farm made by 0.1.1 is left with once its keys are erased: machines without one.
        drop(ServerState::new(&Config::new(), dir.path()).unwrap());
        db::Db::open(&dir.path().join("server/farm.sqlite"))
            .unwrap()
            .save_machine(&db::StoredMachine {
                id: "old".into(),
                name: "NODE-07".into(),
                fingerprint: None,
                approved: false,
                enabled: true,
                caps: Default::default(),
                rank: 0,
                last_seen: 0,
            })
            .unwrap();
        let state = ServerState::new(&Config::new(), dir.path()).unwrap();
        let mut farm = state.farm.lock().unwrap();
        let (first, second) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        // No key yet, so nothing to approve.
        assert!(farm.machine_action("old", "approve").is_err());
        assert!(!farm.machines["old"].approved);
        assert_eq!(connect(&mut farm, "old", &first, 1), Ok(false));
        assert!(connect(&mut farm, "old", &second, 2).is_err());
        farm.machine_action("old", "approve").unwrap();
        assert!(farm.trusts_fingerprint(&first.fingerprint()) && !farm.trusts_fingerprint(&second.fingerprint()));
    }

    /// The farm served on a loopback port of its own. Send on the returned channel to stop it.
    async fn farm_on_a_port(dir: &TestDir) -> (u16, ServerState, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut state = ServerState::new(&Config::new(), dir.path()).unwrap();
        state.port = port;
        let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(async move {
            let _ = axum::serve(listener, app).with_graceful_shutdown(async { drop(stopped.await) }).await;
        });
        (port, state, stop, served)
    }

    #[tokio::test]
    async fn the_server_keeps_web_pages_out_of_what_it_trusts_from_this_computer() {
        use crate::loopback::tests::send;
        let dir = TestDir::new();
        let (port, state, stop, served) = farm_on_a_port(&dir).await;
        let own = format!("127.0.0.1:{port}");
        let host = ("Host", own.as_str());
        let by_name = format!("localhost:{port}");
        let origin = format!("http://localhost:{port}");
        let json = ("Content-Type", "application/json");
        let scenes = || std::fs::read_dir(dir.path().join("server/scenes")).unwrap().count();

        // The local app relaying its user's requests, a script, the server's own agent: trusted
        // from this computer as before, with no `Origin` to show.
        assert_eq!(send(port, "GET /api/v1/state", &[host], "").await.0, 200);
        let (status, scene) = send(port, "POST /api/v1/scenes?name=shot.max", &[host], "a scene").await;
        assert!(status == 200 && scene.contains("\"file_name\":\"shot.max\""), "{status} {scene}");
        let stored = scenes();
        // The farm page opened in a browser on this computer.
        let page = [("Host", by_name.as_str()), ("Sec-Fetch-Site", "none")];
        assert_eq!(send(port, "GET /", &page, "").await.0, 200);
        let from_page = [("Host", by_name.as_str()), ("Origin", origin.as_str()), ("Sec-Fetch-Site", "same-origin"), json];
        assert_eq!(send(port, "GET /api/v1/state", &from_page, "").await.0, 200);
        assert_eq!(send(port, "POST /api/v1/machines/nobody/action", &from_page, r#"{"action":"enable"}"#).await.0, 400);

        // Any other page open in a browser on this computer.
        let rebound = format!("evil.example:{port}");
        let pages: [&[(&str, &str)]; 7] = [
            &[host, ("Origin", "http://evil.example")],
            &[host, ("Origin", "null")],
            // The window's own page is another origin for the server: it never calls it directly.
            &[host, ("Origin", "http://127.0.0.1:47102")],
            &[host, ("Sec-Fetch-Site", "cross-site")],
            &[host, ("Sec-Fetch-Site", "same-site")],
            &[("Host", rebound.as_str()), ("Sec-Fetch-Site", "same-origin")],
            &[("Host", "192.168.2.10:47100")],
        ];
        for page in pages {
            for (request, body) in [
                ("GET /api/v1/state", ""),
                ("GET /api/v1/jobs/1", ""),
                ("POST /api/v1/scenes?name=junk.max", "junk"),
                ("POST /api/v1/scenes/from-path", r#"{"path":"C:\\Windows\\win.ini"}"#),
                ("POST /api/v1/machines/nobody/action", r#"{"action":"approve"}"#),
                ("POST /api/v1/jobs", "{}"),
                ("GET /api/v1/scenes/0000000000000000000000000000000000000000000000000000000000000000", ""),
                ("GET /api/v1/agent", ""),
                ("OPTIONS /api/v1/jobs", ""),
                ("GET /", ""),
                ("GET /app.js", ""),
                ("GET /nothing-here", ""),
            ] {
                let mut headers = page.to_vec();
                if !body.is_empty() {
                    headers.push(json);
                }
                let (status, text) = send(port, request, &headers, body).await;
                assert_eq!(status, 403, "{request} with {page:?}: {text}");
            }
        }
        assert_eq!(scenes(), stored, "a refused upload was stored");
        // A page can open WebSockets to anywhere, but its browser names it: refused before the
        // signature is even looked at. From a program of this computer the signature is what counts.
        let socket = [("Connection", "Upgrade"), ("Upgrade", "websocket"), ("Sec-WebSocket-Version", "13"), ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")];
        let mut from_page = vec![host, ("Origin", "http://evil.example")];
        from_page.extend(socket);
        assert_eq!(send(port, "GET /api/v1/agent", &from_page, "").await.0, 403);
        let mut unsigned = vec![host];
        unsigned.extend(socket);
        assert_eq!(send(port, "GET /api/v1/agent", &unsigned, "").await.0, 426);

        let _ = stop.send(());
        let _ = served.await;
        drop(state);
    }

    /// Folders and files to ask for: `scenes` is the allowed folder, the rest is not.
    fn scene_tree(root: &Path) {
        for folder in ["scenes/sub", "scenes/folder.max", "scenes-evil", "outside"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in ["scenes/shot.max", "scenes/sub/deep.blend", "scenes-evil/steal.max", "outside/steal.max", "secret.txt"] {
            std::fs::write(root.join(file), file).unwrap();
        }
    }

    #[test]
    fn scenes_by_path_come_only_from_the_allowed_folders() {
        let dir = TestDir::new();
        let root = dir.path();
        scene_tree(root);
        let allowed = [root.join("scenes")];
        let real = |path: &str| std::fs::canonicalize(root.join(path)).unwrap();
        let ask = |path: PathBuf| scene_in_folders(&path, &allowed);

        // Inside, however it is spelled: other case, `..` that stays inside, the `\\?\` form.
        assert_eq!(ask(root.join(r"scenes\shot.max")), Some(real("scenes/shot.max")));
        assert_eq!(ask(root.join("scenes/sub/deep.blend")), Some(real("scenes/sub/deep.blend")));
        assert_eq!(ask(PathBuf::from(root.join(r"scenes\shot.max").to_string_lossy().to_uppercase())), Some(real("scenes/shot.max")));
        assert_eq!(ask(root.join(r"scenes\sub\..\shot.max")), Some(real("scenes/shot.max")));
        assert!(real("scenes/shot.max").to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(ask(real("scenes/shot.max")), Some(real("scenes/shot.max")));

        // Out through `..`.
        assert_eq!(ask(root.join(r"scenes\..\secret.txt")), None);
        assert_eq!(ask(root.join(r"scenes\sub\..\..\secret.txt")), None);
        assert_eq!(ask(root.join(r"scenes\..\scenes-evil\steal.max")), None);
        // A folder whose name only starts like the allowed one.
        assert_eq!(ask(root.join(r"scenes-evil\steal.max")), None);
        // Plainly somewhere else, in any spelling.
        assert_eq!(ask(root.join("secret.txt")), None);
        assert_eq!(ask(real("secret.txt")), None);
        assert_eq!(ask(PathBuf::from(r"C:\Windows\win.ini")), None);
        assert_eq!(ask(PathBuf::from(r"\\?\C:\Windows\win.ini")), None);
        // Not a file, or not there.
        assert_eq!(ask(root.join(r"scenes\folder.max")), None);
        assert_eq!(ask(root.join("scenes")), None);
        assert_eq!(ask(root.join(r"scenes\missing.max")), None);
        // Not a full path: it would depend on where the server runs.
        assert_eq!(ask(PathBuf::from(r"scenes\shot.max")), None);
        assert_eq!(ask(PathBuf::from("shot.max")), None);
        assert_eq!(ask(PathBuf::from(r"\scenes\shot.max")), None);
        assert_eq!(ask(PathBuf::from(r"C:scenes\shot.max")), None);
        // Devices.
        assert_eq!(ask(PathBuf::from(r"\\.\NUL")), None);
        assert_eq!(ask(root.join(r"scenes\NUL")), None);
        assert_eq!(ask(root.join(r"scenes\CON")), None);

        // No folder allowed, or one that does not exist: nothing. Several: any of them.
        assert_eq!(scene_in_folders(&root.join(r"scenes\shot.max"), &[]), None);
        assert_eq!(scene_in_folders(&root.join(r"scenes\shot.max"), &[root.join("nowhere")]), None);
        let two = [root.join("nowhere"), root.join("outside"), root.join("scenes")];
        assert_eq!(scene_in_folders(&root.join(r"outside\steal.max"), &two), Some(real("outside/steal.max")));
        assert_eq!(scene_in_folders(&root.join(r"scenes\shot.max"), &two), Some(real("scenes/shot.max")));
    }

    #[test]
    fn links_count_for_where_they_lead() {
        let dir = TestDir::new();
        let root = dir.path();
        scene_tree(root);
        let allowed = [root.join("scenes")];
        let real = |path: &str| std::fs::canonicalize(root.join(path)).unwrap();
        // A junction needs no special rights, so this part always runs on a normal disk.
        let junction = |link: &str, target: &str| {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(root.join(link))
                .arg(root.join(target))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if junction(r"scenes\jump", "outside") && junction(r"scenes\alias", r"scenes\sub") {
            // Out of the allowed folder through a link inside it: refused.
            assert_eq!(scene_in_folders(&root.join(r"scenes\jump\steal.max"), &allowed), None);
            // A link that stays inside is as good as the folder it points to.
            assert_eq!(scene_in_folders(&root.join(r"scenes\alias\deep.blend"), &allowed), Some(real("scenes/sub/deep.blend")));
            // And a link from outside to an allowed folder leads to an allowed file.
            assert!(junction("door", "scenes"));
            assert_eq!(scene_in_folders(&root.join(r"door\shot.max"), &allowed), Some(real("scenes/shot.max")));
        } else {
            eprintln!("junctions cannot be made in {}: not checked", root.display());
        }
        // A link to a file needs a right most users do not have: checked where it can be made.
        if std::os::windows::fs::symlink_file(root.join("secret.txt"), root.join(r"scenes\link.max")).is_ok() {
            assert_eq!(scene_in_folders(&root.join(r"scenes\link.max"), &allowed), None);
        }
    }

    #[test]
    fn inside_means_part_by_part() {
        let inside = |file: &str, folder: &str| is_inside(Path::new(file), Path::new(folder));
        assert!(inside(r"\\?\C:\scenes\a.max", r"\\?\C:\scenes"));
        assert!(inside(r"\\?\C:\Scenes\Shot\a.max", r"\\?\c:\scenes"));
        assert!(inside(r"\\?\C:\scenes\a.max", r"\\?\C:\"));
        assert!(!inside(r"\\?\C:\scenes-evil\a.max", r"\\?\C:\scenes"));
        assert!(!inside(r"\\?\C:\scenesa.max", r"\\?\C:\scenes"));
        assert!(!inside(r"\\?\C:\scenes", r"\\?\C:\scenes"));
        assert!(!inside(r"\\?\C:\a.max", r"\\?\C:\scenes"));
        // Another drive, a share against a drive.
        assert!(!inside(r"\\?\D:\scenes\a.max", r"\\?\C:\scenes"));
        assert!(!inside(r"\\?\UNC\nas\projects\a.max", r"\\?\C:\"));
        // Shares: the server and the share are compared whole, in any case.
        assert!(inside(r"\\?\UNC\nas\projects\shot\a.max", r"\\?\UNC\nas\projects"));
        assert!(inside(r"\\?\UNC\NAS\Projects\shot\a.max", r"\\?\UNC\nas\projects\Shot"));
        assert!(!inside(r"\\?\UNC\nas\projects-old\a.max", r"\\?\UNC\nas\projects"));
        assert!(!inside(r"\\?\UNC\nas2\projects\a.max", r"\\?\UNC\nas\projects"));
        assert!(!inside(r"\\?\UNC\nas\private\a.max", r"\\?\UNC\nas\projects"));
        assert!(!inside(r"\\?\UNC\nas\projects\a.max", r"\\?\UNC\nas\projects\shot"));
    }

    #[tokio::test]
    async fn other_machines_send_scenes_by_path_only_from_the_allowed_folders() {
        let dir = TestDir::new();
        let root = dir.path();
        scene_tree(root);
        let ask = |state: &ServerState, from: &str, path: PathBuf| {
            let addr: SocketAddr = from.parse().unwrap();
            api_scene_from_path(State(state.clone()), ConnectInfo(addr), Json(FromPath { path: path.to_string_lossy().into_owned() }))
        };
        let network = "192.168.2.20:50000";

        // No folder in the settings: the network gets nothing, this computer everything, as before.
        let state = ServerState::new(&Config::new(), &root.join("farm")).unwrap();
        let refused = ask(&state, network, root.join(r"scenes\shot.max")).await.err().unwrap();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert!(refused.1.contains("does not take scenes by their path"), "{}", refused.1);
        let local = ask(&state, "127.0.0.1:50000", root.join("secret.txt")).await.unwrap().0;
        assert_eq!(local.hash, blake3::hash(b"secret.txt").to_hex().to_string());
        drop(state);

        // With a folder: what is in it, and nothing else.
        let cfg = Config { scene_folders: vec![root.join("scenes").to_string_lossy().into_owned(), "relative".into()], ..Config::new() };
        let state = ServerState::new(&cfg, &root.join("farm")).unwrap();
        let scene = ask(&state, network, root.join(r"scenes\sub\deep.blend")).await.unwrap().0;
        assert_eq!((scene.file_name.as_str(), scene.size), ("deep.blend", 21));
        assert_eq!(scene.hash, blake3::hash(b"scenes/sub/deep.blend").to_hex().to_string());
        assert!(root.join("farm/server/scenes").join(&scene.hash).join("deep.blend").is_file());
        for path in [r"secret.txt", r"scenes\..\secret.txt", r"scenes-evil\steal.max", r"scenes\missing.max"] {
            let refused = ask(&state, network, root.join(path)).await.err().unwrap();
            assert_eq!(refused.0, StatusCode::FORBIDDEN, "{path}");
            // The same words whether the file exists or not.
            assert!(refused.1.contains("not a file in one of the folders"), "{}", refused.1);
        }
        assert!(ask(&state, "127.0.0.1:50000", root.join(r"scenes-evil\steal.max")).await.is_ok());
    }

    #[test]
    fn file_names_are_kept_inside_the_store() {
        assert_eq!(safe_file_name("kitchen.max").as_deref(), Some("kitchen.max"));
        assert_eq!(safe_file_name(r"C:\a\b\shot.blend").as_deref(), Some("shot.blend"));
        assert_eq!(safe_file_name(".."), None);
        assert_eq!(safe_file_name("a..b"), None);
        assert_eq!(safe_file_name(""), None);
    }
}
