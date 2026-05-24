# Auto Key Rotation — Implementation Plan

> Status: **Complete.** Implemented, end-to-end validated (fresh install → rotation → inference), reviewed and approved by Bing and Gemini.
>
> Bug fixed during validation: after Phase 3 coordinator restart, stale SQLite Ready state caused `wait-ready` to return a false positive before llama-server was running. Fixed by calling `reset-registry` after Phase 3 and restarting the local controller agent with the new token. Regression test added in `coordinator/src/registry.rs`.

---

## Background

Phase 10 (Security & Auth) is complete. The coordinator supports dual-token rotation
(`MESH_AUTH_TOKEN` + `MESH_AUTH_TOKEN_NEXT`), writes a state file at startup, and
`just set-auth-token` / `just set-fingerprint` already distribute credentials to nodes.

The remaining gap: the coordinator does not auto-generate an auth token when
`MESH_AUTH_TOKEN` is unset — it runs unauthenticated and logs a warning. There is also
no recipe for zero-downtime token rotation.

---

## Scope

| In scope | Out of scope |
|----------|-------------|
| Coordinator auto-generates token on first run | Scheduled/cron rotation |
| Atomic state file writes | Hot-swap without coordinator restart |
| `MESH_AUTH_TOKEN_NEXT` written to state during rotation window | Per-node tokens |
| `just rotate-token` zero-downtime recipe | HMAC message signing (Phase 10 deferred) |
| `restart-coordinator` distributes auto-generated token | |

---

## Files Changed

| File | Change |
|------|--------|
| `coordinator/Cargo.toml` | Add `getrandom = "0.2"` |
| `coordinator/src/state.rs` | Atomic write-rename; add `next_token: Option<&str>` param |
| `coordinator/src/coordinator.rs` | Auto-generate token when unset; pass `next_token` to `state::write` |
| `justfile` | Update `restart-coordinator` to sync token to `~/.bashrc`; add `rotate-token` |

No changes to `shared/`, `agent/`, `cli/`, or any message types. No test breakage —
coordinator unit tests use `Coordinator::new()` which skips TLS and token generation.

---

## Part 1 — `coordinator/Cargo.toml`

Add as a direct dependency (already present as a transitive dep of `ring`/`tokio-rustls`):

```toml
getrandom = "0.2"
```

---

## Part 2 — `coordinator/src/state.rs`

### Changes
1. Add `next_token: Option<&str>` parameter to `write()`.
2. Emit `MESH_AUTH_TOKEN_NEXT=...` when `next_token` is `Some`.
3. Replace `std::fs::write()` with an atomic write-then-rename pattern.

### Atomic write rationale
`std::fs::write()` is not atomic — a shell `source coordinator.state` mid-write could
read a truncated file. Writing to `.tmp` then calling `std::fs::rename()` is atomic on
Linux when source and destination are on the same filesystem (always true here:
`~/.config/ai-mesh/`). Permissions are set on the `.tmp` file before rename so the
final file inherits `0o600`.

### New `write()` signature
```rust
pub fn write(fingerprint: &str, tokens: &[String], next_token: Option<&str>)
```

### New state file format (during rotation window)
```
MESH_TLS_FINGERPRINT=AA:BB:CC:...
MESH_AUTH_TOKEN=<primary>
MESH_AUTH_TOKEN_NEXT=<next>   # only present during rotation
```

---

## Part 3 — `coordinator/src/coordinator.rs`

### Changes
1. After collecting tokens from env, if `tokens` is empty, auto-generate one:
   - `getrandom::getrandom(&mut buf)` fills 32 bytes.
   - Hex-encode with `buf.iter().map(|b| format!("{:02x}", b)).collect::<String>()`.
   - Push to `tokens`. Log clearly so the operator knows a token was generated.
2. Pass `next_token` to `state::write()`:
   - Read `MESH_AUTH_TOKEN_NEXT` from env as `Option<String>`.
   - Pass `.as_deref()` to `state::write`.

### Auto-generation log line
```
auth token auto-generated — run 'just restart-coordinator' to distribute to nodes
```
Replaces the existing `warn!("MESH_AUTH_TOKEN not set — connections will not be authenticated...")`.

### Token accepted log line (unchanged)
```
auth token validation enabled (N token(s) accepted)
```

---

## Part 4 — `justfile`

### 4a — `restart-coordinator`: sync auth token to `~/.bashrc`

After the existing fingerprint `~/.bashrc` update block, add an equivalent block for the
auth token. `coordinator.state` is already sourced at that point, so `MESH_AUTH_TOKEN`
is available.

