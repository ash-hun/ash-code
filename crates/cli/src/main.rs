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
    /// Launch the interactive TUI.
    Tui {
        #[arg(long, default_value = ash_ipc::DEFAULT_SIDECAR_ENDPOINT)]
        sidecar: String,
        #[arg(long, env = "ASH_LLM_PROVIDER", default_value = "anthropic")]
        provider: String,
        #[arg(long, env = "ASH_LLM_MODEL", default_value = "")]
        model: String,
    },

    /// Run the Rust `QueryHost` gRPC server that the Python FastAPI layer
    /// calls from its `/v1/chat` handler. See docs/comparison_api_structure.md.
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = ash_api::DEFAULT_QUERY_HOST_PORT)]
        port: u16,
        /// Python sidecar gRPC endpoint (LlmProvider, Harness, …).
        #[arg(long, default_value = ash_ipc::DEFAULT_SIDECAR_ENDPOINT)]
        sidecar: String,
        /// Default provider name when requests leave it empty.
        #[arg(long, env = "ASH_LLM_PROVIDER", default_value = "anthropic")]
        provider: String,
        /// Default model name when requests leave it empty.
        #[arg(long, env = "ASH_LLM_MODEL", default_value = "")]
        model: String,
    },

    /// Print component versions, optionally probing the Python sidecar.
    Doctor {
        #[arg(long)]
        check_sidecar: bool,
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
        Some(Command::Tui {
            sidecar,
            provider,
            model,
        }) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let config = ash_tui::TuiConfig {
                sidecar_endpoint: sidecar,
                provider,
                model,
                auto_approve: std::env::var("ASH_TUI_AUTO_APPROVE").ok().as_deref()
                    == Some("1"),
            };
            rt.block_on(ash_tui::run(config))?;
        }
        Some(Command::Serve {
            host,
            port,
            sidecar,
            provider,
            model,
        }) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(ash_api::serve(host, port, sidecar, provider, model))?;
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
    let client =
        ash_ipc::SidecarClient::connect(endpoint.to_string(), Duration::from_secs(3)).await?;
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
