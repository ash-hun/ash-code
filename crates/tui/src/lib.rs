//! ash-tui — ratatui-based terminal UI (M7).
//!
//! Entry point: [`run`]. Composes the three internal modules:
//!
//! * [`app`] — pure state + logic (unit-testable)
//! * [`backend`] — HITL-aware [`QueryBackend`] decorator
//! * [`event`] — async event loop tying crossterm, turn execution, and HITL together
//! * [`ui`] — ratatui frame rendering

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use ash_ipc::{SidecarClient, DEFAULT_SIDECAR_ENDPOINT};
use ash_query::{QueryBackend, QueryEngine, Session, SidecarBackend};
use ash_tools::ToolRegistry;

pub mod app;
pub mod backend;
pub mod event;
pub mod ui;

pub use app::{AppState, ChatLine, Mode, PaletteEntry, PaletteKind};
pub use backend::TuiBackend;

pub const CRATE_NAME: &str = "ash-tui";

/// Configuration passed in from `ash tui`.
#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub sidecar_endpoint: String,
    pub provider: String,
    pub model: String,
    pub auto_approve: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            sidecar_endpoint: DEFAULT_SIDECAR_ENDPOINT.to_string(),
            provider: "anthropic".to_string(),
            model: String::new(),
            auto_approve: std::env::var("ASH_TUI_AUTO_APPROVE").ok().as_deref() == Some("1"),
        }
    }
}

/// Main entry point. Assumes a Tokio runtime is already running.
pub async fn run(config: TuiConfig) -> Result<()> {
    let sidecar = connect_with_retry(&config.sidecar_endpoint, 10, Duration::from_millis(300))
        .await
        .with_context(|| format!("could not reach sidecar at {}", config.sidecar_endpoint))?;

    // Load palette entries (commands + skills) from sidecar.
    let palette_entries = load_palette_entries(&sidecar).await;

    let sidecar_for_palette = sidecar.clone();
    let inner_backend: Arc<dyn QueryBackend> = Arc::new(SidecarBackend(sidecar));

    // Approval channel: TuiBackend → UI main loop
    let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let tui_backend = Arc::new(TuiBackend::new(
        inner_backend,
        approval_tx,
        config.auto_approve,
    ));

    let tools = Arc::new(ToolRegistry::with_builtins());
    let engine = Arc::new(QueryEngine::new(tui_backend.clone(), tools));

    let session = Session::new(
        format!("tui-{}", uuid::Uuid::new_v4().simple()),
        &config.provider,
        &config.model,
    );

    event::run_event_loop(engine, session, config, approval_rx, palette_entries, sidecar_for_palette).await
}

async fn load_palette_entries(sidecar: &ash_ipc::SidecarClient) -> Vec<PaletteEntry> {
    let mut entries = Vec::new();

    // Load commands
    match sidecar.list_commands().await {
        Ok(cmds) => {
            for c in cmds {
                entries.push(PaletteEntry {
                    kind: PaletteKind::Command,
                    name: c.name,
                    description: c.description,
                });
            }
        }
        Err(e) => tracing::warn!("failed to load commands for palette: {e:#}"),
    }

    // Load skills
    match sidecar.list_skills().await {
        Ok(skills) => {
            for s in skills {
                entries.push(PaletteEntry {
                    kind: PaletteKind::Skill,
                    name: s.name,
                    description: s.description,
                });
            }
        }
        Err(e) => tracing::warn!("failed to load skills for palette: {e:#}"),
    }

    entries
}

async fn connect_with_retry(
    endpoint: &str,
    attempts: usize,
    delay: Duration,
) -> Result<SidecarClient> {
    let mut last: Option<anyhow::Error> = None;
    for i in 0..attempts {
        match SidecarClient::connect(endpoint.to_string(), Duration::from_secs(2)).await {
            Ok(c) => return Ok(c),
            Err(err) => {
                tracing::warn!("sidecar connect {}/{} failed: {err:#}", i + 1, attempts);
                last = Some(err);
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("sidecar unreachable")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_stable() {
        assert_eq!(super::CRATE_NAME, "ash-tui");
    }
}
