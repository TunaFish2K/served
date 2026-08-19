use std::path::PathBuf;

use crossterm::event::KeyCode;

pub(super) struct CrashLogPrompt {
    pub(super) warning: String,
    pub(super) path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CrashPromptAction {
    Open,
    Cancel,
    Ignore,
}

pub(super) fn crash_prompt_action(code: &KeyCode) -> CrashPromptAction {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => CrashPromptAction::Open,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => CrashPromptAction::Cancel,
        _ => CrashPromptAction::Ignore,
    }
}

pub(super) struct HistoryView {
    pub(super) id: String,
    pub(super) content: String,
    pub(super) offset: u64,
    pub(super) eof: bool,
    pub(super) total_lines: u64,
    pub(super) scroll: u64,
}

impl HistoryView {
    pub(super) fn new(id: String) -> Self {
        Self {
            id,
            content: String::new(),
            offset: 0,
            eof: false,
            total_lines: 0,
            scroll: 0,
        }
    }
}
