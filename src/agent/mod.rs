// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent: this machine's part in the farm. It keeps one WebSocket to the server, reports what
//! the machine has and how busy it is, and runs the tasks it is given, one per slot (the CPU, and
//! each NVIDIA GPU).

pub mod hosts;
// Calls Windows (job objects) to end a render host with every process it started: that takes `unsafe`.
#[allow(unsafe_code)]
pub mod jobobj;
pub mod probe;
pub mod runner;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use openfarm_core::model::{Capabilities, SlotKind};
use openfarm_core::proto::{AgentMsg, Assignment, PROTOCOL, ServerMsg};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use crate::auth::{self, Identity};
use crate::config::{Config, VERSION};
use hosts::HostProc;

/// What the local window shows about the connection.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AgentStatus {
    /// connecting | waiting_approval | connected | error
    pub state: String,
    pub server: String,
    pub farm: String,
    pub message: String,
    /// The farm lets this machine render (suspend/activate from the farm view).
    pub enabled: bool,
    /// This machine's own switch (suspend from its window).
    pub accepting: bool,
    pub tasks: Vec<RunningTask>,
    pub caps: Option<Capabilities>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningTask {
    pub task: u64,
    pub job: String,
    pub frames: String,
    pub slot: String,
}

pub struct AgentCtx {
    pub name: String,
    pub data: PathBuf,
    pub scripts: PathBuf,
    pub server_url: String,
    /// This machine's key pair: it signs what the agent asks of the server.
    pub identity: Arc<Identity>,
    pub caps: Capabilities,
    pub http: reqwest::Client,
    pub out: mpsc::UnboundedSender<AgentMsg>,
    /// Warm hosts by slot ("cpu", "gpu0", …).
    pub hosts: tokio::sync::Mutex<HashMap<String, HostProc>>,
    pub downloads: tokio::sync::Mutex<()>,
    /// The scenes in the cache whose content was checked against their hash since OpenFarm
    /// started. A file that is only "there" is not trusted (see `runner::ensure_scene`).
    pub checked_scenes: Mutex<std::collections::HashSet<PathBuf>>,
}

pub type SharedStatus = Arc<Mutex<AgentStatus>>;

/// The connection to the server.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The tasks in hand: how to stop each one and the slot it holds ("cpu", "gpu0", …).
type Running = HashMap<u64, (CancellationToken, String)>;

