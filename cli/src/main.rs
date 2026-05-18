mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mesh")]
#[command(about = "AI Mesh CLI", long_about = None)]
struct Cli {
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let base_addr = "127.0.0.1";

    match cli.command {
        Commands::Status => commands::status::run().await,
        Commands::Nodes => commands::nodes::run().await,
        Commands::Watch => commands::watch::run().await,
        Commands::Info { id } => commands::info::run(id).await,
        Commands::ResetRegistry => commands::reset::run(base_addr).await,
        Commands::Load {
            node_id,
            model_name,
            size_mb,
        } => commands::load::run(node_id, model_name, size_mb).await,
        Commands::Infer { model_name, prompt } => commands::infer::run(model_name, prompt).await,
    }
}
