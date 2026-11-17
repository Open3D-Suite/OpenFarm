// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFarm: a local render farm for 3ds Max and Blender. One program with three parts that a
//! machine runs depending on its role: the farm server, the agent that renders, and the window.

pub mod agent;
pub mod auth;
pub mod config;
pub mod discovery;
pub mod imaging;
pub mod integrations;
pub mod local;
pub mod server;
pub mod ui;
pub mod window;

/// Starts OpenFarm again with the same arguments (after a change of role), telling the new
/// process to wait for this one's port instead of handing over to it.
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let mut args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--restarting").collect();
        args.push("--restarting".into());
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
}
