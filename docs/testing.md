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

---

## 2. Integration Tests

Integration tests live in the `tests/` directory and verify:

- Coordinator/agent interactions
- Message passing
- Update flows
- Hardware detection behavior

These tests run against the compiled binaries.

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
