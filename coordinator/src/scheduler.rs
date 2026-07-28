use crate::registry::Registry;
use shared::ModelLifecycleState;
use shared::messages::NodeRecordFull;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Scheduler<'a> {
    registry: &'a Registry,
}

impl<'a> Scheduler<'a> {
    pub fn new(registry: &'a Registry) -> Self {
        Self { registry }
    }

    /// Select a Compute node that has the given model loaded and in Ready state.
    /// Picks randomly among all eligible nodes so requests spread across the cluster.
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

        if candidates.is_empty() {
            return None;
        }

        let idx = nanos_hash() % candidates.len();
        Some(candidates.swap_remove(idx))
    }

    /// Select the best Compute node that can fit the given model size (in MB).
    /// Prefers the node with the most remaining capacity so placements spread evenly.
    pub fn select_node_for_model(&self, model_size_mb: u64) -> Option<NodeRecordFull> {
        let mut candidates: Vec<_> = self
            .registry
            .eligible_compute_nodes()
            .into_iter()
            .filter_map(|lite| {
                let remaining = self.model_headroom_mb(&lite.id)?;
                if remaining >= model_size_mb {
                    Some((lite, remaining))
                } else {
                    None
                }
            })
            .collect();

        // Most headroom first so placements spread across nodes.
        candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
        candidates.into_iter().next().map(|(node, _)| node)
    }

    /// RAM budget left for new models on one node: its reported
    /// `max_model_size_gb` minus everything already Ready or Loading.
    /// `None` when the node is unknown or has never reported capabilities.
    pub fn model_headroom_mb(&self, node_id: &str) -> Option<u64> {
        let node = self.registry.get(node_id)?;
        let caps = node.capabilities.as_ref()?;
        let max_mb = (caps.max_model_size_gb * 1024.0) as u64;
        let allocated_mb: u64 = node
            .models
            .values()
            .filter(|m| {
                m.state == ModelLifecycleState::Ready || m.state == ModelLifecycleState::Loading
            })
            .map(|m| m.size_mb)
            .sum();
        Some(max_mb.saturating_sub(allocated_mb))
    }

    /// Validate an *explicitly targeted* model load against the same capacity
    /// rules auto-placement uses — the check that historically only ran when
    /// no node was named. `Err` carries a human-readable refusal reason.
    pub fn check_node_for_model(&self, node_id: &str, model_size_mb: u64) -> Result<(), String> {
        let node = self
            .registry
            .get(node_id)
            .ok_or_else(|| format!("unknown node '{node_id}'"))?;
        if node.identity.role != shared::hardware::NodeRole::Compute {
            return Err(format!(
                "node '{}' is a {:?} node, not Compute — it does not load models",
                node.identity.hostname, node.identity.role
            ));
        }
        let headroom = self.model_headroom_mb(node_id).ok_or_else(|| {
            format!(
                "node '{}' has not reported its model capabilities yet",
                node.identity.hostname
            )
        })?;
        if headroom < model_size_mb {
            return Err(format!(
                "model needs {model_size_mb} MB but node '{}' has only {headroom} MB of model headroom left",
                node.identity.hostname
            ));
        }
        Ok(())
    }
}

fn nanos_hash() -> usize {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    nanos.hash(&mut h);
    h.finish() as usize
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

    #[test]
    fn select_node_for_inference_distributes_across_nodes() {
        let mut registry = Registry::new();

        for name in ["alpha", "beta"] {
            let node = make_identity(name, NodeRole::Compute);
            registry.update_heartbeat(node.clone());
            registry.update_capabilities(
                &node.id,
                NodeCapabilities {
                    max_model_size_gb: 4.0,
                    ..NodeCapabilities::default()
                },
            );
            registry.update_model_status(&node.id, "qwen", 1024, ModelLifecycleState::Ready);
        }

        let scheduler = Scheduler::new(&registry);
        let selections: std::collections::HashSet<String> = (0..40)
            .filter_map(|_| scheduler.select_node_for_inference("qwen"))
            .map(|n| n.id)
            .collect();

        assert_eq!(
            selections.len(),
            2,
            "both nodes should be selected across 40 calls"
        );
    }

    #[test]
    fn select_node_for_model_prefers_most_headroom() {
        let mut registry = Registry::new();

        // node-a: 8 GB capacity, 1 GB allocated → 7 GB free
        let a = make_identity("node-a", NodeRole::Compute);
        registry.update_heartbeat(a.clone());
        registry.update_capabilities(
            &a.id,
            NodeCapabilities {
                max_model_size_gb: 8.0,
                ..NodeCapabilities::default()
            },
        );
        registry.update_model_status(&a.id, "small", 1024, ModelLifecycleState::Ready);

        // node-b: 8 GB capacity, nothing allocated → 8 GB free
        let b = make_identity("node-b", NodeRole::Compute);
        registry.update_heartbeat(b.clone());
        registry.update_capabilities(
            &b.id,
            NodeCapabilities {
                max_model_size_gb: 8.0,
                ..NodeCapabilities::default()
            },
        );

        let scheduler = Scheduler::new(&registry);
        let selected = scheduler.select_node_for_model(2048).unwrap();
        assert_eq!(
            selected.id, b.id,
            "node-b has more headroom and should be preferred"
        );
    }

    #[test]
    fn check_node_for_model_accepts_a_fitting_model() {
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
        assert!(
            Scheduler::new(&registry)
                .check_node_for_model(&compute.id, 2048)
                .is_ok()
        );
    }

    #[test]
    fn check_node_for_model_rejects_when_headroom_exhausted() {
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
        registry.update_model_status(&compute.id, "resident", 3072, ModelLifecycleState::Ready);
        let err = Scheduler::new(&registry)
            .check_node_for_model(&compute.id, 2048)
            .unwrap_err();
        assert!(err.contains("headroom"), "unexpected reason: {err}");
    }

    #[test]
    fn check_node_for_model_rejects_non_compute_nodes() {
        let mut registry = Registry::new();
        let controller = make_identity("controller", NodeRole::Controller);
        registry.update_heartbeat(controller.clone());
        let err = Scheduler::new(&registry)
            .check_node_for_model(&controller.id, 128)
            .unwrap_err();
        assert!(err.contains("not Compute"), "unexpected reason: {err}");
    }

    #[test]
    fn check_node_for_model_rejects_unknown_node_and_missing_capabilities() {
        let mut registry = Registry::new();
        assert!(
            Scheduler::new(&registry)
                .check_node_for_model("ghost", 128)
                .is_err()
        );

        // Known Compute node that has never sent a Capabilities message.
        let compute = make_identity("compute", NodeRole::Compute);
        registry.update_heartbeat(compute.clone());
        let err = Scheduler::new(&registry)
            .check_node_for_model(&compute.id, 128)
            .unwrap_err();
        assert!(err.contains("capabilities"), "unexpected reason: {err}");
    }
}
