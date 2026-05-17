use shared::messages::{InferenceRequest, InferenceResult, WIRE_VERSION};
use std::marker::PhantomData;
use std::time::Instant;

// ── State markers ─────────────────────────────────────────────────────────────

pub struct Unloaded;
pub struct Loading;
pub struct Ready;
pub struct Failed;

/// Sealed trait prevents external crates from implementing `ModelState`.
mod private {
    pub trait Sealed {}
    impl Sealed for super::Unloaded {}
    impl Sealed for super::Loading {}
    impl Sealed for super::Ready {}
    impl Sealed for super::Failed {}
}

pub trait ModelState: private::Sealed {}
impl ModelState for Unloaded {}
impl ModelState for Loading {}
impl ModelState for Ready {}
impl ModelState for Failed {}

// ── Handle ────────────────────────────────────────────────────────────────────

pub struct ModelHandle<S: ModelState> {
    pub node_id: String,
    pub model_name: String,
    _state: PhantomData<S>,
}

// Unloaded ──► Loading
impl ModelHandle<Unloaded> {
    pub fn new(node_id: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            model_name: model_name.into(),
            _state: PhantomData,
        }
    }

    pub fn begin_loading(self) -> ModelHandle<Loading> {
        ModelHandle {
            node_id: self.node_id,
            model_name: self.model_name,
            _state: PhantomData,
        }
    }
}

// Loading ──► Ready | Failed
impl ModelHandle<Loading> {
    pub fn mark_ready(self) -> ModelHandle<Ready> {
        ModelHandle {
            node_id: self.node_id,
            model_name: self.model_name,
            _state: PhantomData,
        }
    }

    pub fn mark_failed(self) -> ModelHandle<Failed> {
        ModelHandle {
            node_id: self.node_id,
            model_name: self.model_name,
            _state: PhantomData,
        }
    }
}

// Ready ──► inference | Unloaded
impl ModelHandle<Ready> {
    /// Execute inference. Only callable in the `Ready` state — compile-time enforced.
    ///
    /// Phase 6 migration: convert to `async fn`, release the registry lock before
    /// calling, and at the call site use:
    ///   `handle.execute_inference(&req).instrument(span).await`
    pub fn execute_inference(&self, request: &InferenceRequest) -> InferenceResult {
        let started = Instant::now();
        InferenceResult {
            request_id: request.request_id.clone(),
            node_id: self.node_id.clone(),
            model_name: self.model_name.clone(),
            output: String::new(),
            tokens_generated: 0,
            duration_ms: started.elapsed().as_millis() as u64,
            error: Some("inference not yet implemented — Phase 6".into()),
            wire_version: WIRE_VERSION,
        }
    }

    pub fn unload(self) -> ModelHandle<Unloaded> {
        ModelHandle {
            node_id: self.node_id,
            model_name: self.model_name,
            _state: PhantomData,
        }
    }
}

// Failed ──► Unloaded (retry)
impl ModelHandle<Failed> {
    pub fn retry(self) -> ModelHandle<Unloaded> {
        ModelHandle {
            node_id: self.node_id,
            model_name: self.model_name,
            _state: PhantomData,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shared::messages::{InferenceRequest, WIRE_VERSION};

    #[test]
    fn unloaded_to_ready_transition() {
        let h = ModelHandle::<Unloaded>::new("node-1", "llama3");
        let h = h.begin_loading();
        let h = h.mark_ready();
        assert_eq!(h.node_id, "node-1");
        assert_eq!(h.model_name, "llama3");
        let _ = h.unload();
    }

    #[test]
    fn loading_to_failed_then_retry() {
        let h = ModelHandle::<Unloaded>::new("node-1", "llama3")
            .begin_loading()
            .mark_failed()
            .retry();
        assert_eq!(h.node_id, "node-1");
    }

    #[test]
    fn execute_inference_returns_result() {
        let h = ModelHandle::<Unloaded>::new("node-1", "llama3")
            .begin_loading()
            .mark_ready();
        let req = InferenceRequest {
            request_id: "r1".into(),
            node_id: "node-1".into(),
            model_name: "llama3".into(),
            prompt: "hello".into(),
            max_tokens: 64,
            wire_version: WIRE_VERSION,
        };
        let result = h.execute_inference(&req);
        assert_eq!(result.request_id, "r1");
        assert_eq!(result.wire_version, WIRE_VERSION);
        assert!(result.error.is_some());
    }
}
