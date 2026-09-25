//! Service configuration form; source edits are delegated to an external editor.
mod document;
mod input;
mod model;
mod restart;
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
    owns_terminal: bool,
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.enhanced {
            let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout(), DisableBracketedPaste, Show);
        if self.owns_terminal {
            let _ = execute!(stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
        }
    }
}
pub(crate) async fn run(path: &Path) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("configuration form requires a terminal; use --path or --editor");
    }
    let mut form = Form::open(path)?;
    let paths = crate::paths::ServedPaths::from_environment()?;
    enable_raw_mode().context("enable form raw mode")?;
    let _guard = TerminalGuard {
        enhanced: false,
        owns_terminal: true,
    };
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    run_form(&mut terminal, &paths, &mut form, None).await
}

pub(super) async fn edit_in_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &crate::paths::ServedPaths,
    path: &Path,
    name: &str,
) -> Result<()> {
    let mut form = Form::open(path)?;
    run_form(terminal, paths, &mut form, Some(name)).await
}

async fn run_form(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &crate::paths::ServedPaths,
    form: &mut Form,
    service_name: Option<&str>,
) -> Result<()> {
    let mut guard = TerminalGuard {
        enhanced: false,
        owns_terminal: false,
    };
    execute!(stdout(), EnableBracketedPaste)?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        guard.enhanced = true;
        form.enhanced_keyboard = true;
    }
    terminal.clear()?;
    loop {
        terminal.draw(|frame| view::draw(frame, form))?;
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
                let exit = form.key(key, usize::from(size.height.saturating_sub(8).max(1)));
                if std::mem::take(&mut form.saved_change) {
                    if let Err(error) = restart::after_save(
                        terminal,
                        paths,
                        &form.document.path,
                        &form.config.name,
                        service_name,
                    )
                    .await
                    {
                        form.error = Some(format!("Configuration saved. {error:#}"));
                        form.reader_scroll = 0;
                        continue;
                    }
                }
                if exit {
                    break;
                }
            }
            Event::Paste(text) if usable => form.paste(&text),
            _ => {}
        }
    }
    Ok(())
}
