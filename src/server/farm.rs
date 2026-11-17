// SPDX-License-Identifier: GPL-3.0-or-later
//! The farm's live state and every rule that changes it. All methods run under one lock and never
//! wait on IO except the local SQLite file.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use openfarm_core::model::{Capabilities, Engine, FrameInfo, FrameState, JobSpec, JobState, SlotKind, Task, TaskState, Telemetry};
use openfarm_core::proto::{Assignment, ServerMsg, TaskEvent};
use openfarm_core::{auto_chunk, frames, order, scheduler};
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use super::db::{Db, StoredJob, StoredMachine};
use crate::config::{VERSION, now_ms};

/// What a machine is told when it comes with the id of a machine the farm knows and another key.
pub const ANOTHER_KEY: &str = "The farm already knows this machine with another key and does not take this one in its place. \
     If this computer lost its key (OpenFarm was installed again, or runs as another Windows user), forget the machine in Machines \
     on the farm's computer: it can then join and be approved as a new one.";

/// A frame is retried on other machines this many times before it counts as failed.
pub const MAX_ATTEMPTS: u32 = 3;
/// A machine that fails this many tasks of one job stops getting that job.
pub const MAX_MACHINE_FAILURES: u32 = 2;

pub struct JobEntry {
    pub id: u64,
    pub spec: JobSpec,
    pub state: JobState,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    pub error: Option<String>,
    pub frames: Vec<FrameInfo>,
    /// Frames still to hand out, in render order.
    pub pending: Vec<i32>,
    pub failures: HashMap<String, u32>,
    pub excluded: Vec<String>,
}

impl JobEntry {
    fn frame_mut(&mut self, f: i32) -> Option<&mut FrameInfo> {
        self.frames.binary_search_by_key(&f, |x| x.frame).ok().map(move |i| &mut self.frames[i])
    }

    pub fn avg_secs(&self) -> Option<f64> {
        let done: Vec<f64> = self.frames.iter().filter(|f| f.state == FrameState::Done).filter_map(|f| f.secs).collect();
        if done.is_empty() { None } else { Some(done.iter().sum::<f64>() / done.len() as f64) }
    }

    fn count(&self, s: FrameState) -> usize {
        self.frames.iter().filter(|f| f.state == s).count()
    }

    fn stored(&self) -> StoredJob {
        StoredJob {
            id: self.id,
            spec: self.spec.clone(),
            state: self.state,
            created: self.created,
            started: self.started,
            finished: self.finished,
            error: self.error.clone(),
            archived: false,
            excluded: self.excluded.clone(),
        }
    }
}

pub struct LiveTask {
    pub task: Task,
    pub frame: Option<i32>,
    pub pct: f32,
}

pub struct MachineEntry {
    pub id: String,
    pub name: String,
    /// Fingerprint of the public key this machine first connected with. The machine id belongs to
    /// that key from then on, approved or not, until the machine is forgotten. `None` only for a
    /// machine that has not connected since the farm stopped keeping keys (a 0.1.1 farm).
    pub fingerprint: Option<String>,
    pub approved: bool,
    pub enabled: bool,
    pub accepting: bool,
    pub caps: Capabilities,
    pub telemetry: Telemetry,
    pub version: String,
    pub rank: u8,
    pub last_seen: u64,
    pub address: String,
    pub tx: Option<mpsc::UnboundedSender<ServerMsg>>,
    pub conn: u64,
    pub warm_job: Option<u64>,
}

impl MachineEntry {
    pub fn online(&self) -> bool {
        self.tx.is_some()
    }

    fn stored(&self) -> StoredMachine {
        StoredMachine {
            id: self.id.clone(),
            name: self.name.clone(),
            fingerprint: self.fingerprint.clone(),
            approved: self.approved,
            enabled: self.enabled,
            caps: self.caps.clone(),
            rank: self.rank,
            last_seen: self.last_seen,
        }
    }
}

