//! ratatui frame rendering — Claude Code-inspired minimal UI.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{AppState, ChatLine, Mode, PaletteKind};

/// Top-level render function called once per frame.
pub fn render(f: &mut Frame, state: &AppState) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),   // header (two-column)
            Constraint::Min(5),      // chat body
            Constraint::Length(2),   // input
            Constraint::Length(1),   // status bar
        ])
        .split(area);

    render_header(f, chunks[0], state);
    render_chat(f, chunks[1], state);
    render_input(f, chunks[2], state);
    render_status(f, chunks[3], state);

    // Slash palette (rendered above input area).
    if matches!(state.mode, Mode::SlashPalette(_)) {
        render_slash_palette(f, chunks[1], state);
    }

    // Modal overlay for approvals.
    if matches!(state.mode, Mode::Approval(_)) {
        render_approval_modal(f, area, state);
    }
}

// ---------------------------------------------------------------------------
// Header — two-column layout inspired by Claude Code
// ---------------------------------------------------------------------------

fn render_header(f: &mut Frame, area: Rect, state: &AppState) {
    // Top line: version banner
    let top_line = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // version
            Constraint::Min(1),   // two-column body
        ])
        .split(area);

    let version_line = Line::from(vec![
        Span::styled(
            "─── ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("ash-code v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::Rgb(255, 120, 50)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " ───",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(version_line), top_line[0]);

    // Two-column body
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40), // left: welcome + model
            Constraint::Percentage(60), // right: tips + activity
        ])
        .split(top_line[1]);

    // Left column: user info
    let model_display = if state.model.is_empty() {
        "(default)".to_string()
    } else {
        state.model.clone()
    };
    let left_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "    Welcome back!",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                format!("{} · {}", state.provider, model_display),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                format!("session: {}", &state.session_id[..state.session_id.len().min(20)]),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(left_lines), cols[0]);

    // Right column: tips + recent activity
    let right_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Tips for getting started",
            Style::default().fg(Color::Rgb(255, 120, 50)).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Type a prompt below and press Enter to send.",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "bash commands trigger an approval dialog.",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "Recent activity",
            Style::default().fg(Color::Rgb(255, 120, 50)).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            if state.chat.is_empty() {
                "No recent activity".to_string()
            } else {
                format!("{} message(s) this session", state.chat.len())
            },
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(right_lines), cols[1]);
}

// ---------------------------------------------------------------------------
// Chat body — borderless, bullet-style messages
// ---------------------------------------------------------------------------

fn render_chat(f: &mut Frame, area: Rect, state: &AppState) {
    // Horizontal padding: 1 char each side
    let padded = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    let mut lines: Vec<Line> = Vec::with_capacity(state.chat.len() * 2);
    for entry in &state.chat {
        match entry {
            ChatLine::User(text) => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        "● ",
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        text.clone(),
                        Style::default().fg(Color::White),
                    ),
                ]));
            }
            ChatLine::Assistant(text) => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        "● ",
                        Style::default().fg(Color::Magenta),
                    ),
                    Span::styled(
                        "ash-code",
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
                        "  ▸ ",
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{name} "),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(args.clone(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatLine::ToolResult { name, ok, body } => {
                let color = if *ok { Color::Green } else { Color::Red };
                let icon = if *ok { "✓" } else { "✗" };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {icon} {name} "),
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
                        "  [{stop_reason} in={input_tokens} out={output_tokens}]"
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            ChatLine::Error(msg) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ✖ ",
                        Style::default().fg(Color::Red),
                    ),
                    Span::styled(
                        msg.clone(),
                        Style::default().fg(Color::Red),
                    ),
                ]));
            }
            ChatLine::Denial { tool, reason } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ⊘ ",
                        Style::default().fg(Color::Red),
                    ),
                    Span::styled(
                        format!("{tool} denied: {reason}"),
                        Style::default().fg(Color::Red),
                    ),
                ]));
            }
        }
    }

    // Estimate wrapped line count for accurate scroll-to-bottom.
    let wrap_width = padded.width.max(1) as usize;
    let mut visual_lines: u16 = 0;
    for line in &lines {
        let line_width: usize = line.spans.iter().map(|s| s.content.len()).sum();
        if line_width == 0 {
            visual_lines += 1;
        } else {
            visual_lines += ((line_width + wrap_width - 1) / wrap_width) as u16;
        }
    }

    let view_h = padded.height;
    let offset = visual_lines
        .saturating_sub(view_h)
        .saturating_sub(state.scroll as u16);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(paragraph, padded);
}

