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
    ];

    Ok(NodeCapabilities {
        cpu_inference,
        gpu_inference,
        ane_inference,
        max_model_size_gb,
        features,
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
