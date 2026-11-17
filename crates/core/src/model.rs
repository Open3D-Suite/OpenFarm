// SPDX-License-Identifier: GPL-3.0-or-later
//! What a farm is made of: engines, jobs, frames, tasks and machines.

use serde::{Deserialize, Serialize};

use crate::order::FrameOrder;

/// The program that opens the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Host {
    Max,
    Blender,
    /// A fake renderer for tests: sleeps and writes a small image.
    Dummy,
}

/// The render engine of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Vray,
    VrayGpu,
    Corona,
    Fstorm,
    Cycles,
    Eevee,
    Dummy,
}

/// Which kind of slot a task takes on a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotKind {
    Cpu,
    Gpu,
}

impl Engine {
    pub const ALL: [Engine; 7] = [Engine::Vray, Engine::VrayGpu, Engine::Corona, Engine::Fstorm, Engine::Cycles, Engine::Eevee, Engine::Dummy];

    pub fn host(self) -> Host {
        match self {
            Engine::Vray | Engine::VrayGpu | Engine::Corona | Engine::Fstorm => Host::Max,
            Engine::Cycles | Engine::Eevee => Host::Blender,
            Engine::Dummy => Host::Dummy,
        }
    }

    /// The slot a task of this engine takes. Cycles depends on the device chosen in the job.
    pub fn slot(self, device: Option<&str>) -> SlotKind {
        match self {
            Engine::VrayGpu | Engine::Fstorm | Engine::Eevee => SlotKind::Gpu,
            Engine::Cycles => match device.unwrap_or("OPTIX").to_ascii_uppercase().as_str() {
                "CPU" => SlotKind::Cpu,
                _ => SlotKind::Gpu,
            },
            Engine::Vray | Engine::Corona | Engine::Dummy => SlotKind::Cpu,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::Vray => "V-Ray",
            Engine::VrayGpu => "V-Ray GPU",
            Engine::Corona => "Corona",
            Engine::Fstorm => "FStorm",
            Engine::Cycles => "Cycles",
            Engine::Eevee => "Eevee",
            Engine::Dummy => "Test renderer",
        }
    }

    /// The software a machine must report to take this engine (see `Software::name`).
    pub fn needs(self) -> &'static [&'static str] {
        match self {
            Engine::Vray | Engine::VrayGpu => &["3dsmax", "vray"],
            Engine::Corona => &["3dsmax", "corona"],
            Engine::Fstorm => &["3dsmax", "fstorm"],
            Engine::Cycles | Engine::Eevee => &["blender"],
            Engine::Dummy => &[],
        }
    }
}

/// A scene stored in the server's content store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneRef {
    /// BLAKE3 of the file, hex.
    pub hash: String,
    /// The file's name as submitted (`kitchen.max`); keeps its extension for the host.
    pub file_name: String,
    /// Where the artist had it; relative texture paths are resolved against this folder.
    pub original_path: String,
    pub size: u64,
}

/// Where frames are written. `name` may contain `####` (frame number, padded to as many digits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSpec {
    pub folder: String,
    pub name: String,
    /// File extension without the dot: exr, png, jpg, tif.
    pub format: String,
}

/// A full Windows path: `X:\…` or `\\server\share\…`. Anything else depends on the current folder or
/// drive of whichever machine renders, and would scatter frames (`\server\share` lands on the current drive).
pub fn is_absolute_path(path: &str) -> bool {
    let p = path.trim();
    let b = p.as_bytes();
    let unc = (p.starts_with("\\\\") || p.starts_with("//")) && p.len() > 2 && !matches!(b[2], b'\\' | b'/');
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    unc || drive
}

impl OutputSpec {
    /// The file name of one frame: `name_####` → `name_0012.exr`. Without `#`, `_####` is added.
    pub fn file_for(&self, frame: i32) -> String {
        let name = if self.name.contains('#') { self.name.clone() } else { format!("{}_####", self.name) };
        let start = name.find('#').unwrap_or(0);
        let count = name[start..].chars().take_while(|c| *c == '#').count();
        let number = if frame < 0 { format!("-{:0width$}", -frame, width = count) } else { format!("{:0width$}", frame, width = count) };
        format!("{}{}{}.{}", &name[..start], number, &name[start + count..], self.format)
    }
}

