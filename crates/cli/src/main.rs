//! ash — unified CLI entry point.

use std::time::Duration;

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
    /// Print component versions, optionally probing the Python sidecar.
    Doctor {
        /// Attempt a Health.Ping to the ashpy gRPC sidecar.
        #[arg(long)]
        check_sidecar: bool,

        /// Sidecar endpoint override.
        #[arg(long, default_value = ash_ipc::DEFAULT_SIDECAR_ENDPOINT)]
        sidecar: String,
    },
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
            println!(
                "[ash] serve: not yet implemented (scheduled for M4) — would bind {host}:{port}"
            );
        }
        Some(Command::Doctor {
            check_sidecar,
            sidecar,
        }) => {
            print_versions(&sidecar);
            if check_sidecar {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let outcome = rt.block_on(probe_sidecar(&sidecar));
                match outcome {
                    Ok(msg) => println!("  sidecar: OK — {msg}"),
                    Err(err) => {
                        println!("  sidecar: FAIL — {err:#}");
                        std::process::exit(2);
                    }
                }
            }
        }
        None => {
            print_versions(ash_ipc::DEFAULT_SIDECAR_ENDPOINT);
        }
    }
    Ok(())
}

fn print_versions(sidecar: &str) {
    println!("ash {}", env!("CARGO_PKG_VERSION"));
    println!("  ash-core  {}", ash_core::version());
    println!("  ash-api   port={}", ash_api::DEFAULT_PORT);
    println!("  ash-ipc   sidecar={sidecar}");
}

async fn probe_sidecar(endpoint: &str) -> anyhow::Result<String> {
    let client = ash_ipc::SidecarClient::connect(endpoint.to_string(), Duration::from_secs(3))
        .await?;
    let started = std::time::Instant::now();
    let resp = client.ping().await?;
    let elapsed = started.elapsed();
    Ok(format!(
        "{} api={} features={} ({:.1} ms)",
        resp.server,
        resp.api_version,
        resp.features.len(),
        elapsed.as_secs_f64() * 1000.0
    ))
}
