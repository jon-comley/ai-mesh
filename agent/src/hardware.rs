use shared::HardwareSpec;
use std::fs;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    #[error("Failed to read /proc/cpuinfo")]
    CpuInfoReadError,
    #[error("Failed to read /proc/meminfo")]
    MemInfoReadError,
    #[error("Failed to parse CPU info")]
    CpuParseError,
    #[error("Failed to parse memory info")]
    MemParseError,
}

pub fn detect_hardware() -> Result<HardwareSpec, HardwareError> {
    let cpu_model = detect_cpu_model()?;
    let (cpu_cores, cpu_threads) = detect_cpu_counts()?;
    let ram_gb = detect_ram_gb()?;
    let os = detect_os();
    let arch = detect_arch();
    let gpu = detect_gpu();

    Ok(HardwareSpec {
        cpu_model,
        cpu_cores,
        cpu_threads,
        ram_gb,
        os,
        arch,
        gpu,
    })
}

fn detect_cpu_model() -> Result<String, HardwareError> {
    let cpuinfo =
        fs::read_to_string("/proc/cpuinfo").map_err(|_| HardwareError::CpuInfoReadError)?;

    for line in cpuinfo.lines() {
        if line.starts_with("model name")
            && let Some(model) = line.split(':').nth(1)
        {
            return Ok(model.trim().to_string());
        }
    }

    Err(HardwareError::CpuParseError)
}

fn detect_cpu_counts() -> Result<(u32, u32), HardwareError> {
    let cpuinfo =
        fs::read_to_string("/proc/cpuinfo").map_err(|_| HardwareError::CpuInfoReadError)?;

    let cores = cpuinfo.matches("processor").count() as u32;

    // Threads per core is not always available; assume 1 if missing
    let threads = cores;

    Ok((cores, threads))
}

fn detect_ram_gb() -> Result<f32, HardwareError> {
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

fn detect_os() -> String {
    std::env::consts::OS.to_string()
}

fn detect_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn detect_gpu() -> Option<String> {
    // Try NVIDIA first
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

    // Try lspci for AMD/Intel
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
    fn test_detect_os() {
        let os = detect_os();
        assert!(!os.is_empty());
    }

    #[test]
    fn test_detect_arch() {
        let arch = detect_arch();
        assert!(!arch.is_empty());
    }

    #[test]
    fn test_detect_hardware_basic() {
        let hw = detect_hardware().unwrap();
        assert!(!hw.cpu_model.is_empty());
        assert!(hw.cpu_cores > 0);
        assert!(hw.cpu_threads > 0);
        assert!(hw.ram_gb > 0.0);
    }
}
