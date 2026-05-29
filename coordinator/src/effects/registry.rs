//! Registry of all known effects.
//!
//! Effects register at coordinator startup via `EffectRegistry::default()`
//! (which calls `register_builtins`). The runner looks effects up by id when
//! a room activates one.

use std::collections::HashMap;
use std::sync::Arc;

use jsonschema::JSONSchema;

use super::aurora::AuroraEffect;
use super::breathing::BreathingEffect;
use super::candlelight::CandlelightEffect;
use super::solar::SolarEffect;
use super::sunrise::SunriseEffect;
use super::sunset::SunsetEffect;
use super::telemetry::TelemetryEffect;
use super::{Effect, EffectCategory};

/// Lightweight metadata for the discovery API (`GET /api/effects`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EffectMetadata {
    pub id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub category: EffectCategory,
    pub default_params: serde_json::Value,
    pub params_schema: serde_json::Value,
}

/// A factory for constructing fresh `Effect` instances. Each room gets its own
/// instance so per-room state (RNG seeds, accumulators) doesn't bleed across
/// rooms.
pub type EffectFactory = Box<dyn Fn() -> Box<dyn Effect> + Send + Sync>;

pub struct EffectRegistry {
    factories: HashMap<&'static str, EffectFactory>,
    metadata: Vec<EffectMetadata>,
    /// Compiled JSON Schemas keyed by effect_id. Populated at `register()` time
    /// so the activation endpoint reuses a single validator across requests
    /// instead of recompiling on every POST.
    compiled_schemas: HashMap<&'static str, Arc<JSONSchema>>,
}

impl EffectRegistry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
            metadata: Vec::new(),
            compiled_schemas: HashMap::new(),
        }
    }

    /// Register an effect by passing a factory closure. Called once at startup.
    pub fn register<F>(&mut self, factory: F)
    where
        F: Fn() -> Box<dyn Effect> + Send + Sync + 'static,
    {
        // Pull metadata from a temporary instance — cheap, only happens at startup.
        let probe = factory();
        let meta = EffectMetadata {
            id: probe.id(),
            display_name: probe.display_name(),
            description: probe.description(),
            category: probe.category(),
            default_params: probe.default_params(),
            params_schema: probe.params_schema(),
        };
        let id = probe.id();
        assert!(
            !self.factories.contains_key(id),
            "duplicate effect id registered: {id:?}"
        );
        // Compile the JSON Schema once. A bad schema is an effect-author bug
        // and must surface at startup, not on the first user POST.
        let compiled = JSONSchema::compile(&meta.params_schema)
            .unwrap_or_else(|e| panic!("invalid params_schema for effect {id:?}: {e}"));
        self.factories.insert(id, Box::new(factory));
        self.metadata.push(meta);
        self.compiled_schemas.insert(id, Arc::new(compiled));
    }

    /// Construct a fresh instance of the effect with the given id.
    pub fn instantiate(&self, id: &str) -> Option<Box<dyn Effect>> {
        self.factories.get(id).map(|f| f())
    }

    /// Snapshot of all registered effects' metadata. Used by the dashboard.
    pub fn list_metadata(&self) -> &[EffectMetadata] {
        &self.metadata
    }

    pub fn contains(&self, id: &str) -> bool {
        self.factories.contains_key(id)
    }

    /// The pre-compiled validator for this effect. `None` if the effect isn't
    /// registered.
    pub fn compiled_schema(&self, id: &str) -> Option<Arc<JSONSchema>> {
        self.compiled_schemas.get(id).cloned()
    }
}

impl Default for EffectRegistry {
    fn default() -> Self {
        let mut reg = Self::new();
        register_builtins(&mut reg);
        reg
    }
}

