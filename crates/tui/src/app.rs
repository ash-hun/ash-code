//! Pure state and logic for the TUI. No I/O, no ratatui, no tokio —
//! everything here is deterministic and unit-testable.

use ash_query::CancellationToken;
use ash_tools::ToolResult;
use tokio::sync::oneshot;

use crate::backend::{ApprovalDecision, PendingApproval};

/// One visible row in the chat scroll buffer.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatLine {
    User(String),
    Assistant(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, ok: bool, body: String },
    Finish { stop_reason: String, input_tokens: i32, output_tokens: i32 },
    Error(String),
    Denial { tool: String, reason: String },
}

/// An entry in the slash palette (command or skill).
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub kind: PaletteKind,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteKind {
    Command,
    Skill,
}

/// State for the slash command palette overlay.
#[derive(Debug)]
pub struct SlashPaletteState {
    pub entries: Vec<PaletteEntry>,
    pub filtered: Vec<usize>, // indices into entries
    pub selected: usize,
    pub filter: String, // characters typed after '/'
}

impl SlashPaletteState {
    pub fn new(entries: Vec<PaletteEntry>) -> Self {
        let filtered: Vec<usize> = (0..entries.len()).collect();
        Self {
            entries,
            filtered,
            selected: 0,
            filter: String::new(),
        }
    }

    pub fn update_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        let lower = filter.to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                lower.is_empty()
                    || e.name.to_lowercase().contains(&lower)
                    || e.description.to_lowercase().contains(&lower)
            })
            .map(|(i, _)| i)
            .collect();
        // Clamp selection
        if self.selected >= self.filtered.len() {
            self.selected = 0;
        }
    }

    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.entries.get(i))
    }

    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + self.filtered.len() - 1) % self.filtered.len();
        }
    }
}

/// Top-level mode of the UI. Only one is active at a time.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Approval(ApprovalState),
    SlashPalette(SlashPaletteState),
}

/// State captured while a bash approval dialog is open.
#[derive(Debug)]
pub struct ApprovalState {
    pub request: PendingApproval,
    pub selected: usize, // 0 = Yes, 1 = No, 2 = Custom
    pub custom_input: String,
    pub editing_custom: bool,
    pub responder: Option<oneshot::Sender<ApprovalDecision>>,
}

impl ApprovalState {
    pub fn new(request: PendingApproval, responder: oneshot::Sender<ApprovalDecision>) -> Self {
        Self {
            request,
            selected: 0,
            custom_input: String::new(),
            editing_custom: false,
            responder: Some(responder),
        }
    }

    pub fn resolve(&mut self, decision: ApprovalDecision) {
        if let Some(tx) = self.responder.take() {
            let _ = tx.send(decision);
        }
    }
}

/// Full mutable state of the TUI.
pub struct AppState {
    pub chat: Vec<ChatLine>,
    pub input: String,
    pub scroll: usize,
    pub mode: Mode,
    pub running_turn: bool,
    pub should_quit: bool,
    /// M8: cancellation token for the in-flight turn (None when idle).
    pub current_cancel: Option<CancellationToken>,

    // Read-only meta for the header/footer
    pub provider: String,
    pub model: String,
    pub session_id: String,
    pub turns_taken: usize,

    /// Cached palette entries loaded at startup.
    pub palette_entries: Vec<PaletteEntry>,
    /// Frame counter for spinner animation.
    pub tick: usize,
}

impl AppState {
    pub fn new(provider: String, model: String, session_id: String) -> Self {
        Self {
            chat: Vec::new(),
            input: String::new(),
            scroll: 0,
            mode: Mode::Normal,
            running_turn: false,
            should_quit: false,
            current_cancel: None,
            provider,
            model,
            session_id,
            turns_taken: 0,
            palette_entries: Vec::new(),
            tick: 0,
        }
    }

    /// Cancel the current turn if one is running. Returns true on hit.
    pub fn request_cancel_turn(&mut self) -> bool {
        if let Some(token) = &self.current_cancel {
            token.cancel();
            true
        } else {
            false
        }
    }

    // --- input editing -----------------------------------------------------

    pub fn push_char(&mut self, c: char) {
        if let Mode::Approval(approval) = &mut self.mode {
            if approval.editing_custom {
                approval.custom_input.push(c);
            }
            return;
        }
        if let Mode::SlashPalette(palette) = &mut self.mode {
            // Typing updates the filter (text after the last '/')
            self.input.push(c);
            let filter = self.input.rsplit('/').next().unwrap_or("").to_string();
            palette.update_filter(&filter);
            return;
        }
        if self.running_turn {
            return;
        }
        // Open slash palette when '/' is typed anywhere
        if c == '/' && !self.palette_entries.is_empty() {
            self.input.push(c);
            self.mode = Mode::SlashPalette(SlashPaletteState::new(
                self.palette_entries.clone(),
            ));
            return;
        }
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        if let Mode::Approval(approval) = &mut self.mode {
            if approval.editing_custom {
                approval.custom_input.pop();
            }
            return;
        }
        if let Mode::SlashPalette(palette) = &mut self.mode {
            self.input.pop();
            // Close palette if the last '/' was deleted
            if !self.input.contains('/') {
                self.mode = Mode::Normal;
                return;
            }
            let filter = self.input.rsplit('/').next().unwrap_or("").to_string();
            palette.update_filter(&filter);
            return;
        }
        if self.running_turn {
            return;
        }
        self.input.pop();
    }

