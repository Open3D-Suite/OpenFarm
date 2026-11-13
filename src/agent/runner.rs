// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs one task on one slot: gets the scene, makes sure the right warm host has it loaded, renders
//! the frames, checks every file and reports back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use openfarm_core::model::{Engine, Host};
use openfarm_core::proto::{AgentMsg, Assignment, TaskEvent};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::hosts::{progress_of, HostEvent, HostProc};
use super::AgentCtx;
use crate::imaging;

/// An error of the scene itself (no camera, wrong renderer): every machine would fail the same way.
#[derive(Debug)]
pub struct SceneError(pub String);

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SceneError {}

/// No output at all from the render program for this long = hung (overridable per job with the
/// `watchdog_minutes` setting).
const DEFAULT_WATCHDOG: Duration = Duration::from_secs(30 * 60);

struct Reporter {
    ctx: Arc<AgentCtx>,
    task: u64,
    log: Vec<String>,
    last_flush: Instant,
}

impl Reporter {
    fn send(&self, event: TaskEvent) {
        let _ = self.ctx.out.send(AgentMsg::Task { task: self.task, event });
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() >= 200 || self.last_flush.elapsed() > Duration::from_secs(1) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.log.is_empty() {
            let text = self.log.join("\n");
            self.log.clear();
            self.send(TaskEvent::Log { text });
        }
        self.last_flush = Instant::now();
    }
}

pub async fn run(ctx: Arc<AgentCtx>, a: Assignment, slot: String, gpu: Option<u32>, cancel: CancellationToken) {
    let mut rep = Reporter { ctx: ctx.clone(), task: a.task, log: Vec::new(), last_flush: Instant::now() };
    rep.send(TaskEvent::Started);
    rep.log(format!(
        "OpenFarm {} on {}: task {} of '{}', frames {}, {}{}",
        crate::config::VERSION,
        ctx.name,
        a.task,
        a.job_name,
        openfarm_core::frames::format(&a.frames),
        a.engine.label(),
        gpu.map(|g| format!(", GPU {g}")).unwrap_or_default()
    ));
    let result = run_inner(&ctx, &a, &slot, gpu, &cancel, &mut rep).await;
    let peak = None;
    match result {
        Ok(()) => {
            rep.flush();
            rep.send(TaskEvent::Finished { ok: true, error: None, fatal: false, peak_ram_mb: peak });
        }
        Err(e) => {
            let canceled = cancel.is_cancelled();
            let fatal = !canceled && e.chain().any(|c| c.downcast_ref::<SceneError>().is_some());
            let text = if canceled { "canceled".to_string() } else { format!("{e:#}") };
            if !canceled {
                rep.log(format!("ERROR: {text}"));
            }
            rep.flush();
            rep.send(TaskEvent::Finished { ok: false, error: Some(text), fatal, peak_ram_mb: peak });
        }
    }
}

async fn run_inner(
    ctx: &Arc<AgentCtx>,
    a: &Assignment,
    slot: &str,
    gpu: Option<u32>,
    cancel: &CancellationToken,
    rep: &mut Reporter,
) -> Result<()> {
    if !openfarm_core::model::is_absolute_path(&a.output.folder) {
        return Err(SceneError(format!("the output folder is not a full path: {}", a.output.folder)).into());
    }
    let tmp_dir = ctx.data.join("out").join(a.task.to_string());
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let outputs: Vec<(i32, PathBuf, PathBuf)> = a
        .frames
        .iter()
        .map(|&f| {
            let name = a.output.file_for(f);
            (f, tmp_dir.join(&name), PathBuf::from(&a.output.folder).join(&name))
        })
        .collect();
    let result = if a.engine == Engine::Dummy {
        dummy(ctx, a, &outputs, cancel, rep).await
    } else {
        let scene = ensure_scene(ctx, a, rep, cancel).await?;
        with_host(ctx, a, slot, gpu, &scene, &outputs, cancel, rep).await
    };
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    result
}

