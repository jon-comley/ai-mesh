//! Transcribe a previously captured raw PCM clip (as saved by the voice
//! capability's crawl-phase `save_clip`) without going through the live
//! ESPHome connection — for validating whisper.cpp accuracy/latency against
//! real captured audio before trusting it in the wired-in wake-word path.
//!
//! Usage: cargo run -p capability-voice --example transcribe -- <path-to-clip.raw>
use std::env;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let path = env::args()
        .nth(1)
        .expect("usage: transcribe <path-to-clip.raw>");
    let pcm = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    println!("loaded {} bytes from {path}", pcm.len());

    capability_voice::stt::ensure_server_running()
        .await
        .expect("failed to start whisper-server");

    let started = std::time::Instant::now();
    let text = capability_voice::stt::transcribe(&pcm)
        .await
        .expect("transcription failed");
    println!("transcript ({:?}): {text}", started.elapsed());
}
