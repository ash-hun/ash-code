//! ash — unified CLI entry point.

use std::time::Duration;

use std::sync::Arc;

use ash_ipc::pb;
use ash_query::{QueryEngine, Session, SidecarBackend, TurnSink};
use ash_tools::{ToolRegistry, ToolResult};
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
            temperature: _temperature,
        } => {
            // M3: route through QueryEngine so Harness hooks + tool dispatch run.
            let backend = Arc::new(SidecarBackend(client));
            let tools = Arc::new(ToolRegistry::with_builtins());
            let engine = QueryEngine::new(backend, tools);

            // If --provider not passed, leave empty so the sidecar uses its
            // active provider (driven by ASH_LLM_PROVIDER env, which defaults
            // to "anthropic" in docker-compose).
            let provider_name = provider
                .or_else(|| std::env::var("ASH_LLM_PROVIDER").ok())
                .unwrap_or_default();
            let model_name = model.unwrap_or_default();
            let mut session = Session::new("cli-session", provider_name, model_name);
            session.push_user(prompt);

            let mut sink = StdoutSink::default();
            let outcome = engine.run_turn(&mut session, &mut sink).await?;
            if outcome.denied {
                println!("[denied] {}", outcome.denial_reason);
            }
            println!(
                "[engine turns={} stop_reason={}]",
                outcome.turns_taken, outcome.stop_reason
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct StdoutSink;

impl TurnSink for StdoutSink {
    fn on_text(&mut self, text: &str) {
        use std::io::Write;
        print!("{text}");
        std::io::stdout().flush().ok();
    }

    fn on_tool_call(&mut self, name: &str, args: &str) {
        println!("\n[tool_call {name}] {args}");
    }

    fn on_tool_result(&mut self, name: &str, result: &ToolResult) {
        let body = if result.ok {
            &result.stdout
        } else {
            &result.stderr
        };
        let snippet: String = body.chars().take(200).collect();
        println!(
            "[tool_result {name} ok={} exit={}] {}",
            result.ok, result.exit_code, snippet
        );
    }

    fn on_finish(&mut self, stop_reason: &str, input: i32, output: i32) {
        println!("\n[finish stop_reason={stop_reason} in={input} out={output}]");
    }

    fn on_error(&mut self, msg: &str) {
        eprintln!("[error] {msg}");
    }
}
