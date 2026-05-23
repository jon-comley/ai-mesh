mod commands;
mod connection;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mesh")]
#[command(about = "AI Mesh CLI", long_about = None)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:9000")]
    coordinator: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Status,
    Nodes,
    Watch,
    Info {
        id: String,
    },
    /// Print the node UUID for a given IP address. Exits non-zero if not found.
    FindNode {
        ip: String,
    },
    /// Live table: wait until all listed compute nodes have a Ready model.
    /// Exits 0 when all Ready, 1 on timeout or abort (q / Ctrl+C).
    WaitReady {
        /// IP addresses of the compute nodes to watch.
        #[arg(required = true)]
        ips: Vec<String>,
        /// Timeout in seconds before giving up.
        #[arg(long, default_value = "600")]
        timeout: u64,
    },
    ResetRegistry,
    Load {
        /// Target node. Omit to let the coordinator pick the best-fit node automatically.
        #[arg(long)]
        node_id: Option<String>,
        model_name: String,
        size_mb: u64,
    },
    Infer {
        model_name: String,
        prompt: String,
    },
    /// Natural language intent — the LLM decides whether to answer or call a tool.
    Intent {
        /// The text to send (e.g. "dim the lights" or "explain TCP keepalive").
        text: String,
        /// Optional model to use. Omit to let the coordinator pick the best available.
        #[arg(long)]
        model: Option<String>,
    },
    Unload {
        node_id: String,
        model_name: String,
    },
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");

    let cli = Cli::parse();
    let addr = cli.coordinator.as_str();

    match cli.command {
        Commands::Status => commands::status::run(addr).await,
        Commands::Nodes => commands::nodes::run(addr).await,
        Commands::Watch => commands::watch::run(addr).await,
        Commands::Info { id } => commands::info::run(addr, id).await,
        Commands::FindNode { ip } => commands::find_node::run(addr, ip).await,
        Commands::WaitReady { ips, timeout } => {
            let ok = commands::wait_ready::run(addr, ips, timeout).await;
            if ok {
                println!(">>> All models Ready.");
            } else {
                eprintln!(">>> Timed out or aborted waiting for Ready state.");
                std::process::exit(1);
            }
        }
        Commands::ResetRegistry => commands::reset::run(addr).await,
        Commands::Load {
            node_id,
            model_name,
            size_mb,
        } => commands::load::run(addr, node_id, model_name, size_mb).await,
        Commands::Infer { model_name, prompt } => {
            commands::infer::run(addr, model_name, prompt).await
        }
        Commands::Intent { text, model } => commands::intent::run(addr, text, model).await,
        Commands::Unload {
            node_id,
            model_name,
        } => commands::unload::run(addr, node_id, model_name).await,
    }
}
