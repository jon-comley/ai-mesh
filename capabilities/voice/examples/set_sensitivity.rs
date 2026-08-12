//! One-shot: set the "Wake word sensitivity" select entity. Options on
//! this firmware: "Slightly sensitive" (factory default), "Moderately
//! sensitive", "Very sensitive". Key 666792156 discovered via the
//! `entities` example.
//!
//! Usage: VOICE_DEVICE_HOST=<puck-ip>:6053 cargo run -p capability-voice \
//!            --example set_sensitivity [-- "Moderately sensitive"]
use esphome_client::{EspHomeClient, types::SelectCommandRequest};

const WAKE_WORD_SENSITIVITY_KEY: u32 = 666792156;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let address = capability_voice::device_host().expect("set VOICE_DEVICE_HOST=<host:port>");
    let level = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Very sensitive".to_string());
    let mut client = EspHomeClient::builder()
        .address(&address)
        .client_info("ai-mesh-set-sensitivity")
        .password("")
        .connect()
        .await
        .expect("connect failed");
    println!("connected, setting wake word sensitivity to {level:?}...");
    client
        .try_write(SelectCommandRequest {
            key: WAKE_WORD_SENSITIVITY_KEY,
            state: level,
        })
        .await
        .unwrap();
    println!("sent — confirm with the entities example");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}
