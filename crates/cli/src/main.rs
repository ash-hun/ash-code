//! ash — unified CLI entry point.
//!
//! M0 scaffold: subcommand surface is declared but implementations
//! land in later milestones (M3 for `run`, M4 for `serve`, M7 for `tui`).

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ash", version, about = "ash-code — containerized coding harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Launch the interactive TUI (M7).
    Tui,
    /// Run the HTTP API + Swagger UI (M4).
    Serve {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        #[arg(long, default_value_t = ash_api::DEFAULT_PORT)]
        port: u16,
    },
    /// Print component versions and exit.
    Doctor,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Tui) => {
            println!("[ash] tui: not yet implemented (scheduled for M7)");
        }
        Some(Command::Serve { host, port }) => {
            println!("[ash] serve: not yet implemented (scheduled for M4) — would bind {host}:{port}");
        }
        Some(Command::Doctor) | None => {
            println!("ash {}", env!("CARGO_PKG_VERSION"));
            println!("  ash-core  {}", ash_core::version());
            println!("  ash-api   port={}", ash_api::DEFAULT_PORT);
            println!("  ash-ipc   sidecar={}", ash_ipc::DEFAULT_SIDECAR_ENDPOINT);
        }
    }
    Ok(())
}
