//! Async event loop — crossterm events + turn-execution events + HITL
//! approval requests — all fed into [`crate::app::AppState`].

use std::io;
use std::sync::Arc;

use anyhow::Result;
use ash_ipc::SidecarClient;
use ash_query::{CancellationToken, QueryEngine, Session, TurnSink};
use ash_tools::ToolResult as RsToolResult;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CtEvent, EventStream, KeyCode,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{AppState, Mode};
use crate::backend::ApprovalEnvelope;
use crate::ui;
use crate::TuiConfig;

/// Events produced by the background turn-execution task.
#[derive(Debug)]
pub enum TurnEvent {
    Text(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, result: RsToolResult },
    Finish { stop_reason: String, input_tokens: i32, output_tokens: i32 },
    Error(String),
    /// The background task has finished; carries the updated session back.
    Done(Result<(Session, TurnOutcomeSummary), String>),
}

#[derive(Debug, Clone)]
pub struct TurnOutcomeSummary {
    pub stop_reason: String,
    pub turns_taken: usize,
    pub denied: bool,
    pub denial_reason: String,
}

/// `TurnSink` implementation that forwards every callback into an
/// unbounded mpsc channel read by the main event loop.
pub struct ChannelSink {
    tx: mpsc::UnboundedSender<TurnEvent>,
}

impl ChannelSink {
    pub fn new(tx: mpsc::UnboundedSender<TurnEvent>) -> Self {
        Self { tx }
    }

    fn send(&self, ev: TurnEvent) {
        let _ = self.tx.send(ev);
    }
}

impl TurnSink for ChannelSink {
    fn on_text(&mut self, text: &str) {
        self.send(TurnEvent::Text(text.to_string()));
    }
    fn on_tool_call(&mut self, name: &str, args: &str) {
        self.send(TurnEvent::ToolCall {
            name: name.to_string(),
            args: args.to_string(),
        });
    }
    fn on_tool_result(&mut self, name: &str, result: &RsToolResult) {
        self.send(TurnEvent::ToolResult {
            name: name.to_string(),
            result: result.clone(),
        });
    }
    fn on_finish(&mut self, stop_reason: &str, input_tokens: i32, output_tokens: i32) {
        self.send(TurnEvent::Finish {
            stop_reason: stop_reason.to_string(),
            input_tokens,
            output_tokens,
        });
    }
    fn on_error(&mut self, message: &str) {
        self.send(TurnEvent::Error(message.to_string()));
    }
}

// ---------------------------------------------------------------------------

