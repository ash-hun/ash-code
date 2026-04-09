//! Async event loop — crossterm events + turn-execution events + HITL
//! approval requests — all fed into [`crate::app::AppState`].

use std::io;
use std::sync::Arc;

use anyhow::Result;
use ash_query::{QueryEngine, Session, TurnSink};
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
    let mut session = initial_session;

    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let mut crossterm_events = EventStream::new();

    // Initial draw
    terminal.draw(|f| ui::render(f, &state))?;

    loop {
        tokio::select! {
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

    // Normal mode.
    match code {
        KeyCode::Enter => {
            if let Some(prompt) = state.take_prompt() {
                spawn_turn(state, session, prompt, engine.clone(), turn_tx.clone());
            }
        }
        KeyCode::Backspace => state.backspace(),
        KeyCode::PageUp => state.scroll_up(5),
        KeyCode::PageDown => state.scroll_down(5),
        KeyCode::End => state.scroll_bottom(),
        KeyCode::Esc => {
            if state.input.is_empty() && !state.running_turn {
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

    let mut working_session = session.clone();
    tokio::spawn(async move {
        let mut sink = ChannelSink::new(turn_tx.clone());
        let outcome = engine.run_turn(&mut working_session, &mut sink).await;
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