/// Everything the submitter sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    pub name: String,
    pub engine: Engine,
    pub scene: SceneRef,
    /// As typed: `1-100`, `1-240:4`, `12`.
    pub frames: String,
    #[serde(default)]
    pub camera: Option<String>,
    pub output: OutputSpec,
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Frames per task; `None` = automatic (single frames first, then sized to ~5 minutes).
    #[serde(default)]
    pub chunk: Option<u32>,
    #[serde(default)]
    pub order: FrameOrder,
    /// Cycles: CPU, CUDA, OPTIX, HIP, ONEAPI.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
    /// Engine-specific settings passed to the host as they are (samples, pass limit, …).
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Version of the host program the scene was saved with (`2027`, `5.3.0`).
    #[serde(default)]
    pub host_version: Option<String>,
    /// Machine and Windows user that sent it.
    #[serde(default)]
    pub submitted_by: String,
    /// Start paused instead of queued.
    #[serde(default)]
    pub paused: bool,
}

fn default_priority() -> u8 {
    50
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Rendering,
    Paused,
    Done,
    /// Stopped because something will fail on every machine (missing scene, every task failing).
    Failed,
    Canceled,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        matches!(self, JobState::Done | JobState::Failed | JobState::Canceled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameState {
    Pending,
    Rendering,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    pub frame: i32,
    pub state: FrameState,
    #[serde(default)]
    pub task: Option<u64>,
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub secs: Option<f64>,
    #[serde(default)]
    pub thumb: bool,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Assigned,
    Rendering,
    Done,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub job: u64,
    pub frames: Vec<i32>,
    pub machine: String,
    pub slot: SlotKind,
    pub state: TaskState,
    pub started: u64,
    #[serde(default)]
    pub finished: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub index: u32,
    pub name: String,
    pub vram_mb: u64,
}

/// A program found on a machine. Names: `3dsmax`, `vray`, `corona`, `fstorm`, `blender`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Software {
    pub name: String,
    pub version: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub cpu: String,
    pub threads: u32,
    pub ram_mb: u64,
    pub gpus: Vec<Gpu>,
    pub software: Vec<Software>,
    pub os: String,
}

impl Capabilities {
    pub fn has(&self, name: &str) -> bool {
        self.software.iter().any(|s| s.name == name)
    }

    pub fn can_render(&self, engine: Engine, device: Option<&str>) -> bool {
        engine.needs().iter().all(|n| self.has(n)) && (engine.slot(device) == SlotKind::Cpu || !self.gpus.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Telemetry {
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    #[serde(default)]
    pub gpus: Vec<GpuLoad>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuLoad {
    pub index: u32,
    pub util_pct: u32,
    pub vram_used_mb: u64,
    pub temp_c: u32,
    pub watts: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names() {
        let o = OutputSpec { folder: "x".into(), name: "shot_####".into(), format: "exr".into() };
        assert_eq!(o.file_for(12), "shot_0012.exr");
        let o = OutputSpec { folder: "x".into(), name: "beauty".into(), format: "png".into() };
        assert_eq!(o.file_for(3), "beauty_0003.png");
        let o = OutputSpec { folder: "x".into(), name: "a.##.b".into(), format: "jpg".into() };
        assert_eq!(o.file_for(7), "a.07.b.jpg");
    }

    #[test]
    fn only_full_paths_are_output_folders() {
        assert!(is_absolute_path(r"\\nas\projects\renders"));
        assert!(is_absolute_path(r"Z:\renders"));
        assert!(is_absolute_path("D:/renders"));
        assert!(!is_absolute_path(r"\nas\projects"));
        assert!(!is_absolute_path("renders"));
        assert!(!is_absolute_path("C:renders"));
        assert!(!is_absolute_path(r"\\\x"));
    }

    #[test]
    fn engines_pick_the_right_slot() {
        assert_eq!(Engine::Corona.slot(None), SlotKind::Cpu);
        assert_eq!(Engine::Cycles.slot(Some("cpu")), SlotKind::Cpu);
        assert_eq!(Engine::Cycles.slot(None), SlotKind::Gpu);
        assert_eq!(Engine::Eevee.slot(None), SlotKind::Gpu);
    }
}
