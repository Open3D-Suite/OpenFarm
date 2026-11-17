// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent: this machine's part in the farm. It keeps one WebSocket to the server, reports what
//! the machine has and how busy it is, and runs the tasks it is given, one per slot (the CPU, and
//! each NVIDIA GPU).

pub mod hosts;
pub mod jobobj;
pub mod probe;
pub mod runner;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use openfarm_core::model::{Capabilities, SlotKind};
use openfarm_core::proto::{AgentMsg, ServerMsg, PROTOCOL};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
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
    let mut running: HashMap<u64, (CancellationToken, String)> = HashMap::new();
    let gpu_count = caps.gpus.len() as u32;
    loop {
        set_state(&status, "connecting", &server_url, "");
        let mut request = ws_url.as_str().into_client_request()?;
        request.headers_mut().insert("x-openfarm-rank", cfg.rank().to_string().parse()?);
        // The request that opens the connection is signed like any other (it has no body): that
        // is how the server knows which key this machine holds. Signed again for every attempt.
        let target = request.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
        let signed = identity.sign(request.method().as_str(), &target, &auth::sha256_hex(b""))?;
        for (name, value) in signed.headers() {
            request.headers_mut().insert(name, value.parse()?);
        }
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
                    match msg {
                        ServerMsg::Welcome { approved, server_name, .. } => {
                            welcomed = true;
                            let mut s = status.lock().unwrap();
                            s.farm = server_name;
                            s.state = if approved { "connected" } else { "waiting_approval" }.into();
                            s.message.clear();
                        }
                        ServerMsg::Error { error } => {
                            refused = Some(error);
                            break;
                        }
                        ServerMsg::SetEnabled { enabled } => status.lock().unwrap().enabled = enabled,
                        ServerMsg::Assign(a) => {
                            let (slot, gpu) = match a.slot {
                                SlotKind::Cpu => ("cpu".to_string(), None),
                                SlotKind::Gpu => {
                                    let busy: Vec<String> = running.values().map(|(_, s)| s.clone()).collect();
                                    let g = (0..gpu_count.max(1)).find(|g| !busy.contains(&format!("gpu{g}"))).unwrap_or(0);
                                    (format!("gpu{g}"), Some(g))
                                }
                            };
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
                                runner::run(ctx2, a, slot, gpu, token).await;
                                st.lock().unwrap().tasks.retain(|t| t.task != task_id);
                                // Tell the loop the slot is free (a Status message does it).
                                let _ = done_tx.send(AgentMsg::Status { telemetry: Default::default(), accepting: true });
                            });
                        }
                        ServerMsg::Cancel { task } => {
                            if let Some((t, _)) = running.get(&task) {
                                t.cancel();
                            }
                        }
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
        set_state(&status, "error", &server_url, &why);
        tokio::time::sleep(Duration::from_secs(3)).await;
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
