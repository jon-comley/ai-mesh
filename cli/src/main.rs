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
    Info { id: String },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => commands::status::run().await,
        Commands::Nodes => commands::nodes::run().await,
        Commands::Watch => commands::watch::run().await,
        Commands::Info { id } => commands::info::run(id).await,
    }
}
