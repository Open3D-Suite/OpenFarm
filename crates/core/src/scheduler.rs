// SPDX-License-Identifier: GPL-3.0-or-later
//! Who renders what. A pure function of the farm's state, so it can be tested and simulated without
//! any machines.
//!
//! Rules, in order:
//! 1. A machine only gets jobs it can render (software, a GPU for GPU engines, not excluded from the job).
//! 2. Higher priority first.
//! 3. A machine that already has a job's scene loaded (its warm host) keeps that job.
//! 4. Among equal jobs, the one with fewer tasks running (so equal jobs share the farm).
//! 5. Then the oldest.
//!
//! Machines are served best first: dedicated nodes, then workstations, then the server; faster first.

use crate::model::{Capabilities, Engine, SlotKind};

#[derive(Debug, Clone)]
pub struct JobQueue {
    pub id: u64,
    pub priority: u8,
    pub created: u64,
    pub engine: Engine,
    pub device: Option<String>,
    /// Frames still to hand out, in render order (first = next).
    pub pending: Vec<i32>,
    pub running: u32,
    /// Frames per task.
    pub chunk: u32,
    /// Machines that failed this job too often.
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MachineSlots {
    pub id: String,
    pub caps: Capabilities,
    pub free_cpu: u32,
    pub free_gpu: u32,
    /// The job whose scene this machine has loaded, if any.
    pub warm_job: Option<u64>,
    /// 0 = render node, 1 = workstation, 2 = server.
    pub rank: u8,
    /// Seconds per frame on a reference job, or 0 if unknown (lower is faster).
    pub speed: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub job: u64,
    pub machine: String,
    pub slot: SlotKind,
    pub frames: Vec<i32>,
}

pub fn plan(jobs: &[JobQueue], machines: &[MachineSlots]) -> Vec<Assignment> {
    let mut jobs: Vec<JobQueue> = jobs.iter().filter(|j| !j.pending.is_empty()).cloned().collect();
    let mut order: Vec<&MachineSlots> = machines.iter().collect();
    order.sort_by(|a, b| a.rank.cmp(&b.rank).then(speed_key(a.speed).partial_cmp(&speed_key(b.speed)).unwrap()).then(a.id.cmp(&b.id)));
    let mut out = Vec::new();
    for m in order {
        let mut free = [(SlotKind::Cpu, m.free_cpu), (SlotKind::Gpu, m.free_gpu)];
        let mut warm = m.warm_job;
        loop {
            let mut best: Option<usize> = None;
            for (i, j) in jobs.iter().enumerate() {
                if j.pending.is_empty() || j.excluded.iter().any(|e| e == &m.id) {
                    continue;
                }
                let kind = j.engine.slot(j.device.as_deref());
                if free.iter().find(|(k, _)| *k == kind).map(|(_, n)| *n).unwrap_or(0) == 0 {
                    continue;
                }
                if !m.caps.can_render(j.engine, j.device.as_deref()) {
                    continue;
                }
                best = match best {
                    None => Some(i),
                    Some(b) if better(j, &jobs[b], warm) => Some(i),
                    keep => keep,
                };
            }
            let Some(i) = best else { break };
            let job = &mut jobs[i];
            let kind = job.engine.slot(job.device.as_deref());
            let take = (job.chunk.max(1) as usize).min(job.pending.len());
            let frames: Vec<i32> = job.pending.drain(..take).collect();
            job.running += 1;
            for f in free.iter_mut() {
                if f.0 == kind {
                    f.1 -= 1;
                }
            }
            // A machine with two free slots should not get the same warm preference twice in a row.
            warm = Some(job.id);
            out.push(Assignment { job: job.id, machine: m.id.clone(), slot: kind, frames });
        }
    }
    out
}

fn speed_key(s: f64) -> f64 {
    if s <= 0.0 { f64::MAX } else { s }
}

/// Is `a` a better pick than `b` for a machine whose warm job is `warm`?
fn better(a: &JobQueue, b: &JobQueue, warm: Option<u64>) -> bool {
    if a.priority != b.priority {
        return a.priority > b.priority;
    }
    let aw = warm == Some(a.id);
    let bw = warm == Some(b.id);
    if aw != bw {
        return aw;
    }
    if a.running != b.running {
        return a.running < b.running;
    }
    if a.created != b.created {
        return a.created < b.created;
    }
    a.id < b.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Gpu, Software};