/// Runs the agent until the process ends, reconnecting whenever the server goes away.
pub async fn run(cfg: Config, data: PathBuf, status: SharedStatus, identity: Arc<Identity>) -> Result<()> {
    let agent_dir = data.join("agent");
    std::fs::create_dir_all(&agent_dir)?;
    let scripts = agent_dir.join("hosts");
    hosts::install_scripts(&scripts)?;
    let mut probe = probe::Probe::new();
    let caps = probe.capabilities(&cfg);
    tracing::info!(cpu = %caps.cpu, gpus = caps.gpus.len(), software = ?caps.software.iter().map(|s| &s.name).collect::<Vec<_>>(), "this machine");
    {
        let mut s = status.lock().unwrap();
        s.caps = Some(caps.clone());
        s.accepting = true;
        s.enabled = true;
    }
    let server_url = cfg.server_url();
    let ws_url = format!("{}/api/v1/agent", server_url.replacen("http://", "ws://", 1));
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<AgentMsg>();
    let ctx = Arc::new(AgentCtx {
        name: cfg.name.clone(),
        data: agent_dir,
        scripts,
        server_url: server_url.clone(),
        identity: identity.clone(),
        caps: caps.clone(),
        http,
        out: out_tx,
        hosts: Default::default(),
        downloads: Default::default(),
        checked_scenes: Default::default(),
    });
    let mut running = Running::new();
    loop {
        set_state(&status, "connecting", &server_url, "");
        let request = opening_request(&ws_url, cfg.rank(), &identity)?;
        let ws = match tokio_tungstenite::connect_async(request).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                set_state(&status, "error", &server_url, &refusal(&e).unwrap_or_else(|| format!("cannot reach the farm server: {e}")));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let (mut sink, mut stream) = ws.split();
        let hello = AgentMsg::Hello {
            machine_id: cfg.machine_id.clone(),
            name: cfg.name.clone(),
            version: VERSION.into(),
            protocol: PROTOCOL,
            caps: caps.clone(),
        };
        if sink.send(Message::Text(serde_json::to_string(&hello)?.into())).await.is_err() {
            continue;
        }
        // Messages queued while disconnected are stale: drop them.
        while out_rx.try_recv().is_ok() {}
        let why = session(&mut sink, &mut stream, &status, &ctx, &mut probe, &mut out_rx, &mut running).await?;
        set_state(&status, "error", &server_url, &why);
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// The request that opens the connection to the server, with this machine's rank and signature.
fn opening_request(ws_url: &str, rank: u8, identity: &Identity) -> Result<Request> {
    let mut request = ws_url.into_client_request()?;
    request.headers_mut().insert("x-openfarm-rank", rank.to_string().parse()?);
    // The request that opens the connection is signed like any other (it has no body): that
    // is how the server knows which key this machine holds. Signed again for every attempt.
    let target = request.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let signed = identity.sign(request.method().as_str(), &target, &auth::sha256_hex(b""))?;
    for (name, value) in signed.headers() {
        request.headers_mut().insert(name, value.parse()?);
    }
    Ok(request)
}

/// One connection to the server, from after `Hello` until it ends: takes what the server says,
/// sends what the runners report and this machine's status every few seconds. Stops the tasks in
/// hand and says why the connection ended.
async fn session(
    sink: &mut SplitSink<Socket, Message>,
    stream: &mut SplitStream<Socket>,
    status: &SharedStatus,
    ctx: &Arc<AgentCtx>,
    probe: &mut probe::Probe,
    out_rx: &mut mpsc::UnboundedReceiver<AgentMsg>,
    running: &mut Running,
) -> Result<String> {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    // Why the server ended the connection, when it said so, and whether it ever answered.
    let mut refused: Option<String> = None;
    let mut welcomed = false;
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) else {
                    tracing::warn!("unreadable message from the server: {text}");
                    continue;
                };
                match on_server_msg(msg, status, ctx, running) {
                    Heard::Welcome => welcomed = true,
                    Heard::Refused(error) => {
                        refused = Some(error);
                        break;
                    }
                    Heard::Other => {}
                }
            }
            Some(out) = out_rx.recv() => {
                // A finished runner leaves a placeholder Status: replace it with the real one.
                let out = match out {
                    AgentMsg::Status { .. } => {
                        let tasks: Vec<u64> = status.lock().unwrap().tasks.iter().map(|t| t.task).collect();
                        running.retain(|id, _| tasks.contains(id));
                        AgentMsg::Status { telemetry: probe.telemetry(), accepting: status.lock().unwrap().accepting }
                    }
                    other => other,
                };
                if sink.send(Message::Text(serde_json::to_string(&out)?.into())).await.is_err() {
                    break;
                }
            }
            _ = tick.tick() => {
                let msg = AgentMsg::Status { telemetry: probe.telemetry(), accepting: status.lock().unwrap().accepting };
                if sink.send(Message::Text(serde_json::to_string(&msg)?.into())).await.is_err() {
                    break;
                }
            }
        }
    }
    // Lost the server: it has already put our tasks back in the queue, so stop them here.
    for (_, (t, _)) in running.drain() {
        t.cancel();
    }
    let why = match refused {
        Some(error) => error,
        // A server that hangs up before saying anything did not understand `Hello`.
        None if !welcomed => "the farm server closed the connection without answering: it may run another version of OpenFarm (every computer of a farm needs the same one); trying again".to_string(),
        None => "lost the connection to the farm server; trying again".to_string(),
    };
    Ok(why)
}

/// What a message from the server means for the connection it came on.
#[derive(Debug, PartialEq)]
enum Heard {
    /// The server answered `Hello`.
    Welcome,
    /// The server ends the connection, and this is why.
    Refused(String),
    /// Nothing that changes the connection.
    Other,
}

