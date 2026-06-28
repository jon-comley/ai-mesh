//! Statistical prompt compression — Phase A of the online-AI ("gateway") feature.
//!
//! Wraps the pure-Rust [`compression_prompt`] crate (statistical IDF/importance
//! filtering — no model, no network, <1 ms). Compression is only ever applied to
//! bulky *context* (device list + conversation history) on the cloud-forward
//! path; the user's actual question and any tool-calling prompt are never
//! compressed. See `intent.rs` for where this is invoked.
//!
//! The crate's defaults already protect structured content inline
//! (`enable_protection_masks` for code/JSON/paths/identifiers, plus negation and
//! comparator preservation), so JSON in a serialized tool-result turn survives
//! without a hand-rolled per-turn guard.

use compression_prompt::{Compressor, CompressorConfig};
use serde::{Deserialize, Serialize};

/// Default target ratio when `PROMPT_COMPRESS_RATIO` is unset (keep ~50%).
const DEFAULT_TARGET_RATIO: f32 = 0.5;

/// Which compression strategy to apply.
///
/// `Statistical` is implemented today. The other two are roadmap variants,
/// surfaced as disabled "coming soon" buttons on the Gateway tab; selecting one
/// of them currently passes the text through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompressionEngine {
    /// Pure-Rust statistical token filtering (`compression-prompt` crate).
    #[default]
    Statistical,
    /// Roadmap: abstractive distillation via ai-mesh's own local LLM. Not yet available.
    LocalLlmDistiller,
    /// Roadmap: Microsoft LLMLingua-2 sidecar. Not yet available.
    Llmlingua2,
}

impl CompressionEngine {
    /// Whether this engine actually compresses today (vs. a roadmap placeholder).
    pub fn is_implemented(self) -> bool {
        matches!(self, CompressionEngine::Statistical)
    }
}

/// Result of a compression attempt. Token counts are the `compression-prompt`
/// crate's own estimates (~chars/4), suitable for relative before/after
/// comparison rather than exact provider billing.
#[derive(Debug, Clone)]
pub struct CompressionOutcome {
    /// Text to use downstream (compressed, or the original on passthrough).
    pub text: String,
    /// Estimated tokens before compression.
    pub orig_tokens: usize,
    /// Estimated tokens after compression (== `orig_tokens` on passthrough).
    pub new_tokens: usize,
    /// `new_tokens / orig_tokens` (1.0 == unchanged).
    pub ratio: f32,
    /// `true` if the text was actually compressed; `false` if passed through
    /// (too short, no net gain, or a roadmap engine).
    pub compressed: bool,
}

impl CompressionOutcome {
    fn passthrough(text: String) -> Self {
        let est = est_tokens(&text);
        Self {
            text,
            orig_tokens: est,
            new_tokens: est,
            ratio: 1.0,
            compressed: false,
        }
    }

    /// Tokens saved (0 on passthrough).
    pub fn tokens_saved(&self) -> usize {
        self.orig_tokens.saturating_sub(self.new_tokens)
    }
}

/// Match the crate's internal rough token estimate (chars / 4).
fn est_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

/// Read the target compression ratio from `PROMPT_COMPRESS_RATIO`, falling back
/// to [`DEFAULT_TARGET_RATIO`]. Clamped to a sane (0.1, 0.95) range.
fn target_ratio() -> f32 {
    std::env::var("PROMPT_COMPRESS_RATIO")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|r| r.clamp(0.1, 0.95))
        .unwrap_or(DEFAULT_TARGET_RATIO)
}

/// Compress `text` with the given engine. Never fails: anything the underlying
/// engine rejects (input too short, negative gain) is returned as a passthrough
/// outcome with the original text intact.
pub fn compress(text: &str, engine: CompressionEngine) -> CompressionOutcome {
    if !engine.is_implemented() {
        return CompressionOutcome::passthrough(text.to_string());
    }

    let config = CompressorConfig {
        target_ratio: target_ratio(),
        ..Default::default()
    };
    match Compressor::new(config).compress(text) {
        Ok(result) => CompressionOutcome {
            orig_tokens: result.original_tokens,
            new_tokens: result.compressed_tokens,
            ratio: result.compression_ratio,
            compressed: true,
            text: result.compressed,
        },
        Err(_) => CompressionOutcome::passthrough(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block comfortably over the crate's 1024-byte / 100-token floor.
    fn long_text() -> String {
        "The coordinator schedules inference requests across the mesh of nodes. \
         Each node advertises its capabilities and the models it currently holds \
         in a Ready state, and the scheduler picks one at random among those that \
         can serve the requested model. Conversation history and the device \
         context are the bulky parts of a prompt, so those are what we compress. "
            .repeat(6)
    }

    #[test]
    fn statistical_compresses_long_text() {
        let input = long_text();
        let out = compress(&input, CompressionEngine::Statistical);
        assert!(out.compressed, "long text should compress");
        assert!(
            out.new_tokens < out.orig_tokens,
            "expected fewer tokens: {} -> {}",
            out.orig_tokens,
            out.new_tokens
        );
        assert!(out.ratio < 1.0 && out.ratio > 0.0);
        assert!(!out.text.is_empty());
    }

    #[test]
    fn short_text_passes_through() {
        let out = compress(
            "turn the kitchen lights blue",
            CompressionEngine::Statistical,
        );
        assert!(!out.compressed, "short text should not be compressed");
        assert_eq!(out.text, "turn the kitchen lights blue");
        assert_eq!(out.ratio, 1.0);
        assert_eq!(out.tokens_saved(), 0);
    }

    #[test]
    fn roadmap_engines_pass_through() {
        let input = long_text();
        for engine in [
            CompressionEngine::LocalLlmDistiller,
            CompressionEngine::Llmlingua2,
        ] {
            let out = compress(&input, engine);
            assert!(!out.compressed);
            assert_eq!(out.text, input);
        }
    }
}