// ---------------------------------------------------------------------------
// Input — borderless minimal prompt
// ---------------------------------------------------------------------------

fn render_input(f: &mut Frame, area: Rect, state: &AppState) {
    let disabled = state.running_turn || matches!(state.mode, Mode::Approval(_));

    // Separator line
    let sep_area = Rect { x: area.x + 1, y: area.y, width: area.width.saturating_sub(2), height: 1 };
    let sep_style = if disabled { Color::DarkGray } else { Color::Cyan };
    let sep = Paragraph::new(Line::from(Span::styled(
        "─".repeat(sep_area.width as usize),
        Style::default().fg(sep_style),
    )));
    f.render_widget(sep, sep_area);

    // Input line
    let input_area = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: 1 };
    let prompt_style = if disabled {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let prompt_char = if disabled { "  " } else { "› " };
    let cursor = if disabled { "" } else { "▏" };
    let content = format!("{}{}{}", prompt_char, state.input, cursor);
    let paragraph = Paragraph::new(content).style(prompt_style);
    f.render_widget(paragraph, input_area);
}

// ---------------------------------------------------------------------------
// Status bar — left: shortcuts, right: model info
// ---------------------------------------------------------------------------

fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let model_display = if state.model.is_empty() {
        "(default)".to_string()
    } else {
        state.model.clone()
    };

    let left = if state.running_turn {
        "Esc cancel · Ctrl-C quit"
    } else {
        "? for shortcuts"
    };
    let right = format!(
        "● {} · {} · turns {}",
        state.provider, model_display, state.turns_taken
    );

    // Pad the right text to right-align it
    let total_width = area.width as usize;
    let left_len = left.len();
    let right_len = right.len();
    let padding = total_width.saturating_sub(left_len + right_len + 2);

    let line = Line::from(vec![
        Span::styled(
            format!(" {left}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            format!("{right} "),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
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

// ---------------------------------------------------------------------------
// Slash palette — bottom-anchored overlay listing commands/skills
// ---------------------------------------------------------------------------

fn render_slash_palette(f: &mut Frame, chat_area: Rect, state: &AppState) {
    let palette = match &state.mode {
        Mode::SlashPalette(p) => p,
        _ => return,
    };

    let max_visible: usize = 8;
    let visible_count = palette.filtered.len().min(max_visible);
    if visible_count == 0 {
        return;
    }

    let height = visible_count as u16 + 2; // +2 for border
    // Position at the bottom of the chat area
    let palette_area = Rect {
        x: chat_area.x + 1,
        y: chat_area.y + chat_area.height.saturating_sub(height),
        width: chat_area.width.saturating_sub(2),
        height,
    };

    f.render_widget(Clear, palette_area);

    let mut lines: Vec<Line> = Vec::new();
    for (vi, &entry_idx) in palette.filtered.iter().take(max_visible).enumerate() {
        let entry = &palette.entries[entry_idx];
        let is_selected = vi == palette.selected;

        let kind_label = match entry.kind {
            PaletteKind::Command => "cmd",
            PaletteKind::Skill => "skill",
        };

        let name_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        };
        let desc_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let kind_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::Yellow)
        };

        let marker = if is_selected { "▸ " } else { "  " };

        lines.push(Line::from(vec![
            Span::styled(marker, name_style),
            Span::styled(format!("/{:<18}", entry.name), name_style),
            Span::styled(format!("[{kind_label}]  "), kind_style),
            Span::styled(
                truncate_str(&entry.description, palette_area.width.saturating_sub(30) as usize),
                desc_style,
            ),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, palette_area);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
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
        assert!(text.contains("Tell ash-code what to do instead"));
    }
}
