use shared::{
    HardwareSpec, ModelLifecycleState, NodeCapabilities, NodeIdentity, NodeRecordFull,
    NodeRecordLite, NodeRole,
};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone)]
pub struct ModelAllocation {
    pub model_name: String,
    pub size_mb: u64,
    pub state: ModelLifecycleState,
    pub last_updated: Instant,
}

#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub identity: NodeIdentity,
    pub hardware: Option<HardwareSpec>,
    pub capabilities: Option<NodeCapabilities>,
    pub last_heartbeat: SystemTime,
    pub models: HashMap<String, ModelAllocation>,
}

#[derive(Debug, Default)]
pub struct Registry {
    nodes: HashMap<String, NodeRecord>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    pub fn update_heartbeat(&mut self, identity: NodeIdentity) {
        let entry = self.nodes.entry(identity.id.clone()).or_insert(NodeRecord {
            identity: identity.clone(),
            hardware: None,
            capabilities: None,
            last_heartbeat: SystemTime::now(),
            models: HashMap::new(),
        });

        entry.identity = identity;
        entry.last_heartbeat = SystemTime::now();
    }

    pub fn update_hardware(&mut self, id: &str, hardware: HardwareSpec) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.hardware = Some(hardware);
        }
    }

    pub fn update_capabilities(&mut self, id: &str, capabilities: NodeCapabilities) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.capabilities = Some(capabilities);
        }
    }

    pub fn get(&self, id: &str) -> Option<&NodeRecord> {
        self.nodes.get(id)
    }

    pub fn prune_stale(&mut self, max_age: Duration) {
        let now = SystemTime::now();
        self.nodes.retain(|_, record| {
            now.duration_since(record.last_heartbeat)
                .map(|age| age < max_age)
                .unwrap_or(false)
        });
    }

    pub fn count(&self) -> usize {
        self.nodes.len()
    }

    pub fn first_node_id(&self) -> Option<String> {
        self.nodes.keys().next().cloned()
    }

    pub fn eligible_compute_nodes(&self) -> Vec<NodeRecordFull> {
        self.nodes
            .values()
            .filter(|n| n.identity.role == NodeRole::Compute)
            .filter_map(|rec| self.get_node_full(&rec.identity.id))
            .collect()
    }

    pub fn list_nodes(&self) -> Vec<NodeRecordLite> {
        let now = SystemTime::now();

        self.nodes
            .values()
            .map(|rec| {
                let age = now
                    .duration_since(rec.last_heartbeat)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);

                NodeRecordLite {
                    id: rec.identity.id.clone(),
                    hostname: rec.identity.hostname.clone(),
                    ip: rec.identity.ip.clone(),
                    role: rec.identity.role.clone(),
                    last_heartbeat_ms: age,
                }
            })
            .collect()
    }

    pub fn clear_all(&mut self) {
        self.nodes.clear();
    }

    pub fn get_node_full(&self, id: &str) -> Option<NodeRecordFull> {
        let now = SystemTime::now();
        let rec = self.nodes.get(id)?;

        let age = now
            .duration_since(rec.last_heartbeat)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        Some(NodeRecordFull {
            id: rec.identity.id.clone(),
            hostname: rec.identity.hostname.clone(),
            ip: rec.identity.ip.clone(),
            role: rec.identity.role.clone(),
            last_heartbeat_ms: age,
            hardware: rec.hardware.clone(),
            capabilities: rec.capabilities.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::NodeRole;

    fn sample_identity(id: &str) -> NodeIdentity {
        NodeIdentity {
            id: id.to_string(),
            hostname: "test-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        }
    }

    fn sample_hardware() -> HardwareSpec {
        HardwareSpec {
            cpu_model: "Test CPU".into(),
            cpu_cores: 4,
            cpu_threads: 8,
            ram_gb: 16.0,
            os: "linux".into(),
            arch: "x86_64".into(),
            gpu: None,
        }
    }

    fn sample_capabilities() -> NodeCapabilities {
        NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 8.0,
        }
    }

    #[test]
    fn test_heartbeat_inserts_node() {
        let mut reg = Registry::new();
        let ident = sample_identity("node1");

        reg.update_heartbeat(ident.clone());

        let rec = reg.get("node1").unwrap();
        assert_eq!(rec.identity.id, "node1");
    }

    #[test]
    fn test_update_hardware() {
        let mut reg = Registry::new();
        let ident = sample_identity("node1");

        reg.update_heartbeat(ident.clone());
        reg.update_hardware("node1", sample_hardware());

        let rec = reg.get("node1").unwrap();
        assert!(rec.hardware.is_some());
    }

    #[test]
    fn test_update_capabilities() {
        let mut reg = Registry::new();
        let ident = sample_identity("node1");

        reg.update_heartbeat(ident.clone());
        reg.update_capabilities("node1", sample_capabilities());

        let rec = reg.get("node1").unwrap();
        assert!(rec.capabilities.is_some());
    }

    #[test]
    fn test_prune_stale() {
        let mut reg = Registry::new();
        let ident = sample_identity("node1");

        reg.update_heartbeat(ident.clone());

        // artificially age the record
        if let Some(node) = reg.nodes.get_mut("node1") {
            node.last_heartbeat = SystemTime::now() - Duration::from_secs(9999);
        }

        reg.prune_stale(Duration::from_secs(10));
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn test_list_nodes() {
        let mut reg = Registry::new();

        reg.update_heartbeat(sample_identity("node1"));
        reg.update_heartbeat(sample_identity("node2"));

        let nodes = reg.list_nodes();
        assert_eq!(nodes.len(), 2);

        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"node1"));
        assert!(ids.contains(&"node2"));
    }

    #[test]
    fn test_get_node_full() {
        let mut reg = Registry::new();
        let ident = sample_identity("node1");

        reg.update_heartbeat(ident.clone());
        reg.update_hardware("node1", sample_hardware());
        reg.update_capabilities("node1", sample_capabilities());

        let full = reg.get_node_full("node1").unwrap();

        assert_eq!(full.id, "node1");
        assert_eq!(full.hostname, "test-host");
        assert!(full.hardware.is_some());
        assert!(full.capabilities.is_some());
    }

    #[test]
    fn test_get_node_full_missing() {
        let reg = Registry::new();
        assert!(reg.get_node_full("nonexistent").is_none());
    }

    fn make_identity(id: &str) -> NodeIdentity {
        NodeIdentity {
            id: id.into(),
            hostname: "OmniBook7".into(),
            ip: "172.20.107.210".into(),
            role: NodeRole::Compute,
        }
    }

    #[test]
    fn eligible_compute_nodes_filters_by_role() {
        let mut registry = Registry::new();

        let controller = NodeIdentity {
            id: "controller".into(),
            hostname: "controller-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Controller,
        };
        let compute = NodeIdentity {
            id: "compute".into(),
            hostname: "compute-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        };

        registry.update_heartbeat(controller.clone());
        registry.update_heartbeat(compute.clone());

        let eligible = registry.eligible_compute_nodes();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, compute.id);
    }

    fn make_hardware() -> HardwareSpec {
        HardwareSpec {
            cpu_model: "AMD Ryzen AI 5 340 w/ Radeon 840M".into(),
            cpu_cores: 12,
            cpu_threads: 12,
            ram_gb: 7.39,
            os: "linux".into(),
            arch: "x86_64".into(),
            gpu: None,
        }
    }

    fn make_caps() -> NodeCapabilities {
        NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 3.69,
        }
    }

    #[test]
    fn list_nodes_returns_lite_records() {
        let mut reg = Registry::new();

        reg.update_heartbeat(make_identity("node-1"));

        let nodes = reg.list_nodes();
        assert_eq!(nodes.len(), 1);
        let n = &nodes[0];

        assert_eq!(n.id, "node-1");
        assert_eq!(n.hostname, "OmniBook7");
        assert_eq!(n.ip, "172.20.107.210");
        assert!(n.last_heartbeat_ms < u128::MAX);
    }

    #[test]
    fn get_node_full_includes_hw_and_caps() {
        let mut reg = Registry::new();

        reg.update_heartbeat(make_identity("node-1"));
        let hw = make_hardware();
        reg.update_hardware("node-1", hw.clone());
        let caps = make_caps();
        reg.update_capabilities("node-1", caps.clone());

        let full = reg.get_node_full("node-1").expect("node should exist");

        assert_eq!(full.id, "node-1");
        assert_eq!(full.hostname, "OmniBook7");
        assert_eq!(full.ip, "172.20.107.210");
        assert_eq!(full.role, NodeRole::Compute);

        let fhw = full.hardware.expect("hardware should be present");
        assert_eq!(fhw.cpu_model, hw.cpu_model);

        let fcaps = full.capabilities.expect("caps should be present");
        assert_eq!(fcaps.cpu_inference, caps.cpu_inference);
        assert_eq!(fcaps.gpu_inference, caps.gpu_inference);
    }

    #[test]
    fn get_node_full_missing_returns_none() {
        let reg = Registry::new();
        assert!(reg.get_node_full("does-not-exist").is_none());
    }
}
