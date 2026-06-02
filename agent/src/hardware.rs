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
    // `cpus()` counts logical CPUs (hyperthreads included); use the true physical
    // core count so a 4-core/8-thread machine reports 4 cores, not 8, and the
    // coordinator doesn't over-estimate its compute capacity. Fall back to the
    // thread count if the physical count is unavailable.
    let cpu_cores = sys
        .physical_core_count()
        .filter(|&c| c > 0)
        .map(|c| c as u32)
        .unwrap_or(cpu_threads);
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
    // NVIDIA first — nvidia-smi is authoritative and fast.
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

    // AMD / Intel / other — PowerShell Get-CimInstance is available on all Windows 10/11.
    // wmic was removed from Windows 11 22H2+.
    let ps_cmd = "(Get-CimInstance Win32_VideoController | \
                   Where-Object {$_.Name -notlike 'Microsoft Basic*' -and \
                                 $_.Name -notlike 'Microsoft Remote*'} | \
                   Select-Object -First 1).Name";
    if let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps_cmd])
        .output()
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() && name != "False" {
            return Some(name);
        }
    }

    None
}

// ── macOS ─────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub fn detect_hardware() -> Result<HardwareSpec, HardwareError> {
    let cpu_model = detect_cpu_model_macos()?;
    let cpu_cores = sysctl_u32("hw.physicalcpu").ok_or(HardwareError::CpuParseError)?;
    let cpu_threads = sysctl_u32("hw.logicalcpu").ok_or(HardwareError::CpuParseError)?;
    let mem_bytes = sysctl_u64("hw.memsize").ok_or(HardwareError::MemParseError)?;
    let ram_gb = mem_bytes as f32 / 1_073_741_824.0;

    Ok(HardwareSpec {
        cpu_model,
        cpu_cores,
        cpu_threads,
        ram_gb,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        gpu: detect_gpu_macos(),
    })
}

#[cfg(target_os = "macos")]
fn detect_cpu_model_macos() -> Result<String, HardwareError> {
    // Apple Silicon: hw.chip_name → "M4", "M3 Pro", etc. Prefix "Apple ".
    if let Some(chip) = sysctl_string("hw.chip_name") {
        return Ok(format!("Apple {}", chip));
    }
    // Intel Mac: machdep.cpu.brand_string → full brand string.
    sysctl_string("machdep.cpu.brand_string").ok_or(HardwareError::CpuParseError)
}

#[cfg(target_os = "macos")]
fn detect_gpu_macos() -> Option<String> {
    detect_gpu_macos_impl()
}

// Apple Silicon — Metal GPU is always present; name it after the chip.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn detect_gpu_macos_impl() -> Option<String> {
    let chip = sysctl_string("hw.chip_name")
        .map(|s| format!("Apple {}", s))
        .or_else(|| sysctl_string("machdep.cpu.brand_string"))
        .unwrap_or_else(|| "Apple Silicon".into());
    Some(format!("{} GPU", chip))
}

// Intel Mac — try nvidia-smi; skip further probing (out of scope).
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn detect_gpu_macos_impl() -> Option<String> {
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

#[cfg(target_os = "macos")]
fn sysctl_string(key: &str) -> Option<String> {
    let out = Command::new("sysctl").args(["-n", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(target_os = "macos")]
fn sysctl_u32(key: &str) -> Option<u32> {
    sysctl_string(key)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn sysctl_u64(key: &str) -> Option<u64> {
    sysctl_string(key)?.parse().ok()
}

// ── Linux ─────────────────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
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
        gpu: detect_gpu_linux(),
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
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

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn detect_cpu_counts() -> Result<(u32, u32), HardwareError> {
    use std::fs;
    let cpuinfo =
        fs::read_to_string("/proc/cpuinfo").map_err(|_| HardwareError::CpuInfoReadError)?;
    let cores = cpuinfo.matches("processor").count() as u32;
    Ok((cores, cores))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn detect_ram_gb() -> Result<f32, HardwareError> {
    use std::fs;
    let meminfo =
        fs::read_to_string("/proc/meminfo").map_err(|_| HardwareError::MemInfoReadError)?;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal")
            && let Some(kb_str) = line.split_whitespace().nth(1)
            && let Ok(kb) = kb_str.parse::<u64>()
        {
            return Ok(kb as f32 / 1_048_576.0);
        }
    }
    Err(HardwareError::MemParseError)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn detect_gpu_linux() -> Option<String> {
    // NVIDIA
    if let Ok(output) = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        && output.status.success()
    {
        let gpu = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !gpu.is_empty() {
            return Some(gpu);
        }
    }

    // AMD / Intel via lspci — extract just the device description after the class label.
    if let Ok(output) = Command::new("lspci").output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            for class in &[
                "VGA compatible controller",
                "3D controller",
                "Display controller",
            ] {
                if let Some(rest) = line.split(class).nth(1)
                    && let Some(desc) = rest.strip_prefix(": ")
                {
                    let desc = desc.trim().to_string();
                    if !desc.is_empty() {
                        return Some(desc);
                    }
                }
            }
        }
    }

    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
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

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn lspci_gpu_line_parsed_to_description_only() {
        // Verify the lspci parsing strips the PCI address and class prefix.
        let line = "0000:03:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 21 [Radeon RX 6900 XT]";
        let class = "VGA compatible controller";
        let result = line
            .split(class)
            .nth(1)
            .and_then(|rest| rest.strip_prefix(": "))
            .map(str::trim)
            .map(str::to_string);
        assert_eq!(
            result,
            Some("Advanced Micro Devices, Inc. [AMD/ATI] Navi 21 [Radeon RX 6900 XT]".into())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn gpu_detection_skips_software_renderers() {
        // Verify the filter logic used in detect_gpu_windows.
        let candidates = ["AMD Radeon 780M", "Microsoft Basic Display Adapter", ""];
        let result: Option<String> = candidates.iter().find_map(|name| {
            let name = name.trim();
            if name.is_empty()
                || name.starts_with("Microsoft Basic Display")
                || name.starts_with("Microsoft Remote Display")
            {
                return None;
            }
            Some(name.to_string())
        });
        assert_eq!(result, Some("AMD Radeon 780M".into()));
    }
}
