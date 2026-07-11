//! Live Bluetooth discovery + pairing, triggered from the dashboard's AV
//! section — the Bluetooth equivalent of `zigbee.rs`'s permit-join, but
//! per-node rather than bridge-wide: Bluetooth capability is tied to
//! whichever specific node has `bluetooth` in its `AUDIO_BACKENDS` (pi2
//! today), unlike Zigbee's single bridge-owning node. Fire-and-forget;
//! results stream back as `DashboardEvent::BluetoothDeviceFound`/
//! `BluetoothPairResult` WS events.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use shared::{BluetoothClearCacheRequest, BluetoothPairRequest, BluetoothScanRequest, MeshMessage};
use std::sync::Arc;

use super::gen_request_id;
use crate::http::auth::Authed;
use crate::http::state::DashboardState;

/// Default scan window — long enough to catch a device the user is
/// actively holding in pair mode, short enough not to leave `bluetoothctl`
/// running unattended. Capped well under Zigbee's 254s (a Bluetooth scan
/// window has no protocol-level cap to mirror; this is just a sane UI
/// default/ceiling).
const DEFAULT_SCAN_SECONDS: u16 = 20;
const MAX_SCAN_SECONDS: u16 = 120;

#[derive(Deserialize, Default)]
pub struct ScanBody {
    #[serde(default)]
    seconds: Option<u16>,
}

/// POST /api/bluetooth/scan/{node_id} — open a live discovery window on
/// that node. Feedback streams to the dashboard as
/// `BluetoothDeviceFound` WS events.
pub async fn scan(
    Path(node_id): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let body: ScanBody = {
        let bytes = axum::body::to_bytes(req.into_body(), 4096)
            .await
            .unwrap_or_default();
        if bytes.is_empty() {
            ScanBody::default()
        } else {
            serde_json::from_slice(&bytes).unwrap_or_default()
        }
    };
    let seconds = body
        .seconds
        .unwrap_or(DEFAULT_SCAN_SECONDS)
        .min(MAX_SCAN_SECONDS) as u8;
    let sent = state.send_to_node(
        &node_id,
        MeshMessage::BluetoothScan(BluetoothScanRequest {
            request_id: gen_request_id(),
            seconds,
        }),
    );
    if sent {
        Json(serde_json::json!({ "seconds": seconds })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "node not reachable").into_response()
    }
}

#[derive(Deserialize)]
pub struct PairBody {
    mac: String,
}

/// POST /api/bluetooth/pair/{node_id} — pair, trust, connect, and adopt as
/// that node's bluetooth sink. Feedback streams to the dashboard as a
/// `BluetoothPairResult` WS event.
pub async fn pair(
    Path(node_id): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<PairBody>,
) -> impl IntoResponse {
    let sent = state.send_to_node(
        &node_id,
        MeshMessage::BluetoothPair(BluetoothPairRequest {
            request_id: gen_request_id(),
            mac: body.mac,
        }),
    );
    if sent {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "node not reachable").into_response()
    }
}

/// POST /api/bluetooth/clear-cache/{node_id} — forget every cached,
/// non-connected BlueZ device on that node. `scan()` seeds its results
/// from that cache, so without this a stale entry (out of range, or from
/// long ago) keeps reappearing indistinguishable from something live
/// right now. Feedback streams to the dashboard as a
/// `BluetoothClearCacheResult` WS event.
pub async fn clear_cache(
    Path(node_id): Path<String>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    let sent = state.send_to_node(
        &node_id,
        MeshMessage::BluetoothClearCache(BluetoothClearCacheRequest {
            request_id: gen_request_id(),
        }),
    );
    if sent {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "node not reachable").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::post;
    use tokio::sync::mpsc;

    fn bluetooth_router(state: Arc<DashboardState>) -> Router {
        Router::new()
            .route("/api/bluetooth/scan/{node_id}", post(scan))
            .route("/api/bluetooth/pair/{node_id}", post(pair))
            .route("/api/bluetooth/clear-cache/{node_id}", post(clear_cache))
            .with_state(state)
    }

    #[tokio::test]
    async fn scan_sends_to_the_named_node_with_default_seconds() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/scan/pi2",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::BluetoothScan(req) => assert_eq!(req.seconds, DEFAULT_SCAN_SECONDS as u8),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn scan_clamps_seconds_to_the_max() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/scan/pi2",
            r#"{"seconds":9999}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::BluetoothScan(req) => {
                assert_eq!(req.seconds, MAX_SCAN_SECONDS as u8)
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn scan_503_when_node_not_connected() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/scan/pi2",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn scan_401_for_wrong_token() {
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/scan/pi2?token=wrong",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pair_sends_the_mac_to_the_named_node() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/pair/pi2",
            r#"{"mac":"AA:BB:CC:DD:EE:FF"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::BluetoothPair(req) => assert_eq!(req.mac, "AA:BB:CC:DD:EE:FF"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pair_503_when_node_not_connected() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/pair/pi2",
            r#"{"mac":"AA:BB:CC:DD:EE:FF"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn clear_cache_sends_to_the_named_node() {
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi2".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/clear-cache/pi2",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::BluetoothClearCache(_) => {}
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_cache_503_when_node_not_connected() {
        let state = make_state(vec![], empty_connections());
        let status = send(
            bluetooth_router(state),
            "POST",
            "/api/bluetooth/clear-cache/pi2",
            "",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
