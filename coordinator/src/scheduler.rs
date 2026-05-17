use crate::registry::Registry;
use shared::messages::NodeRecordFull;

pub struct Scheduler<'a> {
    registry: &'a Registry,
}

impl<'a> Scheduler<'a> {
    pub fn new(registry: &'a Registry) -> Self {
        Self { registry }
    }

    /// Select the best Compute node that can fit the given model size (in MB).
    pub fn select_node_for_model(&self, model_size_mb: u64) -> Option<NodeRecordFull> {
        let mut candidates: Vec<_> = self
            .registry
            .eligible_compute_nodes()
            .into_iter()
            .filter(|n| {
                n.capabilities
                    .as_ref()
                    .map(|c| (c.max_model_size_gb * 1024.0) as u64 >= model_size_mb)
                    .unwrap_or(false)
            })
            .collect();

        candidates.sort_by(|a, b| a.hostname.cmp(&b.hostname));
        candidates.into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use shared::hardware::{NodeCapabilities, NodeIdentity, NodeRole};

    fn make_identity(id: &str, role: NodeRole) -> NodeIdentity {
        NodeIdentity {
            id: id.to_string(),
            hostname: format!("host-{}", id),
            ip: "127.0.0.1".to_string(),
            role,
        }
    }

    #[test]
    fn scheduler_excludes_controller_nodes() {
        let mut registry = Registry::new();

        registry.update_heartbeat(make_identity("controller", NodeRole::Controller));
        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());

        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 1.0,
                ..NodeCapabilities::default()
            },
        );

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_model(512);

        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, compute.id);
    }

    #[test]
    fn scheduler_returns_none_when_no_compute_nodes() {
        let mut registry = Registry::new();

        registry.update_heartbeat(make_identity("controller", NodeRole::Controller));

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_model(128);

        assert!(selected.is_none());
    }

    #[test]
    fn scheduler_returns_none_when_capacity_insufficient() {
        let mut registry = Registry::new();

        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());

        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 0.5,
                ..NodeCapabilities::default()
            },
        );

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_model(1024);

        assert!(selected.is_none());
    }
}
