use std::{future::Future, path::PathBuf};

use anyhow::Result;
use crossterm::event::KeyCode;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleAction {
    Disable,
    Restart,
}

impl LifecycleAction {
    pub(super) fn progress_notice(self, name: &str, exit_when_complete: bool) -> String {
        let action = match self {
            Self::Disable => "disabling",
            Self::Restart => "restarting",
        };
        let exit = if exit_when_complete {
            "; quitting when complete"
        } else {
            ""
        };
        format!("{action} {name}...{exit}")
    }

    fn success_notice(self, name: &str) -> String {
        let action = match self {
            Self::Disable => "disabled",
            Self::Restart => "restarted",
        };
        format!("{action} {name}")
    }

    fn error_notice(self, name: &str, error: impl std::fmt::Display) -> String {
        let action = match self {
            Self::Disable => "disable",
            Self::Restart => "restart",
        };
        format!("{action} {name}: {error}")
    }
}

pub(super) struct LifecycleOutcome {
    pub(super) succeeded: bool,
    pub(super) notice: String,
}

pub(super) struct PendingLifecycleAction {
    action: LifecycleAction,
    name: String,
    task: JoinHandle<Result<()>>,
}

impl PendingLifecycleAction {
    pub(super) fn spawn<F>(action: LifecycleAction, name: String, future: F) -> Self
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        Self {
            action,
            name,
            task: tokio::spawn(future),
        }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(super) fn progress_notice(&self, exit_when_complete: bool) -> String {
        self.action.progress_notice(&self.name, exit_when_complete)
    }

    pub(super) async fn finish(self) -> LifecycleOutcome {
        match self.task.await {
            Ok(Ok(())) => LifecycleOutcome {
                succeeded: true,
                notice: self.action.success_notice(&self.name),
            },
            Ok(Err(error)) => LifecycleOutcome {
                succeeded: false,
                notice: self.action.error_notice(&self.name, error),
            },
            Err(error) => LifecycleOutcome {
                succeeded: false,
                notice: self
                    .action
                    .error_notice(&self.name, format!("background task failed: {error}")),
            },
        }
    }
}

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
