/// GPU metrics — Linux sysfs (amdgpu driver) and Windows Performance Counters.
/// Returns None on CPU-only nodes or any node where GPU metrics are unavailable.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSample {
    pub usage_pct: f32,
    pub vram_used_gb: f32,
    pub vram_total_gb: f32,
}

// ── Linux ─────────────────────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
pub fn read_gpu_sample() -> Option<GpuSample> {
    let usage = read_sysfs_u64("/sys/class/drm/card0/device/gpu_busy_percent")?;
    let used = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_used")?;
    let total = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_total")?;
    if total == 0 {
        return None;
    }
    Some(GpuSample {
        usage_pct: usage as f32,
        vram_used_gb: used as f32 / 1_073_741_824.0,
        vram_total_gb: total as f32 / 1_073_741_824.0,
    })
}

#[cfg(not(target_os = "windows"))]
fn read_sysfs_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ── Windows ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn read_gpu_sample() -> Option<GpuSample> {
    // Spawn PowerShell (always present on Windows 10+) to read GPU perf counters.
    // Two counter paths in one Get-Counter call → one 1-second sample window.
    // Acceptable overhead at the default 30-second heartbeat cadence.
    let script = "\
        $ErrorActionPreference='SilentlyContinue';\
        $cs=(Get-Counter @('\\GPU Engine(*)\\Utilization Percentage',\
             '\\GPU Adapter Memory(*)\\Dedicated Usage')).CounterSamples;\
        $u=($cs|Where-Object Path -like '*Engine*'|Measure-Object CookedValue -Maximum).Maximum;\
        $d=($cs|Where-Object Path -like '*Dedicated*'|Measure-Object CookedValue -Sum).Sum;\
        $t=(Get-CimInstance Win32_VideoController|Select-Object -First 1).AdapterRAM;\
        \"$u $d $t\"";

    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    parse_ps_output(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `"<util_pct> <dedicated_bytes> <total_bytes>"` from the PowerShell script.
/// Extracted for unit-testability without spawning a process.
#[cfg(target_os = "windows")]
fn parse_ps_output(text: &str) -> Option<GpuSample> {
    let mut parts = text
        .split_whitespace()
        .filter_map(|s| s.parse::<f64>().ok());
    let usage_pct = parts.next()? as f32;
    let vram_used_gb = parts.next()? as f32 / 1_073_741_824.0;
    let vram_total_gb = parts.next()? as f32 / 1_073_741_824.0;
    if vram_total_gb == 0.0 {
        return None;
    }
    Some(GpuSample {
        usage_pct: usage_pct.clamp(0.0, 100.0),
        vram_used_gb,
        vram_total_gb,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_gpu_sample_does_not_panic() {
        let _ = read_gpu_sample();
    }

    // Linux-specific tests
    #[cfg(not(target_os = "windows"))]
    mod linux {
        use super::*;

        #[test]
        fn read_sysfs_u64_missing_path_returns_none() {
            assert!(read_sysfs_u64("/nonexistent/path/that/does/not/exist").is_none());
        }

        #[test]
        fn read_sysfs_u64_valid_content_returns_value() {
            use std::io::Write;
            let mut f = tempfile::NamedTempFile::new().unwrap();
            writeln!(f, "42").unwrap();
            assert_eq!(read_sysfs_u64(f.path().to_str().unwrap()), Some(42));
        }

        #[test]
        fn read_sysfs_u64_whitespace_trimmed() {
            use std::io::Write;
            let mut f = tempfile::NamedTempFile::new().unwrap();
            writeln!(f, "  100").unwrap();
            assert_eq!(read_sysfs_u64(f.path().to_str().unwrap()), Some(100));
        }

        #[test]
        fn read_sysfs_u64_non_numeric_returns_none() {
            use std::io::Write;
            let mut f = tempfile::NamedTempFile::new().unwrap();
            write!(f, "not-a-number").unwrap();
            assert!(read_sysfs_u64(f.path().to_str().unwrap()).is_none());
        }
    }

    // Windows-specific tests — parse logic only, no PowerShell spawn.
    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;

        #[test]
        fn parse_ps_output_valid() {
            // 5.3% util, 1 GiB used, 2 GiB total
            let s = parse_ps_output("5.3 1073741824 2147483648").unwrap();
            assert!((s.usage_pct - 5.3).abs() < 0.01);
            assert!((s.vram_used_gb - 1.0).abs() < 0.001);
            assert!((s.vram_total_gb - 2.0).abs() < 0.001);
        }

        #[test]
        fn parse_ps_output_clamps_util_at_100() {
            // Multiple engines can sum >100%; clamp to 100.
            let s = parse_ps_output("150.0 1073741824 2147483648").unwrap();
            assert_eq!(s.usage_pct, 100.0);
        }

        #[test]
        fn parse_ps_output_zero_total_returns_none() {
            assert!(parse_ps_output("50.0 0 0").is_none());
        }

        #[test]
        fn parse_ps_output_empty_returns_none() {
            assert!(parse_ps_output("").is_none());
        }

        #[test]
        fn parse_ps_output_partial_returns_none() {
            assert!(parse_ps_output("5.3 1073741824").is_none());
        }

        #[test]
        fn parse_ps_output_non_numeric_returns_none() {
            assert!(parse_ps_output("N/A N/A N/A").is_none());
        }
    }
}
