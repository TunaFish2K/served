use super::{document::Document, input::Buffer};
use crate::config::{RestartPolicy, ServiceConfig};
use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::Path;

pub(super) const TABS: [&str; 4] = ["Basic", "Runtime", "Logs", "Environment"];
pub(super) const LABELS: [&str; 9] = [
    "Name",
    "Command",
    "Working directory",
    "Restart policy",
    "PTY",
    "Sync terminal size",
    "Persist logs",
    "Max bytes / file",
    "Retained files",
];

pub(super) enum Editing {
    Text {
        field: usize,
        input: Buffer,
        original: String,
    },
    Choice(usize),
    Env {
        old: Option<String>,
        key: Buffer,
        value: Buffer,
        original_value: String,
        value_focus: bool,
        delete_available: bool,
        delete_focus: bool,
    },
}
pub(super) enum Confirm {
    Exit,
    Reload,
    Delete(String),
    DeleteNew,
}

pub(super) struct Form {
    pub document: Document,
    pub config: ServiceConfig,
    pub tab: usize,
    pub selected: usize,
    pub editing: Option<Editing>,
    pub confirm: Option<Confirm>,
    pub confirm_selected: usize,
    pub enhanced_keyboard: bool,
    drafts: Vec<Editing>,
    pub help: bool,
    pub error: Option<String>,
    pub reader_scroll: usize,
    pub inline_error: Option<String>,
    pub inline_error_acknowledged: bool,
    undo: Vec<ServiceConfig>,
    redo: Vec<ServiceConfig>,
}
impl Form {
    pub(super) fn help_content(&self) -> (&'static str, String) {
        let edit_keys = "←→ / home / end  Move cursor\nctrl+z / ctrl+y  Undo / redo";
        let finish = "enter  Finish input\nesc  Back; keep draft";
        let multiline = "↑↓  Move by visual row\npgup / pgdn  Move by a page\nshift+enter  Newline\nctrl+j  Newline (fallback)";
        match self.editing.as_ref() {
            None => {
                let mut keys = "←→  Change page\n↑↓  Select field\nenter  Edit / toggle".to_owned();
                if self.tab == 3 {
                    keys.push_str("\na  Add variable");
                } else if self.tab == 1 || self.tab == 2 {
                    keys.push_str("\nspace  Toggle switch");
                }
                keys.push_str("\nctrl+s  Save\nesc  Quit; confirm changes\nctrl+r  Reload; confirm changes\nctrl+z / ctrl+y  Undo / redo");
                ("Help / edit", keys)
            }
            Some(Editing::Choice(_)) => (
                "Help / restart policy",
                "↑↓ / ←→  Select policy\nenter  Confirm selection\nesc  Back; keep selection"
                    .into(),
            ),
            Some(Editing::Text { field, .. }) => {
                let title = match field {
                    0 => "Help / name",
                    1 => "Help / command",
                    2 => "Help / working directory",
                    7 => "Help / max bytes",
                    _ => "Help / retained files",
                };
                let keys = if *field == 1 {
                    format!("{edit_keys}\n{multiline}\n{finish}")
                } else {
                    format!("{edit_keys}\n{finish}")
                };
                (title, keys)
            }
            Some(Editing::Env {
                delete_focus: true, ..
            }) => (
                "Help / environment · delete",
                "↑  Return to Value\nenter  Request deletion; confirm\nesc  Back; keep draft"
                    .into(),
            ),
            Some(Editing::Env {
                value_focus: false, ..
            }) => (
                "Help / environment · name",
                format!("{edit_keys}\nenter / ↓  Go to Value\nesc  Back; keep draft"),
            ),
            Some(Editing::Env {
                delete_available, ..
            }) => {
                let mut keys = format!("{edit_keys}\n↑  Up; first → Name\n↓  Down");
                if *delete_available {
                    keys.push_str("; last → Delete");
                }
                keys.push_str(&format!(
                    "\npgup / pgdn  Move by a page\nshift+enter  Newline\nctrl+j  Newline (fallback)\n{finish}"
                ));
                ("Help / environment · value", keys)
            }
        }
    }

