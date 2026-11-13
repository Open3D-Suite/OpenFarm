// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFarm's core: the farm's data, frame lists and render order, the agent protocol and the
//! scheduler. Nothing here does IO, so all of it is tested with plain `cargo test`.

pub mod frames;
pub mod model;
pub mod order;
pub mod proto;
pub mod scheduler;

/// Seconds a task should take when the chunk size is automatic.
pub const TARGET_TASK_SECS: f64 = 300.0;

/// Frames per task for an automatic chunk, from the average seconds per frame measured so far.
/// No measure yet = 1 (the first tasks are single frames, which also finds errors fastest).
pub fn auto_chunk(avg_secs_per_frame: Option<f64>) -> u32 {
    match avg_secs_per_frame {
        Some(s) if s > 0.0 => ((TARGET_TASK_SECS / s).round() as u32).clamp(1, 50),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn auto_chunks() {
        assert_eq!(super::auto_chunk(None), 1);
        assert_eq!(super::auto_chunk(Some(600.0)), 1);
        assert_eq!(super::auto_chunk(Some(60.0)), 5);
        assert_eq!(super::auto_chunk(Some(0.5)), 50);
    }
}
