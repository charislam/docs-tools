use anyhow::Result;
use clap::{Parser, Subcommand};
use log::info;

mod commands;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Enable trace level logging
    #[arg(long)]
    trace: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check links recursively starting from a given URL
    LinkCheck {
        /// The base URL of the website (e.g., https://example.com)
        #[arg(short, long = "base")]
        base_url: String,

        /// The starting URL to begin checking from (defaults to base_url if not provided)
        #[arg(short, long = "start")]
        start_url: Option<String>,

        /// Only check links that match the base URL
        #[arg(long)]
        internal_only: bool,

        /// Use a human-like User-Agent header for requests
        #[arg(long)]
        human_agent: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), ()> {
    match run().await {
        Err(_) => Err(()),
        Ok(_) => Ok(()),
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    let env =
        env_logger::Env::default().filter_or("RUST_LOG", if cli.trace { "trace" } else { "warn" });
    env_logger::Builder::from_env(env).init();

    info!("Starting docs-tools");

    match cli.command {
        Commands::LinkCheck {
            base_url,
            start_url,
            internal_only,
            human_agent,
        } => {
            let start_url = start_url.unwrap_or_else(|| base_url.clone());
            commands::link_check::LinkChecker::new(&base_url, internal_only, human_agent)?
                .check(&start_url)
                .await
        }
    }
}
