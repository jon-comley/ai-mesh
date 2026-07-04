//! Sensors — the second device domain module, per the recipe in
//! [`super`]'s module docs. Read-only for now: sensors push their state and
//! take no commands, so the module is just the snapshot list (history later).

use axum::{Json, extract::State, response::IntoResponse};
use std::sync::Arc;

use crate::http::auth::Authed;
use crate::http::state::DashboardState;

/// GET /api/sensors — latest merged readings for every known sensor.
pub async fn list_sensors(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    Json(state.get_sensor_snapshot()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use shared::SensorReport;

    fn sensors_router(state: Arc<DashboardState>) -> Router {
        Router::new()
            .route("/api/sensors", get(list_sensors))
            .with_state(state)
    }

    fn seed_sensor(state: &Arc<DashboardState>, device_id: &str) {
        state.push_sensor_update(SensorReport {
            node_id: "pi1".into(),
            device_id: device_id.into(),
            temperature: Some(21.4),
            humidity: Some(47.0),
            battery: Some(98),
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        });
    }

    #[tokio::test]
    async fn list_sensors_returns_snapshot() {
        let state = make_state(vec![], empty_connections());
        seed_sensor(&state, "office_climate");
        let (status, body) = send_with_body(sensors_router(state), "GET", "/api/sensors", "").await;
        assert_eq!(status, StatusCode::OK);
        let sensors: Vec<SensorReport> = serde_json::from_str(&body).unwrap();
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].device_id, "office_climate");
        assert_eq!(sensors[0].temperature, Some(21.4));
    }

    #[tokio::test]
    async fn list_sensors_empty_is_empty_array() {
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(sensors_router(state), "GET", "/api/sensors", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "[]");
    }

    #[tokio::test]
    async fn list_sensors_returns_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(sensors_router(state), "GET", "/api/sensors?token=wrong", "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