pub struct Farm {
    pub name: String,
    pub data: PathBuf,
    db: Db,
    pub jobs: BTreeMap<u64, JobEntry>,
    pub tasks: HashMap<u64, LiveTask>,
    pub machines: BTreeMap<String, MachineEntry>,
    next_job: u64,
    next_task: u64,
    version: watch::Sender<u64>,
}

pub struct Hello {
    pub machine_id: String,
    pub name: String,
    /// Fingerprint of the key that signed the request that opened the connection.
    pub fingerprint: String,
    pub version: String,
    pub caps: Capabilities,
    pub rank: u8,
    pub address: String,
    pub loopback: bool,
}

impl Farm {
    pub fn open(name: String, data: PathBuf, version: watch::Sender<u64>) -> Result<Farm> {
        std::fs::create_dir_all(&data)?;
        for d in ["scenes", "thumbs", "logs"] {
            std::fs::create_dir_all(data.join(d))?;
        }
        let db = Db::open(&data.join("farm.sqlite"))?;
        let (max_job, max_task) = db.max_ids()?;
        let mut farm = Farm {
            name,
            data,
            db,
            jobs: BTreeMap::new(),
            tasks: HashMap::new(),
            machines: BTreeMap::new(),
            next_job: max_job + 1,
            next_task: max_task + 1,
            version,
        };
        for (j, mut frames) in farm.db.load_jobs()? {
            // Frames that were rendering when the server stopped go back to the queue.
            for f in frames.iter_mut() {
                if f.state == FrameState::Rendering {
                    f.state = FrameState::Pending;
                    f.task = None;
                }
            }
            let pending_frames: Vec<i32> = frames.iter().filter(|f| f.state == FrameState::Pending).map(|f| f.frame).collect();
            let state = if j.state == JobState::Rendering { JobState::Queued } else { j.state };
            let entry = JobEntry {
                id: j.id,
                pending: order::ordered(&pending_frames, j.spec.order),
                spec: j.spec,
                state,
                created: j.created,
                started: j.started,
                finished: j.finished,
                error: j.error,
                frames,
                failures: HashMap::new(),
                excluded: j.excluded,
            };
            farm.jobs.insert(entry.id, entry);
        }
        for m in farm.db.load_machines()? {
            farm.machines.insert(
                m.id.clone(),
                MachineEntry {
                    id: m.id,
                    name: m.name,
                    fingerprint: m.fingerprint,
                    approved: m.approved,
                    enabled: m.enabled,
                    accepting: true,
                    caps: m.caps,
                    telemetry: Telemetry::default(),
                    version: String::new(),
                    rank: m.rank,
                    last_seen: m.last_seen,
                    address: String::new(),
                    tx: None,
                    conn: 0,
                    warm_job: None,
                },
            );
        }
        Ok(farm)
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    /// Is this the fingerprint of a key the farm trusts (an approved machine's)? Fingerprints are
    /// not secret, so a plain comparison gives nothing away.
    pub fn trusts_fingerprint(&self, fingerprint: &str) -> bool {
        !fingerprint.is_empty() && self.machines.values().any(|m| m.approved && m.fingerprint.as_deref() == Some(fingerprint))
    }

    // ---- jobs -------------------------------------------------------------------------------

    pub fn submit(&mut self, spec: JobSpec) -> Result<u64> {
        let list = frames::parse(&spec.frames).map_err(|e| anyhow!("frames: {e}"))?;
        if spec.name.trim().is_empty() {
            bail!("the job needs a name");
        }
        if spec.output.folder.trim().is_empty() {
            bail!("the job needs an output folder");
        }
        if !openfarm_core::model::is_absolute_path(&spec.output.folder) {
            bail!("the output folder must be a full path (\\\\server\\share\\... or X:\\...): {}", spec.output.folder);
        }
        let scene = self.data.join("scenes").join(&spec.scene.hash).join(&spec.scene.file_name);
        if spec.engine != Engine::Dummy && !scene.exists() {
            bail!("the scene {} was not uploaded to the farm", spec.scene.file_name);
        }
        let id = self.next_job;
        self.next_job += 1;
        let frames_info: Vec<FrameInfo> = list
            .iter()
            .map(|&frame| FrameInfo {
                frame,
                state: FrameState::Pending,
                task: None,
                machine: None,
                secs: None,
                thumb: false,
                warning: None,
                attempts: 0,
            })
            .collect();
        let job = JobEntry {
            id,
            pending: order::ordered(&list, spec.order),
            state: if spec.paused { JobState::Paused } else { JobState::Queued },
            spec,
            created: now_ms(),
            started: None,
            finished: None,
            error: None,
            frames: frames_info,
            failures: HashMap::new(),
            excluded: Vec::new(),
        };
        self.db.save_job(&job.stored())?;
        self.db.save_frames(id, &job.frames)?;
        tracing::info!(job = id, name = %job.spec.name, frames = job.frames.len(), "job submitted");
        self.jobs.insert(id, job);
        self.dispatch();
        self.bump();
        Ok(id)
    }

    pub fn job_action(&mut self, id: u64, action: &str, priority: Option<u8>) -> Result<()> {
        let job = self.jobs.get_mut(&id).ok_or_else(|| anyhow!("no job {id}"))?;
        match action {
            "pause" => {
                if !job.state.is_finished() {
                    job.state = JobState::Paused;
                }
            }
            "resume" => {
                if job.state == JobState::Paused {
                    job.state = if job.started.is_some() { JobState::Rendering } else { JobState::Queued };
                }
            }
            "priority" => {
                job.spec.priority = priority.ok_or_else(|| anyhow!("priority missing"))?.min(100);
            }
            "retry_failed" => {
                let mut again = Vec::new();
                for f in job.frames.iter_mut() {
                    if f.state == FrameState::Failed {
                        f.state = FrameState::Pending;
                        f.attempts = 0;
                        f.warning = None;
                        again.push(f.frame);
                    }
                }
                again.append(&mut job.pending);
                job.pending = again;
                job.excluded.clear();
                job.failures.clear();
                if !job.pending.is_empty() && matches!(job.state, JobState::Failed | JobState::Done | JobState::Canceled) {
                    job.state = JobState::Queued;
                    job.finished = None;
                    job.error = None;
                }
                let frames = job.frames.clone();
                self.db.save_frames(id, &frames)?;
            }
            "cancel" => {
                if !job.state.is_finished() {
                    job.state = JobState::Canceled;
                    job.finished = Some(now_ms());
                    job.pending.clear();
                    self.cancel_tasks_of(id);
                }
            }
            "archive" => {
                let job = self.jobs.remove(&id).unwrap();
                self.cancel_tasks_of(id);
                let mut s = job.stored();
                s.archived = true;
                self.db.save_job(&s)?;
                self.dispatch();
                self.bump();
                return Ok(());
            }
            other => bail!("unknown action {other}"),
        }
        let stored = self.jobs[&id].stored();
        self.db.save_job(&stored)?;
        self.dispatch();
        self.bump();
        Ok(())
    }

    fn cancel_tasks_of(&mut self, job: u64) {
        let ids: Vec<u64> = self.tasks.values().filter(|t| t.task.job == job).map(|t| t.task.id).collect();
        for id in ids {
            if let Some(t) = self.tasks.get(&id)
                && let Some(m) = self.machines.get(&t.task.machine)
                && let Some(tx) = &m.tx
            {
                let _ = tx.send(ServerMsg::Cancel { task: id });
            }
        }
    }

    // ---- machines -----------------------------------------------------------------------------

    pub fn machine_action(&mut self, id: &str, action: &str) -> Result<()> {
        let m = self.machines.get_mut(id).ok_or_else(|| anyhow!("no machine {id}"))?;
        match action {
            "approve" => {
                // Approving never chooses a key: it is the one the machine first connected with,
                // which is the one this entry has been showing as waiting.
                if m.fingerprint.is_none() {
                    bail!("the machine has not connected yet; it can be approved when it does");
                }
                m.approved = true;
                if let Some(tx) = &m.tx {
                    let _ = tx.send(ServerMsg::Welcome {
                        approved: true,
                        server_name: self.name.clone(),
                        server_version: VERSION.into(),
                        protocol: openfarm_core::proto::PROTOCOL,
                    });
                    let _ = tx.send(ServerMsg::SetEnabled { enabled: m.enabled });
                }
            }
            "enable" | "disable" => {
                m.enabled = action == "enable";
                if let Some(tx) = &m.tx {
                    let _ = tx.send(ServerMsg::SetEnabled { enabled: m.enabled });
                }
            }
            "forget" => {
                if m.online() {
                    bail!("the machine is connected; close OpenFarm on it first");
                }
                self.db.forget_machine(id)?;
                self.machines.remove(id);
                self.bump();
                return Ok(());
            }
            other => bail!("unknown action {other}"),
        }
        let stored = m.stored();
        self.db.save_machine(&stored)?;
        self.dispatch();
        self.bump();
        Ok(())
    }

    /// A machine connected, having proved that it holds the key `h.fingerprint`. Returns whether
    /// it is approved, or the reason the farm does not take it.
    ///
    /// A machine id belongs to the key that first connected with it, until the machine is
    /// forgotten. The same id with another key is somebody else, or the same computer after it
    /// lost its key: it is turned away and nothing about the machine the farm knows changes. So
    /// nobody can take a machine's place, un-approve it, or get a key of their own approved under
    /// its name.
    pub fn hello(&mut self, h: Hello, conn: u64, tx: mpsc::UnboundedSender<ServerMsg>) -> Result<bool, String> {
        let known = self.machines.get(&h.machine_id).and_then(|m| m.fingerprint.as_deref());
        if known.is_some_and(|key| key != h.fingerprint) {
            return Err(ANOTHER_KEY.to_string());
        }
        let m = self.machines.entry(h.machine_id.clone()).or_insert_with(|| MachineEntry {
            id: h.machine_id.clone(),
            name: h.name.clone(),
            fingerprint: None,
            approved: false,
            enabled: true,
            accepting: true,
            caps: Capabilities::default(),
            telemetry: Telemetry::default(),
            version: String::new(),
            rank: h.rank,
            last_seen: 0,
            address: String::new(),
            tx: None,
            conn: 0,
            warm_job: None,
        });
        m.name = h.name;
        m.caps = h.caps;
        m.version = h.version;
        m.rank = h.rank;
        m.address = h.address;
        // The first connection of a machine binds its id to its key (kept on disk, also while it
        // waits for approval); every later one has just been checked against it.
        m.fingerprint = Some(h.fingerprint);
        m.last_seen = now_ms();
        m.conn = conn;
        m.tx = Some(tx);
        m.warm_job = None;
        // The server's own agent needs no approval.
        if h.loopback {
            m.approved = true;
        }
        let approved = m.approved;
        let stored = m.stored();
        let _ = self.db.save_machine(&stored);
        tracing::info!(machine = %stored.name, approved, "machine connected");
        self.dispatch();
        self.bump();
        Ok(approved)
    }

    pub fn status(&mut self, id: &str, telemetry: Telemetry, accepting: bool) {
        if let Some(m) = self.machines.get_mut(id) {
            m.telemetry = telemetry;
            m.last_seen = now_ms();
            let changed = m.accepting != accepting;
            m.accepting = accepting;
            if changed {
                self.dispatch();
            }
            self.bump();
        }
    }

    pub fn disconnected(&mut self, id: &str, conn: u64) {
        let Some(m) = self.machines.get_mut(id) else { return };
        if m.conn != conn {
            return; // an older connection of a machine that has already reconnected
        }
        m.tx = None;
        m.warm_job = None;
        let stored = m.stored();
        let _ = self.db.save_machine(&stored);
        // Its tasks go back to the queue without counting against the machine.
        let lost: Vec<u64> = self.tasks.values().filter(|t| t.task.machine == id).map(|t| t.task.id).collect();
        for t in lost {
            self.end_task(t, false, Some("the machine disconnected".into()), false, false);
        }
        tracing::info!(machine = %id, "machine disconnected");
        self.dispatch();
        self.bump();
    }

    // ---- tasks ----------------------------------------------------------------------------------

    pub fn task_event(&mut self, machine: &str, task: u64, event: TaskEvent) {
        let Some(live) = self.tasks.get_mut(&task) else { return };
        if live.task.machine != machine {
            return;
        }
        let job_id = live.task.job;
        match event {
            TaskEvent::Started => {
                live.task.state = TaskState::Rendering;
            }
            TaskEvent::Log { text } => {
                use std::io::Write;
                let path = self.data.join("logs").join(format!("{task}.log"));
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = f.write_all(text.as_bytes());
                    if !text.ends_with('\n') {
                        let _ = f.write_all(b"\n");
                    }
                }
                return; // no UI refresh for every log line
            }
            TaskEvent::Progress { frame, pct } => {
                live.frame = Some(frame);
                live.pct = pct;
            }
            TaskEvent::FrameDone { frame, secs, files: _, thumb, warning } => {
                live.frame = None;
                live.pct = 0.0;
                let mut has_thumb = false;
                if let Some(b64) = thumb
                    && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
                {
                    let dir = self.data.join("thumbs").join(job_id.to_string());
                    let _ = std::fs::create_dir_all(&dir);
                    has_thumb = std::fs::write(dir.join(format!("{frame}.jpg")), bytes).is_ok();
                }
                if let Some(job) = self.jobs.get_mut(&job_id)
                    && let Some(f) = job.frame_mut(frame)
                {
                    f.state = FrameState::Done;
                    f.secs = Some(secs);
                    f.thumb = has_thumb;
                    f.warning = warning;
                    f.machine = Some(machine.to_string());
                    let f = f.clone();
                    let _ = self.db.save_frames(job_id, &[f]);
                }
            }
            TaskEvent::FrameFailed { frame, error } => {
                self.fail_frame(job_id, frame, &error, true);
            }
            TaskEvent::Finished { ok, error, fatal, .. } => {
                self.end_task(task, ok, error, fatal, true);
                self.dispatch();
            }
        }
        self.bump();
    }

