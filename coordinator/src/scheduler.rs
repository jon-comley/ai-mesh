use crate::registry::Registry;
use shared::ModelLifecycleState;
use shared::messages::NodeRecordFull;

pub struct Scheduler<'a> {
    registry: &'a Registry,
}

impl<'a> Scheduler<'a> {
    pub fn new(registry: &'a Registry) -> Self {
        Self { registry }
    }

    /// Select a Compute node that has the given model loaded and in Ready state.
    pub fn select_node_for_inference(&self, model_name: &str) -> Option<NodeRecordFull> {
        let mut candidates: Vec<_> = self
            .registry
            .eligible_compute_nodes()
            .into_iter()
            .filter(|node| {
                node.models
                    .iter()
                    .any(|m| m.model_name == model_name && m.state == ModelLifecycleState::Ready)
            })
            .collect();

        candidates.sort_by(|a, b| a.hostname.cmp(&b.hostname));
        candidates.into_iter().next()
    }

    /// Select the best Compute node that can fit the given model size (in MB).
    pub fn select_node_for_model(&self, model_size_mb: u64) -> Option<NodeRecordFull> {
        let mut candidates: Vec<_> = self
            .registry
            .eligible_compute_nodes()
            .into_iter()
            .filter(|lite| {
                self.registry.get(&lite.id).is_some_and(|node| {
                    if let Some(caps) = &node.capabilities {
                        let max_capacity_mb = (caps.max_model_size_gb * 1024.0) as u64;

                        let allocated_mb: u64 = node
                            .models
                            .values()
                            .filter(|m| {
                                m.state == ModelLifecycleState::Ready
                                    || m.state == ModelLifecycleState::Loading
                            })
                            .map(|m| m.size_mb)
                            .sum();

                        allocated_mb + model_size_mb <= max_capacity_mb
                    } else {
                        false
                    }
                })
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
    use shared::ModelLifecycleState;
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
    fn select_node_for_inference_returns_node_with_ready_model() {
        let mut registry = Registry::new();
        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());
        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 4.0,
                ..NodeCapabilities::default()
            },
        );
        registry.update_model_status(&compute.id, "llama3", 4096, ModelLifecycleState::Ready);

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_inference("llama3");

        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, compute.id);
    }

    #[test]
    fn select_node_for_inference_ignores_loading_model() {
        let mut registry = Registry::new();
        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());
        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 4.0,
                ..NodeCapabilities::default()
            },
        );
        registry.update_model_status(&compute.id, "llama3", 4096, ModelLifecycleState::Loading);

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_inference("llama3");

        assert!(
            selected.is_none(),
            "Loading model must not be selected for inference"
        );
    }

    #[test]
    fn select_node_for_inference_returns_none_when_model_absent() {
        let mut registry = Registry::new();
        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());
        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 4.0,
                ..NodeCapabilities::default()
            },
        );

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_inference("llama3");

        assert!(selected.is_none(), "No model loaded — must return None");
    }

    #[test]
    fn select_node_for_inference_excludes_controller_nodes() {
        let mut registry = Registry::new();
        let controller = make_identity("controller", NodeRole::Controller);
        registry.update_heartbeat(controller.clone());
        registry.update_model_status(&controller.id, "llama3", 4096, ModelLifecycleState::Ready);

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_inference("llama3");

        assert!(
            selected.is_none(),
            "Controller nodes must never be selected for inference"
        );
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
    fn scheduler_accounts_for_allocated_model_memory() {
        let mut registry = Registry::new();

        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());

        // 4 GB total capacity
        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 4.0,
                ..NodeCapabilities::default()
            },
        );

        // 3 GB already allocated (Loading state counts)
        registry.update_model_status(&compute.id, "llama3-8b", 3072, ModelLifecycleState::Loading);

        let scheduler = Scheduler::new(&registry);

        // 1.5 GB request — would need 3072 + 1536 = 4608 MB, exceeds 4096 MB capacity
        assert!(scheduler.select_node_for_model(1536).is_none());

        // 512 MB request — 3072 + 512 = 3584 MB, fits within 4096 MB
        assert!(scheduler.select_node_for_model(512).is_some());
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

    #[test]
    fn scheduler_accounts_for_existing_allocations() {
        let mut registry = Registry::new();
        let compute = make_identity("compute-1", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());

        registry.update_capabilities(
            &compute.id,
            NodeCapabilities {
                max_model_size_gb: 2.0, // 2048 MB total capacity
                ..NodeCapabilities::default()
            },
        );

        // Allocate a 1500 MB model already loading or ready
        registry.update_model_status(&compute.id, "qwen-7b", 1500, ModelLifecycleState::Ready);

        {
            let scheduler = Scheduler::new(&registry);
            let selected_large = scheduler.select_node_for_model(1024);
            assert!(
                selected_large.is_none(),
                "Should not fit model exceeding remaining space"
            );

            let selected_small = scheduler.select_node_for_model(512);
            assert!(
                selected_small.is_some(),
                "Should fit within remaining space"
            );
            assert_eq!(selected_small.unwrap().id, compute.id);
        }

        // If the model changes to Unloaded, that memory footprint should free right back up
        registry.update_model_status(&compute.id, "qwen-7b", 1500, ModelLifecycleState::Unloaded);

        let scheduler = Scheduler::new(&registry);
        let selected_after_unload = scheduler.select_node_for_model(1024);
        assert!(
            selected_after_unload.is_some(),
            "Should fit large model easily after previous model is unloaded"
        );
        assert_eq!(selected_after_unload.unwrap().id, compute.id);
    }
}
