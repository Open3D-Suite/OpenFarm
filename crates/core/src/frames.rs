// SPDX-License-Identifier: GPL-3.0-or-later
//! Frame lists as people type them: `12`, `1-100`, `1-240:4`, `1,7,22`, or any mix separated by commas.

use std::collections::BTreeSet;
use std::fmt;

/// A frame list that could not be read, with the part that was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError(pub String);

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FrameError {}

/// The most frames one job may hold (a guard against typing `1-99999999`).
pub const MAX_FRAMES: usize = 100_000;

/// Reads a frame list. Frames come back sorted and without repeats. Negative frames are allowed
/// (`-10--1`), so a range is split at the first `-` that follows a digit.
pub fn parse(text: &str) -> Result<Vec<i32>, FrameError> {
    let mut out = BTreeSet::new();
    let text = text.trim();
    if text.is_empty() {
        return Err(FrameError("no frames given".into()));
    }
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (range, step) = match part.split_once(':') {
            Some((r, s)) => {
                let step: i32 = s.trim().parse().map_err(|_| FrameError(format!("'{part}': the step after ':' is not a number")))?;
                if step < 1 {
                    return Err(FrameError(format!("'{part}': the step must be 1 or more")));
                }
                (r.trim(), step)
            }
            None => (part, 1),
        };
        let (start, end) = split_range(range).ok_or_else(|| FrameError(format!("'{part}' is not a frame or a range")))?;
        if end < start {
            return Err(FrameError(format!("'{part}': the range ends before it starts")));
        }
        let mut f = start;
        while f <= end {
            out.insert(f);
            if out.len() > MAX_FRAMES {
                return Err(FrameError(format!("more than {MAX_FRAMES} frames")));
            }
            match f.checked_add(step) {
                Some(n) => f = n,
                None => break,
            }
        }
    }
    if out.is_empty() {
        return Err(FrameError("no frames given".into()));
    }
    Ok(out.into_iter().collect())
}

fn split_range(text: &str) -> Option<(i32, i32)> {
    let bytes = text.as_bytes();
    // A '-' is a separator when something that can end a number comes before it.
    for i in 1..bytes.len() {
        if bytes[i] == b'-' && (bytes[i - 1].is_ascii_digit() || bytes[i - 1] == b' ') {
            let a = text[..i].trim().parse().ok()?;
            let b = text[i + 1..].trim().parse().ok()?;
            return Some((a, b));
        }
    }
    let n = text.trim().parse().ok()?;
    Some((n, n))
}

/// Writes a sorted frame list back in its shortest form: `1-5,8,10-20:2`.
pub fn format(frames: &[i32]) -> String {
    let mut sorted: Vec<i32> = frames.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        // The longest run with one constant step from here.
        if i + 2 < sorted.len() {
            let step = sorted[i + 1] - start;
            let mut j = i + 1;
            while j + 1 < sorted.len() && sorted[j + 1] - sorted[j] == step {
                j += 1;
            }
            if j - i >= 2 {
                if step == 1 {
                    parts.push(format!("{}-{}", start, sorted[j]));
                } else {
                    parts.push(format!("{}-{}:{}", start, sorted[j], step));
                }
                i = j + 1;
                continue;
            }
        }
        parts.push(start.to_string());
        i += 1;
    }
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_usual_forms() {
        assert_eq!(parse("12").unwrap(), vec![12]);
        assert_eq!(parse("1-5").unwrap(), vec![1, 2, 3, 4, 5]);
        assert_eq!(parse("0-10:5").unwrap(), vec![0, 5, 10]);
        assert_eq!(parse("1, 7,22").unwrap(), vec![1, 7, 22]);
        assert_eq!(parse("3,1-2,3").unwrap(), vec![1, 2, 3]);
        assert_eq!(parse("-2-1").unwrap(), vec![-2, -1, 0, 1]);
        assert_eq!(parse("-5--3").unwrap(), vec![-5, -4, -3]);
    }

    #[test]
    fn refuses_nonsense() {
        assert!(parse("").is_err());
        assert!(parse("a-b").is_err());
        assert!(parse("10-1").is_err());
        assert!(parse("1-10:0").is_err());
        assert!(parse("1-999999999").is_err());
    }

    #[test]
    fn writes_the_shortest_form() {
        assert_eq!(format(&[1, 2, 3, 4, 5]), "1-5");
        assert_eq!(format(&[0, 5, 10, 15]), "0-15:5");
        assert_eq!(format(&[1, 2, 7]), "1,2,7");
        assert_eq!(format(&[1, 2, 3, 9, 20, 21, 22]), "1-3,9,20-22");
        assert_eq!(parse(&format(&[4, 8, 12, 13])).unwrap(), vec![4, 8, 12, 13]);
    }
}