    /// A frame that did not render goes back to the queue, or fails for good after `MAX_ATTEMPTS`.
    fn fail_frame(&mut self, job_id: u64, frame: i32, error: &str, counts: bool) {
        let Some(job) = self.jobs.get_mut(&job_id) else { return };
        let canceled = job.state == JobState::Canceled;
        let (saved, requeue) = {
            let Some(f) = job.frame_mut(frame) else { return };
            if f.state != FrameState::Rendering {
                return;
            }
            if counts {
                f.attempts += 1;
            }
            f.task = None;
            let requeue = if f.attempts >= MAX_ATTEMPTS {
                f.state = FrameState::Failed;
                f.warning = Some(error.to_string());
                false
            } else {
                f.state = FrameState::Pending;
                true
            };
            (f.clone(), requeue)
        };
        if requeue && !canceled {
            job.pending.insert(0, frame);
        }
        let _ = self.db.save_frames(job_id, &[saved]);
    }

    fn end_task(&mut self, task: u64, ok: bool, error: Option<String>, fatal: bool, counts: bool) {
        let Some(mut live) = self.tasks.remove(&task) else { return };
        let job_id = live.task.job;
        let machine = live.task.machine.clone();
        let canceled = error.as_deref() == Some("canceled");
        let err_text = error.clone().unwrap_or_else(|| "the task ended without rendering this frame".into());
        for f in live.task.frames.clone() {
            self.fail_frame(job_id, f, &err_text, counts && !canceled);
        }
        live.task.state = if ok {
            TaskState::Done
        } else if canceled {
            TaskState::Canceled
        } else {
            TaskState::Failed
        };
        live.task.finished = Some(now_ms());
        live.task.error = error.clone();
        let _ = self.db.save_task(&live.task);
        if let Some(m) = self.machines.get_mut(&machine) {
            m.warm_job = if ok { Some(job_id) } else { None };
        }
        let Some(job) = self.jobs.get_mut(&job_id) else { return };
        if !ok && counts && !canceled {
            let n = job.failures.entry(machine.clone()).or_insert(0);
            *n += 1;
            if *n >= MAX_MACHINE_FAILURES && !job.excluded.contains(&machine) {
                job.excluded.push(machine.clone());
            }
        }
        // Every machine that could render it has failed it too often: stop instead of waiting forever.
        let engine = job.spec.engine;
        let device = job.spec.device.clone();
        let nobody_left = !job.excluded.is_empty()
            && !self.machines.values().any(|m| m.approved && m.caps.can_render(engine, device.as_deref()) && !job.excluded.contains(&m.id));
        let fatal = fatal || (nobody_left && !ok && !canceled);
        let error = if nobody_left && !fatal_reason_given(&error) {
            error.map(|e| format!("every machine that can render this job failed it: {e}"))
        } else {
            error
        };
        if fatal && !job.state.is_finished() {
            job.state = JobState::Failed;
            job.error = error;
            job.finished = Some(now_ms());
            job.pending.clear();
            self.cancel_tasks_of(job_id);
        }
        self.update_job_state(job_id);
    }

