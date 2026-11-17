// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs one task on one slot: gets the scene, makes sure the right warm host has it loaded, renders
//! the frames, checks every file and reports back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use futures_util::StreamExt;
use openfarm_core::model::{Engine, Host};
use openfarm_core::proto::{AgentMsg, Assignment, TaskEvent};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::AgentCtx;
use super::hosts::{HostEvent, HostProc, progress_of};
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

async fn run_inner(ctx: &Arc<AgentCtx>, a: &Assignment, slot: &str, gpu: Option<u32>, cancel: &CancellationToken, rep: &mut Reporter) -> Result<()> {
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
///
/// A job names its scene by the BLAKE3 hash of the file, and nothing that is not that file is
/// handed to the render program. A download is hashed as it arrives and only kept if it is the
/// scene. A file already in the cache is hashed the first time this run of OpenFarm uses it: it
/// may have been stored by a version that did not check, or have changed on disk since, so being
/// there does not make it trusted. Whatever fails the check is deleted.
async fn ensure_scene(ctx: &Arc<AgentCtx>, a: &Assignment, rep: &mut Reporter, cancel: &CancellationToken) -> Result<PathBuf> {
    let wanted = a.scene.hash.as_str();
    // Also what keeps the hash, which becomes a folder name, from being anything else.
    if !is_blake3(wanted) {
        bail!("the job does not name its scene by a BLAKE3 hash, so the scene cannot be checked");
    }
    let dir = ctx.data.join("cache").join("scenes").join(wanted);
    let path = dir.join(&a.scene.file_name);
    let checked = || ctx.checked_scenes.lock().unwrap().contains(&path);
    if checked() && tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }
    let _guard = ctx.downloads.lock().await; // one download at a time per machine
    if checked() && tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        if hash_of(path.clone()).await? == wanted {
            ctx.checked_scenes.lock().unwrap().insert(path.clone());
            return Ok(path);
        }
        rep.log(format!("The copy of {} kept on this machine is not the scene the job names: deleted, downloading it again", a.scene.file_name));
        tokio::fs::remove_file(&path).await?;
    }
    rep.log(format!("Downloading the scene {} ({:.1} MB)", a.scene.file_name, a.scene.size as f64 / 1e6));
    tokio::fs::create_dir_all(&dir).await?;
    let started = Instant::now();
    let url = reqwest::Url::parse(&format!("{}{}", ctx.server_url, a.scene_url)).context("the scene's address")?;
    let resp =
        ctx.identity.request(&ctx.http, reqwest::Method::GET, url, &crate::auth::sha256_hex(b""))?.send().await.context("downloading the scene")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let why = resp.text().await.unwrap_or_default();
        bail!("the server did not give the scene ({status}) {}", why.trim());
    }
    store_scene(resp.bytes_stream(), &path, wanted, cancel).await?;
    ctx.checked_scenes.lock().unwrap().insert(path.clone());
    rep.log(format!("Scene downloaded and checked in {:.1} s", started.elapsed().as_secs_f64()));
    Ok(path)
}

