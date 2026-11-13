// SPDX-License-Identifier: GPL-3.0-or-later
//! The 3ds Max button and the Blender add-on that send jobs. They are built into OpenFarm.exe and
//! installed from the window (This machine > Integrations), so they always match this version.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;

const MAX_FILES: &[(&str, &str)] = &[
    ("PackageContents.xml", include_str!("../integrations/max/OpenFarm.bundle/PackageContents.xml")),
    ("Contents/scripts/OpenFarm_bundle.ms", include_str!("../integrations/max/OpenFarm.bundle/Contents/scripts/OpenFarm_bundle.ms")),
    ("Contents/scripts/openfarm_max.py", include_str!("../integrations/max/OpenFarm.bundle/Contents/scripts/openfarm_max.py")),
    ("Contents/macroscripts/OpenFarm.mcr", include_str!("../integrations/max/OpenFarm.bundle/Contents/macroscripts/OpenFarm.mcr")),
];

const BLENDER_FILES: &[(&str, &str)] = &[
    ("__init__.py", include_str!("../integrations/blender/openfarm/__init__.py")),
    ("blender_manifest.toml", include_str!("../integrations/blender/openfarm/blender_manifest.toml")),
];

fn appdata() -> Result<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| anyhow!("APPDATA is not set"))
}

fn write_all(dir: &PathBuf, files: &[(&str, &str)]) -> Result<()> {
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Installs the 3ds Max button as an ApplicationPlugins bundle for this Windows user. 3ds Max loads it
/// on its next start: Customize User Interface > Category "OpenFarm" > "Send to OpenFarm".
pub fn install_max() -> Result<String> {
    let dir = appdata()?.join("Autodesk").join("ApplicationPlugins").join("OpenFarm.bundle");
    write_all(&dir, MAX_FILES)?;
    Ok(dir.to_string_lossy().into_owned())
}

/// Installs the Blender add-on as a user extension and turns it on.
pub fn install_blender(cfg: &Config) -> Result<String> {
    let software = crate::agent::probe::find_software(cfg);
    let blender = software.iter().find(|s| s.name == "blender").ok_or_else(|| anyhow!("Blender was not found on this computer"))?;
    let version: String = blender.version.split('.').take(2).collect::<Vec<_>>().join(".");
    if version.is_empty() || version == "custom" {
        bail!("cannot tell which Blender version is installed");
    }
    let dir = appdata()?
        .join("Blender Foundation")
        .join("Blender")
        .join(&version)
        .join("extensions")
        .join("user_default")
        .join("openfarm");
    write_all(&dir, BLENDER_FILES)?;
    let status = std::process::Command::new(&blender.path)
        .args([
            "-b",
            "--python-expr",
            "import bpy, addon_utils\naddon_utils.enable('bl_ext.user_default.openfarm', default_set=True, persistent=True)\nbpy.ops.wm.save_userpref()",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("starting Blender to turn the add-on on")?;
    if !status.success() {
        bail!("Blender could not turn the add-on on ({status})");
    }
    Ok(dir.to_string_lossy().into_owned())
}