    fn update_job_state(&mut self, id: u64) {
        let running = self.tasks.values().any(|t| t.task.job == id);
        let Some(job) = self.jobs.get_mut(&id) else { return };
        if job.state.is_finished() && job.state != JobState::Canceled {
            // Done / Failed can come back to life only through retry_failed.
        }
        let total = job.frames.len();
        let done = job.count(FrameState::Done);
        let failed = job.count(FrameState::Failed);
        if done == total && !job.state.is_finished() {
            job.state = JobState::Done;
            job.finished = Some(now_ms());
        } else if !running && job.pending.is_empty() && failed > 0 && !job.state.is_finished() {
            job.state = JobState::Failed;
            job.finished = Some(now_ms());
            job.error = Some(format!("{failed} frame(s) failed"));
        }
        let stored = job.stored();
        let _ = self.db.save_job(&stored);
    }

    // ---- scheduling ---------------------------------------------------------------------------

    /// Hands out work to every free slot. Called after anything that can change who renders what.
    pub fn dispatch(&mut self) {
        let mut queues = Vec::new();
        for j in self.jobs.values() {
            if !matches!(j.state, JobState::Queued | JobState::Rendering) || j.pending.is_empty() {
                continue;
            }
            let running = self.tasks.values().filter(|t| t.task.job == j.id).count() as u32;
            // An automatic chunk never takes so many frames that other machines are left without work:
            // at most a share of what is left for every slot that could render this job.
            let auto = j.spec.chunk.is_none();
            let mut chunk = j.spec.chunk.unwrap_or_else(|| auto_chunk(j.avg_secs()));
            if auto {
                let kind = j.spec.engine.slot(j.spec.device.as_deref());
                let slots: usize = self
                    .machines
                    .values()
                    .filter(|m| m.online() && m.approved && m.enabled && !j.excluded.contains(&m.id))
                    .filter(|m| m.caps.can_render(j.spec.engine, j.spec.device.as_deref()))
                    .map(|m| if kind == SlotKind::Cpu { 1 } else { m.caps.gpus.len().max(1) })
                    .sum();
                let share = j.pending.len().div_ceil(slots.max(1) * 2).max(1) as u32;
                chunk = chunk.min(share);
            }
            queues.push(scheduler::JobQueue {
                id: j.id,
                priority: j.spec.priority,
                created: j.created,
                engine: j.spec.engine,
                device: j.spec.device.clone(),
                pending: j.pending.clone(),
                running,
                chunk,
                excluded: j.excluded.clone(),
            });
        }
        if queues.is_empty() {
            return;
        }
        let mut slots = Vec::new();
        for m in self.machines.values() {
            if !(m.online() && m.approved && m.enabled && m.accepting) {
                continue;
            }
            let busy_cpu = self.tasks.values().filter(|t| t.task.machine == m.id && t.task.slot == SlotKind::Cpu).count();
            let busy_gpu = self.tasks.values().filter(|t| t.task.machine == m.id && t.task.slot == SlotKind::Gpu).count();
            slots.push(scheduler::MachineSlots {
                id: m.id.clone(),
                caps: m.caps.clone(),
                free_cpu: 1u32.saturating_sub(busy_cpu as u32),
                free_gpu: (m.caps.gpus.len() as u32).saturating_sub(busy_gpu as u32),
                warm_job: m.warm_job,
                rank: m.rank,
                speed: 0.0,
            });
        }
        for a in scheduler::plan(&queues, &slots) {
            self.assign(a);
        }
    }