    pub fn open(path: &Path) -> Result<Self> {
        let document = Document::open(path)?;
        Ok(Self {
            config: document.config.clone(),
            document,
            tab: 0,
            selected: 0,
            editing: None,
            confirm: None,
            confirm_selected: 0,
            enhanced_keyboard: false,
            drafts: Vec::new(),
            help: false,
            error: None,
            reader_scroll: 0,
            inline_error: None,
            inline_error_acknowledged: false,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }
    pub fn field(&self) -> usize {
        self.tab * 3 + self.selected
    }
    pub fn count(&self) -> usize {
        if self.tab == 3 {
            self.env_rows().len().max(1)
        } else {
            3
        }
    }
    pub fn env_key(&self) -> Option<String> {
        self.config.env.keys().nth(self.selected).cloned()
    }
    pub fn dirty(&self) -> bool {
        !self.drafts.is_empty()
            || self.config != self.document.config
            || match &self.editing {
                Some(Editing::Text {
                    input, original, ..
                }) => input.text != *original,
                Some(Editing::Choice(index)) => *index != policy_index(self.config.restart),
                Some(Editing::Env {
                    old,
                    key,
                    value,
                    original_value,
                    ..
                }) => {
                    if old.is_none() {
                        !key.text.is_empty() || !value.text.is_empty()
                    } else {
                        old.as_deref() != Some(key.text.as_str()) || value.text != *original_value
                    }
                }
                None => false,
            }
    }
    pub fn disabled(&self, field: usize) -> bool {
        (field == 5 && !self.config.tty) || (matches!(field, 7 | 8) && !self.config.persist_logs)
    }
    pub fn value(&self, field: usize) -> String {
        if let Some(Editing::Text { input, .. }) = self
            .drafts
            .iter()
            .find(|edit| matches!(edit, Editing::Text { field: f, .. } if *f == field))
        {
            return input.text.clone();
        }
        match field {
            0 => self.config.name.clone(),
            1 => self.config.command.clone(),
            2 => self.config.cwd.clone().unwrap_or_default(),
            3 => self.config.restart.as_str().into(),
            4 => self.config.tty.to_string(),
            5 => self.config.sync_rows_cols.to_string(),
            6 => self.config.persist_logs.to_string(),
            7 => self.config.log_max_bytes.to_string(),
            8 => self.config.log_max_files.to_string(),
            _ => String::new(),
        }
    }
    fn change(&mut self, next: ServiceConfig) {
        if next != self.config {
            self.undo.push(self.config.clone());
            if self.undo.len() > 32 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.config = next;
        }
        self.inline_error = None;
    }
    fn undo(&mut self, redo: bool) {
        let next = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        };
        if let Some(next) = next {
            let current = self.config.clone();
            if redo {
                self.undo.push(current);
            } else {
                self.redo.push(current);
            }
            self.config = next;
            self.selected = self.selected.min(self.count() - 1);
            self.inline_error = None;
        }
    }
    fn toggle(&mut self, field: usize) {
        if self.disabled(field) {
            return;
        }
        let mut config = self.config.clone();
        match field {
            4 => config.tty = !config.tty,
            5 => config.sync_rows_cols = !config.sync_rows_cols,
            6 => config.persist_logs = !config.persist_logs,
            _ => return,
        }
        self.change(config);
    }
    pub fn env_rows(&self) -> Vec<(String, String)> {
        let mut rows: Vec<_> = self
            .config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for edit in &self.drafts {
            if let Editing::Env {
                old, key, value, ..
            } = edit
            {
                if let Some(index) = old
                    .as_ref()
                    .and_then(|name| self.config.env.keys().position(|k| k == name))
                {
                    rows[index] = (key.text.clone(), value.text.clone());
                } else if old.is_none() {
                    rows.push((
                        if key.text.is_empty() {
                            "(new variable)".into()
                        } else {
                            key.text.clone()
                        },
                        value.text.clone(),
                    ));
                }
            }
        }
        rows
    }
    fn restore_draft(&mut self, field: Option<usize>, old: Option<&str>) -> bool {
        let index = self.drafts.iter().position(|edit| match edit {
            Editing::Text { field: f, .. } => field == Some(*f),
            Editing::Env { old: name, .. } => field.is_none() && name.as_deref() == old,
            _ => false,
        });
        if let Some(index) = index {
            self.editing = Some(self.drafts.remove(index));
            true
        } else {
            false
        }
    }
    fn leave_input(&mut self) {
        if matches!(&self.editing, Some(Editing::Env { old: None, key, value, .. }) if key.text.is_empty() && value.text.is_empty())
        {
            self.editing = None;
            self.selected = self.selected.min(self.count() - 1);
            return;
        }
        if !self.apply() {
            if let Some(edit) = self.editing.take() {
                self.drafts.push(edit);
            }
        }
    }
    fn move_focus(&mut self, backwards: bool) {
        let count = self.count();
        self.selected = (self.selected + if backwards { count - 1 } else { 1 }) % count;
    }
    fn ask(&mut self, confirm: Confirm) {
        self.reader_scroll = 0;
        self.confirm = Some(confirm);
        self.confirm_selected = 0;
    }
    fn request_exit(&mut self) -> bool {
        if self.dirty() {
            self.ask(Confirm::Exit);
            false
        } else {
            true
        }
    }
    fn begin(&mut self) {
        self.inline_error = None;
        if self.tab == 3 {
            self.begin_env(false);
            return;
        }
        let field = self.field();
        if self.restore_draft(Some(field), None) {
            return;
        }
        if self.disabled(field) {
            return;
        }
        match field {
            3 => self.editing = Some(Editing::Choice(policy_index(self.config.restart))),
            4..=6 => self.toggle(field),
            _ => {
                let original = self.value(field);
                let mut input = Buffer::new(original.clone());
                input.end(true);
                self.editing = Some(Editing::Text {
                    field,
                    input,
                    original,
                });
            }
        }
    }
    fn begin_env(&mut self, new: bool) {
        let old = if new { None } else { self.env_key() };
        if self.restore_draft(None, old.as_deref()) {
            if let Some(Editing::Env {
                delete_available,
                delete_focus,
                ..
            }) = &mut self.editing
            {
                *delete_available = true;
                *delete_focus = false;
            }
            return;
        }
        let original_value = old
            .as_ref()
            .and_then(|k| self.config.env.get(k))
            .cloned()
            .unwrap_or_default();
        self.editing = Some(Editing::Env {
            key: Buffer::new(old.clone().unwrap_or_default()),
            value: Buffer::new(original_value.clone()),
            old: old.clone(),
            original_value,
            value_focus: false,
            delete_available: old.is_some(),
            delete_focus: false,
        });
        self.inline_error = None;
    }
    fn apply(&mut self) -> bool {
        let Some(edit) = &self.editing else {
            return true;
        };
        let mut next = self.config.clone();
        let result: Result<()> = (|| {
            match edit {
                Editing::Text { field, input, .. } => match field {
                    0 => next.name = input.text.clone(),
                    1 => next.command = input.text.clone(),
                    2 => next.cwd = (!input.text.is_empty()).then(|| input.text.clone()),
                    7 => {
                        next.log_max_bytes = input.text.parse().map_err(|_| {
                            anyhow::anyhow!("Enter a whole number of bytes (1 to {}).", u64::MAX)
                        })?
                    }
                    8 => {
                        next.log_max_files = input.text.parse().map_err(|_| {
                            anyhow::anyhow!("Enter a whole number of files (1 to {}).", u32::MAX)
                        })?
                    }
                    _ => {}
                },
                Editing::Choice(index) => next.restart = policies()[*index],
                Editing::Env {
                    old, key, value, ..
                } => {
                    if key.text.is_empty() || key.text.contains(['=', '\0', '\n']) {
                        bail!("Enter a nonempty key without '=', NUL or a newline.");
                    }
                    if old.as_deref() != Some(key.text.as_str()) && next.env.contains_key(&key.text)
                    {
                        bail!("This environment key already exists.");
                    }
                    if let Some(old) = old {
                        next.env.remove(old);
                    }
                    next.env.insert(key.text.clone(), value.text.clone());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(Editing::Env {
                value_focus,
                delete_focus,
                ..
            }) = &mut self.editing
            {
                *value_focus = false;
                *delete_focus = false;
            }
            self.set_inline_error(error.to_string());
            return false;
        }
        self.editing = None;
        self.change(next);
        self.selected = self.selected.min(self.count() - 1);
        true
    }
    fn validate(&mut self) -> bool {
        let error = if self.config.name.is_empty()
            || matches!(self.config.name.as_str(), "." | "..")
            || self
                .config
                .name
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !"._-".contains(c))
        {
            Some((0, "Use letters, digits, '.', '_' or '-' for the name."))
        } else if self.config.command.trim().is_empty() {
            Some((1, "Command must not be empty."))
        } else if self.config.log_max_bytes == 0 {
            Some((7, "Max bytes must be greater than zero."))
        } else if self.config.log_max_files == 0 {
            Some((8, "Retained files must be greater than zero."))
        } else {
            None
        };
        if let Some((field, message)) = error {
            self.tab = field / 3;
            self.selected = field % 3;
            self.set_inline_error(message.into());
            return false;
        }
        if let Err(error) = self.config.validate() {
            self.tab = 3;
            self.selected = 0;
            self.set_inline_error(error.to_string());
            return false;
        }
        true
    }
    fn set_inline_error(&mut self, error: String) {
        self.inline_error = Some(error);
        self.inline_error_acknowledged = false;
    }

    fn save(&mut self) -> bool {
        if !self.apply() {
            return false;
        }
        while !self.drafts.is_empty() {
            let edit = self.drafts.remove(0);
            match &edit {
                Editing::Text { field, .. } => {
                    self.tab = field / 3;
                    self.selected = field % 3;
                }
                Editing::Env { old, .. } => {
                    self.tab = 3;
                    self.selected = old
                        .as_ref()
                        .and_then(|name| self.config.env.keys().position(|k| k == name))
                        .unwrap_or(self.config.env.len());
                }
                _ => {}
            }
            self.editing = Some(edit);
            if !self.apply() {
                return false;
            }
        }
        if !self.validate() {
            return false;
        }
        match self.document.save_config(&self.config) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.reader_scroll = 0;
                false
            }
        }
    }
    fn reload(&mut self) {
        match Self::open(&self.document.path) {
            Ok(mut next) => {
                next.enhanced_keyboard = self.enhanced_keyboard;
                *self = next
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.reader_scroll = 0;
            }
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.confirm.is_some() || self.help || self.error.is_some() {
            return;
        }
        if let Some(edit) = &mut self.editing {
            let (input, multiline) = input_for(edit);
            if let Some(input) = input {
                if input.text.len() + text.len() <= 1024 * 1024 {
                    input.insert(&if multiline {
                        text.to_owned()
                    } else {
                        text.replace(['\r', '\n'], " ")
                    });
                }
            }
        }
    }
    pub fn key(&mut self, key: KeyEvent, rows: usize) -> bool {
        use KeyCode::*;
        use crossterm::event::KeyEventKind;
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let plain = key.modifiers.is_empty();
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        if matches!(key.code, Tab | BackTab) {
            return false;
        }
        let exit = ctrl && matches!(key.code, Char('q' | 'c'));
        let help = (plain && key.code == F(1))
            || (self.editing.is_none()
                && matches!(key.code, Char('?' | '？'))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER));
        if self.help || self.error.is_some() {
            if exit || help || (plain && matches!(key.code, Esc | Enter)) {
                self.help = false;
                self.error = None;
            } else if plain {
                super::super::model::scroll_key(&mut self.reader_scroll, key.code, rows);
            }
            return false;
        }
        if let Some(confirm) = self.confirm.take() {
            if key.kind == KeyEventKind::Repeat {
                self.confirm = Some(confirm);
                return false;
            }
            if exit || (plain && key.code == Esc) {
                return false;
            }
            let count = if matches!(confirm, Confirm::Exit) {
                3
            } else {
                2
            };
            if plain && matches!(key.code, PageUp | PageDown | Home | End) {
                super::super::model::scroll_key(&mut self.reader_scroll, key.code, rows);
            } else if plain && matches!(key.code, Up | Left) {
                self.confirm_selected = (self.confirm_selected + count - 1) % count;
            } else if plain && matches!(key.code, Down | Right) {
                self.confirm_selected = (self.confirm_selected + 1) % count;
            } else if plain && key.code == Enter {
                match (confirm, self.confirm_selected) {
                    (_, 0) => {}
                    (Confirm::Exit, 1) => return self.save(),
                    (Confirm::Exit, 2) => return true,
                    (Confirm::Reload, _) => self.reload(),
                    (Confirm::DeleteNew, _) => {
                        self.editing = None;
                        self.inline_error = None;
                        self.drafts
                            .retain(|edit| !matches!(edit, Editing::Env { old: None, .. }));
                        self.selected = self.selected.min(self.count() - 1);
                    }
                    (Confirm::Delete(name), _) => {
                        self.editing = None;
                        let mut next = self.config.clone();
                        next.env.remove(&name);
                        self.drafts.retain(|edit| !matches!(edit, Editing::Env { old, .. } if old.as_deref() == Some(name.as_str())));
                        self.change(next);
                        self.selected = self.selected.min(self.count() - 1);
                    }
                    _ => {}
                }
                return false;
            }
            self.confirm = Some(confirm);
            return false;
        }
        if exit || (plain && key.code == Esc && self.editing.is_none()) {
            return self.request_exit();
        }
        if help {
            self.help = true;
            self.reader_scroll = 0;
            return false;
        }
        if ctrl && key.code == Char('s') {
            if self.editing.is_none() {
                self.save();
            }
            return false;
        }
        if ctrl && key.code == Char('r') {
            if self.dirty() {
                self.ask(Confirm::Reload);
            } else {
                self.reload();
            }
            return false;
        }
        if self.editing.is_some() {
            let newline = (key.code == Enter && key.modifiers == KeyModifiers::SHIFT)
                || (ctrl && key.code == Char('j'));
            if newline {
                if let Some(edit) = &mut self.editing {
                    if let (Some(input), true) = input_for(edit) {
                        input.newline();
                    }
                }
                return false;
            }
            if plain && key.code == Esc {
                self.leave_input();
                return false;
            }
            if let Some(Editing::Env {
                old,
                delete_focus: true,
                ..
            }) = &self.editing
            {
                if plain && key.code == Enter {
                    let confirm = old.clone().map_or(Confirm::DeleteNew, Confirm::Delete);
                    self.ask(confirm);
                } else if plain && key.code == Up {
                    if let Some(Editing::Env {
                        delete_focus,
                        value_focus,
                        ..
                    }) = &mut self.editing
                    {
                        *delete_focus = false;
                        *value_focus = true;
                    }
                }
                return false;
            }
            if plain && key.code == Enter {
                if let Some(Editing::Env {
                    value_focus: false, ..
                }) = &self.editing
                {
                    if let Some(Editing::Env { value_focus, .. }) = &mut self.editing {
                        *value_focus = true;
                    }
                    return false;
                }
                self.leave_input();
                return false;
            }
            if plain {
                if let Some(Editing::Env {
                    value,
                    value_focus,
                    delete_available,
                    delete_focus,
                    ..
                }) = &mut self.editing
                {
                    if key.code == Down && *value_focus && *delete_available {
                        let layout = value.layout();
                        if layout.row + 1 == layout.lines.len() {
                            *delete_focus = true;
                            return false;
                        }
                    }
                    if key.code == Down && !*value_focus {
                        *value_focus = true;
                        return false;
                    }
                    if key.code == Up && *value_focus && value.layout().row == 0 {
                        *value_focus = false;
                        return false;
                    }
                }
            }
            if let Some(Editing::Choice(index)) = &mut self.editing {
                if plain {
                    match key.code {
                        Up | Left => *index = (*index + 2) % 3,
                        Down | Right => *index = (*index + 1) % 3,
                        _ => {}
                    }
                }
                return false;
            }
            if let Some(edit) = &mut self.editing {
                if let (Some(input), multiline) = input_for(edit) {
                    if ctrl {
                        match key.code {
                            Char('z') => input.undo(false),
                            Char('y') => input.undo(true),
                            Home => input.home(true),
                            End => input.end(true),
                            _ => {}
                        }
                    } else if plain || key.modifiers == KeyModifiers::SHIFT {
                        match key.code {
                            Left if plain => input.left(),
                            Right if plain => input.right(),
                            Up if plain && multiline => input.vertical(-1),
                            Down if plain && multiline => input.vertical(1),
                            Home if plain => input.home(false),
                            End if plain => input.end(false),
                            PageUp if plain && multiline => {
                                input.vertical(-(input.viewport_height.max(1) as isize))
                            }
                            PageDown if plain && multiline => {
                                input.vertical(input.viewport_height.max(1) as isize)
                            }
                            Backspace if plain => input.backspace(),
                            Delete if plain => input.delete(),
                            Char(c) => input.insert(&c.to_string()),
                            _ => {}
                        }
                    }
                }
            }
            return false;
        }
        if ctrl {
            match key.code {
                Char('z') => self.undo(false),
                Char('y') => self.undo(true),
                _ => {}
            }
            return false;
        }
        if !plain {
            return false;
        }
        match key.code {
            Left | Right => {
                self.tab = (self.tab + if key.code == Left { 3 } else { 1 }) % 4;
                self.selected = 0;
                self.inline_error = None;
            }
            Up => self.move_focus(true),
            Down => self.move_focus(false),
            Enter => self.begin(),
            Char(' ') if self.tab < 3 && self.selected < self.count() => self.toggle(self.field()),
            Char('a') if self.tab == 3 => {
                self.selected = self.config.env.len();
                self.begin_env(true);
            }
            _ => {}
        }
        false
    }
}
pub(super) fn input_for(edit: &mut Editing) -> (Option<&mut Buffer>, bool) {
    match edit {
        Editing::Text { field, input, .. } => (Some(input), *field == 1),
        Editing::Env {
            delete_focus: true, ..
        } => (None, false),
        Editing::Env {
            key,
            value,
            value_focus,
            ..
        } => {
            if *value_focus {
                (Some(value), true)
            } else {
                (Some(key), false)
            }
        }
        _ => (None, false),
    }
}
fn policies() -> [RestartPolicy; 3] {
    [
        RestartPolicy::Never,
        RestartPolicy::OnFailure,
        RestartPolicy::Always,
    ]
}
fn policy_index(policy: RestartPolicy) -> usize {
    policies().iter().position(|p| *p == policy).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode::*, KeyEventKind};
    fn key(f: &mut Form, code: KeyCode) -> bool {
        f.key(KeyEvent::new(code, KeyModifiers::NONE), 16)
    }
    fn ctrl(f: &mut Form, c: char) -> bool {
        f.key(KeyEvent::new(Char(c), KeyModifiers::CONTROL), 16)
    }
    fn fixture() -> (tempfile::TempDir, Form) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "{name:'api',command:'true',env:{A:'one'},persist_logs:true}",
        )
        .unwrap();
        let form = Form::open(&path).unwrap();
        (dir, form)
    }
    #[test]
    fn enter_finishes_shift_enter_and_ctrl_j_insert_newlines() {
        let (_dir, mut f) = fixture();
        key(&mut f, Down);
        key(&mut f, Enter);
        f.key(KeyEvent::new(Enter, KeyModifiers::SHIFT), 16);
        f.paste("echo second");
        ctrl(&mut f, 'j');
        f.paste("echo third");
        key(&mut f, Enter);
        assert!(f.editing.is_none());
        assert_eq!(f.config.command, "true\necho second\necho third");
        assert!(f.dirty());
        assert_eq!(f.document.config.command, "true");
        key(&mut f, Up);
        key(&mut f, Enter);
        ctrl(&mut f, 'j');
        f.key(KeyEvent::new(Enter, KeyModifiers::SHIFT), 16);
        key(&mut f, Enter);
        assert_eq!(f.config.name, "api");
    }
    #[test]
    fn escape_keeps_input_and_navigation_stays_in_panel() {
        let (_dir, mut f) = fixture();
        key(&mut f, Enter);
        f.paste("-new");
        key(&mut f, Esc);
        assert_eq!(f.config.name, "api-new");
        key(&mut f, Enter);
        f.paste("er");
        key(&mut f, Tab);
        key(&mut f, BackTab);
        assert_eq!(f.selected, 0);
        assert!(f.editing.is_some());
        ctrl(&mut f, 's');
        assert_eq!(f.document.config.name, "api");
        key(&mut f, Esc);
        assert_eq!(f.config.name, "api-newer");
        key(&mut f, Tab);
        assert_eq!(f.selected, 0);
        for tab in 0..4 {
            f.tab = tab;
            f.selected = 0;
            key(&mut f, Up);
            assert_eq!(f.selected, f.count() - 1);
            key(&mut f, Down);
            assert_eq!(f.selected, 0);
            for _ in 0..f.count() {
                key(&mut f, Down);
            }
            assert_eq!(f.selected, 0);
        }
        ctrl(&mut f, 's');
        assert!(!f.dirty());
        assert!(key(&mut f, Esc));
    }
    #[test]
    fn invalid_drafts_survive_navigation_and_save_returns_to_error() {
        let (_dir, mut f) = fixture();
        f.tab = 2;
        f.selected = 1;
        key(&mut f, Enter);
        f.paste("invalid");
        key(&mut f, Esc);
        assert!(f.editing.is_none());
        assert!(f.value(7).ends_with("invalid"));
        assert_eq!(f.config.log_max_bytes, 10 * 1024 * 1024);
        key(&mut f, Right);
        ctrl(&mut f, 's');
        assert_eq!(f.field(), 7);
        assert!(f.editing.is_some());
        assert!(f.inline_error.is_some());
        ctrl(&mut f, 'z');
        key(&mut f, Enter);
        ctrl(&mut f, 's');
        assert!(!f.dirty());
    }
    #[test]
    fn environment_draft_is_atomic_and_can_be_resumed_or_deleted() {
        let (_dir, mut f) = fixture();
        f.tab = 3;
        key(&mut f, Char('a'));
        f.paste("A");
        key(&mut f, Enter);
        f.paste("two");
        key(&mut f, Esc);
        assert_eq!(f.config.env["A"], "one");
        assert_eq!(f.env_rows().len(), 2);
        key(&mut f, Char('a')); // Resume, never replace an unfinished addition.
        key(&mut f, Up);
        key(&mut f, Backspace);
        f.paste("B");
        key(&mut f, Down);
        ctrl(&mut f, 'j');
        f.paste("three");
        key(&mut f, Enter);
        assert_eq!(f.config.env["B"], "two\nthree");
        assert_eq!(f.config.env["A"], "one");
        f.selected = 1;
        key(&mut f, Char('d'));
        assert!(f.confirm.is_none());
        key(&mut f, Enter);
        key(&mut f, Down);
        ctrl(&mut f, 'e');
        f.key(KeyEvent::new(End, KeyModifiers::CONTROL), 16);
        key(&mut f, Down);
        assert!(matches!(
            &f.editing,
            Some(Editing::Env {
                delete_focus: true,
                ..
            })
        ));
        key(&mut f, Enter);
        key(&mut f, Enter);
        assert_eq!(f.config.env.len(), 2);
        key(&mut f, Enter);
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(!f.config.env.contains_key("B"));
        assert!(f.editing.is_none());
        ctrl(&mut f, 'z');
        assert!(f.config.env.contains_key("B"));
        key(&mut f, Char('a'));
        f.paste("A");
        key(&mut f, Esc);
        f.selected = 2;
        key(&mut f, Enter);
        key(&mut f, Down);
        key(&mut f, Down);
        key(&mut f, Enter);
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(f.drafts.is_empty());
        assert!(f.editing.is_none());
    }
    #[test]
    fn confirmations_default_to_keep_and_reject_modifiers_and_repeats() {
        let (_dir, mut f) = fixture();
        key(&mut f, Enter);
        f.paste("-new");
        ctrl(&mut f, 'q');
        assert!(f.confirm.is_some());
        assert!(!key(&mut f, Enter));
        assert!(f.editing.is_some());
        ctrl(&mut f, 'q');
        key(&mut f, Up);
        assert_eq!(f.confirm_selected, 2);
        assert!(!f.key(KeyEvent::new(Enter, KeyModifiers::ALT), 16));
        assert!(f.confirm.is_some());
        assert!(!f.key(
            KeyEvent::new_with_kind(Enter, KeyModifiers::NONE, KeyEventKind::Repeat),
            16
        ));
        assert!(f.confirm.is_some());
        assert!(key(&mut f, Enter));
        assert_eq!(f.document.config.name, "api");
    }
    #[test]
    fn switches_undo_and_reload_keep_protocol_capability() {
        let (_dir, mut f) = fixture();
        f.enhanced_keyboard = true;
        f.tab = 1;
        key(&mut f, Enter);
        key(&mut f, Down);
        key(&mut f, Enter);
        assert_eq!(f.config.restart, RestartPolicy::OnFailure);
        key(&mut f, Down);
        key(&mut f, Char(' '));
        assert!(!f.config.tty);
        assert!(f.disabled(5));
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(f.config.sync_rows_cols);
        ctrl(&mut f, 'z');
        assert!(f.config.tty);
        ctrl(&mut f, 'y');
        assert!(!f.config.tty);
        ctrl(&mut f, 'r');
        key(&mut f, Enter);
        assert!(!f.config.tty);
        ctrl(&mut f, 'r');
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(f.config.tty);
        assert!(f.enhanced_keyboard);
    }
    #[test]
    fn environment_focus_uses_visual_boundaries_and_preserves_cursors() {
        let (_dir, mut f) = fixture();
        f.tab = 3;
        key(&mut f, Enter);
        key(&mut f, Down);
        if let Some(Editing::Env { value, .. }) = &mut f.editing {
            value.text = "abcdefghijklmnop".into();
            value.cursor = 6;
            value.viewport_width = 5;
        }
        key(&mut f, Up);
        assert!(
            matches!(&f.editing, Some(Editing::Env {value_focus:true,value,..}) if value.cursor == 1)
        );
        key(&mut f, Up);
        assert!(matches!(
            &f.editing,
            Some(Editing::Env {
                value_focus: false,
                ..
            })
        ));
        key(&mut f, Down);
        assert!(
            matches!(&f.editing, Some(Editing::Env {value_focus:true,value,..}) if value.cursor == 1)
        );
        key(&mut f, Tab);
        key(&mut f, BackTab);
        assert!(
            matches!(&f.editing, Some(Editing::Env {value_focus:true,value,..}) if value.cursor == 1)
        );
        ctrl(&mut f, 's');
        assert_eq!(f.document.config.env["A"], "one");
        key(&mut f, Esc);
        ctrl(&mut f, 's');
        assert_eq!(f.document.config.env["A"], "abcdefghijklmnop");
    }
    #[test]
    fn delete_targets_original_identity_and_cancel_keeps_rename_input() {
        let (_dir, mut f) = fixture();
        f.tab = 3;
        key(&mut f, Enter);
        f.paste("renamed-");
        key(&mut f, Char('d'));
        key(&mut f, Down);
        f.key(KeyEvent::new(End, KeyModifiers::CONTROL), 16);
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(matches!(&f.confirm,Some(Confirm::Delete(name)) if name=="A"));
        key(&mut f, Esc);
        assert!(
            matches!(&f.editing,Some(Editing::Env{key,delete_focus:true,..}) if key.text=="renamed-dA")
        );
        key(&mut f, Enter);
        key(&mut f, Down);
        key(&mut f, Enter);
        assert!(f.config.env.is_empty());
        assert_eq!(f.document.config.env["A"], "one");
        assert!(f.dirty());
        ctrl(&mut f, 's');
        assert!(Form::open(&f.document.path).unwrap().config.env.is_empty());
    }
}
