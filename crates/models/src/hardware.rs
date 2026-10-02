//! What this machine can do. Detected once; tests pass profiles explicitly.

use std::path::Path;
use std::process::Command;

use ancilo_core::{Config, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Gpu {
    /// Apple Silicon: unified memory shared with the CPU.
    Metal,
    Cuda,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HardwareProfile {
    pub os: String,
    pub arch: String,
    pub chip: String,
    pub total_ram_bytes: u64,
    pub gpu: Gpu,
    /// Memory the GPU may use for models. On Apple Silicon this is the Metal
    /// working-set limit (a share of unified memory).
    pub gpu_memory_bytes: Option<u64>,
    pub performance_cores: u32,
    pub total_cores: u32,
    /// Free space where Ancilo stores models.
    pub free_disk_bytes: u64,
}

pub const GIB: u64 = 1024 * 1024 * 1024;

impl HardwareProfile {
    /// A typical Apple Silicon machine (used in tests and docs).
    pub fn apple(ram_gib: u64) -> Self {
        let ram = ram_gib * GIB;
        Self {
            os: "macos".into(),
            arch: "aarch64".into(),
            chip: format!("Apple M-series ({ram_gib} GB)"),
            total_ram_bytes: ram,
            gpu: Gpu::Metal,
            gpu_memory_bytes: Some(metal_working_set(ram)),
            performance_cores: 8,
            total_cores: 12,
            free_disk_bytes: 500 * GIB,
        }
    }
}

/// Default Metal working-set limit: macOS lets the GPU wire about two thirds
/// of RAM on small machines and three quarters on larger ones.
pub fn metal_working_set(total_ram: u64) -> u64 {
    if total_ram > 36 * GIB {
        total_ram / 4 * 3
    } else {
        total_ram / 3 * 2
    }
}

fn sysctl(name: &str) -> Option<String> {
    let out = Command::new("sysctl").arg("-n").arg(name).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn free_disk(path: &Path) -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    disks
        .list()
        .iter()
        .filter(|d| target.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
        .unwrap_or(0)
}

/// Memory other programs leave free right now (live – changes as programs
/// open and close).
pub fn available_memory_now() -> Option<u64> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    Some(sys.available_memory()).filter(|a| *a > 0)
}

/// Detects the hardware, or reads `config.hardware_override` (tests).
pub fn detect(config: &Config, data_dir: &Path) -> Result<HardwareProfile> {
    if let Some(path) = &config.hardware_override {
        let text = std::fs::read_to_string(path)?;
        return serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("hardware override {}: {e}", path.display())));
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let free_disk_bytes = free_disk(data_dir);

    if os == "macos" {
        let total_ram = sysctl("hw.memsize")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| sys.total_memory());
        let wired_limit = sysctl("iogpu.wired_limit_mb")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|mb| *mb > 0)
            .map(|mb| mb * 1024 * 1024);
        let apple = arch == "aarch64";
        return Ok(HardwareProfile {
            chip: sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "Mac".into()),
            gpu: if apple { Gpu::Metal } else { Gpu::None },
            gpu_memory_bytes: apple
                .then(|| wired_limit.unwrap_or_else(|| metal_working_set(total_ram))),
            performance_cores: sysctl("hw.perflevel0.physicalcpu")
                .and_then(|s| s.parse().ok())
                .unwrap_or(total_cores),
            total_cores,
            total_ram_bytes: total_ram,
            free_disk_bytes,
            os,
            arch,
        });
    }
    Ok(HardwareProfile {
        chip: sysinfo::System::cpu_arch(),
        gpu: Gpu::None,
        gpu_memory_bytes: None,
        performance_cores: total_cores,
        total_cores,
        total_ram_bytes: sys.total_memory(),
        free_disk_bytes,
        os,
        arch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_something_plausible() {
        let dir = tempfile::tempdir().unwrap();
        let hw = detect(&Config::default(), dir.path()).unwrap();
        assert!(hw.total_ram_bytes > GIB);
        assert!(hw.total_cores >= 1);
        if hw.gpu == Gpu::Metal {
            assert!(hw.gpu_memory_bytes.unwrap() < hw.total_ram_bytes);
        }
    }

    #[test]
    fn override_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hw.json");
        std::fs::write(
            &file,
            serde_json::to_string(&HardwareProfile::apple(16)).unwrap(),
        )
        .unwrap();
        let config = Config {
            hardware_override: Some(file),
            ..Config::default()
        };
        assert_eq!(
            detect(&config, dir.path()).unwrap().total_ram_bytes,
            16 * GIB
        );
    }
}
