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

    // Max model size = 50% of RAM (simple heuristic), overridable per node.
    //
    // **The heuristic is actively wrong on a box with a UMA iGPU — 2026-09-12.**
    // `beelink1` carves 16 GB of its 32 GB out for the Radeon 780M, so Windows
    // reports 15.8 GB and this line advertises a **7.9 GB** ceiling. The
    // coordinator then refuses every 14B — `model needs 8573 MB but node has
    // only 7900 MB of model headroom left` — which reaches the dashboard as
    // "not enough memory".
    //
    // The refusal is backwards: the model does not live in system RAM on that
    // node at all. Vulkan reports **24.4 GB, 23.2 GB free** on the same box, and
    // `DeepSeek-R1-Distill-Qwen-14B-Q4_K_M` loads there in six seconds and runs
    // at 9.0 tok/s. So the very carve-out that makes the model loadable is what
    // halves the number the mesh judges it by.
    //
    // Properly fixing the heuristic needs VRAM in `HardwareInfo`, which only
    // carries `gpu: Option<String>` today — that is the real fix and it is
    // bigger than this. `MAX_MODEL_SIZE_GB` is the escape hatch in the
    // meantime, set per node beside `LLAMA_SERVER_BIN` and the rest, and it
    // stays useful afterwards for any node whose ceiling is a judgement rather
    // than a formula.
    let max_model_size_gb = std::env::var("MAX_MODEL_SIZE_GB")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(hw.ram_gb * 0.5);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not two: `cargo test` runs a module's tests on parallel
    /// threads and the environment is per-process, so a second test touching
    /// the same variable races this one and both fail intermittently. Asking
    /// for `--test-threads=1` to keep them separate would be a worse trade.
    #[test]
    fn max_model_size_takes_the_env_override_when_it_is_a_number() {
        unsafe { std::env::set_var("MAX_MODEL_SIZE_GB", "14") };
        let overridden = detect_capabilities().unwrap();
        assert_eq!(overridden.max_model_size_gb, 14.0);

        // Rubbish falls back to the RAM heuristic rather than to zero, which
        // would advertise a node that can hold nothing.
        unsafe { std::env::set_var("MAX_MODEL_SIZE_GB", "not-a-number") };
        let fallback = detect_capabilities().unwrap();
        assert!(fallback.max_model_size_gb > 0.0);

        unsafe { std::env::remove_var("MAX_MODEL_SIZE_GB") };
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
