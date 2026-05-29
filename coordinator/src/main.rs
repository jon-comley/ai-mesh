use std::sync::Arc;

use coordinator::coordinator::Coordinator;
use coordinator::effects::registry::EffectRegistry;
use coordinator::effects::runner::EffectRunner;

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");
    tracing_subscriber::fmt().init();
    println!("Starting AI Mesh Coordinator on 0.0.0.0:9000...");

    let coord = Coordinator::new_persistent("0.0.0.0:9000", "ai_mesh.db");
    let (_handle, dashboard) = coord.start().await;

    let _mdns = coordinator::mdns::advertise(9000);

    let effects = Arc::new(EffectRegistry::default());
    let runner = EffectRunner::new(coord.registry.clone(), dashboard.clone(), effects);
    tokio::spawn(runner.run());

    let http_port: u16 = std::env::var("MESH_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);
    tokio::spawn(coordinator::http::start(
        http_port,
        dashboard,
        coord.registry.clone(),
    ));

    println!("Coordinator is running. Press Ctrl+C to stop.");

    // Keep the process alive forever
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("Shutting down coordinator...");
}
