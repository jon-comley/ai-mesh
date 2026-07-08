//! One-shot: command the device's "Mute" switch entity off.
//!
//! Note: the physical slide switch overrides this (correct privacy design)
//! — if the hardware switch is engaged, the state snaps straight back to
//! muted. Verify with the `entities` example (key 2974103762 on this
//! device, discovered via its entity listing).
//!
//! Usage: VOICE_DEVICE_HOST=10.0.0.14:6053 cargo run -p capability-voice --example unmute
use esphome_client::{EspHomeClient, types::SwitchCommandRequest};

const MUTE_SWITCH_KEY: u32 = 2974103762;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let address = capability_voice::device_host().expect("set VOICE_DEVICE_HOST=<host:port>");
    let mut client = EspHomeClient::builder()
        .address(&address)
        .client_info("ai-mesh-unmute")
        .password("")
        .connect()
        .await
        .expect("connect failed");
    println!("connected, sending unmute command...");
    client
        .try_write(SwitchCommandRequest {
            key: MUTE_SWITCH_KEY,
            state: false,
        })
        .await
        .unwrap();
    println!("sent — confirm with the entities example (hardware switch overrides this)");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}
