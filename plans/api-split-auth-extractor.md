# Auth Extractor + api.rs Domain Split (lights ⁄ rooms separated)

## Context

`coordinator/src/http/api.rs` is the largest file in the repo (3,917 lines — 40 handlers + a 2,288-line test module), and 37 of its handlers hand-roll the same three-line auth check. Two goals from the 2026-07-03 review backlog, requested together:

1. **Auth as a type** — an axum `FromRequestParts` extractor so auth is impossible to forget on new routes and ~120 lines of boilerplate disappear.
2. **Domain module split** — with the explicit requirement that **lights (a device domain) and rooms (the spatial container) become separate modules**, because aircon, blinds, and sensors are coming (roadmap Phase 11.8): future domains get sibling modules parallel to `lights.rs`, and rooms stay domain-agnostic.

Pure refactor: no route paths, wire messages, or behavior change (one deliberate, additive exception: `/api/*` will also accept `Authorization: Bearer`, unifying token extraction with `/v1/*`).

## Verified terrain (from exploration)

- All 40 handlers registered in `http/mod.rs:65-133`; only cross-file dependency is `openai.rs:12` importing `TokenQuery`. Re-export shims NOT needed — update both call sites directly (no-shims convention).
- **`solar_config` (api.rs:504) is deliberately unauthenticated** — the one census deviation; it must NOT get the extractor.
- Genuinely cross-section items: `gen_request_id`, `TokenQuery`, `build_light_action` + `LightCommandBody` (used by lights, `room_command`, `recall_scene`). Everything else is section-local.
- Test module groups already align with the split; only `make_state`/`empty_connections` are shared helpers.
- No existing `FromRequestParts` impl in the repo — this introduces the first.

## Design

### New `coordinator/src/http/auth.rs`
- `TokenQuery` moves here (unchanged shape).
- `pub(crate) fn token_from(parts…)` — Bearer header first, `?token=` fallback (generalizes openai.rs's `request_token`).
- **`Authed` extractor**: `impl FromRequestParts<Arc<DashboardState>> for Authed` — extracts the token, checks `state.auth_ok`, rejection = plain `StatusCode::UNAUTHORIZED` (exactly today's behavior). Handlers take `_: Authed` as their first param.
- openai.rs keeps its own envelope-shaped 401 but imports `TokenQuery` + the token helper from here (deletes its local `request_token`).

### `api.rs` → `coordinator/src/http/api/` directory

| New file | Contents (from the terrain map) |
|---|---|
| `api/mod.rs` | `pub mod …` declarations; shared `gen_request_id()`; `#[cfg(test)] pub(crate) mod test_util` (`make_state`, `empty_connections`). **Module doc = the domain recipe**: adding a device domain (aircon, blinds, sensors) means a new sibling module parallel to `lights.rs` with its own command primitives; `rooms.rs` stays domain-agnostic and never gains device-specific logic. |
| `api/nodes.rs` | `set_heartbeat_interval`, `load_model`, `unload_model`, `get_reaper_state` (node/agent control + status) |
| `api/lights.rs` | `light_command`, `group_light_command`, `delete_device`, `rename_device`, `get_device_names`, `get_light_position`, `update_light_position`, **`build_light_action` + `LightCommandBody`** (lighting-domain primitives — rooms/scenes import them from here; when aircon/blinds arrive they get parallel primitives in their own modules) |
| `api/rooms.rs` | rooms CRUD + reorder + `modify_room_devices` + `reorder_room_devices`, `room_command`, orientation/origin/dimensions, `get_room_positions`, openings CRUD + `VALID_WALL_EDGES`, `rooms_from_registry`, `solar_config` (public — doc-comment why it skips `Authed`). Module doc: rooms are the domain-agnostic spatial container. |
| `api/scenes.rs` | scenes CRUD/reorder/recall, `scenes_from_registry` |
| `api/effects.rs` | `list_effects`, `set_room_effect`, `clear_room_effect`, `patch_effect_override`, `merge_with_defaults`, `persist_active_effect` |
| `api/chat.rs` | `chat` handler |
| `api/gateway.rs` | `get_gateway`, `set_gateway`, `test_gateway`, `gateway_snapshot` |
| `api/prefs.rs` | preferences handlers + `PREF_USER_ID`, `PrefBody` |

Each handler's local body/response structs move with it. Each module carries its own `#[cfg(test)]` tests (the existing per-group routers — `rooms_router`, `scenes_router`, `effects_router`, etc. — move wholesale; they only need `test_util` imports).

### Handler signature change (×37)
`Query(q): Query<TokenQuery>` + `if !state.auth_ok(&q.token) {…}` → `_: Authed` (first extractor). `solar_config` untouched. Handlers that used `q.token` for nothing else lose the Query extractor entirely.

### Route table (`http/mod.rs`)
Update all 40 registrations to module paths (`api::lights::light_command`, …), grouped by module with a comment per domain block. The `solar_config` registration gets an explicit `// PUBLIC — no Authed:` comment at the registration site (future routes are added by copying from this block, so the justification must live here, not only on the handler).

## Steps

1. Create `http/auth.rs` (TokenQuery, token helper, `Authed` + unit tests: bearer accepted, query fallback, wrong/missing → 401, dev-mode empty-token-list accepts). Register `mod auth;` in `http/mod.rs`.
2. Convert `api.rs` → `api/` directory: create `api/mod.rs` with shared items + test_util.
3. Move sections into the eight domain files, converting each handler to `Authed` as it moves (one pass per file; compiler drives completeness since old `api::X` paths die).
4. Update `http/mod.rs` route table + `openai.rs` imports (`auth::TokenQuery`, delete `request_token`, use shared helper).
5. Move test groups into their module files; shared helpers to `api/mod.rs::test_util`.
6. Sweep: `grep -rn "api::" coordinator/src` (only mod.rs routes remain), `grep TokenQuery`, fmt, clippy, full test suite.
7. Docs: `docs/coordinator.md` module-layout note if it describes api.rs; tick the two boxes in the roadmap backlog; note the additive Bearer support on `/api/*` in `docs/openai-api.md`'s auth section (one line).

## Risks / gotchas

- **`solar_config` must stay public** — the dashboard JS calls it before auth in some flows; test asserts it (existing test at old api.rs:3239 moves to rooms.rs).
- Extractor ordering: `Authed` is a parts-extractor so it can precede `Path`/`Extension`; `Json` bodies stay last. Compiler enforces.
- `patch_effect_override` is `pub(crate)` — keep visibility as-is.
- Tests reference `super::` items heavily; moving groups means fixing `use` paths — mechanical, compiler-driven.
- Behavior deltas: exactly one, additive (`/api/*` accepts Bearer). Everything else byte-identical, including the 401 shape.

## Verification

1. `cargo test --workspace` (749 tests must stay green — the moved tests are the regression net) + new auth.rs tests; clippy; fmt.
2. `just build` then live after next deploy (no urgency — behavior-preserving): spot-check one route per module with `?token=`, one with Bearer, `solar_config` with no token, and a wrong-token 401.
3. Line-count sanity: no `api/*.rs` file above ~800 lines including tests; `grep -c auth_ok coordinator/src/http/api/` returns 0 (all via extractor).
4. Deferred idea (from review): a no-token sweep test asserting 401 on every `/api/*` route except `solar_config` — requires a hand-maintained path list (axum routers aren't introspectable), which reintroduces drift; the extractor-as-parameter is the primary guarantee. Revisit if an auth omission ever slips through.
