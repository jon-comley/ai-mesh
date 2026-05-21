# Testing Strategy

The ai-mesh project uses a strict testing philosophy to ensure reliability, stability, and long-term maintainability.

---

## 1. Unit Tests

Each module includes unit tests that verify:

- Struct construction
- Serialization/deserialization
- Round-trip invariants
- Enum behavior
- Error handling

Unit tests live inside the module using:

```rust
#[cfg(test)]
mod tests { ... }
```

### Agent — `llama.rs` — GGUF resolution and chat response parsing

Tests the pure functions that map model names to GGUF file specs and the deserialization of llama-server `/v1/chat/completions` responses:

| Test | What it pins |
|------|-------------|
| `resolve_gguf_unknown_model_returns_err` | Unsupported model name produces an error containing the name |
| `resolve_gguf_1_5b_single_shard` | 1.5b resolves to correct repo and single-shard filename |
| `resolve_gguf_7b_two_shards` | 7b resolves to correct repo and both shard filenames in order |
| `resolve_gguf_14b_three_shards` | 14b resolves to three shards |
| `resolve_gguf_32b_five_shards` | 32b resolves to five shards |
| `resolve_gguf_multi_shard_models_start_with_shard_1` | First shard always contains `00001` (llama-server loads from first) |
| `resolve_gguf_shards_agree_on_total_count` | All shards embed the same total count |
| `llama_host_returns_http_url` | `LLAMA_HOST` env var produces an http:// URL |
| `gpu_layers_defaults_to_zero_when_unset` | `LLAMA_GPU_LAYERS` unset → 0 |
| `flash_attn_defaults_to_false_when_unset` | `LLAMA_FLASH_ATTN` unset → false |
| `model_dir_ends_with_ai_mesh_models_when_unset` | Default model dir ends with `.ai-mesh/models` |
| `chat_response_parses_full_response` | Full JSON with choices, usage, timings all deserialised correctly |
| `chat_response_usage_and_timings_default_when_absent` | Missing usage/timings fields default to zero |
| `chat_response_empty_choices_is_valid` | Empty choices array is valid |
| `chat_response_timings_zero_triggers_wall_clock_fallback` | `predicted_ms = 0` causes wall-clock branch in `generate()` |
| `unload_model_is_ok_with_no_process` | `unload_model()` is a no-op when no process is running |

---

### CLI — `commands/watch.rs` — `diff` event detection

Tests the pure `diff(prev, current)` function that generates event log entries for the TUI:

| Test | What it pins |
|------|-------------|
| `no_change_produces_no_events` | Identical snapshots produce no events |
| `node_join_detected` | New node produces a `[+]` event with hostname |
| `node_leave_detected` | Missing node produces a `[-]` event with hostname |
| `model_state_change_detected` | `Loading → Ready` produces a `[M]` event |
| `new_model_on_existing_node_detected` | Model appearing on existing node produces a `[M]` event |
| `unchanged_model_produces_no_event` | Same model/state produces no event |
| `model_removal_detected` | Dropped model produces a `[M] removed` event |

### CLI — `commands/info.rs` — `format_info` output formatting

Tests the pure `format_info(node) -> String` function used by `just hardware-report` and `mesh info`:

| Test | What it pins |
|------|-------------|
| `contains_basic_identity_fields` | ID, hostname, IP, role, heartbeat always present |
| `no_hardware_shows_placeholder` | `None` hardware renders `(no hardware report)` |
| `no_capabilities_shows_placeholder` | `None` capabilities renders `(no capabilities report)` |
| `hardware_fields_formatted_correctly` | CPU model/cores/threads, RAM, OS/arch rendered as labelled fields |
| `gpu_present_shown_in_output` | GPU name appears when `Some(...)` |
| `capabilities_fields_formatted_correctly` | All four capability flags and max model size rendered correctly |
| `empty_models_omits_models_section` | No Models section when `models` is empty |
| `models_listed_when_present` | Each model's name, size, and lifecycle state all present |

---

## 2. Agent Disconnect Tests

`agent/src/agent.rs` includes tests for graceful channel closure — the failure mode that previously caused panics during reconnect cycles:

- `start_once_returns_false_on_closed_channel` — drops the receiver before calling `start_once()`; asserts `Ok(false)` rather than a panic or error
- `run_exits_cleanly_on_closed_channel` — same setup for `run()`; asserts `Ok(())` so the reconnect loop gets a clean exit

These tests pin the contract: a closed channel is a normal condition (the TCP connection dropped), not an error.

---

### Agent — `identity.rs` — Persistent node UUID

| Test | What it pins |
|------|-------------|
| `test_generate_node_id` | UUID is written to `~/.ai-mesh/node-id` on first call and returned unchanged on subsequent calls (same machine, same UUID) |
| `test_detect_hostname` | Hostname is non-empty |
| `test_detect_local_ip` | Returns a valid dotted IPv4 address |
| `test_detect_identity` | Full identity struct is non-empty for all fields |

---

### CLI — `commands/wait_ready.rs` — state helpers

| Test | What it pins |
|------|-------------|
| `format_last_seen_ms` | Values under 2000 ms rendered as `Xms` |
| `format_last_seen_seconds` | Values 2–59 s rendered as `Xs` |
| `format_last_seen_minutes` | Values ≥ 60 s rendered as `Xm Ys` |
| `ready_count_counts_ready_targets` | Only target IPs with a Ready model are counted |
| `all_ready_false_when_no_targets` | Empty target set → not ready |
| `all_ready_true_when_all_ready` | All targets Ready → true |

---

## 3. Known Gaps

These behaviours are implemented but not yet covered by automated tests:

| Area | Gap | Reason not yet tested |
|------|-----|-----------------------|
| `agent/src/main.rs` | TCP keepalive actually set on socket | Requires mock network; `socket2` call is one line |
| `agent/src/main.rs` | INFER_SEM serialises concurrent inferences | Requires two concurrent async tasks racing on a real or mock llama-server |
| `coordinator/src/server.rs` | Fast-fail on agent disconnect during pending inference | Integration test harness exists but teardown-mid-request is tricky to orchestrate |
| `cli/src/commands/wait_ready.rs` | TTY fallback path (`run_plain`) | Requires mocking `stdin` TTY detection |

---

## 4. Integration Tests

Integration tests live in the `tests/` directory and verify:

- Coordinator/agent interactions
- Message passing
- End-to-end ModelLoad forwarding
- End-to-end inference request routing and result return

These tests spin up an in-process coordinator and assert on the full message round-trip.

---

## 5. Coverage

Coverage is measured using:

```
cargo llvm-cov
```

Coverage goals:

- 80% minimum
- 90% target
- 100% for shared crate

---

## 6. Pre-commit Requirements

Before any commit:

- All tests must pass
- No warnings allowed
- Code must be formatted
- Clippy must be clean

These rules are enforced by the pre-commit hook.

---

## 7. AI Collaboration

AIs (Copilot, , Gemini) are expected to:

- Generate tests alongside code
- Review diffs
- Explain failures
- Suggest improvements

This ensures a high-quality, AI-augmented development workflow.

---

This document will evolve as the testing system grows.
