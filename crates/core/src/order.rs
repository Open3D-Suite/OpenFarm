// SPDX-License-Identifier: GPL-3.0-or-later
//! The order in which a job's frames are handed out.
//!
//! `Smart` renders the first, last and middle frames first (so a broken scene shows up in the first
//! minute, not at frame 800) and then fills the gaps by halving them, so the whole animation can be
//! previewed at a low frame rate early and gets denser until it is complete.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FrameOrder {
    #[default]
    Smart,
    Linear,
}

/// Puts sorted frames in the given order. Every frame appears exactly once.
pub fn ordered(frames: &[i32], order: FrameOrder) -> Vec<i32> {
    match order {
        FrameOrder::Linear => frames.to_vec(),
        FrameOrder::Smart => smart(frames),
    }
}

fn smart(frames: &[i32]) -> Vec<i32> {
    let n = frames.len();
    if n <= 3 {
        return frames.to_vec();
    }
    let mut taken = vec![false; n];
    let mut out = Vec::with_capacity(n);
    let mut take = |i: usize, out: &mut Vec<i32>| {
        if !taken[i] {
            taken[i] = true;
            out.push(frames[i]);
        }
    };
    take(0, &mut out);
    take(n - 1, &mut out);
    take((n - 1) / 2, &mut out);
    // Breadth-first halving of the gaps between frames already taken.
    let mut gaps = vec![(0usize, (n - 1) / 2), ((n - 1) / 2, n - 1)];
    while !gaps.is_empty() {
        let mut next = Vec::new();
        for (a, b) in gaps {
            if b > a + 1 {
                let m = a + (b - a) / 2;
                take(m, &mut out);
                next.push((a, m));
                next.push((m, b));
            }
        }
        gaps = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_starts_with_the_ends_and_the_middle() {
        let frames: Vec<i32> = (1..=100).collect();
        let o = ordered(&frames, FrameOrder::Smart);
        assert_eq!(&o[..3], &[1, 100, 50]);
        let mut sorted = o.clone();
        sorted.sort();
        assert_eq!(sorted, frames, "every frame exactly once");
    }

    #[test]
    fn smart_spreads_early_frames_over_the_whole_range() {
        let frames: Vec<i32> = (0..1000).collect();
        let o = ordered(&frames, FrameOrder::Smart);
        // After 20 frames the biggest hole is at most ~1/16 of the animation.
        let mut first: Vec<i32> = o[..20].to_vec();
        first.sort();
        let biggest = first.windows(2).map(|w| w[1] - w[0]).max().unwrap();
        assert!(biggest <= 1000 / 16 + 1, "biggest gap {biggest}");
    }

    #[test]
    fn short_lists_stay_as_they_are() {
        assert_eq!(ordered(&[5, 6], FrameOrder::Smart), vec![5, 6]);
        assert_eq!(ordered(&[1, 2, 3, 4], FrameOrder::Linear), vec![1, 2, 3, 4]);
    }
}
