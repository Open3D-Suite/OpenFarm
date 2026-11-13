// SPDX-License-Identifier: GPL-3.0-or-later
//! This machine's settings: its role in the farm, its identity and where its data lives.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SERVER_PORT: u16 = 47100;
pub const DISCOVERY_PORT: u16 = 47101;
pub const LOCAL_PORT: u16 = 47102;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Not chosen yet: the window shows the first-run page.
    Unset,
    /// Runs the farm (and may render too).
    Server,
    /// Joins a farm to render and to send jobs.
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MachineKind {
    /// Someone works on it; it comes after dedicated nodes when work is handed out.
    Workstation,
    /// A dedicated render machine.
    Node,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub role: Role,
    #[serde(default = "default_kind")]
    pub kind: MachineKind,
    /// The farm server's address (`192.168.2.10` or `192.168.2.10:47100`) for an agent.
    #[serde(default)]
    pub server: String,
    /// The server renders too.
    #[serde(default = "yes")]
    pub render: bool,
    /// Shown in the farm. Defaults to the computer's name.
    #[serde(default)]
    pub name: String,
    /// Stable identity of this machine in the farm.
    pub machine_id: String,
    /// Secret sent to the server; the server remembers it once the machine is approved.
    pub key: String,
    /// The farm's name, on a server.
    #[serde(default)]
    pub farm_name: String,
    /// Override the program paths found automatically.
    #[serde(default)]
    pub blender_path: Option<String>,
    #[serde(default)]
    pub max_batch_path: Option<String>,
    /// Ports (tests run several machines on one computer).
    #[serde(default = "server_port")]
    pub server_port: u16,
    #[serde(default = "local_port")]
    pub local_port: u16,
    /// Turn the LAN discovery answer off.
    #[serde(default = "yes")]
    pub discovery: bool,
}

fn default_kind() -> MachineKind {
    MachineKind::Workstation
}
fn yes() -> bool {
    true
}
fn server_port() -> u16 {
    SERVER_PORT
}
fn local_port() -> u16 {
    LOCAL_PORT
}

impl Config {
    pub fn new() -> Config {
        Config {
            role: Role::Unset,
            kind: MachineKind::Workstation,
            server: String::new(),
            render: true,
            name: computer_name(),
            machine_id: uuid::Uuid::new_v4().to_string(),
            key: format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
            farm_name: format!("{} farm", computer_name()),
            blender_path: None,
            max_batch_path: None,
            server_port: SERVER_PORT,
            local_port: LOCAL_PORT,
            discovery: true,
        }
    }

    pub fn load_or_create(data: &Path) -> Result<Config> {
        let path = data.join("config.json");
        if path.exists() {
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let mut cfg: Config = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
            if cfg.name.trim().is_empty() {
                cfg.name = computer_name();
            }
            return Ok(cfg);
        }
        let cfg = Config::new();
        cfg.save(data)?;
        Ok(cfg)
    }

    pub fn save(&self, data: &Path) -> Result<()> {
        std::fs::create_dir_all(data)?;
        let tmp = data.join("config.json.part");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, data.join("config.json"))?;
        Ok(())
    }

    /// `http://host:port` of the farm server as this machine reaches it.
    pub fn server_url(&self) -> String {
        match self.role {
            Role::Server => format!("http://127.0.0.1:{}", self.server_port),
            _ => {
                let s = self.server.trim().trim_start_matches("http://").trim_end_matches('/');
                if s.contains(':') {
                    format!("http://{s}")
                } else {
                    format!("http://{s}:{}", SERVER_PORT)
                }
            }
        }
    }

    /// 0 = render node, 1 = workstation, 2 = the server (served last).
    pub fn rank(&self) -> u8 {
        match (self.role, self.kind) {
            (Role::Server, _) => 2,
            (_, MachineKind::Node) => 0,
            _ => 1,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::new()
    }
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this-computer".into())
}

/// `%LOCALAPPDATA%\OpenFarm` unless `--data` says otherwise.
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OpenFarm")
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        let mut c = Config::new();
        c.role = Role::Agent;
        c.server = "192.168.2.10".into();
        assert_eq!(c.server_url(), "http://192.168.2.10:47100");
        c.server = "http://farm:5000/".into();
        assert_eq!(c.server_url(), "http://farm:5000");
        c.role = Role::Server;
        assert_eq!(c.server_url(), "http://127.0.0.1:47100");
    }
}