    fn caps(software: &[&str], gpus: u32) -> Capabilities {
        Capabilities {
            software: software.iter().map(|s| Software { name: s.to_string(), version: "1".into(), path: String::new() }).collect(),
            gpus: (0..gpus).map(|i| Gpu { index: i, name: "gpu".into(), vram_mb: 8000 }).collect(),
            ..Default::default()
        }
    }

    fn job(id: u64, engine: Engine, frames: std::ops::Range<i32>, priority: u8) -> JobQueue {
        JobQueue { id, priority, created: id, engine, device: None, pending: frames.collect(), running: 0, chunk: 1, excluded: vec![] }
    }

    fn machine(id: &str, sw: &[&str], gpus: u32, rank: u8) -> MachineSlots {
        MachineSlots { id: id.into(), caps: caps(sw, gpus), free_cpu: 1, free_gpu: gpus, warm_job: None, rank, speed: 0.0 }
    }

    #[test]
    fn machines_only_get_what_they_can_render() {
        let jobs = [job(1, Engine::Corona, 0..10, 50), job(2, Engine::Cycles, 0..10, 50)];
        let ms = [machine("max-only", &["3dsmax", "corona"], 0, 0), machine("blender-gpu", &["blender"], 1, 0)];
        let a = plan(&jobs, &ms);
        assert!(a.iter().all(|x| (x.machine == "max-only") == (x.job == 1)), "{a:?}");
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn priority_wins_then_jobs_share() {
        let jobs = [job(1, Engine::Dummy, 0..10, 50), job(2, Engine::Dummy, 0..10, 80), job(3, Engine::Dummy, 0..10, 50)];
        let ms: Vec<_> = (0..4).map(|i| machine(&format!("m{i}"), &[], 0, 0)).collect();
        let mut jobs = jobs.to_vec();
        jobs[1].pending.truncate(1); // the urgent job has a single frame left
        let a = plan(&jobs, &ms);
        assert_eq!(a[0].job, 2);
        // The other three machines are shared between jobs 1 and 3.
        let ones = a.iter().filter(|x| x.job == 1).count();
        let threes = a.iter().filter(|x| x.job == 3).count();
        assert_eq!((ones, threes), (2, 1));
    }

    #[test]
    fn warm_machines_keep_their_job() {
        let jobs = [job(1, Engine::Dummy, 0..10, 50), job(2, Engine::Dummy, 0..10, 50)];
        let mut m = machine("a", &[], 0, 0);
        m.warm_job = Some(2);
        let a = plan(&jobs, &[m]);
        assert_eq!(a[0].job, 2);
    }

    #[test]
    fn chunks_and_exclusions() {
        let mut j = job(1, Engine::Dummy, 0..10, 50);
        j.chunk = 4;
        j.excluded = vec!["bad".into()];
        let a = plan(&[j], &[machine("bad", &[], 0, 0), machine("good", &[], 0, 1)]);
        assert_eq!(a, vec![Assignment { job: 1, machine: "good".into(), slot: SlotKind::Cpu, frames: vec![0, 1, 2, 3] }]);
    }

    #[test]
    fn gpu_slots_are_used_one_task_each() {
        let jobs = [job(1, Engine::Eevee, 0..10, 50)];
        let a = plan(&jobs, &[machine("two-gpus", &["blender"], 2, 0)]);
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|x| x.slot == SlotKind::Gpu));
    }

    #[test]
    fn best_machines_are_served_first() {
        let jobs = [job(1, Engine::Dummy, 0..1, 50)];
        let a = plan(&jobs, &[machine("server", &[], 0, 2), machine("node", &[], 0, 0)]);
        assert_eq!(a[0].machine, "node");
    }
}
