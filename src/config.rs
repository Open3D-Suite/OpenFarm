// SPDX-License-Identifier: GPL-3.0-or-later
//! This machine's settings: its role in the farm, its identity and where its data lives.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::auth::Identity;

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
    /// This machine's private key (Ed25519), which signs what it sends to the farm server. It is
    /// kept encrypted by Windows for this user (DPAPI) and written in base64, so the file gives
    /// nothing away and is useless on another computer. Made on the first start.
    #[serde(default)]
    pub private_key: String,
    /// The farm's name, on a server.
    #[serde(default)]
    pub farm_name: String,
    /// On a server: the folders (full paths) from which the other machines of the farm may send
    /// a scene by its path, with the Submit page. Empty: none, only this computer can. Scenes sent
    /// from 3ds Max and Blender are uploads and do not need it.
    #[serde(default)]
    pub scene_folders: Vec<String>,
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
            private_key: String::new(),
            farm_name: format!("{} farm", computer_name()),
            scene_folders: Vec::new(),
            blender_path: None,
            max_batch_path: None,
            server_port: SERVER_PORT,
            local_port: LOCAL_PORT,
            discovery: true,
        }
    }

    pub fn load_or_create(data: &Path) -> Result<Config> {
        let path = data.join("config.json");
        let (mut cfg, mut save) = if path.exists() {
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
            // OpenFarm 0.1.1 kept the machine's secret in clear under "key". It is not read any
            // more; saving the settings again takes it out of the file.
            let old_key = value.get("key").is_some();
            let cfg: Config = serde_json::from_value(value).with_context(|| format!("reading {}", path.display()))?;
            (cfg, old_key)
        } else {
            (Config::new(), true)
        };
        if cfg.name.trim().is_empty() {
            cfg.name = computer_name();
        }
        if let Err(e) = cfg.identity() {
            if !cfg.private_key.is_empty() {
                // Settings from another Windows user or another computer: the key in them is not
                // ours to read. The farm knows this machine by that key and turns the new one away
                // until the owner lets the old machine go.
                tracing::warn!(
                    "this machine's key cannot be read ({e:#}); it gets a new one. The farm will refuse it until this machine is forgotten in Machines on the farm's computer; after that it can join and be approved again"
                );
            }
            cfg.private_key = protected(&Identity::generate()?)?;
            save = true;
        }
        if save {
            cfg.save(data)?;
        }
        Ok(cfg)
    }

    /// This machine's key pair, read back from its protected form.
    pub fn identity(&self) -> Result<Identity> {
        if self.private_key.is_empty() {
            bail!("this machine has no key yet");
        }
        let blob = BASE64.decode(&self.private_key).context("this machine's key is not base64")?;
        let mut secret = crate::dpapi::unprotect(&blob)?;
        let identity = Identity::from_secret(&secret);
        secret.fill(0);
        identity
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
                if s.contains(':') { format!("http://{s}") } else { format!("http://{s}:{}", SERVER_PORT) }
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

/// A private key as it is written in the settings: encrypted for this Windows user, in base64.
fn protected(identity: &Identity) -> Result<String> {
    let mut secret = identity.secret();
    let blob = crate::dpapi::protect(&secret);
    secret.fill(0);
    Ok(BASE64.encode(blob?))
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this-computer".into())
}

/// `%LOCALAPPDATA%\OpenFarm` unless `--data` says otherwise.
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("OpenFarm")
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestDir;

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

    fn on_disk(dir: &TestDir) -> (String, serde_json::Value) {
        let text = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        let value = serde_json::from_str(&text).unwrap();
        (text, value)
    }

    #[test]
    fn a_new_machine_gets_a_key_that_is_not_readable_on_disk() {
        let dir = TestDir::new();
        let cfg = Config::load_or_create(dir.path()).unwrap();
        let identity = cfg.identity().unwrap();
        let (text, value) = on_disk(&dir);
        assert!(value.get("key").is_none());
        let blob = BASE64.decode(value["private_key"].as_str().unwrap()).unwrap();
        let secret = identity.secret();
        assert!(!blob.windows(secret.len()).any(|w| w == secret), "the private key is on disk in clear");
        // Starting again finds the same key and leaves the file alone.
        let again = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(again.identity().unwrap().fingerprint(), identity.fingerprint());
        assert_eq!(on_disk(&dir).0, text);
    }

    #[test]
    fn the_plain_key_of_0_1_1_is_dropped() {
        let dir = TestDir::new();
        let old_key = "5f1d0c6a3b2e4f708192a3b4c5d6e7f85f1d0c6a3b2e4f708192a3b4c5d6e7f8";
        let old = format!(
            r#"{{
  "role": "agent",
  "kind": "node",
  "server": "192.168.2.10",
  "render": true,
  "name": "NODE-07",
  "machine_id": "0b6c2a54-6f0e-4c0e-9a51-0d4d0f4b7a11",
  "key": "{old_key}",
  "farm_name": "NODE-07 farm",
  "blender_path": null,
  "max_batch_path": "D:\\Max\\3dsmaxbatch.exe",
  "server_port": 47100,
  "local_port": 47102,
  "discovery": true
}}"#
        );
        std::fs::write(dir.path().join("config.json"), old).unwrap();
        let cfg = Config::load_or_create(dir.path()).unwrap();
        // Everything else is kept…
        assert_eq!((cfg.role, cfg.kind), (Role::Agent, MachineKind::Node));
        assert_eq!((cfg.server.as_str(), cfg.name.as_str()), ("192.168.2.10", "NODE-07"));
        assert_eq!(cfg.machine_id, "0b6c2a54-6f0e-4c0e-9a51-0d4d0f4b7a11");
        assert_eq!(cfg.max_batch_path.as_deref(), Some(r"D:\Max\3dsmaxbatch.exe"));
        // …the machine has a key pair now, and the old secret is gone from the file.
        let identity = cfg.identity().unwrap();
        let (text, value) = on_disk(&dir);
        assert!(value.get("key").is_none() && !text.contains(old_key), "{text}");
        assert_eq!(value["machine_id"], "0b6c2a54-6f0e-4c0e-9a51-0d4d0f4b7a11");
        let again = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(again.identity().unwrap().fingerprint(), identity.fingerprint());
    }

    #[test]
    fn a_key_from_another_computer_is_replaced() {
        let dir = TestDir::new();
        let mut cfg = Config::load_or_create(dir.path()).unwrap();
        cfg.private_key = BASE64.encode(b"protected somewhere else");
        cfg.save(dir.path()).unwrap();
        assert!(cfg.identity().is_err());
        let fixed = Config::load_or_create(dir.path()).unwrap();
        assert!(fixed.identity().is_ok());
        assert_eq!(fixed.machine_id, cfg.machine_id);
        assert_eq!(on_disk(&dir).1["private_key"], fixed.private_key.as_str());
    }
}