/// Is this what a BLAKE3 hash looks like in hex (64 lowercase digits)?
fn is_blake3(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The BLAKE3 hash of a file, in hex.
async fn hash_of(path: PathBuf) -> Result<String> {
    tokio::task::spawn_blocking(move || -> Result<String> {
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut std::fs::File::open(&path)?, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await?
}

/// Keeps a scene that is arriving as `path`, but only if it turns out to be the file whose hash is
/// `wanted`. It is written next to its place and hashed as it comes; a file that is not the scene
/// (changed or damaged on the way), or that did not arrive whole, is deleted.
async fn store_scene<S, B, E>(mut stream: S, path: &Path, wanted: &str, cancel: &CancellationToken) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let received: Result<String> = async {
        let mut file = tokio::fs::File::create(&part).await?;
        let mut hasher = blake3::Hasher::new();
        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                bail!("canceled");
            }
            let chunk = chunk.context("downloading the scene")?;
            hasher.update(chunk.as_ref());
            file.write_all(chunk.as_ref()).await?;
        }
        file.flush().await?;
        Ok(hasher.finalize().to_hex().to_string())
    }
    .await;
    let arrived = match received {
        Ok(hash) => hash,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    if arrived != wanted {
        let _ = tokio::fs::remove_file(&part).await;
        bail!(
            "the scene that arrived is not the one the job names (its BLAKE3 is {arrived}, not {wanted}): it was changed or damaged on its way from the farm server and was deleted without rendering it"
        );
    }
    tokio::fs::rename(&part, path).await?;
    Ok(())
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
    let watchdog = a.settings.get("watchdog_minutes").and_then(|v| v.as_f64()).map(|m| Duration::from_secs_f64(m * 60.0)).unwrap_or(DEFAULT_WATCHDOG);
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
                "frame_failed" => {
                    on_frame(HostFrame::Failed(v["frame"].as_i64().unwrap_or(0) as i32, v["error"].as_str().unwrap_or("failed").to_string()))?
                }
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
    let fail: Vec<i64> =
        a.settings.get("fail_frames").and_then(|v| v.as_array()).map(|x| x.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use openfarm_core::model::{OutputSpec, SceneRef, SlotKind};

    use super::*;
    use crate::TestDir;

    /// A farm server reduced to its scene store: it gives `content` to whoever asks for a scene,
    /// whatever the scene, and counts how often it was asked.
    struct Store {
        content: Mutex<Vec<u8>>,
        asked: AtomicUsize,
    }

    impl Store {
        fn new(content: &[u8]) -> Arc<Store> {
            Arc::new(Store { content: Mutex::new(content.to_vec()), asked: AtomicUsize::new(0) })
        }

        fn gives(&self, content: &[u8]) {
            *self.content.lock().unwrap() = content.to_vec();
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }

        /// Starts serving and returns the server's address.
        async fn serve(self: &Arc<Store>) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let store = self.clone();
            let scene = axum::routing::get(move || {
                let store = store.clone();
                async move {
                    store.asked.fetch_add(1, Ordering::SeqCst);
                    store.content.lock().unwrap().clone()
                }
            });
            let app = axum::Router::new().route("/api/v1/scenes/{hash}", scene);
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            url
        }
    }

    /// An agent as it is right after OpenFarm starts: nothing checked yet.
    fn agent(dir: &TestDir, server_url: &str) -> Arc<AgentCtx> {
        Arc::new(AgentCtx {
            name: "NODE".into(),
            data: dir.path().join("agent"),
            scripts: dir.path().join("agent").join("hosts"),
            server_url: server_url.into(),
            identity: Arc::new(crate::auth::Identity::generate().unwrap()),
            caps: Default::default(),
            http: reqwest::Client::new(),
            out: tokio::sync::mpsc::unbounded_channel().0,
            hosts: Default::default(),
            downloads: Default::default(),
            checked_scenes: Default::default(),
        })
    }

    /// A task of a job whose scene is `scene`.
    fn task_for(scene: &[u8]) -> Assignment {
        let hash = blake3::hash(scene).to_hex().to_string();
        Assignment {
            task: 1,
            job: 1,
            job_name: "shot".into(),
            engine: Engine::Cycles,
            scene_url: format!("/api/v1/scenes/{hash}"),
            scene: SceneRef { hash, file_name: "shot.blend".into(), original_path: String::new(), size: scene.len() as u64 },
            frames: vec![1],
            camera: None,
            output: OutputSpec { folder: r"\\nas\renders".into(), name: "shot_####".into(), format: "png".into() },
            device: None,
            resolution: None,
            settings: Default::default(),
            slot: SlotKind::Cpu,
        }
    }

    async fn fetch(ctx: &Arc<AgentCtx>, a: &Assignment) -> Result<PathBuf> {
        let mut rep = Reporter { ctx: ctx.clone(), task: a.task, log: Vec::new(), last_flush: Instant::now() };
        ensure_scene(ctx, a, &mut rep, &CancellationToken::new()).await
    }

    /// Where the agent keeps the scene of a task, and how many files are in that folder.
    fn cache(dir: &TestDir, a: &Assignment) -> (PathBuf, usize) {
        let path = dir.path().join("agent").join("cache").join("scenes").join(&a.scene.hash).join(&a.scene.file_name);
        let files = std::fs::read_dir(path.parent().unwrap()).map(|d| d.count()).unwrap_or(0);
        (path, files)
    }

    const SCENE: &[u8] = b"the scene, as the artist sent it to the farm";

    #[tokio::test]
    async fn a_download_is_kept_only_if_it_is_the_scene_the_job_names() {
        let dir = TestDir::new();
        let store = Store::new(SCENE);
        let ctx = agent(&dir, &store.serve().await);
        let a = task_for(SCENE);

        // Changed on the way, by one letter or cut short: the task fails with the reason, nothing
        // is kept, and trying again does not make it acceptable.
        for wrong in [&b"the scene, as somebody changed it on the way"[..], &SCENE[..20], &b""[..]] {
            store.gives(wrong);
            for _ in 0..2 {
                let failed = fetch(&ctx, &a).await.unwrap_err().to_string();
                assert!(failed.contains("is not the one the job names") && failed.contains("deleted without rendering it"), "{failed}");
                assert!(failed.contains(&a.scene.hash) && failed.contains(blake3::hash(wrong).to_hex().as_str()), "{failed}");
                assert_eq!(cache(&dir, &a).1, 0, "something was kept");
            }
        }
        assert_eq!(store.asked(), 6);

        // As it was sent: kept, and handed over.
        store.gives(SCENE);
        let path = fetch(&ctx, &a).await.unwrap();
        assert_eq!((path.clone(), 1), cache(&dir, &a));
        assert_eq!(std::fs::read(&path).unwrap(), SCENE);
        // The next task of the job finds it there, already checked: the server is not asked again.
        assert_eq!(fetch(&ctx, &a).await.unwrap(), path);
        assert_eq!(store.asked(), 7);
    }

    #[tokio::test]
    async fn a_scene_kept_from_before_is_checked_like_a_new_one() {
        let dir = TestDir::new();
        let store = Store::new(SCENE);
        let url = store.serve().await;
        let a = task_for(SCENE);
        let (path, _) = cache(&dir, &a);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A file an earlier version of OpenFarm stored without looking at it, and it is not the
        // scene: replaced by the scene, not rendered.
        std::fs::write(&path, b"what an agent of 0.1.1 was given under this name").unwrap();
        assert_eq!(fetch(&agent(&dir, &url), &a).await.unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), SCENE);
        assert_eq!(store.asked(), 1);

        // OpenFarm starts again. The file is the scene: it is used, and the server is not asked.
        let restarted = agent(&dir, &url);
        assert_eq!(fetch(&restarted, &a).await.unwrap(), path);
        assert_eq!(store.asked(), 1);
        assert!(restarted.checked_scenes.lock().unwrap().contains(&path));

        // It changed on disk while OpenFarm was not running: found out, and fetched again.
        std::fs::write(&path, b"changed on disk").unwrap();
        assert_eq!(fetch(&agent(&dir, &url), &a).await.unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), SCENE);
        assert_eq!(store.asked(), 2);

        // A wrong file in the cache and a wrong one from the server: nothing is left to render.
        std::fs::write(&path, b"changed on disk").unwrap();
        store.gives(b"changed on the way");
        assert!(fetch(&agent(&dir, &url), &a).await.is_err());
        assert_eq!(cache(&dir, &a).1, 0);
    }

    #[tokio::test]
    async fn a_job_that_names_no_hash_gets_no_scene() {
        let dir = TestDir::new();
        let store = Store::new(SCENE);
        let ctx = agent(&dir, &store.serve().await);
        let (capitals, not_hex, almost) = ("A".repeat(64), "g".repeat(64), format!("{}/", "0".repeat(63)));
        for hash in ["", "abc", r"..\..\..\Windows\System32", capitals.as_str(), not_hex.as_str(), almost.as_str()] {
            let mut a = task_for(SCENE);
            a.scene.hash = hash.to_string();
            let failed = fetch(&ctx, &a).await.unwrap_err().to_string();
            assert!(failed.contains("BLAKE3"), "{failed}");
        }
        // Nothing was asked of the server and nothing was written anywhere.
        assert_eq!(store.asked(), 0);
        assert!(!dir.path().join("agent").exists());
        assert!(is_blake3(&task_for(SCENE).scene.hash));
    }

    #[tokio::test]
    async fn a_download_that_does_not_arrive_whole_leaves_nothing() {
        let dir = TestDir::new();
        let path = dir.path().join("shot.blend");
        let wanted = blake3::hash(SCENE).to_hex().to_string();
        let files = || std::fs::read_dir(dir.path()).unwrap().count();
        let chunks = |parts: Vec<std::io::Result<Vec<u8>>>| futures_util::stream::iter(parts);
        let go = CancellationToken::new();

        // The connection breaks half way.
        let broken = chunks(vec![Ok(SCENE[..10].to_vec()), Err(std::io::Error::other("connection reset"))]);
        assert!(store_scene(broken, &path, &wanted, &go).await.is_err());
        assert_eq!(files(), 0);
        // The task is canceled meanwhile.
        let canceled = CancellationToken::new();
        canceled.cancel();
        let failed = store_scene(chunks(vec![Ok(SCENE.to_vec())]), &path, &wanted, &canceled).await.unwrap_err();
        assert_eq!(failed.to_string(), "canceled");
        assert_eq!(files(), 0);
        // In pieces and whole: kept under its name, and nothing else next to it.
        let whole = chunks(vec![Ok(SCENE[..10].to_vec()), Ok(SCENE[10..].to_vec())]);
        store_scene(whole, &path, &wanted, &go).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), SCENE);
        assert_eq!(files(), 1);
        assert_eq!(hash_of(path).await.unwrap(), wanted);
    }
}
