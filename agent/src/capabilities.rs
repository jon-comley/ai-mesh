use crate::hardware::{HardwareError, detect_hardware};
use shared::NodeCapabilities;

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("Hardware detection failed: {0}")]
    Hardware(#[from] HardwareError),
}

pub fn detect_capabilities() -> Result<NodeCapabilities, CapabilityError> {
    let hw = detect_hardware()?;

    // CPU inference is always available
    let cpu_inference = true;

    // GPU inference available if GPU is detected
    let gpu_inference = hw.gpu.is_some();

    // ANE inference (Apple Neural Engine) — not available on Linux
    let ane_inference = false;

    // Max model size = 50% of RAM (simple heuristic)
    let max_model_size_gb = hw.ram_gb * 0.5;

    Ok(NodeCapabilities {
        cpu_inference,
        gpu_inference,
        ane_inference,
        max_model_size_gb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_capabilities() {
        let caps = detect_capabilities().unwrap();
        assert!(caps.max_model_size_gb > 0.0);
        assert!(caps.cpu_inference);
    }
}
