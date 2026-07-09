use shared::{MeshMessage, NodeRecordFull};

pub async fn run(coordinator: &str, id: String) {
    match fetch_info(coordinator, id).await {
        Ok(info) => print!("{}", format_info(&info)),
        Err(e) => println!("Error: {}", e),
    }
}

async fn fetch_info(
    coordinator: &str,
    id: String,
) -> Result<NodeRecordFull, Box<dyn std::error::Error>> {
    let mut stream = crate::connection::connect(coordinator).await?;
    match crate::connection::send_recv(&mut stream, &MeshMessage::RequestNodeInfo(id)).await? {
        MeshMessage::NodeInfo(info) => Ok(info),
        MeshMessage::Error(e) => Err(e.into()),
        other => Err(format!("Unexpected response: {:?}", other).into()),
    }
}

pub(crate) fn format_info(n: &NodeRecordFull) -> String {
    let mut out = String::new();

    out.push_str(&format!("  ID:             {}\n", n.id));
    out.push_str(&format!("  Hostname:       {}\n", n.hostname));
    out.push_str(&format!("  IP:             {}\n", n.ip));
    out.push_str(&format!("  Role:           {:?}\n", n.role));
    out.push_str(&format!(
        "  Last heartbeat: {} ms ago\n",
        n.last_heartbeat_ms
    ));

    out.push_str("\n  Hardware:\n");
    match &n.hardware {
        Some(hw) => {
            out.push_str(&format!(
                "    CPU:   {} ({} cores / {} threads)\n",
                hw.cpu_model, hw.cpu_cores, hw.cpu_threads
            ));
            out.push_str(&format!("    RAM:   {:.1} GB\n", hw.ram_gb));
            out.push_str(&format!("    OS:    {} ({})\n", hw.os, hw.arch));
            out.push_str(&format!(
                "    GPU:   {}\n",
                hw.gpu.as_deref().unwrap_or("none")
            ));
        }
        None => out.push_str("    (no hardware report)\n"),
    }

    out.push_str("\n  Capabilities:\n");
    match &n.capabilities {
        Some(c) => {
            out.push_str(&format!("    CPU inference:  {}\n", c.cpu_inference));
            out.push_str(&format!("    GPU inference:  {}\n", c.gpu_inference));
            out.push_str(&format!("    ANE inference:  {}\n", c.ane_inference));
            out.push_str(&format!(
                "    Max model:      {:.1} GB\n",
                c.max_model_size_gb
            ));
        }
        None => out.push_str("    (no capabilities report)\n"),
    }

    if !n.models.is_empty() {
        out.push_str("\n  Models:\n");
        for m in &n.models {
            out.push_str(&format!(
                "    {} ({} MB) — {:?}\n",
                m.model_name, m.size_mb, m.state
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        HardwareSpec, ModelAllocationFull, ModelLifecycleState, NodeCapabilities, NodeRole,
    };

    fn base_node() -> NodeRecordFull {
        NodeRecordFull {
            id: "abc-123".into(),
            hostname: "pi1".into(),
            ip: "192.168.1.11".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 500,
            hardware: None,
            capabilities: None,
            models: vec![],
        }
    }

    fn full_hardware() -> HardwareSpec {
        HardwareSpec {
            cpu_model: "Raspberry Pi 5".into(),
            cpu_cores: 4,
            cpu_threads: 4,
            ram_gb: 7.9,
            os: "linux".into(),
            arch: "aarch64".into(),
            gpu: None,
        }
    }

    fn full_capabilities() -> NodeCapabilities {
        NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 3.9,
            features: vec![shared::Feature::Llm],
            audio_backends: vec![],
        }
    }

    #[test]
    fn contains_basic_identity_fields() {
        let out = format_info(&base_node());
        assert!(out.contains("abc-123"));
        assert!(out.contains("pi1"));
        assert!(out.contains("192.168.1.11"));
        assert!(out.contains("Compute"));
        assert!(out.contains("500 ms ago"));
    }

    #[test]
    fn no_hardware_shows_placeholder() {
        let out = format_info(&base_node());
        assert!(out.contains("(no hardware report)"));
    }

    #[test]
    fn no_capabilities_shows_placeholder() {
        let out = format_info(&base_node());
        assert!(out.contains("(no capabilities report)"));
    }

    #[test]
    fn hardware_fields_formatted_correctly() {
        let mut n = base_node();
        n.hardware = Some(full_hardware());
        let out = format_info(&n);
        assert!(out.contains("Raspberry Pi 5"));
        assert!(out.contains("4 cores / 4 threads"));
        assert!(out.contains("7.9 GB"));
        assert!(out.contains("linux (aarch64)"));
        assert!(out.contains("GPU:   none"));
    }

    #[test]
    fn gpu_present_shown_in_output() {
        let mut n = base_node();
        let mut hw = full_hardware();
        hw.gpu = Some("NVIDIA RTX 4090".into());
        n.hardware = Some(hw);
        let out = format_info(&n);
        assert!(out.contains("NVIDIA RTX 4090"));
    }

    #[test]
    fn capabilities_fields_formatted_correctly() {
        let mut n = base_node();
        n.capabilities = Some(full_capabilities());
        let out = format_info(&n);
        assert!(out.contains("CPU inference:  true"));
        assert!(out.contains("GPU inference:  false"));
        assert!(out.contains("ANE inference:  false"));
        assert!(out.contains("Max model:      3.9 GB"));
    }

    #[test]
    fn empty_models_omits_models_section() {
        let out = format_info(&base_node());
        assert!(!out.contains("Models:"));
    }

    #[test]
    fn models_listed_when_present() {
        let mut n = base_node();
        n.models = vec![
            ModelAllocationFull {
                model_name: "qwen2.5:0.5b".into(),
                size_mb: 400,
                state: ModelLifecycleState::Ready,
            },
            ModelAllocationFull {
                model_name: "llama3".into(),
                size_mb: 4096,
                state: ModelLifecycleState::Loading,
            },
        ];
        let out = format_info(&n);
        assert!(out.contains("Models:"));
        assert!(out.contains("qwen2.5:0.5b"));
        assert!(out.contains("400 MB"));
        assert!(out.contains("llama3"));
        assert!(out.contains("4096 MB"));
        assert!(out.contains("Loading"));
    }
}
