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

## 3. Integration Tests

Integration tests live in the `tests/` directory and verify:

- Coordinator/agent interactions
- Message passing
- End-to-end ModelLoad forwarding
- End-to-end inference request routing and result return

These tests spin up an in-process coordinator and assert on the full message round-trip.

---

## 3. Coverage

Coverage is measured using:

```
cargo llvm-cov
```

Coverage goals:

- 80% minimum
- 90% target
- 100% for shared crate

---

## 4. Pre-commit Requirements

Before any commit:

- All tests must pass
- No warnings allowed
- Code must be formatted
- Clippy must be clean

These rules are enforced by the pre-commit hook.

---

## 5. AI Collaboration

AIs (Copilot, , Gemini) are expected to:

- Generate tests alongside code
- Review diffs
- Explain failures
- Suggest improvements

This ensures a high-quality, AI-augmented development workflow.

---

This document will evolve as the testing system grows.
