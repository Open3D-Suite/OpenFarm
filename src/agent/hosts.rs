// SPDX-License-Identifier: GPL-3.0-or-later
//! Warm render hosts: a Blender or 3ds Max process that stays open between tasks and takes orders
//! over a local socket, so the scene is loaded once per job instead of once per frame (Phase 0:
//! Max takes 25-60 s to start; a 14 M-face Blender scene renders 5.6x faster per frame this way).
//!
//! Protocol (one JSON object per line): the host connects, says `{"ev":"ready","token":…}`, then
//! answers each command with events and ends it with `{"ev":"done"}` or `{"ev":"error"}`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use openfarm_core::model::{Capabilities, Host};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use super::jobobj::ProcessGroup;

pub const MAX_HOST: &str = include_str!("../../hosts/max_host.py");
pub const BLENDER_HOST: &str = include_str!("../../hosts/blender_host.py");

/// Writes the host scripts next to the agent's data (they must match this version of OpenFarm).
pub fn install_scripts(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("max_host.py"), MAX_HOST)?;
    std::fs::write(dir.join("blender_host.py"), BLENDER_HOST)?;
    Ok(())
}

pub enum HostEvent {
    Event(Value),
    /// A line the program printed (logs, render progress).
    Output(String),
    Closed,
}

pub struct HostProc {
    pub kind: Host,
    pub gpu: Option<u32>,
    /// Hash of the scene loaded now.
    pub scene: Option<String>,
    pub started: Instant,
    child: tokio::process::Child,
    pub group: ProcessGroup,
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    output: mpsc::UnboundedReceiver<String>,
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl HostProc {
    pub async fn spawn(kind: Host, caps: &Capabilities, scripts: &Path, gpu: Option<u32>, logs: &Path) -> Result<HostProc> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let program = |name: &str| -> Result<PathBuf> {
            caps.software
                .iter()
                .find(|s| s.name == name)
                .map(|s| PathBuf::from(&s.path))
                .ok_or_else(|| anyhow!("{name} is not installed on this machine"))
        };
        let mut cmd = match kind {
            Host::Blender => {
                let mut c = tokio::process::Command::new(program("blender")?);
                c.arg("-b")
                    .arg("--python")
                    .arg(scripts.join("blender_host.py"))
                    .arg("--")
                    .arg("--port")
                    .arg(port.to_string())
                    .arg("--token")
                    .arg(&token);
                c
            }
            Host::Max => {
                std::fs::create_dir_all(logs)?;
                let mut c = tokio::process::Command::new(program("3dsmax")?);
                c.arg(scripts.join("max_host.py"))
                    .arg("-mxsString")
                    .arg(format!("port:{port}"))
                    .arg("-mxsString")
                    .arg(format!("token:{token}"))
                    .arg("-dm")
                    .arg("on")
                    .arg("-v")
                    .arg("3")
                    .arg("-listenerlog")
                    .arg(logs.join(format!("listener-{port}.log")));
                c
            }
            Host::Dummy => bail!("the test renderer runs inside the agent"),
        };
        if let Some(g) = gpu {
            cmd.env("CUDA_VISIBLE_DEVICES", g.to_string());
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn().context("starting the render program")?;
        let group = ProcessGroup::new()?;
        if let Some(pid) = child.id() {
            group.add(pid)?;
        }
        let (tx, output) = mpsc::unbounded_channel();
        if let Some(out) = child.stdout.take() {
            tokio::spawn(read_lines(out, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(read_lines(err, tx));
        }
        let wait = match kind {
            Host::Max => Duration::from_secs(300),
            _ => Duration::from_secs(120),
        };
        let deadline = tokio::time::Instant::now() + wait;
        let stream = loop {
            tokio::select! {
                r = listener.accept() => break r?.0,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if let Some(status) = child.try_wait()? {
                        bail!("the render program closed while starting ({status})");
                    }
                    if tokio::time::Instant::now() > deadline {
                        group.kill();
                        bail!("the render program did not start in {} s", wait.as_secs());
                    }
                }
            }
        };
        stream.set_nodelay(true)?;
        let (read, writer) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await??.unwrap_or_default();
        let hello: Value = serde_json::from_str(&first).unwrap_or_default();
        if hello["ev"] != "ready" || hello["token"] != token.as_str() {
            group.kill();
            bail!("the render program answered something unexpected: {first}");
        }
        Ok(HostProc { kind, gpu, scene: None, started: Instant::now(), child, group, lines, writer, output })
    }

    pub async fn send(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn next(&mut self) -> HostEvent {
        tokio::select! {
            line = self.lines.next_line() => match line {
                Ok(Some(l)) => match serde_json::from_str(&l) {
                    Ok(v) => HostEvent::Event(v),
                    Err(_) => HostEvent::Output(l),
                },
                _ => HostEvent::Closed,
            },
            Some(out) = self.output.recv() => HostEvent::Output(out),
        }
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        self.group.kill();
        let _ = self.child.start_kill();
    }

    /// Asks the host to close, then makes sure (Max may hang on exit).
    pub async fn quit(mut self) {
        let _ = self.send(&serde_json::json!({"op": "quit"})).await;
        if tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await.is_err() {
            self.kill();
        }
    }
}

/// Lines a program prints. 3dsmaxbatch prints UTF-16, so NUL bytes are dropped first.
async fn read_lines(mut r: impl AsyncRead + Unpin, tx: mpsc::UnboundedSender<String>) {
    let mut buf = vec![0u8; 16 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        pending.extend(buf[..n].iter().copied().filter(|b| *b != 0));
        while let Some(i) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line).trim_end().to_string();
            if !text.is_empty() && tx.send(text).is_err() {
                return;
            }
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&pending).trim_end().to_string());
    }
}

/// Render progress in a printed line, if it has any: Blender's `Sample 32/128`.
pub fn progress_of(line: &str) -> Option<f32> {
    let i = line.rfind("Sample ")?;
    let rest = &line[i + 7..];
    let (a, b) = rest.split_once('/')?;
    let done: f32 = a.trim().parse().ok()?;
    let total: f32 = b.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    (total > 0.0).then(|| (done / total * 100.0).min(100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blender_progress() {
        assert_eq!(progress_of("Fra:1 Mem:12M | Time:00:01.20 | Scene, ViewLayer | Sample 32/128"), Some(25.0));
        assert_eq!(progress_of("Fra:1 | Sample 64/64 (Using optix)"), Some(100.0));
        assert_eq!(progress_of("Saved: 'x.exr'"), None);
    }
}