/// The scene from the agent's cache, downloading it from the server the first time.
async fn ensure_scene(ctx: &Arc<AgentCtx>, a: &Assignment, rep: &mut Reporter, cancel: &CancellationToken) -> Result<PathBuf> {
    let dir = ctx.data.join("cache").join("scenes").join(&a.scene.hash);
    let path = dir.join(&a.scene.file_name);
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }
    let _guard = ctx.downloads.lock().await; // one download at a time per machine
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }
    rep.log(format!("Downloading the scene {} ({:.1} MB)", a.scene.file_name, a.scene.size as f64 / 1e6));
    tokio::fs::create_dir_all(&dir).await?;
    let started = Instant::now();
    let resp = ctx
        .http
        .get(format!("{}{}", ctx.server_url, a.scene_url))
        .header("x-openfarm-key", &ctx.key)
        .send()
        .await
        .context("downloading the scene")?;
    if !resp.status().is_success() {
        bail!("the server did not give the scene ({})", resp.status());
    }
    let part = dir.join(format!("{}.part", a.scene.file_name));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            bail!("canceled");
        }
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, &path).await?;
    rep.log(format!("Scene downloaded in {:.1} s", started.elapsed().as_secs_f64()));
    Ok(path)
}

#[allow(clippy::too_many_arguments)]
async fn with_host(
    ctx: &Arc<AgentCtx>,
    a: &Assignment,
    slot: &str,
    gpu: Option<u32>,
    scene: &Path,
    outputs: &[(i32, PathBuf, PathBuf)],
    cancel: &CancellationToken,
    rep: &mut Reporter,
) -> Result<()> {
    let kind = a.engine.host();
    let existing = ctx.hosts.lock().await.remove(slot);
    let reuse = match existing {
        Some(mut h) => {
            if h.kind == kind && h.gpu == gpu && h.alive() {
                Some(h)
            } else {
                h.quit().await;
                None
            }
        }
        None => None,
    };
    let mut host = match reuse {
        Some(h) => h,
        None => {
            rep.log(format!("Starting {}…", if kind == Host::Max { "3ds Max" } else { "Blender" }));
            let t = Instant::now();
            let logs = ctx.data.join("logs");
            let h = tokio::select! {
                h = HostProc::spawn(kind, &ctx.caps, &ctx.scripts, gpu, &logs) => h?,
                _ = cancel.cancelled() => bail!("canceled"),
            };
            rep.log(format!("Render program ready in {:.1} s", t.elapsed().as_secs_f64()));
            h
        }
    };
    let watchdog = a
        .settings
        .get("watchdog_minutes")
        .and_then(|v| v.as_f64())
        .map(|m| Duration::from_secs_f64(m * 60.0))
        .unwrap_or(DEFAULT_WATCHDOG);
    let result = drive(ctx, a, &mut host, scene, outputs, cancel, rep, watchdog).await;
    match &result {
        Ok(()) => {
            ctx.hosts.lock().await.insert(slot.to_string(), host);
        }
        Err(_) => {
            // After any failure the host may be in a bad state: start a fresh one next time.
            host.kill();
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    ctx: &Arc<AgentCtx>,
    a: &Assignment,
    host: &mut HostProc,
    scene: &Path,
    outputs: &[(i32, PathBuf, PathBuf)],
    cancel: &CancellationToken,
    rep: &mut Reporter,
    watchdog: Duration,
) -> Result<()> {
    if host.scene.as_deref() != Some(a.scene.hash.as_str()) {
        let original_dir = Path::new(&a.scene.original_path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        host.send(&json!({"op": "load", "scene": scene.to_string_lossy(), "original_dir": original_dir})).await?;
        let t = Instant::now();
        wait_done(host, cancel, rep, watchdog, |_| Ok(())).await.context("loading the scene")?;
        host.scene = Some(a.scene.hash.clone());
        rep.log(format!("Scene loaded in {:.1} s", t.elapsed().as_secs_f64()));
    }
    let files: serde_json::Map<String, Value> = outputs.iter().map(|(f, tmp, _)| (f.to_string(), json!(tmp.to_string_lossy()))).collect();
    host.send(&json!({
        "op": "render",
        "engine": a.engine,
        "camera": a.camera,
        "frames": a.frames,
        "files": files,
        "format": a.output.format,
        "resolution": a.resolution,
        "device": a.device,
        "settings": a.settings,
    }))
    .await?;
    let mut done = 0usize;
    let mut current: Option<i32> = None;
    wait_done(host, cancel, rep, watchdog, |ev| {
        match ev {
            HostFrame::Start(f) => current = Some(f),
            HostFrame::Progress(p) => {
                if let Some(f) = current {
                    rep_send(ctx, a.task, TaskEvent::Progress { frame: f, pct: p });
                }
            }
            HostFrame::Done(f, secs, extra) => {
                if let Some((_, tmp, dest)) = outputs.iter().find(|o| o.0 == f) {
                    match finish_frame(tmp, dest) {
                        Ok((thumb, warning)) => {
                            let mut files = vec![dest.to_string_lossy().into_owned()];
                            files.extend(extra);
                            rep_send(ctx, a.task, TaskEvent::FrameDone { frame: f, secs, files, thumb, warning });
                            done += 1;
                        }
                        Err(e) => rep_send(ctx, a.task, TaskEvent::FrameFailed { frame: f, error: format!("{e:#}") }),
                    }
                }
                current = None;
            }
            HostFrame::Failed(f, error) => {
                rep_send(ctx, a.task, TaskEvent::FrameFailed { frame: f, error });
                current = None;
            }
        }
        Ok(())
    })
    .await?;
    if done < outputs.len() {
        bail!("{} of {} frames were not rendered", outputs.len() - done, outputs.len());
    }
    Ok(())
}

fn rep_send(ctx: &Arc<AgentCtx>, task: u64, event: TaskEvent) {
    let _ = ctx.out.send(AgentMsg::Task { task, event });
}

enum HostFrame {
    Start(i32),
    Progress(f32),
    Done(i32, f64, Vec<String>),
    Failed(i32, String),
}

/// Reads the host until the command ends, passing frame events to `on_frame` and printed lines to
/// the task log.
async fn wait_done(
    host: &mut HostProc,
    cancel: &CancellationToken,
    rep: &mut Reporter,
    watchdog: Duration,
    mut on_frame: impl FnMut(HostFrame) -> Result<()>,
) -> Result<()> {
    loop {
        let ev = tokio::select! {
            ev = tokio::time::timeout(watchdog, host.next()) => match ev {
                Ok(ev) => ev,
                Err(_) => bail!("the render program said nothing for {} minutes and was stopped", watchdog.as_secs() / 60),
            },
            _ = cancel.cancelled() => bail!("canceled"),
        };
        match ev {
            HostEvent::Closed => bail!("the render program closed unexpectedly"),
            HostEvent::Output(line) => {
                if let Some(p) = progress_of(&line) {
                    on_frame(HostFrame::Progress(p))?;
                }
                rep.log(line);
            }
            HostEvent::Event(v) => match v["ev"].as_str().unwrap_or("") {
                "done" => return Ok(()),
                "error" => {
                    let msg = v["msg"].as_str().unwrap_or("unknown error").to_string();
                    rep.log(msg.clone());
                    if v["fatal"].as_bool().unwrap_or(false) {
                        return Err(SceneError(first_line(&msg)).into());
                    }
                    return Err(anyhow!(first_line(&msg)));
                }
                "log" => rep.log(v["text"].as_str().unwrap_or("").to_string()),
                "frame_start" => on_frame(HostFrame::Start(v["frame"].as_i64().unwrap_or(0) as i32))?,
                "progress" => on_frame(HostFrame::Progress(v["pct"].as_f64().unwrap_or(0.0) as f32))?,
                "frame_done" => {
                    let extra = v["extra"].as_array().map(|x| x.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
                    on_frame(HostFrame::Done(v["frame"].as_i64().unwrap_or(0) as i32, v["secs"].as_f64().unwrap_or(0.0), extra))?
                }
                "frame_failed" => on_frame(HostFrame::Failed(
                    v["frame"].as_i64().unwrap_or(0) as i32,
                    v["error"].as_str().unwrap_or("failed").to_string(),
                ))?,
                _ => {}
            },
        }
    }
}

fn first_line(s: &str) -> String {
    // Python tracebacks end with the useful line.
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(s).trim().to_string()
}

/// Checks a frame the host wrote, makes its preview and moves it to the job's output folder.
fn finish_frame(tmp: &Path, dest: &Path) -> Result<(Option<String>, Option<String>)> {
    if !tmp.exists() {
        bail!("the render program reported the frame but wrote no file (no licence, wrong output path?)");
    }
    let inspection = imaging::inspect(tmp)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if std::fs::rename(tmp, dest).is_err() {
        // Another drive or a share: copy next to it, then rename, so nobody reads half a file.
        let part = dest.with_extension(format!("{}.part", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
        std::fs::copy(tmp, &part).with_context(|| format!("writing {}", dest.display()))?;
        let _ = std::fs::remove_file(dest);
        std::fs::rename(&part, dest)?;
        let _ = std::fs::remove_file(tmp);
    }
    let thumb = inspection.thumb_jpeg.map(|j| base64::engine::general_purpose::STANDARD.encode(j));
    Ok((thumb, inspection.warning))
}

/// The test renderer: waits `secs` per frame and writes a gradient with the frame's colour.
async fn dummy(
    ctx: &Arc<AgentCtx>,
    a: &Assignment,
    outputs: &[(i32, PathBuf, PathBuf)],
    cancel: &CancellationToken,
    rep: &mut Reporter,
) -> Result<()> {
    let secs = a.settings.get("secs").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let fail: Vec<i64> = a.settings.get("fail_frames").and_then(|v| v.as_array()).map(|x| x.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let black = a.settings.get("black").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut failed = 0;
    for (f, tmp, dest) in outputs {
        rep.log(format!("Test render of frame {f}"));
        let t = Instant::now();
        let steps = 10;
        for i in 0..steps {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(secs / steps as f64)) => {}
                _ = cancel.cancelled() => bail!("canceled"),
            }
            rep_send(ctx, a.task, TaskEvent::Progress { frame: *f, pct: (i + 1) as f32 * 100.0 / steps as f32 });
        }
        if fail.contains(&(*f as i64)) {
            rep_send(ctx, a.task, TaskEvent::FrameFailed { frame: *f, error: "asked to fail (test)".into() });
            failed += 1;
            continue;
        }
        let frame = *f;
        let tmp2 = tmp.clone();
        tokio::task::spawn_blocking(move || {
            let img = image::RgbImage::from_fn(320, 180, |x, y| {
                if black {
                    image::Rgb([0, 0, 0])
                } else {
                    let hue = (frame.rem_euclid(24) as f32) / 24.0;
                    image::Rgb([(255.0 * hue) as u8, (x * 255 / 320) as u8, (y * 255 / 180) as u8])
                }
            });
            img.save(&tmp2)
        })
        .await??;
        match finish_frame(tmp, dest) {
            Ok((thumb, warning)) => rep_send(
                ctx,
                a.task,
                TaskEvent::FrameDone { frame: *f, secs: t.elapsed().as_secs_f64(), files: vec![dest.to_string_lossy().into_owned()], thumb, warning },
            ),
            Err(e) => {
                rep_send(ctx, a.task, TaskEvent::FrameFailed { frame: *f, error: format!("{e:#}") });
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} frame(s) failed");
    }
    Ok(())
}