    fn assign(&mut self, a: scheduler::Assignment) {
        let id = self.next_task;
        self.next_task += 1;
        let Some(job) = self.jobs.get_mut(&a.job) else { return };
        let Some(m) = self.machines.get(&a.machine) else { return };
        let Some(tx) = m.tx.clone() else { return };
        let msg = ServerMsg::Assign(Assignment {
            task: id,
            job: job.id,
            job_name: job.spec.name.clone(),
            engine: job.spec.engine,
            scene: job.spec.scene.clone(),
            scene_url: format!("/api/v1/scenes/{}", job.spec.scene.hash),
            frames: a.frames.clone(),
            camera: job.spec.camera.clone(),
            output: job.spec.output.clone(),
            device: job.spec.device.clone(),
            resolution: job.spec.resolution,
            settings: job.spec.settings.clone(),
            slot: a.slot,
        });
        if tx.send(msg).is_err() {
            return;
        }
        job.pending.retain(|f| !a.frames.contains(f));
        let mut changed = Vec::new();
        for f in &a.frames {
            if let Some(fi) = job.frame_mut(*f) {
                fi.state = FrameState::Rendering;
                fi.task = Some(id);
                fi.machine = Some(a.machine.clone());
                changed.push(fi.clone());
            }
        }
        if job.state == JobState::Queued {
            job.state = JobState::Rendering;
        }
        if job.started.is_none() {
            job.started = Some(now_ms());
        }
        let stored = job.stored();
        let task = Task {
            id,
            job: a.job,
            frames: a.frames,
            machine: a.machine,
            slot: a.slot,
            state: TaskState::Assigned,
            started: now_ms(),
            finished: None,
            error: None,
        };
        let _ = self.db.save_task(&task);
        let _ = self.db.save_frames(a.job, &changed);
        let _ = self.db.save_job(&stored);
        self.tasks.insert(id, LiveTask { task, frame: None, pct: 0.0 });
    }

