//! Room effects: registry listing, activation, per-device overrides.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::effects::registry::EffectRegistry;
use crate::http::state::DashboardState;
use crate::registry::Registry;

use crate::http::auth::Authed;

// ── Effects (F-Effects-2.2) ──────────────────────────────────────────────────

/// GET /api/effects — list every registered effect's metadata.
/// Cacheable: called once on dashboard load.
pub async fn list_effects(
    Extension(effects): Extension<Arc<EffectRegistry>>,
    _: Authed,
) -> impl IntoResponse {
    Json(effects.list_metadata().to_vec()).into_response()
}

#[derive(Deserialize)]
pub struct SetEffectBody {
    pub effect_id: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// POST /api/rooms/{id}/effect — set the active effect for the room.
/// Validates `effect_id` against the registry and `params` against the
/// effect's JSON Schema. 204 on success.
pub async fn set_room_effect(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Extension(effects): Extension<Arc<EffectRegistry>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SetEffectBody>,
) -> impl IntoResponse {
    // Look up the effect's metadata so we can validate params and fall back to
    // defaults if none were supplied.
    let metadata = match effects
        .list_metadata()
        .iter()
        .find(|m| m.id == body.effect_id)
        .cloned()
    {
        Some(m) => m,
        None => return (StatusCode::BAD_REQUEST, "unknown effect_id").into_response(),
    };

    // Merge any caller-supplied params on top of the effect's defaults so the
    // stored row is always fully filled. A partial body like
    // {"duration_secs":600} for Sunset keeps the default peak_warmth +
    // start_at without forcing the effect tick to handle missing keys.
    let params = merge_with_defaults(body.params, &metadata.default_params);

    // Use the pre-compiled validator from the registry — compiled once at
    // startup, reused across requests. `None` here is impossible (we already
    // matched the metadata above) but we degrade gracefully if some future
    // code path registers without compiling.
    if let Some(schema) = effects.compiled_schema(&body.effect_id)
        && let Err(errors) = schema.validate(&params)
    {
        let msg = errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        return (StatusCode::BAD_REQUEST, format!("invalid params: {msg}")).into_response();
    }

    persist_active_effect(&registry, &state, &room_id, &body.effect_id, &params)
}

/// Returns a JSON object: the effect's `defaults`, shallow-overlaid with
/// whatever the caller supplied in `body`. When `body` is `None` the defaults
/// are returned as-is. When either side isn't a JSON object the caller's value
/// wins outright (passthrough; the schema validator will reject anything
/// that's not the right shape).
fn merge_with_defaults(
    body: Option<serde_json::Value>,
    defaults: &serde_json::Value,
) -> serde_json::Value {
    let Some(body_val) = body else {
        return defaults.clone();
    };
    let (Some(body_obj), Some(default_obj)) = (body_val.as_object(), defaults.as_object()) else {
        return body_val;
    };
    let mut merged = serde_json::Map::new();
    for (k, v) in default_obj {
        merged.insert(k.clone(), v.clone());
    }
    for (k, v) in body_obj {
        merged.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(merged)
}

/// Reactivate `effect_id` with `params` for `room_id` and broadcast the
/// update. Shared by `set_room_effect` (a validated dashboard request) and
/// scene recall (`scenes.rs`) reactivating a scene's captured effect —
/// already-known-good params from when the scene was saved, so recall
/// doesn't re-validate against the schema.
pub(crate) fn persist_active_effect(
    registry: &Arc<Mutex<Registry>>,
    state: &Arc<DashboardState>,
    room_id: &str,
    effect_id: &str,
    params: &serde_json::Value,
) -> axum::response::Response {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let params_json = params.to_string();
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Err(e) = reg.set_active_effect(room_id, effect_id, &params_json, None, now_ms) {
            tracing::warn!(error = %e, "set_active_effect failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    state.push_effect_update(
        room_id.to_string(),
        Some(effect_id.to_string()),
        params.clone(),
        vec![],
    );
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

/// PATCH /api/rooms/{id}/effect/override — add or remove a device from the
/// per-effect override list. Excluded devices are skipped by the runner.
pub(crate) async fn patch_effect_override(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let device_id = match body.get("device_id").and_then(|v| v.as_str()) {
        Some(d) => d.to_string(),
        None => return (StatusCode::BAD_REQUEST, "missing device_id").into_response(),
    };
    let excluded = body
        .get("excluded")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Single lock: set the override and read back params atomically so there
    // is no TOCTOU window where the effect could be cleared between the two.
    let (new_overrides, effect_id, params) = {
        let mut reg = registry.lock().unwrap();
        let overrides = match reg.set_effect_override(&room_id, &device_id, excluded) {
            Ok(Some(list)) => list,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(e) => {
                tracing::warn!(error = %e, "set_effect_override failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        match reg.get_active_effect(&room_id) {
            Some(r) => {
                let p = serde_json::from_str(&r.params_json).unwrap_or(serde_json::json!({}));
                (overrides, r.effect_id, p)
            }
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    state.push_effect_update(room_id, Some(effect_id), params, new_overrides);
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

/// Clear the active effect for a room and broadcast the update. Shared by
/// `clear_room_effect` and scene recall (`scenes.rs`) — cancelling a
/// running effect before applying a scene's snapshot so the effect can't
/// fight the recalled state on its next tick.
pub(crate) fn clear_active_effect(
    registry: &Arc<Mutex<Registry>>,
    state: &Arc<DashboardState>,
    room_id: &str,
) -> axum::response::Response {
    {
        let mut reg = registry.lock().unwrap();
        if !reg.room_exists(room_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Err(e) = reg.disable_active_effect(room_id) {
            tracing::warn!(error = %e, "disable_active_effect failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    state.push_effect_update(room_id.to_string(), None, serde_json::json!({}), vec![]);
    state.solar_sweep_notify.notify_one();
    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /api/rooms/{id}/effect — clear the active effect.
pub async fn clear_room_effect(
    Path(room_id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    clear_active_effect(&registry, &state, &room_id)
}

/// Broadcast a room's current effect state (id/params/overrides, or the
/// "no effect" shape) to the dashboard. Shared by anything that mutates
/// effect state directly through the registry instead of through
/// `persist_active_effect`/`clear_active_effect` — currently scene recall
/// (`scenes.rs`) and manual/voice light commands excluding a device from
/// its room's effect (`exclude_device_from_its_active_effect`, below).
pub(crate) fn broadcast_active_effect_state(
    registry: &Arc<Mutex<Registry>>,
    dashboard: &Arc<DashboardState>,
    room_id: &str,
) {
    match registry.lock().unwrap().get_active_effect(room_id) {
        Some(a) => {
            let overrides: Vec<String> =
                serde_json::from_str(&a.overrides_json).unwrap_or_default();
            let params: serde_json::Value =
                serde_json::from_str(&a.params_json).unwrap_or(serde_json::json!({}));
            dashboard.push_effect_update(room_id.to_string(), Some(a.effect_id), params, overrides);
        }
        None => {
            dashboard.push_effect_update(room_id.to_string(), None, serde_json::json!({}), vec![])
        }
    }
    dashboard.solar_sweep_notify.notify_one();
}

/// If `device_id` belongs to a room with a currently-active effect, exclude
/// it from that effect and broadcast the update — so a manual or voice/chat
/// light command doesn't get silently reverted by the effect's next tick.
/// This is the same protection the dashboard's own bulb toggle already gets
/// via `excludeFromEffect()` in `rooms.js`, but that's client-side JS only;
/// this makes it hold for every caller (the `light_command` HTTP endpoint,
/// the `light_command` intent tool used by voice/chat), not just dashboard
/// clicks. A device with no room, or a room with no active effect, is a
/// no-op — cheap enough to call unconditionally on every command.
pub(crate) fn exclude_device_from_its_active_effect(
    registry: &Arc<Mutex<Registry>>,
    dashboard: Option<&Arc<DashboardState>>,
    device_id: &str,
) {
    let room_id = {
        let reg = registry.lock().unwrap();
        reg.list_rooms()
            .into_iter()
            .find(|r| r.device_ids.iter().any(|d| d == device_id))
            .map(|r| r.id)
    };
    let Some(room_id) = room_id else {
        return;
    };

    let excluded = {
        let mut reg = registry.lock().unwrap();
        if reg.get_active_effect(&room_id).is_none() {
            return;
        }
        reg.set_effect_override(&room_id, device_id, true)
    };
    if !matches!(excluded, Ok(Some(_))) {
        return;
    }

    if let Some(dash) = dashboard {
        broadcast_active_effect_state(registry, dash, &room_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, patch, post};
    use std::sync::Mutex;

    // ── effects (F-Effects-2.2) ─────────────────────────────────────────────

    fn effects_router(
        state: Arc<DashboardState>,
        registry: Arc<Mutex<Registry>>,
        effects: Arc<EffectRegistry>,
    ) -> Router {
        Router::new()
            .route("/api/effects", get(list_effects))
            .route(
                "/api/rooms/{id}/effect",
                post(set_room_effect).delete(clear_room_effect),
            )
            .route(
                "/api/rooms/{id}/effect/override",
                patch(patch_effect_override),
            )
            .layer(axum::Extension(registry))
            .layer(axum::Extension(effects))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_effects_returns_solar_metadata() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let (status, body) = send_with_body(
            effects_router(state, registry, effects),
            "GET",
            "/api/effects?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("\"id\":\"solar\""),
            "missing solar entry: {body}"
        );
        assert!(body.contains("TimeOfDay"), "missing category: {body}");
    }

    #[tokio::test]
    async fn list_effects_requires_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "GET",
            "/api/effects?token=nope",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn set_room_effect_unknown_effect_returns_400() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"does-not-exist"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_room_effect_missing_room_returns_404() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(state, registry, effects),
            "POST",
            "/api/rooms/no-such-room/effect?token=",
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_room_effect_valid_returns_204_and_persists() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let active = registry.lock().unwrap().get_active_effect(&room_id);
        assert_eq!(active.unwrap().effect_id, "solar");
    }

    #[tokio::test]
    async fn set_room_effect_omits_params_uses_default() {
        // Body without `params` should 204 and store the effect's default params.
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        // Stored params should be Solar's defaults merged in.
        let stored: serde_json::Value = serde_json::from_str(&active.params_json).unwrap();
        assert_eq!(stored["min_brightness"], 1);
        assert_eq!(stored["max_brightness"], 254);
        assert!((stored["ct_warmth"].as_f64().unwrap() - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn set_room_effect_partial_params_merge_defaults() {
        // Sunset has three defaulted params. A body that supplies only one
        // should land in the DB with all three (the body's value plus the
        // effect's defaults for the rest).
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"sunset","params":{"duration_secs":600}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let stored: serde_json::Value = serde_json::from_str(
            &registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .unwrap()
                .params_json,
        )
        .unwrap();
        assert_eq!(stored["duration_secs"], 600);
        assert_eq!(stored["peak_warmth"], 0.7); // default kept
        assert_eq!(stored["start_at"], "now"); // default kept
    }

    #[test]
    fn merge_with_defaults_overlays_partial_body_on_defaults() {
        let defaults = serde_json::json!({"a": 1, "b": 2, "c": 3});
        let body = Some(serde_json::json!({"b": 99}));
        let merged = merge_with_defaults(body, &defaults);
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 99);
        assert_eq!(merged["c"], 3);
    }

    #[test]
    fn merge_with_defaults_none_body_returns_defaults() {
        let defaults = serde_json::json!({"a": 1});
        let merged = merge_with_defaults(None, &defaults);
        assert_eq!(merged, defaults);
    }

    #[tokio::test]
    async fn clear_room_effect_returns_204_and_disables() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        // Activate first.
        let _ = send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        // Clear.
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "DELETE",
            &format!("/api/rooms/{room_id}/effect?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn set_room_effect_broadcasts_effect_update() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());
        let mut rx = state.tx.subscribe();
        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"solar"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // The handler broadcasts RoomsUpdate (from the legacy mirror) and
        // EffectUpdate. We don't care about ordering — scan for the EffectUpdate.
        let mut saw_effect = false;
        while let Ok(evt) = rx.try_recv() {
            if let crate::http::state::DashboardEvent::EffectUpdate {
                room_id: rid,
                effect_id,
                ..
            } = evt
            {
                assert_eq!(rid, room_id);
                assert_eq!(effect_id, Some("solar".into()));
                saw_effect = true;
            }
        }
        assert!(saw_effect, "EffectUpdate was not broadcast");
    }

    #[tokio::test]
    async fn patch_effect_override_excludes_device() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());

        // Activate an effect first.
        send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"breathing"}"#,
        )
        .await;

        // Exclude a device.
        let status = send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "PATCH",
            &format!("/api/rooms/{room_id}/effect/override?token="),
            r#"{"device_id":"bulb-1","excluded":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        let stored: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert_eq!(stored, vec!["bulb-1"]);
    }

    #[tokio::test]
    async fn patch_effect_override_broadcasts_effect_update_with_overrides() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());

        send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"breathing"}"#,
        )
        .await;

        let mut rx = state.tx.subscribe();
        send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "PATCH",
            &format!("/api/rooms/{room_id}/effect/override?token="),
            r#"{"device_id":"bulb-1","excluded":true}"#,
        )
        .await;

        let mut saw_overrides = false;
        while let Ok(evt) = rx.try_recv() {
            if let crate::http::state::DashboardEvent::EffectUpdate { overrides, .. } = evt {
                assert!(overrides.contains(&"bulb-1".to_string()));
                saw_overrides = true;
            }
        }
        assert!(
            saw_overrides,
            "EffectUpdate with overrides was not broadcast"
        );
    }

    #[tokio::test]
    async fn patch_effect_override_without_active_effect_returns_404() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());

        let status = send(
            effects_router(Arc::clone(&state), Arc::clone(&registry), effects),
            "PATCH",
            &format!("/api/rooms/{room_id}/effect/override?token="),
            r#"{"device_id":"bulb-1","excluded":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn patch_effect_override_include_removes_from_list() {
        let state = make_state(vec![], empty_connections());
        let registry = make_registry();
        let room_id = make_room(&registry, "Lounge");
        let effects = Arc::new(EffectRegistry::default());

        send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "POST",
            &format!("/api/rooms/{room_id}/effect?token="),
            r#"{"effect_id":"breathing"}"#,
        )
        .await;
        // Exclude.
        send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "PATCH",
            &format!("/api/rooms/{room_id}/effect/override?token="),
            r#"{"device_id":"bulb-1","excluded":true}"#,
        )
        .await;
        // Re-include.
        let status = send(
            effects_router(
                Arc::clone(&state),
                Arc::clone(&registry),
                Arc::clone(&effects),
            ),
            "PATCH",
            &format!("/api/rooms/{room_id}/effect/override?token="),
            r#"{"device_id":"bulb-1","excluded":false}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        let stored: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert!(stored.is_empty(), "bulb-1 should be re-included");
    }

    // ── exclude_device_from_its_active_effect ────────────────────────────────

    #[test]
    fn exclude_device_adds_it_to_the_active_effect_overrides() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        registry
            .lock()
            .unwrap()
            .set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
            .unwrap();

        exclude_device_from_its_active_effect(&registry, None, "bulb1");

        let active = registry
            .lock()
            .unwrap()
            .get_active_effect(&room_id)
            .unwrap();
        let overrides: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert_eq!(overrides, vec!["bulb1".to_string()]);
    }

    #[test]
    fn exclude_device_broadcasts_the_updated_effect_state() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");
        registry
            .lock()
            .unwrap()
            .set_active_effect(&room_id, "aurora", r#"{"speed":1}"#, None, 0)
            .unwrap();
        let state = make_state(vec![], empty_connections());
        let mut rx = state.tx.subscribe();

        exclude_device_from_its_active_effect(&registry, Some(&state), "bulb1");

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::http::state::DashboardEvent::EffectUpdate { effect_id: Some(id), overrides, .. }
                    if id == "aurora" && overrides == &vec!["bulb1".to_string()]
            )),
            "expected an EffectUpdate reflecting the new override: {events:?}"
        );
    }

    #[test]
    fn exclude_device_is_a_noop_when_room_has_no_active_effect() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Bedroom");
        registry
            .lock()
            .unwrap()
            .add_device_to_room(&room_id, "bulb1");

        // Must not panic even though no effect is active.
        exclude_device_from_its_active_effect(&registry, None, "bulb1");

        assert!(
            registry
                .lock()
                .unwrap()
                .get_active_effect(&room_id)
                .is_none()
        );
    }

    #[test]
    fn exclude_device_is_a_noop_for_a_device_with_no_room() {
        let registry = make_registry();
        // Must not panic for a device that isn't in any room.
        exclude_device_from_its_active_effect(&registry, None, "orphan_bulb");
    }
}