    /// Returns the prompt to submit, and clears the input buffer.
    pub fn take_prompt(&mut self) -> Option<String> {
        if self.running_turn {
            return None;
        }
        let trimmed = self.input.trim().to_string();
        self.input.clear();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    // --- chat log mutation -------------------------------------------------
    // Every mutation auto-scrolls to bottom (scroll=0) so the latest
    // content is always visible during streaming.

    pub fn push_user(&mut self, text: String) {
        self.chat.push(ChatLine::User(text));
        self.scroll = 0;
    }

    /// Append streaming text from the assistant.
    /// Coalesces into the most recent Assistant line when possible so that
    /// streaming deltas do not produce one line per token.
    pub fn push_text_delta(&mut self, text: &str) {
        if let Some(ChatLine::Assistant(existing)) = self.chat.last_mut() {
            existing.push_str(text);
        } else {
            self.chat.push(ChatLine::Assistant(text.to_string()));
        }
        self.scroll = 0;
    }

    pub fn push_tool_call(&mut self, name: &str, args: &str) {
        self.chat.push(ChatLine::ToolCall {
            name: name.to_string(),
            args: args.to_string(),
        });
        self.scroll = 0;
    }

    pub fn push_tool_result(&mut self, name: &str, result: &ToolResult) {
        let body = if result.ok {
            &result.stdout
        } else {
            &result.stderr
        };
        let snippet: String = body.chars().take(400).collect();
        self.chat.push(ChatLine::ToolResult {
            name: name.to_string(),
            ok: result.ok,
            body: snippet,
        });
        self.scroll = 0;
    }

    pub fn push_finish(&mut self, stop_reason: &str, input_tokens: i32, output_tokens: i32) {
        self.chat.push(ChatLine::Finish {
            stop_reason: stop_reason.to_string(),
            input_tokens,
            output_tokens,
        });
        self.scroll = 0;
    }

    pub fn push_error(&mut self, message: String) {
        self.chat.push(ChatLine::Error(message));
        self.scroll = 0;
    }

    pub fn push_denial(&mut self, tool: String, reason: String) {
        self.chat.push(ChatLine::Denial { tool, reason });
        self.scroll = 0;
    }

    // --- approval mode ----------------------------------------------------

    pub fn enter_approval(
        &mut self,
        request: PendingApproval,
        responder: oneshot::Sender<ApprovalDecision>,
    ) {
        self.mode = Mode::Approval(ApprovalState::new(request, responder));
    }

    pub fn approval_select_next(&mut self) {
        if let Mode::Approval(a) = &mut self.mode {
            if !a.editing_custom {
                a.selected = (a.selected + 1) % 3;
            }
        }
    }

    pub fn approval_select_prev(&mut self) {
        if let Mode::Approval(a) = &mut self.mode {
            if !a.editing_custom {
                a.selected = (a.selected + 2) % 3;
            }
        }
    }

    pub fn approval_confirm(&mut self) {
        let decision = if let Mode::Approval(a) = &mut self.mode {
            match a.selected {
                0 => Some(ApprovalDecision::Allow),
                1 => Some(ApprovalDecision::Deny {
                    reason: "user denied".to_string(),
                }),
                2 => {
                    if !a.editing_custom {
                        a.editing_custom = true;
                        return;
                    }
                    let reason = a.custom_input.trim().to_string();
                    if reason.is_empty() {
                        None
                    } else {
                        Some(ApprovalDecision::Deny { reason })
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        if let Some(d) = decision {
            if let Mode::Approval(a) = &mut self.mode {
                a.resolve(d);
            }
            self.mode = Mode::Normal;
        }
    }

    pub fn approval_cancel(&mut self) {
        if let Mode::Approval(a) = &mut self.mode {
            a.resolve(ApprovalDecision::Deny {
                reason: "user cancelled".to_string(),
            });
        }
        self.mode = Mode::Normal;
    }

    // --- slash palette ----------------------------------------------------

    /// Select the current palette entry and place its name in the input as
    /// `/<name> `, then close the palette.
    pub fn palette_confirm(&mut self) -> Option<String> {
        if let Mode::SlashPalette(palette) = &self.mode {
            if let Some(entry) = palette.selected_entry() {
                let name = entry.name.clone();
                // Replace from the last '/' onward with /<name>
                if let Some(slash_pos) = self.input.rfind('/') {
                    self.input.truncate(slash_pos);
                }
                self.input.push_str(&format!("/{name} "));
                self.mode = Mode::Normal;
                return Some(name);
            }
        }
        self.palette_dismiss();
        None
    }

    pub fn palette_dismiss(&mut self) {
        if matches!(self.mode, Mode::SlashPalette(_)) {
            self.mode = Mode::Normal;
        }
    }

    pub fn palette_next(&mut self) {
        if let Mode::SlashPalette(p) = &mut self.mode {
            p.select_next();
        }
    }

    pub fn palette_prev(&mut self) {
        if let Mode::SlashPalette(p) = &mut self.mode {
            p.select_prev();
        }
    }

    // --- scroll -----------------------------------------------------------

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn fresh() -> AppState {
        AppState::new(
            "anthropic".to_string(),
            "claude-opus-4-5".to_string(),
            "sess-test".to_string(),
        )
    }

    #[test]
    fn input_editing_basic() {
        let mut s = fresh();
        s.push_char('h');
        s.push_char('i');
        assert_eq!(s.input, "hi");
        s.backspace();
        assert_eq!(s.input, "h");
    }

    #[test]
    fn take_prompt_returns_trimmed_and_clears() {
        let mut s = fresh();
        s.input = "  hello  ".to_string();
        assert_eq!(s.take_prompt().as_deref(), Some("hello"));
        assert_eq!(s.input, "");
    }

    #[test]
    fn take_prompt_empty_returns_none() {
        let mut s = fresh();
        s.input = "   ".to_string();
        assert!(s.take_prompt().is_none());
    }

    #[test]
    fn take_prompt_blocked_while_running() {
        let mut s = fresh();
        s.input = "hi".to_string();
        s.running_turn = true;
        assert!(s.take_prompt().is_none());
    }

    #[test]
    fn text_delta_coalesces_into_last_assistant_line() {
        let mut s = fresh();
        s.push_text_delta("hel");
        s.push_text_delta("lo");
        assert_eq!(s.chat.len(), 1);
        if let ChatLine::Assistant(text) = &s.chat[0] {
            assert_eq!(text, "hello");
        } else {
            panic!("expected assistant line");
        }
    }

    #[test]
    fn text_delta_after_tool_call_starts_new_line() {
        let mut s = fresh();
        s.push_text_delta("hel");
        s.push_tool_call("bash", "{}");
        s.push_text_delta("lo");
        assert_eq!(s.chat.len(), 3);
    }

    #[test]
    fn approval_cycle_yes() {
        let mut s = fresh();
        let (tx, mut rx) = oneshot::channel::<ApprovalDecision>();
        s.enter_approval(
            PendingApproval {
                tool_name: "bash".to_string(),
                arguments: "{\"command\":\"ls\"}".to_string(),
            },
            tx,
        );
        assert!(matches!(s.mode, Mode::Approval(_)));
        s.approval_confirm(); // selected=0 → Allow
        assert!(matches!(s.mode, Mode::Normal));
        let decision = rx.try_recv().unwrap();
        assert!(matches!(decision, ApprovalDecision::Allow));
    }

    #[test]
    fn approval_cycle_no() {
        let mut s = fresh();
        let (tx, mut rx) = oneshot::channel::<ApprovalDecision>();
        s.enter_approval(
            PendingApproval {
                tool_name: "bash".to_string(),
                arguments: "{}".to_string(),
            },
            tx,
        );
        s.approval_select_next(); // → 1
        s.approval_confirm();
        let decision = rx.try_recv().unwrap();
        match decision {
            ApprovalDecision::Deny { reason } => assert!(reason.contains("denied")),
            _ => panic!(),
        }
    }

    #[test]
    fn approval_cycle_custom_feedback() {
        let mut s = fresh();
        let (tx, mut rx) = oneshot::channel::<ApprovalDecision>();
        s.enter_approval(
            PendingApproval {
                tool_name: "bash".to_string(),
                arguments: "{}".to_string(),
            },
            tx,
        );
        s.approval_select_next(); // → 1
        s.approval_select_next(); // → 2
        s.approval_confirm(); // enters custom edit mode
        for c in "please ls -la instead".chars() {
            s.push_char(c);
        }
        s.approval_confirm(); // submit custom feedback
        let decision = rx.try_recv().unwrap();
        match decision {
            ApprovalDecision::Deny { reason } => assert_eq!(reason, "please ls -la instead"),
            _ => panic!(),
        }
    }

    #[test]
    fn approval_cancel_emits_deny() {
        let mut s = fresh();
        let (tx, mut rx) = oneshot::channel::<ApprovalDecision>();
        s.enter_approval(
            PendingApproval {
                tool_name: "bash".to_string(),
                arguments: "{}".to_string(),
            },
            tx,
        );
        s.approval_cancel();
        assert!(matches!(s.mode, Mode::Normal));
        let decision = rx.try_recv().unwrap();
        assert!(matches!(decision, ApprovalDecision::Deny { .. }));
    }
}