/// Does what one message from the server says.
fn on_server_msg(msg: ServerMsg, status: &SharedStatus, ctx: &Arc<AgentCtx>, running: &mut Running) -> Heard {
    match msg {
        ServerMsg::Welcome { approved, server_name, .. } => {
            let mut s = status.lock().unwrap();
            s.farm = server_name;
            s.state = if approved { "connected" } else { "waiting_approval" }.into();
            s.message.clear();
            return Heard::Welcome;
        }
        ServerMsg::Error { error } => return Heard::Refused(error),
        ServerMsg::SetEnabled { enabled } => status.lock().unwrap().enabled = enabled,
        ServerMsg::Assign(a) => start_task(a, status, ctx, running),
        ServerMsg::Cancel { task } => {
            if let Some((t, _)) = running.get(&task) {
                t.cancel();
            }
        }
    }
    Heard::Other
}

/// Starts a task the server assigned, on a free slot of its kind.
fn start_task(a: Box<Assignment>, status: &SharedStatus, ctx: &Arc<AgentCtx>, running: &mut Running) {
    let (slot, gpu) = slot_for(a.slot, running, ctx.caps.gpus.len() as u32);
    let token = CancellationToken::new();
    running.insert(a.task, (token.clone(), slot.clone()));
    status.lock().unwrap().tasks.push(RunningTask {
        task: a.task,
        job: a.job_name.clone(),
        frames: openfarm_core::frames::format(&a.frames),
        slot: slot.clone(),
    });
    let ctx2 = ctx.clone();
    let st = status.clone();
    let task_id = a.task;
    let done_tx = ctx.out.clone();
    tokio::spawn(async move {
        runner::run(ctx2, *a, slot, gpu, token).await;
        st.lock().unwrap().tasks.retain(|t| t.task != task_id);
        // Tell the loop the slot is free (a Status message does it).
        let _ = done_tx.send(AgentMsg::Status { telemetry: Default::default(), accepting: true });
    });
}

/// The slot a new task takes, and the GPU it renders on: the CPU, or the first GPU with no task.
fn slot_for(kind: SlotKind, running: &Running, gpu_count: u32) -> (String, Option<u32>) {
    match kind {
        SlotKind::Cpu => ("cpu".to_string(), None),
        SlotKind::Gpu => {
            let busy: Vec<String> = running.values().map(|(_, s)| s.clone()).collect();
            let g = (0..gpu_count.max(1)).find(|g| !busy.contains(&format!("gpu{g}"))).unwrap_or(0);
            (format!("gpu{g}"), Some(g))
        }
    }
}

/// What the server answered when it refused the connection (the clocks differ, an old version),
/// in its own words.
fn refusal(e: &tokio_tungstenite::tungstenite::Error) -> Option<String> {
    let tokio_tungstenite::tungstenite::Error::Http(response) = e else { return None };
    let text = String::from_utf8_lossy(response.body().as_deref()?).trim().to_string();
    (!text.is_empty()).then(|| format!("the farm server refused the connection: {text}"))
}