    // ---- views for the UI and the API ---------------------------------------------------------

    pub fn state_view(&self) -> StateView {
        let mut jobs: Vec<JobView> = self.jobs.values().map(|j| self.job_summary(j)).collect();
        jobs.sort_by_key(|a| std::cmp::Reverse(a.id));
        let machines = self
            .machines
            .values()
            .map(|m| MachineView {
                id: m.id.clone(),
                name: m.name.clone(),
                online: m.online(),
                approved: m.approved,
                enabled: m.enabled,
                accepting: m.accepting,
                rank: m.rank,
                version: m.version.clone(),
                address: m.address.clone(),
                caps: m.caps.clone(),
                telemetry: m.telemetry.clone(),
                last_seen: m.last_seen,
                tasks: self
                    .tasks
                    .values()
                    .filter(|t| t.task.machine == m.id)
                    .map(|t| TaskView {
                        id: t.task.id,
                        job: t.task.job,
                        job_name: self.jobs.get(&t.task.job).map(|j| j.spec.name.clone()).unwrap_or_default(),
                        frames: frames::format(&t.task.frames),
                        slot: t.task.slot,
                        state: t.task.state,
                        frame: t.frame,
                        pct: t.pct,
                        started: t.task.started,
                    })
                    .collect(),
            })
            .collect();
        StateView { farm: self.name.clone(), version: VERSION.into(), jobs, machines }
    }

