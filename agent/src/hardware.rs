use shared::HardwareSpec;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    #[error("Failed to read CPU info")]
    CpuInfoReadError,
    #[error("Failed to read memory info")]
    MemInfoReadError,
    #[error("Failed to parse CPU info")]
    CpuParseError,
    #[error("Failed to parse memory info")]
    MemParseError,
}

// ── Windows ──────────────────────────────────────────────────────────────────
//
// Uses the `sysinfo` crate — no child-process spawning, so the Windows
// service can be stopped cleanly at any point during hardware detection.

#[cfg(target_os = "windows")]
pub fn detect_hardware() -> Result<HardwareSpec, HardwareError> {
    use sysinfo::System;

    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cpus = sys.cpus();
    if cpus.is_empty() {
        return Err(HardwareError::CpuParseError);
    }

    let cpu_model = cpus[0].brand().trim().to_string();
    let cpu_threads = cpus.len() as u32;
    // sysinfo doesn't directly expose physical core count; use thread count as fallback.
    let cpu_cores = cpu_threads;
    let ram_gb = sys.total_memory() as f32 / 1_073_741_824.0;

    Ok(HardwareSpec {
        cpu_model,
        cpu_cores,
        cpu_threads,
        ram_gb,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        gpu: detect_gpu_windows(),
    })
}

#[cfg(target_os = "windows")]
fn detect_gpu_windows() -> Option<String> {
    // Try NVIDIA first (nvidia-smi is a small native binary, not a full shell).
    if let Ok(out) = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

// ── Linux / macOS ─────────────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
pub fn detect_hardware() -> Result<HardwareSpec, HardwareError> {
    let cpu_model = detect_cpu_model()?;
    let (cpu_cores, cpu_threads) = detect_cpu_counts()?;
    let ram_gb = detect_ram_gb()?;

    Ok(HardwareSpec {
        cpu_model,
        cpu_cores,
        cpu_threads,
        ram_gb,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        gpu: detect_gpu(),
    })
}

#[cfg(not(target_os = "windows"))]
fn detect_cpu_model() -> Result<String, HardwareError> {
    use std::fs;
    let cpuinfo =
        fs::read_to_string("/proc/cpuinfo").map_err(|_| HardwareError::CpuInfoReadError)?;
    // x86: each core has "model name : Intel Core i7-..."
    // ARM: no "model name"; board identity is in "Model : Raspberry Pi 5 ..."
    for line in cpuinfo.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            if key == "model name" || key == "Model" {
                return Ok(value.trim().to_string());
            }
        }
    }
    Err(HardwareError::CpuParseError)
}

#[cfg(not(target_os = "windows"))]
fn detect_cpu_counts() -> Result<(u32, u32), HardwareError> {
    use std::fs;
    let cpuinfo =
        fs::read_to_string("/proc/cpuinfo").map_err(|_| HardwareError::CpuInfoReadError)?;
    let cores = cpuinfo.matches("processor").count() as u32;
    Ok((cores, cores))
}

#[cfg(not(target_os = "windows"))]
fn detect_ram_gb() -> Result<f32, HardwareError> {
    use std::fs;
    let meminfo =
        fs::read_to_string("/proc/meminfo").map_err(|_| HardwareError::MemInfoReadError)?;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal")
            && let Some(kb_str) = line.split_whitespace().nth(1)
            && let Ok(kb) = kb_str.parse::<u64>()
        {
            return Ok(kb as f32 / 1_048_576.0); // KB → GB
        }
    }
    Err(HardwareError::MemParseError)
}

#[cfg(not(target_os = "windows"))]
fn detect_gpu() -> Option<String> {
    if let Ok(output) = Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output()
        && output.status.success()
    {
        let gpu = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !gpu.is_empty() {
            return Some(gpu);
        }
    }
    if let Ok(output) = Command::new("lspci").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("VGA compatible controller") {
                return Some(line.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_hardware_basic() {
        let hw = detect_hardware().unwrap();
        assert!(!hw.cpu_model.is_empty());
        assert!(hw.cpu_cores > 0);
        assert!(hw.cpu_threads > 0);
        assert!(hw.ram_gb > 0.0);
        assert!(!hw.os.is_empty());
        assert!(!hw.arch.is_empty());
    }

    #[test]
    fn test_cpu_model_no_trailing_whitespace() {
        // sysinfo brand() strings from Windows often have trailing spaces.
        let raw = "AMD Ryzen 7 8745HS w/ Radeon 780M Graphics     ";
        assert_eq!(
            raw.trim().to_string(),
            "AMD Ryzen 7 8745HS w/ Radeon 780M Graphics"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_cpu_model_arm_fallback() {
        // ARM /proc/cpuinfo has no "model name" — only "Model" (board identity).
        let cpuinfo =
            "processor\t: 0\nBogoMIPS\t: 108.00\nModel\t: Raspberry Pi 5 Model B Rev 1.1\n";
        let result = cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "model name" || k.trim() == "Model").then(|| v.trim().to_string())
        });
        assert_eq!(result, Some("Raspberry Pi 5 Model B Rev 1.1".to_string()));
    }
}
