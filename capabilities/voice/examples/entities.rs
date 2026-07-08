//! Diagnostic: list every entity the device exposes (name/object_id/key),
//! then stream live state updates for 60s. This is how you find entity
//! keys for the other tools, and how the physically-engaged mute switch
//! was diagnosed when the LED ring gave no hint — query the device's own
//! state instead of guessing from LEDs.
//!
//! Usage: VOICE_DEVICE_HOST=10.0.0.14:6053 cargo run -p capability-voice --example entities
use esphome_client::{
    EspHomeClient,
    types::{EspHomeMessage, ListEntitiesRequest, SubscribeStatesRequest},
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let address = capability_voice::device_host().expect("set VOICE_DEVICE_HOST=<host:port>");

    let mut client = EspHomeClient::builder()
        .address(&address)
        .client_info("ai-mesh-entities")
        // Empty password forces the ConnectRequest handshake the crate
        // otherwise skips — see the comment in capability-voice's lib.rs.
        .password("")
        .connect()
        .await
        .expect("connect failed");
    println!("connected, listing entities...");

    client.try_write(ListEntitiesRequest {}).await.unwrap();

    loop {
        match client.try_read().await {
            Ok(EspHomeMessage::ListEntitiesDoneResponse(_)) => {
                println!("--- entity list done, streaming state changes for 60s ---");
                break;
            }
            Ok(other) => println!("ENTITY: {other:?}"),
            Err(e) => {
                println!("READ ERROR during listing: {e}");
                return;
            }
        }
    }

    client.try_write(SubscribeStatesRequest {}).await.unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, client.try_read()).await {
            Ok(Ok(msg)) => println!("STATE: {msg:?}"),
            Ok(Err(e)) => {
                println!("READ ERROR: {e}");
                break;
            }
            Err(_elapsed) => break,
        }
    }
    println!("done");
}
