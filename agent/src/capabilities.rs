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

    // Max model size = 50% of RAM (simple heuristic).
    let max_model_size_gb = hw.ram_gb * 0.5;

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

#[cfg(test)]
mod tests {
    use super::*;

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