pub async fn run_event_loop(
    engine: Arc<QueryEngine>,
    initial_session: Session,
    config: TuiConfig,
    mut approval_rx: mpsc::UnboundedReceiver<ApprovalEnvelope>,
    palette_entries: Vec<crate::app::PaletteEntry>,
    sidecar: SidecarClient,
) -> Result<()> {
    // --- terminal setup ---
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Ensure we always tear down on exit.
    let _guard = TerminalGuard;

    let mut state = AppState::new(
        config.provider.clone(),
        config.model.clone(),
        initial_session.id.clone(),
    );
    state.palette_entries = palette_entries;
    let mut session = initial_session;

    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let mut crossterm_events = EventStream::new();

    // Tick interval for spinner animation (100ms)
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));

    // Initial draw
    terminal.draw(|f| ui::render(f, &state))?;

    loop {
        tokio::select! {
            _ = tick_interval.tick() => {
                if state.running_turn {
                    state.tick = state.tick.wrapping_add(1);
                    terminal.draw(|f| ui::render(f, &state))?;
                }
                continue;
            }
            Some(ct_event) = crossterm_events.next() => {
                if let Ok(CtEvent::Key(key)) = ct_event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_key(
                        &mut state,
                        &mut session,
                        key.code,
                        key.modifiers,
                        &engine,
                        &turn_tx,
                        &sidecar,
                    );
                }
            }
            Some(turn_event) = turn_rx.recv() => {
                match turn_event {
                    TurnEvent::Text(t) => state.push_text_delta(&t),
                    TurnEvent::ToolCall { name, args } => state.push_tool_call(&name, &args),
                    TurnEvent::ToolResult { name, result } => state.push_tool_result(&name, &result),
                    TurnEvent::Finish { stop_reason, input_tokens, output_tokens } => {
                        state.push_finish(&stop_reason, input_tokens, output_tokens);
                    }
                    TurnEvent::Error(msg) => state.push_error(msg),
                    TurnEvent::Done(result) => {
                        state.running_turn = false;
                        state.current_cancel = None;
                        match result {
                            Ok((new_session, summary)) => {
                                session = new_session;
                                state.turns_taken += summary.turns_taken;
                                if summary.denied {
                                    state.push_denial(
                                        "harness".to_string(),
                                        summary.denial_reason,
                                    );
                                }
                            }
                            Err(err) => state.push_error(format!("engine error: {err}")),
                        }
                    }
                }
            }
            Some(envelope) = approval_rx.recv() => {
                state.enter_approval(envelope.request, envelope.responder);
            }
        }

        terminal.draw(|f| ui::render(f, &state))?;
        if state.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_key(
    state: &mut AppState,
    session: &mut Session,
    code: KeyCode,
    modifiers: KeyModifiers,
    engine: &Arc<QueryEngine>,
    turn_tx: &mpsc::UnboundedSender<TurnEvent>,
    sidecar: &SidecarClient,
) {
    // Global: Ctrl-C always quits.
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        state.should_quit = true;
        return;
    }

    // Approval mode takes priority.
    if matches!(state.mode, Mode::Approval(_)) {
        match code {
            KeyCode::Char('1') => {
                if let Mode::Approval(a) = &mut state.mode {
                    if !a.editing_custom {
                        a.selected = 0;
                    }
                }
                state.approval_confirm();
            }
            KeyCode::Char('2') => {
                if let Mode::Approval(a) = &mut state.mode {
                    if !a.editing_custom {
                        a.selected = 1;
                    }
                }
                state.approval_confirm();
            }
            KeyCode::Char('3') => {
                if let Mode::Approval(a) = &mut state.mode {
                    if !a.editing_custom {
                        a.selected = 2;
                    }
                }
                state.approval_confirm();
            }
            KeyCode::Up => state.approval_select_prev(),
            KeyCode::Down => state.approval_select_next(),
            KeyCode::Enter => state.approval_confirm(),
            KeyCode::Esc => state.approval_cancel(),
            KeyCode::Backspace => state.backspace(),
            KeyCode::Char(c) => state.push_char(c),
            _ => {}
        }
        return;
    }

    // Slash palette mode.
    if matches!(state.mode, Mode::SlashPalette(_)) {
        match code {
            KeyCode::Enter => {
                state.palette_confirm();
            }
            KeyCode::Esc => {
                state.palette_dismiss();
                state.input.clear();
            }
            KeyCode::Up => state.palette_prev(),
            KeyCode::Down | KeyCode::Tab => state.palette_next(),
            KeyCode::Backspace => state.backspace(),
            KeyCode::Char(c) => state.push_char(c),
            _ => {}
        }
        return;
    }

    // Normal mode.
    match code {
        KeyCode::Enter => {
            if let Some(prompt) = state.take_prompt() {
                if prompt.starts_with('/') {
                    // Parse: /<name> <args...>
                    let without_slash = &prompt[1..];
                    let (name, rest) = match without_slash.split_once(' ') {
                        Some((n, r)) => (n.to_string(), r.trim().to_string()),
                        None => (without_slash.to_string(), String::new()),
                    };

                    // Find the entry in palette to determine kind
                    let entry = state.palette_entries.iter().find(|e| e.name == name);
                    if let Some(entry) = entry {
                        let kind = entry.kind.clone();
                        spawn_slash_turn(
                            state, session, name, rest, kind,
                            engine.clone(), turn_tx.clone(), sidecar.clone(),
                        );
                    } else {
                        // Unknown slash command — send as plain prompt
                        spawn_turn(state, session, prompt, engine.clone(), turn_tx.clone());
                    }
                } else {
                    spawn_turn(state, session, prompt, engine.clone(), turn_tx.clone());
                }
            }
        }
        KeyCode::Backspace => state.backspace(),
        KeyCode::PageUp => state.scroll_up(5),
        KeyCode::PageDown => state.scroll_down(5),
        KeyCode::End => state.scroll_bottom(),
        KeyCode::Esc => {
            // M8: priority shifts —
            //   running_turn  → cancel current turn (process stays alive)
            //   input empty   → quit
            //   input filled  → no-op
            if state.running_turn {
                state.request_cancel_turn();
            } else if state.input.is_empty() {
                state.should_quit = true;
            }
        }
        KeyCode::Char(c) => state.push_char(c),
        _ => {}
    }
}

