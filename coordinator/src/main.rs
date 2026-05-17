use coordinator::coordinator::Coordinator;

#[tokio::main]
async fn main() {
    println!("Starting AI Mesh Coordinator on 127.0.0.1:9000...");

    let coord = Coordinator::new("127.0.0.1:9000");
    let _handle = coord.start().await;

    println!("Coordinator is running. Press Ctrl+C to stop.");

    // Keep the process alive forever
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("Shutting down coordinator...");
}
