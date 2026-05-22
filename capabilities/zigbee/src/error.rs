use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZigbeeError {
    #[error("MQTT client error: {0}")]
    Client(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
