// SPDX-License-Identifier: GPL-3.0-or-later
//! What this machine has: CPU, memory, NVIDIA GPUs and the render software installed, plus live
//! load for the farm view.

use std::path::{Path, PathBuf};

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use openfarm_core::model::{Capabilities, Gpu, GpuLoad, Software, Telemetry};
use sysinfo::System;

use crate::config::Config;

pub struct Probe {
    sys: System,
    nvml: Option<Nvml>,
}

impl Probe {
    pub fn new() -> Probe {
        let mut sys = System::new().expect("system information");
        sys.refresh_cpu_all();
        sys.refresh_memory();
        Probe { sys, nvml: Nvml::init().ok() }
    }

    pub fn capabilities(&mut self, cfg: &Config) -> Capabilities {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        let cpu = self.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
        let mut gpus = Vec::new();
        if let Some(nvml) = &self.nvml {
            for i in 0..nvml.device_count().unwrap_or(0) {
                if let Ok(d) = nvml.device_by_index(i) {
                    gpus.push(Gpu {
                        index: i,
                        name: d.name().unwrap_or_default(),
                        vram_mb: d.memory_info().map(|m| m.total / (1024 * 1024)).unwrap_or(0),
                    });
                }
            }
        }
        Capabilities {
            cpu,
            threads: self.sys.cpus().len() as u32,
            ram_mb: self.sys.total_memory() / (1024 * 1024),
            gpus,
            software: find_software(cfg),
            os: System::long_os_version().unwrap_or_default(),
        }
    }

    pub fn telemetry(&mut self) -> Telemetry {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let mut gpus = Vec::new();
        if let Some(nvml) = &self.nvml {
            for i in 0..nvml.device_count().unwrap_or(0) {
                if let Ok(d) = nvml.device_by_index(i) {
                    gpus.push(GpuLoad {
                        index: i,
                        util_pct: d.utilization_rates().map(|u| u.gpu).unwrap_or(0),
                        vram_used_mb: d.memory_info().map(|m| m.used / (1024 * 1024)).unwrap_or(0),
                        temp_c: d.temperature(TemperatureSensor::Gpu).unwrap_or(0),
                        watts: d.power_usage().map(|mw| mw as f32 / 1000.0).unwrap_or(0.0),
                    });
                }
            }
        }
        Telemetry {
            cpu_pct: self.sys.global_cpu_usage(),
            ram_used_mb: self.sys.used_memory() / (1024 * 1024),
            gpus,
        }
    }
}

impl Default for Probe {
    fn default() -> Self {
        Probe::new()
    }
}

fn program_files() -> PathBuf {
    std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// Folders inside `dir` whose name starts with `prefix`, newest name last.
fn dirs_starting(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter(|e| e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix.to_lowercase()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn any_file(dir: &Path, test: impl Fn(&str) -> bool) -> bool {
    std::fs::read_dir(dir)
        .map(|r| r.flatten().any(|e| test(&e.file_name().to_string_lossy().to_lowercase())))
        .unwrap_or(false)
}

/// The newest 3ds Max with `3dsmaxbatch.exe` and the Blender to use. A path in the settings wins.
pub fn find_software(cfg: &Config) -> Vec<Software> {
    let mut out = Vec::new();
    let mut maxes: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = &cfg.max_batch_path {
        let p = PathBuf::from(p);
        if p.exists() {
            let year = p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().replace("3ds Max ", "")).unwrap_or_default();
            maxes.push((year, p));
        }
    }
    for dir in dirs_starting(&program_files().join("Autodesk"), "3ds Max ") {
        let batch = dir.join("3dsmaxbatch.exe");
        if batch.exists() {
            let year = dir.file_name().unwrap().to_string_lossy().replace("3ds Max ", "");
            maxes.push((year, batch));
        }
    }
    if let Some((year, batch)) = maxes.into_iter().max_by(|a, b| a.0.cmp(&b.0)) {
        let max_dir = batch.parent().unwrap().to_path_buf();
        let plugins = max_dir.join("plugins");
        let app_plugins = program_data().join("Autodesk").join("ApplicationPlugins");
        out.push(Software { name: "3dsmax".into(), version: year.clone(), path: batch.to_string_lossy().into_owned() });
        let corona = plugins.join(format!("CoronaMax{year}.dlr"));
        if corona.exists() {
            out.push(Software { name: "corona".into(), version: year.clone(), path: corona.to_string_lossy().into_owned() });
        }
        let vray_bundle = app_plugins.join(format!("VRay3dsMax{year}"));
        if vray_bundle.exists() || plugins.join(format!("vrender{year}.dlr")).exists() {
            out.push(Software { name: "vray".into(), version: year.clone(), path: vray_bundle.to_string_lossy().into_owned() });
        }
        let fstorm = any_file(&plugins, |n| n.starts_with("fstorm") && n.ends_with(".dlr"))
            || !dirs_starting(&app_plugins, "fstorm").is_empty();
        if fstorm {
            out.push(Software { name: "fstorm".into(), version: year.clone(), path: plugins.to_string_lossy().into_owned() });
        }
    }
    let mut blenders: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = &cfg.blender_path {
        let p = PathBuf::from(p);
        if p.exists() {
            blenders.push(("custom".into(), p));
        }
    }
    for dir in dirs_starting(&program_files().join("Blender Foundation"), "Blender") {
        let exe = dir.join("blender.exe");
        if exe.exists() {
            let v = dir.file_name().unwrap().to_string_lossy().replace("Blender", "").trim().to_string();
            blenders.push((v, exe));
        }
    }
    if let Some((v, exe)) = blenders.into_iter().max_by(|a, b| version_key(&a.0).cmp(&version_key(&b.0))) {
        out.push(Software { name: "blender".into(), version: v, path: exe.to_string_lossy().into_owned() });
    }
    out
}

fn version_key(v: &str) -> Vec<u32> {
    if v == "custom" {
        return vec![u32::MAX];
    }
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version_key("5.10") > version_key("5.9"));
        assert!(version_key("custom") > version_key("9.0"));
    }
}
