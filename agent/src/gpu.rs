/// GPU metrics — Linux sysfs (amdgpu driver), Windows Performance Counters and
/// macOS's IORegistry (Apple Silicon).
/// Returns None on CPU-only nodes or any node where GPU metrics are unavailable.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSample {
    pub usage_pct: f32,
    pub vram_used_gb: f32,
    pub vram_total_gb: f32,
}

// ── Linux ─────────────────────────────────────────────────────────────────────

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub fn read_gpu_sample() -> Option<GpuSample> {
    let usage = read_sysfs_u64("/sys/class/drm/card0/device/gpu_busy_percent")?;
    let used = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vis_vram_used")
        .or_else(|| read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_used"))?;
    // vis_vram_total = dedicated/visible VRAM only; on AMD APUs (780M etc.) this
    // is the BIOS-configured allocation rather than the full GTT pool.
    let total = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vis_vram_total")
        .or_else(|| read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_total"))?;
    if total == 0 {
        return None;
    }
    Some(GpuSample {
        usage_pct: usage as f32,
        vram_used_gb: used as f32 / 1_073_741_824.0,
        vram_total_gb: total as f32 / 1_073_741_824.0,
    })
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn read_sysfs_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ── macOS (Apple Silicon) ─────────────────────────────────────────────────────

/// There is no VRAM: the GPU draws on unified memory, so "total" is how much of
/// it macOS lets the GPU use — `iogpu.wired_limit_mb` when someone has raised
/// it, else macOS's default (the same rule the model ceiling uses). Busy % and
/// memory in use come from the GPU driver's statistics, readable without sudo.
#[cfg(target_os = "macos")]
pub fn read_gpu_sample() -> Option<GpuSample> {
    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new(cmd).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let ioreg = run(
        "ioreg",
        &["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator"],
    )?;
    let (usage_pct, in_use_bytes) = parse_ioreg_perf(&ioreg)?;

    let wired_mb: u64 = run("sysctl", &["-n", "iogpu.wired_limit_mb"])
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let total_gb = if wired_mb > 0 {
        wired_mb as f32 / 1024.0
    } else {
        let ram_bytes: u64 = run("sysctl", &["-n", "hw.memsize"])?.trim().parse().ok()?;
        crate::capabilities::unified_gpu_budget_gb(ram_bytes as f32 / 1_073_741_824.0, true)?
    };
    if total_gb <= 0.0 {
        return None;
    }
    let used_gb = in_use_bytes as f32 / 1_073_741_824.0;
    Some(GpuSample {
        usage_pct: usage_pct.clamp(0.0, 100.0),
        vram_used_gb: used_gb.clamp(0.0, total_gb),
        vram_total_gb: total_gb,
    })
}

/// GPU busy % and bytes in use from `ioreg -c IOAccelerator` output, whose
/// driver entry carries a line like
/// `"PerformanceStatistics" = {"Device Utilization %"=12,…,"In use system memory"=4096,…}`.
/// The key match includes the closing quote so `"In use system memory (driver)"`
/// is never taken for the real figure.
#[cfg(any(target_os = "macos", test))]
fn parse_ioreg_perf(text: &str) -> Option<(f32, u64)> {
    let stats = text
        .lines()
        .find(|l| l.contains("\"PerformanceStatistics\""))?;
    let number_after = |key: &str| -> Option<u64> {
        let at = stats.find(key)? + key.len();
        let digits: String = stats[at..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    };
    let usage = number_after("\"Device Utilization %\"=")? as f32;
    let in_use = number_after("\"In use system memory\"=")?;
    Some((usage, in_use))
}

// ── Windows ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn read_gpu_sample() -> Option<GpuSample> {
    // Spawn PowerShell (always present on Windows 10+) to read GPU perf counters.
    // Three counter paths in one Get-Counter call → one 1-second sample window.
    // Acceptable overhead at the default 30-second heartbeat cadence.
    //
    // GPU memory selection: take max(Dedicated, Shared).
    //   Discrete GPU: Dedicated holds the model weights (GBs); Shared is small.
    //   UMA (AMD 780M etc.): Dedicated is the fixed framebuffer (~100 MB);
    //     Shared holds Vulkan allocations from the unified memory pool.
    //   max() picks the right pool for both architectures with no threshold to tune.
    let script = "\
        $ErrorActionPreference='SilentlyContinue';\
        $cs=(Get-Counter @('\\GPU Engine(*)\\Utilization Percentage',\
             '\\GPU Adapter Memory(*)\\Dedicated Usage',\
             '\\GPU Adapter Memory(*)\\Shared Usage')).CounterSamples;\
        $u=($cs|Where-Object Path -like '*Engine*'|Measure-Object CookedValue -Maximum).Maximum;\
        $ded=($cs|Where-Object Path -like '*Dedicated*'|Measure-Object CookedValue -Sum).Sum;\
        $shr=($cs|Where-Object Path -like '*Shared*'|Measure-Object CookedValue -Sum).Sum;\
        $d=if($ded -gt $shr){$ded}else{$shr};\
        $t=(Get-ChildItem 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}' \
            -ErrorAction SilentlyContinue|ForEach-Object{$_.GetValue('HardwareInformation.qwMemorySize')}\
            |Where-Object{$_ -gt 0}|Select-Object -First 1);\
        if(-not $t){$t=(Get-CimInstance Win32_VideoController|Select-Object -First 1).AdapterRAM};\
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

/// Parse `"<util_pct> <used_bytes> <total_bytes>"` from the PowerShell script.
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
    let vram_total_gb = vram_total_gb.clamp(0.0, 1024.0);
    Some(GpuSample {
        usage_pct: usage_pct.clamp(0.0, 100.0),
        vram_used_gb: vram_used_gb.clamp(0.0, vram_total_gb),
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

    // The macOS parser runs everywhere: it is plain text handling.
    mod macos {
        use super::*;

        // As `ioreg -r -d 1 -w 0 -c IOAccelerator` prints it on Apple Silicon,
        // trimmed to the parts that matter.
        const SAMPLE: &str = r#"+-o AGXAcceleratorG16X  <class AGXAcceleratorG16X, id 0x100000a2c, registered, matched, active, busy 0 (0 ms), retain 92>
    {
      "IOClass" = "AGXAcceleratorG16X"
      "PerformanceStatistics" = {"In use system memory (driver)"=0,"Alloc system memory"=43980465111,"Tiler Utilization %"=3,"recoveryCount"=0,"Renderer Utilization %"=9,"Device Utilization %"=11,"SplitSceneCount"=0,"In use system memory"=41875931136}
      "IOGLBundleName" = "AppleMetalOpenGLRenderer"
    }"#;

        #[test]
        fn reads_busy_percent_and_memory_in_use() {
            let (usage, in_use) = parse_ioreg_perf(SAMPLE).unwrap();
            assert_eq!(usage, 11.0);
            assert_eq!(in_use, 41_875_931_136);
        }

        #[test]
        fn the_driver_figure_is_not_taken_for_memory_in_use() {
            // "(driver)" comes first and is 0; the real key comes last.
            let (_, in_use) = parse_ioreg_perf(SAMPLE).unwrap();
            assert_ne!(in_use, 0);
        }

        #[test]
        fn missing_statistics_or_keys_give_nothing() {
            assert!(parse_ioreg_perf("").is_none());
            assert!(parse_ioreg_perf(r#""IOClass" = "AGXAccelerator""#).is_none());
            assert!(
                parse_ioreg_perf(r#""PerformanceStatistics" = {"Device Utilization %"=5}"#)
                    .is_none()
            );
        }
    }

    // Linux-specific tests
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
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
            // 5.3% util, 1 GiB used (max of dedicated/shared), 2 GiB total
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
        fn parse_ps_output_clamps_negative_used_to_zero() {
            // Buggy driver may return negative CookedValue bytes; clamp to 0.
            // Total = 2 GiB (2147483648), used reported as -1 (underflow sentinel).
            let s = parse_ps_output("10.0 -1073741824 2147483648").unwrap();
            assert_eq!(s.vram_used_gb, 0.0);
            assert!((s.vram_total_gb - 2.0).abs() < 0.001);
        }

        #[test]
        fn parse_ps_output_clamps_used_to_total() {
            // used > total should not happen but can with buggy drivers.
            let total_bytes = 2_147_483_648u64;
            let used_bytes = 3_221_225_472u64; // 3 GiB > 2 GiB total
            let s = parse_ps_output(&format!("10.0 {used_bytes} {total_bytes}")).unwrap();
            assert!((s.vram_used_gb - s.vram_total_gb).abs() < 0.001);
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
