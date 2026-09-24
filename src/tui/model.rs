use std::{future::Future, path::PathBuf};

use anyhow::Result;
use crossterm::event::KeyCode;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleAction {
    Disable,
    Restart,
    Start,
    Stop,
}

impl LifecycleAction {
    pub(super) fn progress_notice(self, name: &str, exit_when_complete: bool) -> String {
        let action = match self {
            Self::Disable => "disabling",
            Self::Restart => "restarting",
            Self::Start => "starting",
            Self::Stop => "stopping",
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
            Self::Start => "started",
            Self::Stop => "stopped",
        };
        format!("{action} {name}")
    }

    fn error_notice(self, name: &str, error: impl std::fmt::Display) -> String {
        let action = match self {
            Self::Disable => "disable",
            Self::Restart => "restart",
            Self::Start => "start",
            Self::Stop => "stop",
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

/// One action catalog drives menus, accelerators and contextual help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceAction {
    Attach,
    Start,
    Stop,
    Restart,
    History,
    Disable,
}

impl ServiceAction {
    pub(super) const ALL: [Self; 6] = [
        Self::Attach,
        Self::Start,
        Self::Stop,
        Self::Restart,
        Self::History,
        Self::Disable,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Attach => "Attach",
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
            Self::History => "History",
            Self::Disable => "Disable",
        }
    }

    pub(super) fn key(self) -> char {
        match self {
            Self::Attach => 'a',
            Self::Start => 's',
            Self::Stop => 'x',
            Self::Restart => 'r',
            Self::History => 'h',
            Self::Disable => 'd',
        }
    }

    pub(super) fn from_key(key: KeyCode) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| key == KeyCode::Char(action.key()))
    }

    pub(super) fn lifecycle(self) -> Option<LifecycleAction> {
        match self {
            Self::Start => Some(LifecycleAction::Start),
            Self::Stop => Some(LifecycleAction::Stop),
            Self::Restart => Some(LifecycleAction::Restart),
            Self::Disable => Some(LifecycleAction::Disable),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) enum Page {
    #[default]
    Services,
    Actions {
        selected: usize,
        scroll: usize,
    },
    ConfirmDisable {
        confirm: bool,
        menu: Option<(usize, usize)>,
    },
}

pub(super) struct Reader {
    pub(super) title: String,
    pub(super) content: String,
    pub(super) scroll: usize,
}

impl Reader {
    pub(super) fn new(title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: content.into(),
            scroll: 0,
        }
    }

    pub(super) fn key(&mut self, key: KeyCode, page_rows: usize) {
        scroll_key(&mut self.scroll, key, page_rows);
    }
}

