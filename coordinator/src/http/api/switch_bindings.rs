//! Switch → action bindings — REST CRUD only. See `server.rs`'s
//! `MeshMessage::SwitchAction` handler (`dispatch_switch_binding`) for where
//! a binding actually fires; this module just manages the binding rows.

use axum::{
    Json,
    extract::{Extension, Path},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::http::auth::Authed;
use crate::registry::Registry;

fn valid_command(command: &str, step_delta: Option<i32>) -> bool {
    match command {
        "on" | "off" | "toggle" => true,
        "brightness_step" => step_delta.is_some(),
        _ => false,
    }
}

/// `GET /api/switch-bindings` — every configured binding.
pub async fn list_switch_bindings(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
) -> impl IntoResponse {
    match registry.lock().unwrap().list_switch_bindings() {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "list_switch_bindings failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct CreateSwitchBindingBody {
    device_id: String,
    action: String,
    target_kind: String,
    target_id: String,
    command: String,
    #[serde(default)]
    step_delta: Option<i32>,
}

/// `POST /api/switch-bindings` — bind a switch's exact (device_id, action)
/// pair to a room/group command. Re-posting the same (device_id, action)
/// replaces the existing binding (see `Registry::create_switch_binding`).
pub async fn create_switch_binding(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Json(body): Json<CreateSwitchBindingBody>,
) -> impl IntoResponse {
    let device_id = body.device_id.trim();
    let action = body.action.trim();
    if device_id.is_empty() || action.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "device_id and action must not be empty",
        )
            .into_response();
    }
    if body.target_kind != "room" && body.target_kind != "group" {
        return (
            StatusCode::BAD_REQUEST,
            "target_kind must be 'room' or 'group'",
        )
            .into_response();
    }
    if !valid_command(&body.command, body.step_delta) {
        return (
            StatusCode::BAD_REQUEST,
            "command must be on/off/toggle, or brightness_step with a step_delta",
        )
            .into_response();
    }

    let mut reg = registry.lock().unwrap();
    let target_exists = match body.target_kind.as_str() {
        "room" => reg.room_exists(&body.target_id),
        _ => reg.get_room_group(&body.target_id).is_some(),
    };
    if !target_exists {
        return (StatusCode::NOT_FOUND, "target room/group does not exist").into_response();
    }

    match reg.create_switch_binding(
        device_id,
        action,
        &body.target_kind,
        &body.target_id,
        &body.command,
        body.step_delta,
    ) {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "create_switch_binding failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `DELETE /api/switch-bindings/{id}`
pub async fn delete_switch_binding(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match registry.lock().unwrap().delete_switch_binding(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "delete_switch_binding failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, post};

    fn router(registry: Arc<Mutex<Registry>>) -> Router {
        let state = make_state(vec![], empty_connections());
        Router::new()
            .route(
                "/api/switch-bindings",
                get(list_switch_bindings).post(create_switch_binding),
            )
            .route(
                "/api/switch-bindings/{id}",
                axum::routing::delete(delete_switch_binding),
            )
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_returns_empty_array_initially() {
        let registry = make_registry();
        let (status, body) =
            send_with_body(router(registry), "GET", "/api/switch-bindings?token=", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.trim(), "[]");
    }

    #[tokio::test]
    async fn create_returns_400_for_empty_device_id() {
        let registry = make_registry();
        let status = send(
            router(registry),
            "POST",
            "/api/switch-bindings?token=",
            r#"{"device_id":"  ","action":"button_1_press","target_kind":"room","target_id":"r1","command":"toggle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_returns_400_for_bad_target_kind() {
        let registry = make_registry();
        let status = send(
            router(registry),
            "POST",
            "/api/switch-bindings?token=",
            r#"{"device_id":"dial1","action":"button_1_press","target_kind":"device","target_id":"r1","command":"toggle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_returns_400_for_brightness_step_without_delta() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Larder");
        let status = send(
            router(registry),
            "POST",
            "/api/switch-bindings?token=",
            &format!(
                r#"{{"device_id":"dial1","action":"brightness_step_up","target_kind":"room","target_id":"{room_id}","command":"brightness_step"}}"#
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_returns_404_for_unknown_room() {
        let registry = make_registry();
        let status = send(
            router(registry),
            "POST",
            "/api/switch-bindings?token=",
            r#"{"device_id":"dial1","action":"button_1_press","target_kind":"room","target_id":"nope","command":"toggle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_then_list_roundtrip() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Larder");
        let status = send(
            router(registry.clone()),
            "POST",
            "/api/switch-bindings?token=",
            &format!(
                r#"{{"device_id":"dial1","action":"button_1_press","target_kind":"room","target_id":"{room_id}","command":"toggle"}}"#
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) =
            send_with_body(router(registry), "GET", "/api/switch-bindings?token=", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("dial1"), "body: {body}");
        assert!(body.contains("button_1_press"), "body: {body}");
    }

    #[tokio::test]
    async fn create_returns_401_for_wrong_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let r = Router::new()
            .route("/api/switch-bindings", post(create_switch_binding))
            .layer(axum::Extension(registry))
            .with_state(state);
        let status = send(
            r,
            "POST",
            "/api/switch-bindings?token=wrong",
            r#"{"device_id":"dial1","action":"button_1_press","target_kind":"room","target_id":"r1","command":"toggle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_returns_404_for_unknown_id() {
        let registry = make_registry();
        let status = send(
            router(registry),
            "DELETE",
            "/api/switch-bindings/nope?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_removes_an_existing_binding() {
        let registry = make_registry();
        let room_id = make_room(&registry, "Larder");
        let id = {
            let mut reg = registry.lock().unwrap();
            reg.create_switch_binding("dial1", "button_1_press", "room", &room_id, "toggle", None)
                .unwrap()
        };
        let status = send(
            router(registry),
            "DELETE",
            &format!("/api/switch-bindings/{id}?token="),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}
