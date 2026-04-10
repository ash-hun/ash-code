//! ratatui frame rendering.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{AppState, ChatLine, Mode};

/// Top-level render function called once per frame.
pub fn render(f: &mut Frame, state: &AppState) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),   // header (banner + tips + recent activity)
            Constraint::Min(5),      // chat body
            Constraint::Length(3),   // input
            Constraint::Length(1),   // status bar
        ])
        .split(area);

    render_header(f, chunks[0], state);
    render_chat(f, chunks[1], state);
    render_input(f, chunks[2], state);
    render_status(f, chunks[3], state);

    // Modal overlay for approvals.
    if matches!(state.mode, Mode::Approval(_)) {
        render_approval_modal(f, area, state);
    }
}

// ---------------------------------------------------------------------------
// Header — claurst-style banner + tips + recent activity (Q3=b)
// ---------------------------------------------------------------------------

fn render_header(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" ash-code v{} ", env!("CARGO_PKG_VERSION")),
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "Welcome back!",
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Start with small features or bug fixes, let ash-code propose a plan, and verify."),
        ]),
        Line::from(vec![
            Span::styled(
                "Tips for getting started",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::raw("  · Type a prompt below and press Enter to send.")),
        Line::from(Span::raw("  · bash commands trigger an approval dialog — [1] Yes, [2] No, [3] feedback.")),
        Line::from(vec![
            Span::styled(
                "Recent activity",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                if state.chat.is_empty() {
                    "No recent activity".to_string()
                } else {
                    format!("{} message(s) this session", state.chat.len())
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// Chat body
// ---------------------------------------------------------------------------

fn render_chat(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title(" chat ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::with_capacity(state.chat.len() * 2);
    for entry in &state.chat {
        match entry {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "You › ",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.clone()),
                ]));
            }
            ChatLine::Assistant(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "ash › ",
                        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                    ),
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::White),
                    )));
                }
                if text.is_empty() {
                    lines.push(Line::from(Span::raw("  ")));
                }
            }
            ChatLine::ToolCall { name, args } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("⚙  tool_call · {name} "),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(args.clone(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatLine::ToolResult { name, ok, body } => {
                let color = if *ok { Color::Green } else { Color::Red };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  ↳ {name} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if *ok { "ok" } else { "fail" },
                        Style::default().fg(color),
                    ),
                ]));
                for line in body.lines().take(8) {
                    lines.push(Line::from(Span::styled(
                        format!("    {line}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            ChatLine::Finish { stop_reason, input_tokens, output_tokens } => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  [finish stop_reason={stop_reason} in={input_tokens} out={output_tokens}]"
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
            }
            ChatLine::Error(msg) => {
                lines.push(Line::from(Span::styled(
                    format!("  ✖ {msg}"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )));
            }
            ChatLine::Denial { tool, reason } => {
                lines.push(Line::from(Span::styled(
                    format!("  ⊘ {tool} denied: {reason}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }

    let scroll_total: u16 = lines.len().min(u16::MAX as usize) as u16;
    let view_h = inner.height;
    let offset = scroll_total.saturating_sub(view_h).saturating_sub(state.scroll as u16);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// Input line
// ---------------------------------------------------------------------------

fn render_input(f: &mut Frame, area: Rect, state: &AppState) {
    let disabled = state.running_turn || matches!(state.mode, Mode::Approval(_));
    let prompt_style = if disabled {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(if disabled { " input (locked) " } else { " input " })
        .border_style(if disabled {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Cyan)
        });
    let content = format!(
        "> {}{}",
        state.input,
        if disabled { "" } else { "▏" }
    );
    let paragraph = Paragraph::new(content).style(prompt_style).block(block);
    f.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let model_display = if state.model.is_empty() {
        "(default)".to_string()
    } else {
        state.model.clone()
    };
    let text = format!(
        " {} · {} · session={} · turns={} · Ctrl-C quit ",
        state.provider, model_display, state.session_id, state.turns_taken
    );
    let paragraph = Paragraph::new(text).style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Approval modal (HITL)
// ---------------------------------------------------------------------------

fn render_approval_modal(f: &mut Frame, area: Rect, state: &AppState) {
    let approval = match &state.mode {
        Mode::Approval(a) => a,
        _ => return,
    };

    let modal_area = centered_rect(70, 50, area);
    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Allow this {} command? ", approval.request.tool_name),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),   // command preview
            Constraint::Length(5),   // options
            Constraint::Min(1),      // custom input (when editing)
        ])
        .split(inner);

    // command preview
    let preview = Paragraph::new(approval.request.arguments.clone())
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title(" command "));
    f.render_widget(preview, chunks[0]);

    // options
    let mut option_lines: Vec<Line> = Vec::new();
    let options = [
        "1  Yes",
        "2  No",
        "3  Tell ash-code what to do instead",
    ];
    for (i, label) in options.iter().enumerate() {
        let is_selected = i == approval.selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let marker = if is_selected { "▶ " } else { "  " };
        option_lines.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
    }
    let options_p = Paragraph::new(option_lines);
    f.render_widget(options_p, chunks[1]);

    // custom input box
    if approval.editing_custom {
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" feedback (Enter to submit, Esc to cancel) ")
            .border_style(Style::default().fg(Color::Cyan));
        let content = format!("{}▏", approval.custom_input);
        let input_p = Paragraph::new(content).block(input_block);
        f.render_widget(input_p, chunks[2]);
    } else {
        let hint = Paragraph::new("↑↓ select · 1/2/3 jump · Enter confirm · Esc cancel")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, chunks[2]);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn state_with_messages() -> AppState {
        let mut s = AppState::new(
            "anthropic".to_string(),
            "claude-opus-4-5".to_string(),
            "sess-snap".to_string(),
        );
        s.push_user("hello".to_string());
        s.push_text_delta("hi there");
        s
    }

    #[test]
    fn renders_without_panic_and_shows_meta() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = state_with_messages();
        terminal.draw(|f| render(f, &state)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let text: String = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("ash-code"));
        assert!(text.contains("anthropic"));
        assert!(text.contains("claude-opus-4-5"));
        assert!(text.contains("hello"));
        assert!(text.contains("hi there"));
    }

    #[test]
    fn renders_approval_modal_without_panic() {
        use crate::backend::PendingApproval;
        use tokio::sync::oneshot;

        let mut s = AppState::new(
            "anthropic".to_string(),
            "claude-opus-4-5".to_string(),
            "sess".to_string(),
        );
        let (tx, _rx) = oneshot::channel();
        s.enter_approval(
            PendingApproval {
                tool_name: "bash".to_string(),
                arguments: "{\"command\":\"rm -rf /tmp/foo\"}".to_string(),
            },
            tx,
        );
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &s)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("Allow this bash command"));
        assert!(text.contains("Tell ash-code"));
    }
}
