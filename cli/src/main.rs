mod commands;

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
    ResetRegistry,
    Load {
        node_id: String,
        model_name: String,
        size_mb: u64,
    },
    Infer {
        model_name: String,
        prompt: String,
    },
    Unload {
        node_id: String,
        model_name: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let addr = cli.coordinator.as_str();

    match cli.command {
        Commands::Status => commands::status::run(addr).await,
        Commands::Nodes => commands::nodes::run(addr).await,
        Commands::Watch => commands::watch::run(addr).await,
        Commands::Info { id } => commands::info::run(addr, id).await,
        Commands::ResetRegistry => commands::reset::run(addr).await,
        Commands::Load {
            node_id,
            model_name,
            size_mb,
        } => commands::load::run(addr, node_id, model_name, size_mb).await,
        Commands::Infer { model_name, prompt } => {
            commands::infer::run(addr, model_name, prompt).await
        }
        Commands::Unload {
            node_id,
            model_name,
        } => commands::unload::run(addr, node_id, model_name).await,
    }
}