/// Register every effect that ships in the binary.
pub fn register_builtins(reg: &mut EffectRegistry) {
    reg.register(|| Box::new(SolarEffect::new()));
    reg.register(|| Box::new(SunsetEffect::new()));
    reg.register(|| Box::new(SunriseEffect::new()));
    reg.register(|| Box::new(BreathingEffect::new()));
    reg.register(|| Box::new(CandlelightEffect::new()));
    reg.register(|| Box::new(AuroraEffect::new()));
    reg.register(|| Box::new(TelemetryEffect::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_includes_solar() {
        let reg = EffectRegistry::default();
        assert!(reg.contains("solar"));
        assert!(reg.instantiate("solar").is_some());
    }

    #[test]
    #[should_panic(expected = "duplicate effect id registered")]
    fn duplicate_registration_panics() {
        let mut reg = EffectRegistry::default();
        reg.register(|| Box::new(crate::effects::solar::SolarEffect::new()));
    }

    #[test]
    fn unknown_effect_returns_none() {
        let reg = EffectRegistry::default();
        assert!(!reg.contains("does-not-exist"));
        assert!(reg.instantiate("does-not-exist").is_none());
    }

    /// Invariant: every registered effect's `default_params` must validate
    /// against that effect's own `params_schema`. Catches "added a new effect
    /// but forgot to keep the schema in sync with the defaults" at test time.
    #[test]
    fn every_registered_effect_default_params_validate_against_its_schema() {
        let reg = EffectRegistry::default();
        for meta in reg.list_metadata() {
            let schema = jsonschema::JSONSchema::compile(&meta.params_schema)
                .unwrap_or_else(|e| panic!("invalid schema for {:?}: {e}", meta.id));
            assert!(
                schema.is_valid(&meta.default_params),
                "effect {:?} default_params don't validate: defaults={}, schema={}",
                meta.id,
                meta.default_params,
                meta.params_schema,
            );
        }
    }

    #[test]
    fn list_metadata_returns_one_entry_per_registered_effect() {
        let reg = EffectRegistry::default();
        let meta = reg.list_metadata();
        let ids: Vec<&str> = meta.iter().map(|m| m.id).collect();
        for expected in [
            "solar",
            "sunset",
            "sunrise",
            "breathing",
            "candlelight",
            "aurora",
            "telemetry",
        ] {
            assert!(ids.contains(&expected), "missing effect: {expected}");
        }
        let by_id = |id: &str| meta.iter().find(|m| m.id == id).unwrap().category;
        assert_eq!(by_id("solar"), EffectCategory::TimeOfDay);
        assert_eq!(by_id("breathing"), EffectCategory::Ambient);
        assert_eq!(by_id("candlelight"), EffectCategory::Ambient);
        assert_eq!(by_id("aurora"), EffectCategory::Game);
        assert_eq!(by_id("telemetry"), EffectCategory::Reactive);
    }

    #[test]
    fn instantiate_runs_factory_each_call() {
        // Proves the factory pattern actually runs each call by counting via an
        // observed side effect rather than pointer identity (SolarEffect today
        // is a ZST so Box allocation reuses the same dangling address).
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);

        struct CountedEffect;
        impl Effect for CountedEffect {
            fn id(&self) -> &'static str {
                "counted"
            }
            fn display_name(&self) -> &'static str {
                "Counted"
            }
            fn description(&self) -> &'static str {
                "test"
            }
            fn category(&self) -> EffectCategory {
                EffectCategory::Ambient
            }
            fn cadence(&self) -> crate::effects::EffectCadence {
                crate::effects::EffectCadence::OnePerMinute
            }
            fn params_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn default_params(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn tick(
                &mut self,
                _: &crate::effects::EffectCtx,
            ) -> Vec<crate::effects::EffectCommand> {
                vec![]
            }
        }

        let mut reg = EffectRegistry::new();
        reg.register(|| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Box::new(CountedEffect)
        });
        // `register` calls the closure once to probe metadata.
        let baseline = CALLS.load(Ordering::SeqCst);
        let _a = reg.instantiate("counted").unwrap();
        let _b = reg.instantiate("counted").unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst) - baseline, 2);
    }
}
