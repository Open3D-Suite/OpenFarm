// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages between an agent (a render machine) and the server, sent as JSON over one WebSocket.

use serde::{Deserialize, Serialize};

use crate::model::{Capabilities, Engine, OutputSpec, SceneRef, SlotKind, Telemetry};

/// Bumped when a message changes in a way an older side can't read.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMsg {
    /// First message on every connection.
    Hello {
        machine_id: String,
        name: String,
        /// The agent's secret, made on its first start. The server keeps it once the machine is approved.
        key: String,
        version: String,
        protocol: u32,
        caps: Capabilities,
    },
    /// Every few seconds.
    Status {
        telemetry: Telemetry,
        /// false = the user suspended this machine from its own window.
        accepting: bool,
    },
    Task {
        task: u64,
        event: TaskEvent,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    /// The host is up and the scene is loading.
    Started,
    Log {
        text: String,
    },
    Progress {
        frame: i32,
        pct: f32,
    },
    FrameDone {
        frame: i32,
        secs: f64,
        files: Vec<String>,
        /// A small JPEG preview, base64.
        #[serde(default)]
        thumb: Option<String>,
        /// Written but suspicious (black, empty, NaN).
        #[serde(default)]
        warning: Option<String>,
    },
    FrameFailed {
        frame: i32,
        error: String,
    },
    /// The task ended. Frames not reported as done count as failed.
    Finished {
        ok: bool,
        #[serde(default)]
        error: Option<String>,
        /// Fails the same way on every machine (missing scene, wrong camera): stop the job.
        #[serde(default)]
        fatal: bool,
        #[serde(default)]
        peak_ram_mb: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        approved: bool,
        server_name: String,
        server_version: String,
        protocol: u32,
    },
    Assign(Assignment),
    Cancel {
        task: u64,
    },
    /// The server's view of whether this machine should take work (suspend / activate from the UI).
    SetEnabled {
        enabled: bool,
    },
}

/// One task for one slot of the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub task: u64,
    pub job: u64,
    pub job_name: String,
    pub engine: Engine,
    pub scene: SceneRef,
    /// Path on the server to download the scene from (`/api/v1/scenes/<hash>`).
    pub scene_url: String,
    pub frames: Vec<i32>,
    #[serde(default)]
    pub camera: Option<String>,
    pub output: OutputSpec,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    pub slot: SlotKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let m = AgentMsg::Task {
            task: 7,
            event: TaskEvent::FrameDone { frame: 3, secs: 1.5, files: vec!["a.exr".into()], thumb: None, warning: None },
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"type\":\"task\"") && s.contains("\"kind\":\"frame_done\""), "{s}");
        assert_eq!(serde_json::from_str::<AgentMsg>(&s).unwrap(), m);
        let w = ServerMsg::SetEnabled { enabled: false };
        let s = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<ServerMsg>(&s).unwrap(), w);
    }
}
