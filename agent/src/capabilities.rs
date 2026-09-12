use crate::hardware::{HardwareError, detect_hardware};
use shared::NodeCapabilities;

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("Hardware detection failed: {0}")]
    Hardware(#[from] HardwareError),
}

pub fn detect_capabilities() -> Result<NodeCapabilities, CapabilityError> {
    let hw = detect_hardware()?;

    // CPU inference is always available.
    let cpu_inference = true;

    // GPU inference available if any GPU was detected.
    let gpu_inference = hw.gpu.is_some();

    // ANE (Apple Neural Engine) is present on all Apple Silicon (M-series) chips.
    // Compile-time constant — the binary is always built for a specific target.
    let ane_inference = cfg!(all(target_os = "macos", target_arch = "aarch64"));

    // See `model_ceiling_gb` below for what this number means and why it is
    // not simply half the RAM.
    let max_model_size_gb = model_ceiling_gb(
        hw.ram_gb,
        hw.gpu_vram_gb,
        std::env::var("MAX_MODEL_SIZE_GB").ok().as_deref(),
    );

    let features: Vec<shared::Feature> = vec![
        #[cfg(feature = "llm")]
        shared::Feature::Llm,
        #[cfg(feature = "lighting")]
        shared::Feature::Lighting,
        #[cfg(feature = "reaper")]
        shared::Feature::Reaper,
        #[cfg(feature = "art")]
        shared::Feature::Art,
        #[cfg(feature = "voice")]
        shared::Feature::Voice,
        #[cfg(feature = "audio")]
        shared::Feature::Audio,
        #[cfg(feature = "music")]
        shared::Feature::Music,
    ];

    // Report which audio backends this node runs so the coordinator can
    // list each as a distinct room-assignable sink. Same parser the
    // capability itself uses (AUDIO_BACKENDS env) — no config drift.
    #[cfg(feature = "audio")]
    let audio_backends = capability_audio::configured_backends();
    #[cfg(not(feature = "audio"))]
    let audio_backends = vec![];

    Ok(NodeCapabilities {
        cpu_inference,
        gpu_inference,
        ane_inference,
        max_model_size_gb,
        features,
        audio_backends,
    })
}

