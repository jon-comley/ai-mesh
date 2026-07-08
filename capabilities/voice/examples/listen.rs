//! Run the real voice capability against the device: connect, subscribe,
//! and save a clip on every wake-word trigger.
//!
//! Usage: VOICE_DEVICE_HOST=10.0.0.14:6053 cargo run -p capability-voice --example listen
use capability_core::Capability;
use capability_voice::VoiceCapability;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    // The capability itself silently stubs out without this — fail loudly
    // here instead of "listening" to nothing.
    capability_voice::device_host().expect("set VOICE_DEVICE_HOST=<host:port>");
    let cap = VoiceCapability::new("test-node");
    let (tx, _rx) = mpsc::channel(8);
    cap.start(tx).await.expect("start failed");
    println!("Listening until killed — say the wake word whenever...");
    // Runs until externally terminated. A fixed exit here drops the API
    // connection, which the device renders as its red "no Home Assistant
    // connection" twinkle — exactly the false alarm that burned a night.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
