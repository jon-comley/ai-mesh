use coordinator::coordinator::Coordinator;

#[tokio::main]
async fn main() {
    println!("Starting AI Mesh Coordinator on 0.0.0.0:9000...");

    let coord = Coordinator::new_persistent("0.0.0.0:9000", "ai_mesh.db");
    let _handle = coord.start().await;

    println!("Coordinator is running. Press Ctrl+C to stop.");

    // Keep the process alive forever
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("Shutting down coordinator...");
}
