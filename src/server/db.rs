// SPDX-License-Identifier: GPL-3.0-or-later
//! The server's SQLite file. The farm's state lives in memory (`Farm`); every change is also written
//! here so a restart loses nothing. Always on a local disk, never on a network share.

use std::path::Path;

use anyhow::Result;
use openfarm_core::model::{Capabilities, FrameInfo, JobSpec, JobState, Task};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub struct Db {
    conn: Connection,
}

pub struct StoredJob {
    pub id: u64,
    pub spec: JobSpec,
    pub state: JobState,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    pub error: Option<String>,
    pub archived: bool,
    pub excluded: Vec<String>,
}

pub struct StoredMachine {
    pub id: String,
    pub name: String,
    pub key: Option<String>,
    pub approved: bool,
    pub enabled: bool,
    pub caps: Capabilities,
    pub rank: u8,
    pub last_seen: u64,
}

fn word<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        other => other.map(|v| v.to_string()).unwrap_or_default(),
    }
}

fn from_word<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY, spec TEXT NOT NULL, state TEXT NOT NULL, created INTEGER NOT NULL,
                started INTEGER, finished INTEGER, error TEXT, archived INTEGER NOT NULL DEFAULT 0,
                excluded TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE IF NOT EXISTS frames (
                job INTEGER NOT NULL, frame INTEGER NOT NULL, state TEXT NOT NULL, task INTEGER, machine TEXT,
                secs REAL, thumb INTEGER NOT NULL DEFAULT 0, warning TEXT, attempts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (job, frame));
             CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY, job INTEGER NOT NULL, frames TEXT NOT NULL, machine TEXT NOT NULL,
                slot TEXT NOT NULL, state TEXT NOT NULL, started INTEGER NOT NULL, finished INTEGER, error TEXT);
             CREATE TABLE IF NOT EXISTS machines (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, key TEXT, approved INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1, caps TEXT NOT NULL DEFAULT '{}', rank INTEGER NOT NULL DEFAULT 1,
                last_seen INTEGER NOT NULL DEFAULT 0);",
        )?;
        Ok(Db { conn })
    }

    pub fn save_job(&self, j: &StoredJob) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs (id, spec, state, created, started, finished, error, archived, excluded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET spec=?2, state=?3, started=?5, finished=?6, error=?7, archived=?8, excluded=?9",
            params![
                j.id as i64,
                serde_json::to_string(&j.spec)?,
                word(&j.state),
                j.created as i64,
                j.started.map(|v| v as i64),
                j.finished.map(|v| v as i64),
                j.error,
                j.archived as i64,
                serde_json::to_string(&j.excluded)?
            ],
        )?;
        Ok(())
    }

    pub fn save_frames(&mut self, job: u64, frames: &[FrameInfo]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT INTO frames (job, frame, state, task, machine, secs, thumb, warning, attempts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(job, frame) DO UPDATE SET state=?3, task=?4, machine=?5, secs=?6, thumb=?7, warning=?8, attempts=?9",
            )?;
            for f in frames {
                st.execute(params![
                    job as i64,
                    f.frame,
                    word(&f.state),
                    f.task.map(|t| t as i64),
                    f.machine,
                    f.secs,
                    f.thumb as i64,
                    f.warning,
                    f.attempts
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn save_task(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, job, frames, machine, slot, state, started, finished, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET state=?6, finished=?8, error=?9",
            params![
                t.id as i64,
                t.job as i64,
                serde_json::to_string(&t.frames)?,
                t.machine,
                word(&t.slot),
                word(&t.state),
                t.started as i64,
                t.finished.map(|v| v as i64),
                t.error
            ],
        )?;
        Ok(())
    }

    pub fn save_machine(&self, m: &StoredMachine) -> Result<()> {
        self.conn.execute(
            "INSERT INTO machines (id, name, key, approved, enabled, caps, rank, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET name=?2, key=?3, approved=?4, enabled=?5, caps=?6, rank=?7, last_seen=?8",
            params![
                m.id,
                m.name,
                m.key,
                m.approved as i64,
                m.enabled as i64,
                serde_json::to_string(&m.caps)?,
                m.rank,
                m.last_seen as i64
            ],
        )?;
        Ok(())
    }

    pub fn forget_machine(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM machines WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn load_jobs(&self) -> Result<Vec<(StoredJob, Vec<FrameInfo>)>> {
        let mut st = self.conn.prepare(
            "SELECT id, spec, state, created, started, finished, error, archived, excluded FROM jobs WHERE archived = 0 ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, spec, state, created, started, finished, error, archived, excluded) = row?;
            let Ok(spec) = serde_json::from_str::<JobSpec>(&spec) else { continue };
            let job = StoredJob {
                id: id as u64,
                spec,
                state: from_word(&state).unwrap_or(JobState::Paused),
                created: created as u64,
                started: started.map(|v| v as u64),
                finished: finished.map(|v| v as u64),
                error,
                archived: archived != 0,
                excluded: serde_json::from_str(&excluded).unwrap_or_default(),
            };
            let frames = self.load_frames(job.id)?;
            out.push((job, frames));
        }
        Ok(out)
    }

    fn load_frames(&self, job: u64) -> Result<Vec<FrameInfo>> {
        let mut st = self.conn.prepare(
            "SELECT frame, state, task, machine, secs, thumb, warning, attempts FROM frames WHERE job = ?1 ORDER BY frame",
        )?;
        let rows = st.query_map(params![job as i64], |r| {
            Ok(FrameInfo {
                frame: r.get(0)?,
                state: from_word(&r.get::<_, String>(1)?).unwrap_or(openfarm_core::model::FrameState::Pending),
                task: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                machine: r.get(3)?,
                secs: r.get(4)?,
                thumb: r.get::<_, i64>(5)? != 0,
                warning: r.get(6)?,
                attempts: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn load_machines(&self) -> Result<Vec<StoredMachine>> {
        let mut st =
            self.conn.prepare("SELECT id, name, key, approved, enabled, caps, rank, last_seen FROM machines ORDER BY name")?;
        let rows = st.query_map([], |r| {
            Ok(StoredMachine {
                id: r.get(0)?,
                name: r.get(1)?,
                key: r.get(2)?,
                approved: r.get::<_, i64>(3)? != 0,
                enabled: r.get::<_, i64>(4)? != 0,
                caps: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                rank: r.get::<_, i64>(6)? as u8,
                last_seen: r.get::<_, i64>(7)? as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn max_ids(&self) -> Result<(u64, u64)> {
        let job: Option<i64> = self.conn.query_row("SELECT MAX(id) FROM jobs", [], |r| r.get(0)).optional()?.flatten();
        let task: Option<i64> = self.conn.query_row("SELECT MAX(id) FROM tasks", [], |r| r.get(0)).optional()?.flatten();
        Ok((job.unwrap_or(0) as u64, task.unwrap_or(0) as u64))
    }

    pub fn task_logs_of_job(&self, job: u64) -> Result<Vec<Task>> {
        let mut st = self.conn.prepare(
            "SELECT id, job, frames, machine, slot, state, started, finished, error FROM tasks WHERE job = ?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![job as i64], |r| {
            Ok(Task {
                id: r.get::<_, i64>(0)? as u64,
                job: r.get::<_, i64>(1)? as u64,
                frames: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                machine: r.get(3)?,
                slot: from_word(&r.get::<_, String>(4)?).unwrap_or(openfarm_core::model::SlotKind::Cpu),
                state: from_word(&r.get::<_, String>(5)?).unwrap_or(openfarm_core::model::TaskState::Failed),
                started: r.get::<_, i64>(6)? as u64,
                finished: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                error: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