pub(super) fn scroll_key(scroll: &mut usize, key: KeyCode, page_rows: usize) {
    match key {
        KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(page_rows.max(1)),
        KeyCode::PageDown => *scroll = scroll.saturating_add(page_rows.max(1)),
        KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
        KeyCode::End | KeyCode::Char('G') => *scroll = usize::MAX,
        _ => {}
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Intent {
    None,
    Quit,
    Act(ServiceAction, String),
}

#[derive(Default)]
pub(super) struct MainUi {
    pub(super) services: Vec<crate::protocol::ServiceInfo>,
    pub(super) selected: usize,
    pub(super) page: Page,
    pub(super) help: Option<Reader>,
    pub(super) message: Option<Reader>,
    pub(super) unavailable: Option<String>,
    pub(super) notice: Option<(String, std::time::Instant)>,
    pub(super) viewport: (u16, u16),
}

impl MainUi {
    pub(super) fn refresh(&mut self, services: Vec<crate::protocol::ServiceInfo>) {
        let name = self.services.get(self.selected).map(|s| s.name.as_str());
        let matched = name.and_then(|name| services.iter().position(|s| s.name == name));
        if name.is_some() && matched.is_none() {
            self.page = Page::Services;
        }
        self.selected = matched.unwrap_or(self.selected.min(services.len().saturating_sub(1)));
        self.services = services;
        self.unavailable = None;
    }

    pub(super) fn notice(&self, now: std::time::Instant) -> &str {
        self.notice
            .as_ref()
            .filter(|(_, until)| now < *until)
            .map_or("", |(text, _)| text)
    }

    pub(super) fn key(&mut self, key: KeyCode, pending: bool, rows: usize) -> Intent {
        if let Some(help) = &mut self.help {
            if matches!(key, KeyCode::Esc | KeyCode::Char('q' | '?')) {
                self.help = None;
            } else {
                help.key(key, rows);
            }
            return Intent::None;
        }
        if let Some(message) = &mut self.message {
            if matches!(key, KeyCode::Esc | KeyCode::Char('q')) {
                self.message = None;
            } else {
                message.key(key, rows);
            }
            return Intent::None;
        }
        if key == KeyCode::Char('?') {
            let mut text = match self.page {
                Page::Services => "Up/Down, j/k  Select service\nEnter  Open actions\nEsc/q  Quit\n\n".to_owned(),
                Page::Actions { .. } => "Up/Down, j/k  Select action\nEnter  Execute action\nPgUp/PgDn  Move by a page\nHome/End  First/last action\nEsc/q  Back\n\n".to_owned(),
                Page::ConfirmDisable { .. } => "Up/Down, j/k  Select Cancel or Disable\nEnter  Confirm selection\nEsc/q  Cancel\n".to_owned(),
            };
            if !matches!(self.page, Page::ConfirmDisable { .. }) {
                for action in ServiceAction::ALL {
                    text.push_str(&format!("{}  {}\n", action.key(), action.label()));
                }
                text.push_str("\nDisable requires confirmation.\nCtrl+C  Quit (wait for a pending operation)\nTTY attach accepts input; pipe attach is read-only.\nCtrl+C detaches from either session.");
            }
            if matches!(self.page, Page::Actions { .. }) {
                if let Some(service) = self.services.get(self.selected) {
                    let kind = match service.kind {
                        crate::protocol::ServiceKind::Enabled => "enabled",
                        crate::protocol::ServiceKind::Temporary => "temporary",
                    };
                    text = format!(
                        "Service: {}\nDirectory: {}\nType: {}\n\n{}",
                        service.name, service.directory, kind, text
                    );
                }
            }
            if pending {
                text.push_str("\n\nOperation in progress: further actions are blocked.");
            }
            if let Some(error) = &self.unavailable {
                text.push_str(&format!("\n\nManager unavailable; data is stale:\n{error}"));
            }
            self.help = Some(Reader::new("Help", text));
            return Intent::None;
        }
        let mut action = None;
        match &mut self.page {
            Page::Services => match key {
                KeyCode::Esc | KeyCode::Char('q') => return Intent::Quit,
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = self
                        .selected
                        .saturating_add(1)
                        .min(self.services.len().saturating_sub(1))
                }
                KeyCode::Enter if !self.services.is_empty() => {
                    self.page = Page::Actions {
                        selected: 0,
                        scroll: 0,
                    }
                }
                _ => action = ServiceAction::from_key(key),
            },
            Page::Actions { selected, scroll } => match key {
                KeyCode::Esc | KeyCode::Char('q') => self.page = Page::Services,
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = selected.saturating_sub(1);
                    *scroll = (*scroll).min(*selected);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(ServiceAction::ALL.len() - 1);
                    *scroll = selected.saturating_sub(rows.saturating_sub(1));
                }
                KeyCode::Enter => action = Some(ServiceAction::ALL[*selected]),
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                    *selected = match key {
                        KeyCode::PageUp => selected.saturating_sub(rows.max(1)),
                        KeyCode::PageDown => selected
                            .saturating_add(rows.max(1))
                            .min(ServiceAction::ALL.len() - 1),
                        KeyCode::Home => 0,
                        KeyCode::End => ServiceAction::ALL.len() - 1,
                        _ => unreachable!(),
                    };
                    *scroll = (*scroll)
                        .min(*selected)
                        .max(selected.saturating_sub(rows.saturating_sub(1)));
                }
                _ => action = ServiceAction::from_key(key),
            },
            Page::ConfirmDisable { confirm, menu } => match key {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.page = if let Some((selected, scroll)) = *menu {
                        Page::Actions {
                            selected,
                            scroll: scroll.max(selected.saturating_sub(rows.saturating_sub(1))),
                        }
                    } else {
                        Page::Services
                    };
                }
                KeyCode::Up | KeyCode::Char('k') => *confirm = false,
                KeyCode::Down | KeyCode::Char('j') => *confirm = true,
                KeyCode::Enter if *confirm => {
                    if !pending && self.unavailable.is_none() {
                        if let Some(service) = self.services.get(self.selected) {
                            let name = service.name.clone();
                            self.page = Page::Services;
                            return Intent::Act(ServiceAction::Disable, name);
                        }
                    }
                }
                KeyCode::Enter => {
                    self.page = if let Some((selected, scroll)) = *menu {
                        Page::Actions {
                            selected,
                            scroll: scroll.max(selected.saturating_sub(rows.saturating_sub(1))),
                        }
                    } else {
                        Page::Services
                    }
                }
                _ => {}
            },
        }
        if !pending && self.unavailable.is_none() {
            if let (Some(action), Some(service)) = (action, self.services.get(self.selected)) {
                if action == ServiceAction::Disable {
                    self.page = Page::ConfirmDisable {
                        confirm: false,
                        menu: match self.page {
                            Page::Actions { selected, scroll } => Some((selected, scroll)),
                            _ => None,
                        },
                    };
                } else {
                    return Intent::Act(action, service.name.clone());
                }
            }
        }
        Intent::None
    }
}
