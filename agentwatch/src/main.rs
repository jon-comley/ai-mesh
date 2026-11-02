//! `agentwatch`: the agent watch page on lenovo1 (plans/agent-watch.md).
//!
//! `AGENTWATCH_DIR` (default `~/.ai-mesh/agents`) holds `agents.toml`,
//! `agents.db`, `keys/` and `repos/`. `AGENTWATCH_ADDR` (default `0.0.0.0:80`)
//! is where the page is served.

mod server;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentwatch::config::Config;
use server::App;
use server::store::Store;

/// How often every worker is read again.
const POLL: Duration = Duration::from_secs(15 * 60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let dir = std::env::var_os("AGENTWATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".ai-mesh/agents")
        });
    std::fs::create_dir_all(dir.join("keys"))?;
    let config_path = dir.join("agents.toml");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(text) => Config::parse(&text)?,
        Err(_) => Config::default(),
    };
    let store = Store::open(&dir.join("agents.db"))?;
    let app = Arc::new(App::new(dir, config, store));

    let poller = app.clone();
    tokio::spawn(async move {
        loop {
            poller.refresh_all().await;
            tokio::time::sleep(POLL).await;
        }
    });

    let addr = std::env::var("AGENTWATCH_ADDR").unwrap_or_else(|_| "0.0.0.0:80".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("agent watch on http://{addr}/");
    axum::serve(listener, server::web::router(app)).await?;
    Ok(())
}