    fn job_summary(&self, j: &JobEntry) -> JobView {
        let running = self.tasks.values().filter(|t| t.task.job == j.id).count();
        let done = j.count(FrameState::Done);
        let avg = j.avg_secs();
        let left = j.frames.len() - done - j.count(FrameState::Failed);
        let eta = match (avg, j.state) {
            (Some(a), JobState::Rendering) if running > 0 => Some(a * left as f64 / running as f64),
            _ => None,
        };
        JobView {
            id: j.id,
            name: j.spec.name.clone(),
            engine: j.spec.engine,
            state: j.state,
            priority: j.spec.priority,
            created: j.created,
            started: j.started,
            finished: j.finished,
            total: j.frames.len(),
            done,
            rendering: j.count(FrameState::Rendering),
            failed: j.count(FrameState::Failed),
            warnings: j.frames.iter().filter(|f| f.state == FrameState::Done && f.warning.is_some()).count(),
            running_tasks: running,
            avg_secs: avg,
            eta_secs: eta,
            submitted_by: j.spec.submitted_by.clone(),
            scene: j.spec.scene.file_name.clone(),
            output: j.spec.output.folder.clone(),
            frames: j.spec.frames.clone(),
            error: j.error.clone(),
            excluded: j.excluded.iter().map(|id| self.machine_name(id)).collect(),
        }
    }