```bash
# Sync auto-generated (or existing) auth token to ~/.bashrc
TOKEN="${MESH_AUTH_TOKEN:-}"
if [ -n "$TOKEN" ]; then
    if grep -q "MESH_AUTH_TOKEN" "$HOME/.bashrc" 2>/dev/null; then
        if [[ "$(uname -s)" == "Darwin" ]]; then
            sed -i '' "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
        else
            sed -i "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
        fi
    else
        printf '\n# ai-mesh auth token — managed by just restart-coordinator\nexport MESH_AUTH_TOKEN=%s\n' "${TOKEN}" >> "$HOME/.bashrc"
    fi
    export MESH_AUTH_TOKEN="${TOKEN}"
    echo ">>> MESH_AUTH_TOKEN set from coordinator state"
fi
```

### 4b — New `rotate-token` recipe

Zero-downtime rotation using the existing dual-token window:

```
Step 1  Source coordinator.state → OLD_TOKEN (fail if unset)
Step 2  Generate NEW_TOKEN=$(openssl rand -hex 32)
Step 3  Restart coordinator with MESH_AUTH_TOKEN=OLD + MESH_AUTH_TOKEN_NEXT=NEW
        → Both tokens accepted. Rotation window opens.
        → coordinator.state written with both tokens.
Step 4  just set-auth-token NEW_TOKEN
        → Pushes to all compute nodes (Linux: systemd drop-in; Windows: NSSM AppEnvironmentExtra)
        → Updates ~/.bashrc
Step 5  cargo run -p cli -- wait-ready <all-compute-node-IPs> --timeout 120
        → Confirms all nodes reconnected before revoking old token
        → If timeout: exit non-zero, old token still active, safe to re-run
Step 6  Restart coordinator with MESH_AUTH_TOKEN=NEW only
        → Old token revoked. Rotation window closes.
        → coordinator.state rewritten with new token only (NEXT removed naturally).
```

The coordinator restart logic is a shell function inlined in the recipe (not a separate
justfile recipe) to avoid cross-recipe env-var passing limitations.

#### Interruption safety
- Interrupted at step 3: coordinator still running with old token. Re-run safe.
- Interrupted at step 4: some nodes have new token, some have old. Coordinator accepts
  both. Re-run `just rotate-token` from scratch is safe (step 1 will read NEW_TOKEN as
  OLD and generate another new token — valid but creates a third generation; acceptable).
- Interrupted at step 5: wait-ready fails, exits before step 6. Old token still valid.
- Interrupted at step 6: coordinator down; `just restart-coordinator` recovers.

---

## Test Plan

### Existing tests (must not regress)
- All coordinator unit tests use `Coordinator::new()` (TLS + token generation disabled).
  No changes needed.
- `state.rs` tests: update to pass `None` as the new `next_token` argument; add two new
  tests: `state_contains_next_token_when_set` and `state_omits_next_token_when_absent`.

### New manual validation after implementation
```bash
# 1. Clear MESH_AUTH_TOKEN from env, restart coordinator — confirm token auto-generated
unset MESH_AUTH_TOKEN
just restart-coordinator
# Expect: log shows "auth token auto-generated"
# Expect: coordinator.state contains MESH_AUTH_TOKEN=<64 hex chars>
# Expect: ~/.bashrc updated with new token
# Expect: nodes reconnect cleanly

# 2. Run rotation
just rotate-token
# Expect: old token no longer accepted by coordinator
# Expect: coordinator.state has new token only (no MESH_AUTH_TOKEN_NEXT)
# Expect: both nodes show Ready after rotation
```

---

## Design Decisions

**Why inline the restart helper as a shell function rather than a separate justfile recipe?**
Justfile recipes each run in their own shell. Passing `MESH_AUTH_TOKEN` and
`MESH_AUTH_TOKEN_NEXT` between recipes via exported env vars is fragile and
platform-dependent. A local shell function called twice within one `#!/usr/bin/env bash`
recipe block is clean and unambiguous.

**Why `openssl rand -hex 32` in the justfile recipe rather than Rust?**
The justfile rotation recipe is orchestration, not the coordinator binary. `openssl` is
available on all target platforms. The coordinator binary uses `getrandom` (portable,
no subprocess) for its auto-generation path where shell tools are not appropriate.

**Why not hot-swap tokens at runtime?**
`Server.auth_tokens` is `Arc<Vec<String>>` set at startup. Adding a runtime reload
(SIGHUP handler or admin message) would require significant Rust changes. Given
coordinator restart takes ~3 seconds and agents reconnect automatically within 5s,
restart-based rotation is operationally equivalent for this cluster size.

**Why keep rotation on-demand rather than scheduled?**
Home-lab cluster with two nodes. The operational overhead of a cron job is not
justified yet. `just rotate-token` can be added to a crontab trivially later with:
`0 3 * * 0 cd /path/to/ai-mesh && just rotate-token >> /tmp/mesh-rotation.log 2>&1`