/// How large a model this node should admit, in GB.
///
/// **Why this is not simply half the RAM.** The old line was `ram_gb * 0.5`. On
/// `beelink1` — 32 GB with 16 GB given to a UMA Radeon 780M — Windows reports
/// 15.8 GB, so the node advertised a 7.9 GB ceiling and the coordinator refused
/// every 14B: *"model needs 8573 MB but node has only 7900 MB of model headroom
/// left"*, which reached the dashboard as "not enough memory". Backwards,
/// because on that node the model does not live in system RAM at all: Vulkan
/// reports 24.4 GB with 23.2 free, and `DeepSeek-R1-Distill-Qwen-14B-Q4_K_M`
/// loads there in six seconds and runs at 9.0 tok/s. **The carve-out that makes
/// the model loadable was halving the number the mesh judged it by.**
///
/// So take whichever is larger:
///
/// * **half of system RAM** — the CPU-inference case, unchanged, and still
///   right for a node with no usable GPU.
/// * **90% of VRAM** — the GPU case. Not all of it, because the KV cache and the
///   runtime's own buffers share that memory with the weights, and a ceiling
///   that admits a model with nothing left for context admits a model that
///   cannot answer.
///
/// `max` rather than a GPU-only branch, because a discrete card is usually
/// *smaller* than half its host's RAM (a 24 GB card in a 64 GB box) and a node
/// that can hold a big model in RAM should not be told it cannot merely because
/// its GPU is modest.
///
/// `MAX_MODEL_SIZE_GB` overrides both, and stays because a ceiling is sometimes
/// a judgement — "nothing bigger than this on that machine" — rather than a
/// property of the hardware. Rubbish in the variable is ignored rather than
/// obeyed: parsing `""` as zero would advertise a node that can hold nothing.
fn model_ceiling_gb(ram_gb: f32, gpu_vram_gb: Option<f32>, override_env: Option<&str>) -> f32 {
    if let Some(v) = override_env
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v > 0.0)
    {
        return v;
    }
    let ram_ceiling = ram_gb * 0.5;
    match gpu_vram_gb.filter(|v| *v > 0.0).map(|v| v * 0.9) {
        Some(vram_ceiling) if vram_ceiling > ram_ceiling => vram_ceiling,
        _ => ram_ceiling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_prefers_vram_when_the_gpu_is_the_bigger_half() {
        // beelink1 as it actually reports: 15.8 GB visible to Windows because
        // 16 GB went to the iGPU. Half the RAM is 7.9 and refuses every 14B;
        // 90% of the carve-out is 14.4 and admits them.
        let c = model_ceiling_gb(15.8, Some(16.0), None);
        assert!((c - 14.4).abs() < 0.01, "got {c}");
    }

    #[test]
    fn ceiling_keeps_the_ram_half_when_the_card_is_the_smaller_one() {
        // A 24 GB card in a 64 GB box: 21.6 against 32, so RAM wins and a node
        // that can hold a big model is not told otherwise by a modest GPU.
        assert_eq!(model_ceiling_gb(64.0, Some(24.0), None), 32.0);
    }

    #[test]
    fn ceiling_falls_back_to_ram_with_no_gpu_or_a_nonsense_one() {
        assert_eq!(model_ceiling_gb(16.0, None, None), 8.0);
        assert_eq!(model_ceiling_gb(16.0, Some(0.0), None), 8.0);
    }

    #[test]
    fn ceiling_lets_the_override_win_but_ignores_rubbish() {
        assert_eq!(model_ceiling_gb(15.8, Some(16.0), Some("20")), 20.0);
        assert_eq!(model_ceiling_gb(15.8, Some(16.0), Some(" 20 ")), 20.0);
        // Neither of these may become a zero ceiling.
        assert!((model_ceiling_gb(15.8, Some(16.0), Some("not-a-number")) - 14.4).abs() < 0.01);
        assert!((model_ceiling_gb(15.8, Some(16.0), Some("")) - 14.4).abs() < 0.01);
        assert!((model_ceiling_gb(15.8, Some(16.0), Some("-5")) - 14.4).abs() < 0.01);
    }

    #[test]
    fn test_detect_capabilities() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.cpu_inference);
        assert!(caps.max_model_size_gb > 0.0);
    }

    #[cfg(feature = "llm")]
    #[test]
    fn features_includes_llm_when_built_with_llm_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.contains(&shared::Feature::Llm));
    }

    #[cfg(feature = "art")]
    #[test]
    fn features_includes_art_when_built_with_art_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.contains(&shared::Feature::Art));
    }

    #[cfg(feature = "voice")]
    #[test]
    fn features_includes_voice_when_built_with_voice_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.contains(&shared::Feature::Voice));
    }

    #[cfg(feature = "music")]
    #[test]
    fn features_includes_music_when_built_with_music_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.contains(&shared::Feature::Music));
    }

    #[cfg(feature = "audio")]
    #[test]
    fn features_includes_audio_when_built_with_audio_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.contains(&shared::Feature::Audio));
        // The capability defaults to bluetooth with no env set, so the
        // reported backend list is never empty on an audio node.
        assert!(!caps.audio_backends.is_empty());
    }

    #[cfg(not(feature = "audio"))]
    #[test]
    fn audio_backends_empty_without_audio_feature() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.audio_backends.is_empty());
    }

    #[cfg(not(feature = "llm"))]
    #[test]
    fn features_empty_without_feature_flags() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.features.is_empty());
    }

    // ANE is only available on Apple Silicon macOS.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn ane_true_on_apple_silicon() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.ane_inference);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn ane_false_on_non_apple_silicon() {
        let caps = detect_capabilities().unwrap();
        assert!(!caps.ane_inference);
    }
}