fn spawn_turn(
    state: &mut AppState,
    session: &mut Session,
    prompt: String,
    engine: Arc<QueryEngine>,
    turn_tx: mpsc::UnboundedSender<TurnEvent>,
) {
    state.push_user(prompt.clone());
    session.push_user(&prompt);
    state.running_turn = true;
    let cancel = CancellationToken::new();
    state.current_cancel = Some(cancel.clone());

    let mut working_session = session.clone();
    tokio::spawn(async move {
        let mut sink = ChannelSink::new(turn_tx.clone());
        let outcome = engine.run_turn(&mut working_session, &mut sink, cancel).await;
        let msg = match outcome {
            Ok(outcome) => Ok((
                working_session,
                TurnOutcomeSummary {
                    stop_reason: outcome.stop_reason,
                    turns_taken: outcome.turns_taken,
                    denied: outcome.denied,
                    denial_reason: outcome.denial_reason,
                },
            )),
            Err(err) => Err(format!("{err:#}")),
        };
        let _ = turn_tx.send(TurnEvent::Done(msg));
    });
}

/// Resolve a slash command/skill via the sidecar, then run the rendered
/// prompt as a normal turn.
fn spawn_slash_turn(
    state: &mut AppState,
    session: &mut Session,
    name: String,
    args_str: String,
    kind: crate::app::PaletteKind,
    engine: Arc<QueryEngine>,
    turn_tx: mpsc::UnboundedSender<TurnEvent>,
    sidecar: SidecarClient,
) {
    let display = if args_str.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {args_str}")
    };
    state.push_user(display);
    state.running_turn = true;
    let cancel = CancellationToken::new();
    state.current_cancel = Some(cancel.clone());

    // Build simple args map: the rest of the input goes as "input" key
    let mut args = std::collections::HashMap::new();
    if !args_str.is_empty() {
        // Put the raw text as multiple possible arg keys for flexibility
        args.insert("input".to_string(), args_str.clone());
        args.insert("path".to_string(), args_str.clone());
        args.insert("focus".to_string(), args_str.clone());
        args.insert("target".to_string(), args_str.clone());
        args.insert("error".to_string(), args_str.clone());
    }

    let mut working_session = session.clone();
    tokio::spawn(async move {
        // Resolve the slash command to a rendered prompt via sidecar
        let rendered = match kind {
            crate::app::PaletteKind::Skill => {
                match sidecar.invoke_skill(&name, args).await {
                    Ok(resp) => resp.rendered_prompt,
                    Err(e) => {
                        let _ = turn_tx.send(TurnEvent::Error(
                            format!("skill '/{name}' resolve failed: {e:#}"),
                        ));
                        let _ = turn_tx.send(TurnEvent::Done(Err(format!("{e:#}"))));
                        return;
                    }
                }
            }
            crate::app::PaletteKind::Command => {
                match sidecar.render_command(&name, args).await {
                    Ok(resp) => resp.rendered_prompt,
                    Err(e) => {
                        let _ = turn_tx.send(TurnEvent::Error(
                            format!("command '/{name}' resolve failed: {e:#}"),
                        ));
                        let _ = turn_tx.send(TurnEvent::Done(Err(format!("{e:#}"))));
                        return;
                    }
                }
            }
        };

        // Push the rendered prompt as a user message and run the turn
        working_session.push_user(&rendered);
        let mut sink = ChannelSink::new(turn_tx.clone());
        let outcome = engine.run_turn(&mut working_session, &mut sink, cancel).await;
        let msg = match outcome {
            Ok(outcome) => Ok((
                working_session,
                TurnOutcomeSummary {
                    stop_reason: outcome.stop_reason,
                    turns_taken: outcome.turns_taken,
                    denied: outcome.denied,
                    denial_reason: outcome.denial_reason,
                },
            )),
            Err(err) => Err(format!("{err:#}")),
        };
        let _ = turn_tx.send(TurnEvent::Done(msg));
    });
}

// ---------------------------------------------------------------------------
// Terminal RAII guard: always restore state even on panic.
// ---------------------------------------------------------------------------

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    }
}
