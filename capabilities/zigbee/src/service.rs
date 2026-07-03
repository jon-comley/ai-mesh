//! Process-wide shared [`ZigbeeClient`]: one MQTT connection per node,
//! fanned out to every zigbee-backed capability via broadcast
//! subscriptions (`client.subscribe()` hands each caller an independent
//! receiver).
//!
//! Lifecycle is deliberately simple (see plans/multi-domain-home.md):
//! capabilities are compile-time features, so the client's lifetime is the
//! agent process lifetime — built on first use, never torn down. The bounded
//! broadcast channel isolates a slow subscriber (it lags and drops oldest
//! events; it can never block sibling capabilities or the MQTT poll loop).

use crate::{ZigbeeClient, ZigbeeError};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::info;

static CLIENT: OnceCell<Arc<ZigbeeClient>> = OnceCell::const_new();

/// The node's shared Zigbee client, connecting on first call.
///
/// Returns `Ok(None)` in stub mode (`MQTT_HOST` unset) — callers report
/// their domain's offline status themselves. Subsequent callers get the same
/// `Arc`; each should `subscribe()` for its own event stream and filter by
/// device type.
pub async fn shared_client(node_id: &str) -> Result<Option<Arc<ZigbeeClient>>, ZigbeeError> {
    let Ok(host) = std::env::var("MQTT_HOST") else {
        return Ok(None);
    };
    let port: u16 = std::env::var("MQTT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1883);
    let node_id = node_id.to_string();
    let client = CLIENT
        .get_or_try_init(|| async {
            info!(host = %host, port, "zigbee: connecting shared client");
            let (client, _initial_rx) = ZigbeeClient::connect(&host, port, node_id).await?;
            Ok::<_, ZigbeeError>(Arc::new(client))
        })
        .await?;
    Ok(Some(Arc::clone(client)))
}
