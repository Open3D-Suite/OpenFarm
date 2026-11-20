// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFarm: a local render farm for 3ds Max and Blender. One program with three parts that a
//! machine runs depending on its role: the farm server, the agent that renders, and the window.

pub mod agent;
pub mod auth;
pub mod config;
pub mod discovery;
// Calls Windows (the Data Protection API) to encrypt this machine's key: that takes `unsafe`.
#[allow(unsafe_code)]
pub mod dpapi;
pub mod imaging;
pub mod integrations;
pub mod local;
pub mod loopback;
// The language shared by every Open3D app: a copy of a common file. Its one call to Windows, for
// the language of Windows, allows `unsafe` by name.
pub mod open3d_settings;
pub mod server;
pub mod ui;
pub mod window;

/// A folder of its own for one test, removed when the test ends.
#[cfg(test)]
pub(crate) struct TestDir(std::path::PathBuf);

#[cfg(test)]
impl TestDir {
    pub(crate) fn new() -> TestDir {
        let dir = std::env::temp_dir().join(format!("openfarm-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("a folder for the test");
        TestDir(dir)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Starts OpenFarm again with the same arguments (after a change of role), telling the new
/// process to wait for this one's port instead of handing over to it.
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let mut args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--restarting").collect();
        args.push("--restarting".into());
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
}
