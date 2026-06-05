use std::sync::Arc;

use coordinator::coordinator::Coordinator;
use coordinator::effects::registry::EffectRegistry;
use coordinator::effects::runner::EffectRunner;
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");
    {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(coordinator::logging::ErrorCaptureLayer)
            .init();
    }
    println!("Starting AI Mesh Coordinator on 0.0.0.0:9000...");

    // Free any stale processes holding our ports before we try to bind.
    #[cfg(target_os = "linux")]
    free_port(9000);
    let http_port: u16 = std::env::var("MESH_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);
    #[cfg(target_os = "linux")]
    free_port(http_port);

    let coord = Coordinator::new_persistent("0.0.0.0:9000", "ai_mesh.db");
    let (_handle, dashboard) = coord.start().await;
    coordinator::logging::bind(dashboard.clone());

    let _mdns = coordinator::mdns::advertise(9000);

    let effects = Arc::new(EffectRegistry::default());
    let runner = EffectRunner::new(coord.registry.clone(), dashboard.clone(), effects.clone());
    tokio::spawn(runner.run());

    tokio::spawn(coordinator::http::start(
        http_port,
        dashboard,
        coord.registry.clone(),
        effects,
    ));

    println!("Coordinator is running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("Shutting down coordinator...");
}

/// If `port` is already in use, find the occupying PID via `ss` and send
/// SIGTERM. Waits up to 1 s for the port to become free. Logs but never
/// panics — if we can't free it the subsequent bind will surface the error.
#[cfg(target_os = "linux")]
fn free_port(port: u16) {
    if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
        return; // port is free
    }
    warn!(port, "port already in use — attempting to free it");

    // Parse the PID from `ss -tlnp sport = :PORT` output.
    // The relevant field looks like: users:(("coordinator",pid=12345,fd=6))
    let ss_out = std::process::Command::new("ss")
        .args(["-tlnp", &format!("sport = :{port}")])
        .output();

    let pid: Option<u32> = ss_out.ok().and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout);
        // Find pid=NNN inside the users:((...)) field.
        text.split("pid=")
            .nth(1)
            .and_then(|s| s.split([',', ')']).next())
            .and_then(|s| s.trim().parse().ok())
    });

    match pid {
        Some(pid) => {
            info!(port, pid, "sending SIGTERM to port occupant");
            // Safety: kill(2) with SIGTERM is always safe to call.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            // Give the process up to 1 s to exit.
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
                    info!(port, "port is now free");
                    return;
                }
            }
            warn!(
                port,
                pid, "process did not exit after SIGTERM — sending SIGKILL"
            );
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        None => {
            warn!(
                port,
                "could not identify port occupant via ss — proceeding anyway"
            );
        }
    }
}