fn set_state(status: &SharedStatus, state: &str, server: &str, message: &str) {
    let mut s = status.lock().unwrap();
    s.state = state.into();
    s.server = server.into();
    s.message = message.into();
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use ed25519_dalek::{Signature, VerifyingKey};
    use tokio_tungstenite::tungstenite::handshake::client::generate_request;

    use super::*;
    use crate::auth::Signed;
    use crate::config::now_ms;

    const WS_URL: &str = "ws://192.168.1.10:47100/api/v1/agent";
    /// SHA-256 of nothing: the request that opens the connection has no body.
    const NO_BODY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /// The public key of `test_identity`, worked out with another implementation (OpenSSL).
    const TEST_PUBLIC_KEY: &str = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";

    /// A key pair whose private key is the byte 7, 32 times.
    fn test_identity() -> Identity {
        Identity::from_secret(&[7u8; 32]).unwrap()
    }

    fn is_nonce(s: &str) -> bool {
        s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn the_request_that_opens_the_connection_goes_out_signed_as_the_server_expects() {
        let identity = test_identity();
        let before = now_ms();
        let request = opening_request(WS_URL, 0, &identity).unwrap();
        let after = now_ms();
        assert_eq!(request.method().as_str(), "GET");
        assert_eq!(request.uri().to_string(), WS_URL);
        let value = |name: &str| request.headers()[name].to_str().unwrap().to_string();
        let (key, timestamp, nonce, signature) = (value("sec-websocket-key"), value(auth::TIMESTAMP), value(auth::NONCE), value(auth::SIGNATURE));
        // Signed now, with a nonce of its own.
        let time: u64 = timestamp.parse().unwrap();
        assert!((before..=after).contains(&time), "{before} {time} {after}");
        assert!(is_nonce(&nonce), "{nonce}");
        // What is signed is these six lines and nothing else: checked here without `crate::auth`.
        let text = format!("OPENFARM-ED25519-V1\nGET\n/api/v1/agent\n{timestamp}\n{nonce}\n{NO_BODY}");
        let public_key: [u8; 32] = BASE64.decode(TEST_PUBLIC_KEY).unwrap().try_into().unwrap();
        let bytes: [u8; 64] = BASE64.decode(&signature).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&public_key).unwrap().verify_strict(text.as_bytes(), &Signature::from_bytes(&bytes)).unwrap();
        // The server reads the same out of the headers.
        let signed = Signed::from_headers(request.headers()).unwrap();
        assert_eq!(signed.verify("GET", "/api/v1/agent", time).unwrap(), identity.fingerprint());
        // The method, the path and the body's hash that it signs, at a fixed time and with a fixed
        // nonce, give the signature that OpenSSL gives for the same six lines.
        let target = request.uri().path_and_query().unwrap().as_str();
        let fixed = identity.sign_at(request.method().as_str(), target, &signed.content, 1_794_000_000_000, "000102030405060708090a0b0c0d0e0f");
        assert_eq!(fixed.signature, "wvbu7ci3rK6X5xLrNU9gbDRJ81Ct5rYkdk1DiBlPpXOiO8d0mqjTBlPbMuV3MAviyy0Fd3W83+fCtSFtjSHbCQ==");
        // Every byte that travels to the server, as tungstenite writes this request. Four values
        // change from one request to the next; the rest, and the order of the lines, do not.
        let (sent, _) = generate_request(request).unwrap();
        let expected = format!(
            "GET /api/v1/agent HTTP/1.1\r\n\
             Host: 192.168.1.10:47100\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             x-openfarm-signature: {signature}\r\n\
             x-openfarm-content-sha256: {NO_BODY}\r\n\
             x-openfarm-nonce: {nonce}\r\n\
             x-openfarm-timestamp: {timestamp}\r\n\
             x-openfarm-public-key: {TEST_PUBLIC_KEY}\r\n\
             x-openfarm-rank: 0\r\n\
             \r\n"
        );
        assert_eq!(String::from_utf8(sent).unwrap(), expected);
    }

    #[test]
    fn every_attempt_to_connect_is_signed_again_and_says_the_rank() {
        let identity = test_identity();
        let value = |request: &Request, name: &str| request.headers()[name].to_str().unwrap().to_string();
        let first = opening_request(WS_URL, 1, &identity).unwrap();
        let second = opening_request(WS_URL, 2, &identity).unwrap();
        assert_eq!((value(&first, "x-openfarm-rank"), value(&second, "x-openfarm-rank")), ("1".to_string(), "2".to_string()));
        assert_ne!(value(&first, auth::NONCE), value(&second, auth::NONCE));
        assert_ne!(value(&first, auth::SIGNATURE), value(&second, auth::SIGNATURE));
        assert_eq!(value(&first, auth::PUBLIC_KEY), value(&second, auth::PUBLIC_KEY));
        for request in [&first, &second] {
            let signed = Signed::from_headers(request.headers()).unwrap();
            assert_eq!(signed.verify("GET", "/api/v1/agent", now_ms()).unwrap(), identity.fingerprint());
        }
        // A server address that cannot be part of a URL: there is no request to send.
        assert!(opening_request("ws://not an address:47100/api/v1/agent", 1, &identity).is_err());
    }

    #[test]
    fn a_task_takes_the_cpu_or_the_first_gpu_without_a_task() {
        let cpu = ("cpu".to_string(), None);
        let gpu = |g: u32| (format!("gpu{g}"), Some(g));
        let mut running = Running::new();
        assert_eq!(slot_for(SlotKind::Cpu, &running, 2), cpu);
        assert_eq!(slot_for(SlotKind::Gpu, &running, 2), gpu(0));
        running.insert(1, (CancellationToken::new(), "gpu0".into()));
        assert_eq!(slot_for(SlotKind::Gpu, &running, 2), gpu(1));
        // The tasks on the CPU are not counted here: another one gets the same slot. It takes no GPU.
        running.insert(2, (CancellationToken::new(), "cpu".into()));
        assert_eq!(slot_for(SlotKind::Cpu, &running, 2), cpu);
        assert_eq!(slot_for(SlotKind::Gpu, &running, 2), gpu(1));
        // Every GPU has a task: the first one gets another.
        running.insert(3, (CancellationToken::new(), "gpu1".into()));
        assert_eq!(slot_for(SlotKind::Gpu, &running, 2), gpu(0));
        // A GPU task on a machine that reported no GPU still names one.
        assert_eq!(slot_for(SlotKind::Gpu, &Running::new(), 0), gpu(0));
    }

    fn test_ctx() -> Arc<AgentCtx> {
        Arc::new(AgentCtx {
            name: "NODE-07".into(),
            data: PathBuf::new(),
            scripts: PathBuf::new(),
            server_url: "http://192.168.1.10:47100".into(),
            identity: Arc::new(test_identity()),
            caps: Capabilities::default(),
            http: reqwest::Client::new(),
            out: mpsc::unbounded_channel().0,
            hosts: Default::default(),
            downloads: Default::default(),
            checked_scenes: Default::default(),
        })
    }

    #[test]
    fn the_servers_answer_to_hello_says_where_this_machine_stands() {
        let (ctx, status, mut running) = (test_ctx(), SharedStatus::default(), Running::new());
        set_state(&status, "connecting", "http://192.168.1.10:47100", "lost the connection to the farm server; trying again");
        let welcome =
            |approved| ServerMsg::Welcome { approved, server_name: "Studio farm".into(), server_version: VERSION.into(), protocol: PROTOCOL };
        let shown = || {
            let s = status.lock().unwrap();
            (s.state.clone(), s.farm.clone(), s.message.clone(), s.server.clone())
        };
        assert_eq!(on_server_msg(welcome(false), &status, &ctx, &mut running), Heard::Welcome);
        assert_eq!(shown(), ("waiting_approval".into(), "Studio farm".into(), String::new(), "http://192.168.1.10:47100".into()));
        assert_eq!(on_server_msg(welcome(true), &status, &ctx, &mut running), Heard::Welcome);
        assert_eq!(shown(), ("connected".into(), "Studio farm".into(), String::new(), "http://192.168.1.10:47100".into()));
        // A refusal is handed back in the server's words; the status is left for the end of the connection.
        let refusal = ServerMsg::Error { error: "This farm runs another version.".into() };
        assert_eq!(on_server_msg(refusal, &status, &ctx, &mut running), Heard::Refused("This farm runs another version.".into()));
        assert_eq!(shown().0, "connected");
    }

    #[test]
    fn the_server_suspends_this_machine_and_cancels_its_tasks() {
        let (ctx, status, mut running) = (test_ctx(), SharedStatus::default(), Running::new());
        status.lock().unwrap().enabled = true;
        assert_eq!(on_server_msg(ServerMsg::SetEnabled { enabled: false }, &status, &ctx, &mut running), Heard::Other);
        assert!(!status.lock().unwrap().enabled);
        assert_eq!(on_server_msg(ServerMsg::SetEnabled { enabled: true }, &status, &ctx, &mut running), Heard::Other);
        assert!(status.lock().unwrap().enabled);

        let (asked, other) = (CancellationToken::new(), CancellationToken::new());
        running.insert(4, (asked.clone(), "cpu".into()));
        running.insert(5, (other.clone(), "gpu0".into()));
        assert_eq!(on_server_msg(ServerMsg::Cancel { task: 4 }, &status, &ctx, &mut running), Heard::Other);
        assert!(asked.is_cancelled() && !other.is_cancelled());
        // A task this machine does not have: nothing to stop.
        assert_eq!(on_server_msg(ServerMsg::Cancel { task: 99 }, &status, &ctx, &mut running), Heard::Other);
        assert!(!other.is_cancelled());
        // A canceled task keeps its slot until its runner has ended.
        assert_eq!(running.len(), 2);
    }
}