    pub fn machine_name(&self, id: &str) -> String {
        self.machines.get(id).map(|m| m.name.clone()).unwrap_or_else(|| id.to_string())
    }

    pub fn job_detail(&self, id: u64) -> Option<JobDetail> {
        let j = self.jobs.get(&id)?;
        let mut frames = j.frames.clone();
        for f in frames.iter_mut() {
            if let Some(m) = &f.machine {
                f.machine = Some(self.machine_name(m));
            }
        }
        let tasks = self.db.task_logs_of_job(id).unwrap_or_default();
        Some(JobDetail { summary: self.job_summary(j), spec: j.spec.clone(), frames, tasks })
    }
}

fn fatal_reason_given(e: &Option<String>) -> bool {
    e.as_deref().map(|e| e.starts_with("every machine")).unwrap_or(false)
}

#[derive(Serialize)]
pub struct StateView {
    pub farm: String,
    pub version: String,
    pub jobs: Vec<JobView>,
    pub machines: Vec<MachineView>,
}

#[derive(Serialize)]
pub struct JobView {
    pub id: u64,
    pub name: String,
    pub engine: Engine,
    pub state: JobState,
    pub priority: u8,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    pub total: usize,
    pub done: usize,
    pub rendering: usize,
    pub failed: usize,
    pub warnings: usize,
    pub running_tasks: usize,
    pub avg_secs: Option<f64>,
    pub eta_secs: Option<f64>,
    pub submitted_by: String,
    pub scene: String,
    pub output: String,
    pub frames: String,
    pub error: Option<String>,
    pub excluded: Vec<String>,
}

#[derive(Serialize)]
pub struct MachineView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub approved: bool,
    pub enabled: bool,
    pub accepting: bool,
    pub rank: u8,
    pub version: String,
    pub address: String,
    pub caps: Capabilities,
    pub telemetry: Telemetry,
    pub last_seen: u64,
    pub tasks: Vec<TaskView>,
}

#[derive(Serialize)]
pub struct TaskView {
    pub id: u64,
    pub job: u64,
    pub job_name: String,
    pub frames: String,
    pub slot: SlotKind,
    pub state: TaskState,
    pub frame: Option<i32>,
    pub pct: f32,
    pub started: u64,
}

#[derive(Serialize)]
pub struct JobDetail {
    pub summary: JobView,
    pub spec: JobSpec,
    pub frames: Vec<FrameInfo>,
    pub tasks: Vec<Task>,
}
