//! ash — unified CLI entry point.

use std::time::Duration;

use ash_ipc::pb;
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
        #[arg(long)]
        check_sidecar: bool,
        #[arg(long, default_value = ash_ipc::DEFAULT_SIDECAR_ENDPOINT)]
        sidecar: String,
    },

    /// Temporary LLM smoke-test commands (M2-only; removed in M7).
    Llm {
        #[command(subcommand)]
        action: LlmAction,
        #[arg(long, default_value = ash_ipc::DEFAULT_SIDECAR_ENDPOINT, global = true)]
        sidecar: String,
    },
}

#[derive(Subcommand, Debug)]
enum LlmAction {
    /// Enumerate every provider the sidecar knows about.
    List,
    /// Stream a chat completion using the active (or selected) provider.
    Chat {
        /// The user prompt.
        prompt: String,
        /// Override the provider name (defaults to the sidecar's active provider).
        #[arg(long)]
        provider: Option<String>,
        /// Override the model.
        #[arg(long)]
        model: Option<String>,
        /// Temperature.
        #[arg(long, default_value_t = 0.2)]
        temperature: f32,
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
        Some(Command::Llm { action, sidecar }) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_llm(&sidecar, action))?;
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

async fn run_llm(sidecar: &str, action: LlmAction) -> anyhow::Result<()> {
    let client =
        ash_ipc::SidecarClient::connect(sidecar.to_string(), Duration::from_secs(3)).await?;
    match action {
        LlmAction::List => {
            let providers = client.list_providers().await?;
            if providers.is_empty() {
                println!("(no providers registered)");
                return Ok(());
            }
            println!("{:<12} {:<32} {:<8} {:<8} {}", "name", "default_model", "tools", "vision", "source");
            for p in providers {
                println!(
                    "{:<12} {:<32} {:<8} {:<8} {}",
                    p.name,
                    p.default_model,
                    if p.supports_tools { "yes" } else { "no" },
                    if p.supports_vision { "yes" } else { "no" },
                    p.source
                );
            }
        }
        LlmAction::Chat {
            prompt,
            provider,
            model,
            temperature,
        } => {
            let req = pb::ChatRequest {
                provider: provider.unwrap_or_default(),
                model: model.unwrap_or_default(),
                messages: vec![pb::ChatMessage {
                    role: "user".to_string(),
                    content: prompt,
                    tool_call_id: String::new(),
                }],
                temperature,
                tools: Vec::new(),
            };

            use tokio_stream::StreamExt;

            let mut stream = client.chat_stream(req).await?;
            let mut saw_finish = false;
            while let Some(item) = stream.next().await {
                let delta = item?;
                match delta.kind {
                    Some(pb::chat_delta::Kind::Text(t)) => {
                        use std::io::Write;
                        print!("{t}");
                        std::io::stdout().flush().ok();
                    }
                    Some(pb::chat_delta::Kind::ToolCall(tc)) => {
                        println!("\n[tool_call {}] {}", tc.name, String::from_utf8_lossy(&tc.arguments));
                    }
                    Some(pb::chat_delta::Kind::Finish(f)) => {
                        saw_finish = true;
                        println!(
                            "\n[finish stop_reason={} in={} out={}]",
                            f.stop_reason, f.input_tokens, f.output_tokens
                        );
                    }
                    None => {}
                }
            }
            if !saw_finish {
                println!();
            }
        }
    }
    Ok(())
}
