//! Service configuration form; source edits are delegated to an external editor.
mod document;
mod input;
mod model;
mod source;
mod view;
use anyhow::{Context, Result, bail};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use model::Form;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::{self, IsTerminal, stdout},
    path::Path,
    time::Duration,
};
struct TerminalGuard {
    enhanced: bool,
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.enhanced {
            let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout(), DisableBracketedPaste, Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
pub(crate) fn run(path: &Path) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("configuration form requires a terminal; use --path or --editor");
    }
    let mut form = Form::open(path)?;
    enable_raw_mode().context("enable form raw mode")?;
    let mut guard = TerminalGuard { enhanced: false };
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        guard.enhanced = true;
        form.enhanced_keyboard = true;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    loop {
        terminal.draw(|frame| view::draw(frame, &mut form))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let size = terminal.size()?;
        let usable = size.width >= 40 && size.height >= 10;
        match event::read()? {
            Event::Key(key) => {
                let exit = key.code == KeyCode::Esc
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('q' | 'c')));
                if !usable && !exit {
                    continue;
                }
                if form.key(key, usize::from(size.height.saturating_sub(8).max(1))) {
                    break;
                }
            }
            Event::Paste(text) if usable => form.paste(&text),
            _ => {}
        }
    }
    Ok(())
}
