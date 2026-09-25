use super::super::{
    model::Reader,
    view::{
        Footer, clip, colors_enabled, danger_style, draw_help, draw_reader, key_hints, wrapped,
    },
};
use super::{
    input::{Buffer, cells, visible_grapheme},
    model::{Confirm, Editing, Form, LABELS, TABS},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;

fn color(c: Color) -> Style {
    if colors_enabled() {
        Style::default().fg(c)
    } else {
        Style::default()
    }
}
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn whitespace(base: Style) -> Style {
    if !colors_enabled() {
        return base.add_modifier(Modifier::DIM);
    }
    let base = base.remove_modifier(Modifier::DIM);
    // Reverse swaps foreground and background; keep the selection background intact.
    if base.add_modifier.contains(Modifier::REVERSED) {
        base.bg(Color::Indexed(242))
    } else {
        base.fg(Color::Indexed(242))
    }
}

fn selected() -> Style {
    Style::default()
        .fg(Color::Reset)
        .bg(Color::Reset)
        .add_modifier(Modifier::REVERSED)
}
fn put(frame: &mut Frame<'_>, area: Rect, text: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(Span::styled(clip(text, area.width as usize, false), style)),
        area,
    );
}
fn row(area: Rect, y: u16) -> Rect {
    Rect::new(area.x, y, area.width, 1)
}

fn hints(frame: &mut Frame<'_>, area: Rect, keys: Footer) {
    frame.render_widget(Paragraph::new(key_hints(keys, "  ")), area);
}

fn error_lines(form: &mut Form, width: u16, capacity: u16) -> Vec<String> {
    let Some(error) = &form.inline_error else {
        return Vec::new();
    };
    if form.inline_error_acknowledged {
        return vec![clip(&error.replace('\n', " "), usize::from(width), false)];
    }
    let lines = wrapped(error, usize::from(width));
    if lines.len() > usize::from(capacity.min(3)) {
        form.error = Some(error.clone());
        form.inline_error_acknowledged = true;
        form.reader_scroll = 0;
        return Vec::new();
    }
    lines
}

fn draw_error(frame: &mut Frame<'_>, form: &mut Form) {
    let mut reader = Reader::error("Error / edit", form.error.as_deref().unwrap());
    reader.scroll = form.reader_scroll;
    draw_reader(frame, &mut reader, &[("esc", "back")]);
    form.reader_scroll = reader.scroll;
}

fn draw_errors(frame: &mut Frame<'_>, work: Rect, y: u16, lines: Vec<String>) {
    let height = lines.len() as u16;
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .style(color(Color::Red)),
        Rect::new(work.x, y, work.width, height),
    );
}

fn draw_confirmation(frame: &mut Frame<'_>, form: &mut Form) {
    let all = frame.area();
    let margin = if all.width >= 80 { 2 } else { 1 };
    let work = Rect::new(
        all.x + margin,
        all.y,
        (all.width - 2 * margin).min(76),
        all.height,
    );
    let title_y = all.y + u16::from(all.height >= 18);
    let body_y = title_y + 3;
    let confirm = form.confirm.as_ref().unwrap();
    let (title, message, options, dangerous): (&str, String, &[&str], usize) = match confirm {
        Confirm::Exit => (
            "Save changes?",
            "Save changes before closing?".into(),
            &["Continue editing", "Save and exit", "Discard changes"],
            2,
        ),
        Confirm::Reload => (
            "Reload configuration?",
            "Discard changes and reload?".into(),
            &["Cancel", "Reload"],
            1,
        ),
        Confirm::DeleteNew => (
            "Delete variable?",
            "Discard new variable draft?".into(),
            &["Cancel", "Delete"],
            1,
        ),
        Confirm::Delete(key) => (
            "Delete variable?",
            format!("Delete variable {key}?"),
            &["Cancel", "Delete"],
            1,
        ),
    };
    let tone = color(if matches!(confirm, Confirm::Exit) {
        Color::Yellow
    } else {
        Color::Red
    });
    put(
        frame,
        row(work, title_y),
        title,
        color(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    put(
        frame,
        row(work, title_y + 1),
        &clip(
            &form.document.path.to_string_lossy(),
            usize::from(work.width),
            true,
        ),
        dim(),
    );
    let lines = wrapped(&message, usize::from(work.width));
    let capacity = usize::from(
        (all.bottom() - body_y - 3)
            .saturating_sub(options.len() as u16)
            .max(1),
    );
    let visible = lines.len().min(capacity);
    form.reader_scroll = form.reader_scroll.min(lines.len().saturating_sub(visible));
    frame.render_widget(
        Paragraph::new(
            lines
                .iter()
                .skip(form.reader_scroll)
                .take(visible)
                .cloned()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .style(tone),
        Rect::new(work.x, body_y, work.width, visible as u16),
    );
    for (index, option) in options.iter().enumerate() {
        put(
            frame,
            row(work, body_y + visible as u16 + index as u16),
            option,
            if index == dangerous {
                danger_style(index == form.confirm_selected)
            } else if index == form.confirm_selected {
                selected()
            } else {
                Style::default()
            },
        );
    }
    let footer_y = body_y + visible as u16 + options.len() as u16 + 1;
    hints(
        frame,
        row(work, footer_y),
        &[("enter", "confirm"), ("esc", "back")],
    );
    if lines.len() > visible {
        let position = format!("{}/{}", form.reader_scroll + 1, lines.len());
        let width = cells(&position) as u16;
        put(
            frame,
            Rect::new(work.right() - width, footer_y, width, 1),
            &position,
            dim(),
        );
    }
}

pub(super) fn draw(frame: &mut Frame<'_>, form: &mut Form) {
    let all = frame.area();
    if all.width < 40 || all.height < 10 {
        let text = match &form.confirm {
            Some(Confirm::Exit) => "Unsaved changes\nResize to choose; Esc returns",
            Some(Confirm::Reload) => "Discard changes?\nResize to choose; Esc cancels",
            Some(Confirm::Delete(_) | Confirm::DeleteNew) => {
                "Delete variable?\nResize to choose; Esc cancels"
            }
            None => "Resize to 40x10\nCtrl+Q exit (confirm changes)",
        };
        frame.render_widget(Paragraph::new(text), all);
        return;
    }
    if form.help {
        let (title, content) = form.help_content();
        draw_help(
            frame,
            title,
            &content,
            &mut form.reader_scroll,
            &[("esc", "back")],
        );
        return;
    }
    if form.error.is_some() {
        draw_error(frame, form);
        return;
    }
    if form.confirm.is_some() {
        draw_confirmation(frame, form);
        return;
    }
    if form.editing.is_some() {
        draw_editor(frame, form);
        return;
    }
    let margin = if all.width >= 80 { 2 } else { 1 };
    let work = Rect::new(
        all.x + margin,
        all.y,
        (all.width - 2 * margin).min(76),
        all.height,
    );
    let compact = all.height < 18;
    let title_y = all.y + if compact { 0 } else { 1 };
    let tabs_y = all.y + if compact { 2 } else { 4 };
    let available = all.bottom() - 2 - (tabs_y + 2);
    let errors = error_lines(form, work.width, available.saturating_sub(2));
    if form.error.is_some() {
        draw_error(frame, form);
        return;
    }
    let body = Rect::new(
        work.x,
        tabs_y + 2,
        work.width,
        available - errors.len() as u16,
    );
    let title = if form.dirty() {
        "served / edit · modified"
    } else {
        "served / edit"
    };
    put(
        frame,
        row(work, title_y),
        title,
        color(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    put(
        frame,
        row(work, title_y + 1),
        &clip(
            &form.document.path.to_string_lossy(),
            work.width as usize,
            true,
        ),
        dim(),
    );
    let tabs = if work.width < 50 {
        vec!["Basic", "Run", "Logs", "Env"]
    } else {
        TABS.to_vec()
    };
    let mut spans = Vec::new();
    for (i, tab) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            *tab,
            if i == form.tab {
                color(Color::Cyan).add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
            } else {
                dim()
            },
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), row(work, tabs_y));
    if form.tab < 3 {
        let stacked = body.width < 60;
        let step = if stacked { 3 } else { 2 };
        let visible = (body.height as usize / step).max(1);
        let start = (form.selected.min(form.count() - 1) + 1).saturating_sub(visible);
        for i in start..3 {
            let y = body.y + ((i - start) * step) as u16;
            if y >= body.bottom() {
                break;
            }
            let field = form.tab * 3 + i;
            let disabled = form.disabled(field);
            let raw = form.value(field);
            let value = match field {
                2 if raw.is_empty() => "(config directory)".into(),
                3 => format!("< {raw} >"),
                4..=6 => {
                    if raw == "true" {
                        "[ on ]".into()
                    } else {
                        "[ off ]".into()
                    }
                }
                _ => raw.replace('\n', " ↵ "),
            };
            let style = if disabled {
                dim()
            } else if i == form.selected {
                selected()
            } else if matches!(field, 4..=6) && raw == "true" {
                color(Color::Green)
            } else if field == 3 {
                color(Color::Cyan)
            } else {
                Style::default()
            };
            if stacked {
                put(
                    frame,
                    row(body, y),
                    LABELS[field],
                    if disabled { dim() } else { color(Color::Cyan) },
                );
                if y + 1 < body.bottom() {
                    let area = Rect::new(body.x + 2, y + 1, body.width - 2, 1);
                    if field == 1 {
                        put_preview(frame, area, &raw, style);
                    } else {
                        put(frame, area, &value, style);
                    }
                }
            } else {
                put(
                    frame,
                    Rect::new(body.x + 2, y, 22, 1),
                    LABELS[field],
                    if disabled { dim() } else { Style::default() },
                );
                let area = Rect::new(body.x + 25, y, body.width - 25, 1);
                if field == 1 {
                    put_preview(frame, area, &raw, style);
                } else {
                    put(frame, area, &value, style);
                }
            }
        }
    } else {
        if form.env_rows().is_empty() {
            put(frame, row(body, body.y), "No variables", dim());
        }
        let step = 2;
        let visible = (body.height as usize / step).max(1);
        let start = (form.selected.min(form.count() - 1) + 1).saturating_sub(visible);
        for (i, (key, value)) in form.env_rows().iter().enumerate().skip(start).take(visible) {
            let y = body.y + ((i - start) * step) as u16;
            let style = if i == form.selected {
                selected()
            } else {
                Style::default()
            };
            put(
                frame,
                row(body, y),
                key,
                if i == form.selected {
                    selected()
                } else {
                    color(Color::Cyan)
                },
            );
            if y + 1 < body.bottom() {
                put_preview(
                    frame,
                    Rect::new(body.x + 2, y + 1, body.width - 2, 1),
                    value,
                    style,
                );
            }
        }
    }
    draw_errors(frame, work, body.bottom(), errors);
    put(
        frame,
        row(work, all.bottom() - 2),
        &"─".repeat(work.width as usize),
        dim(),
    );
    hints(
        frame,
        row(work, all.bottom() - 1),
        if form.tab == 3 {
            &[
                ("ctrl+s", "save"),
                ("esc", "quit"),
                ("a", "add"),
                ("?", "help"),
            ]
        } else {
            &[("ctrl+s", "save"), ("esc", "quit"), ("?", "help")]
        },
    );
}

fn draw_editor(frame: &mut Frame<'_>, form: &mut Form) {
    let all = frame.area();
    let margin = if all.width >= 80 { 2 } else { 1 };
    let work = Rect::new(all.x + margin, all.y, all.width - 2 * margin, all.height);
    let title_y = all.y + u16::from(all.height >= 18);
    let body_y = title_y + if all.height < 18 { 2 } else { 3 };
    let minimum = match form.editing.as_ref().unwrap() {
        Editing::Env {
            delete_available, ..
        } => 4 + u16::from(*delete_available),
        _ => 1,
    };
    let errors = error_lines(
        form,
        work.width,
        (all.bottom() - 2 - body_y).saturating_sub(minimum),
    );
    if form.error.is_some() {
        draw_error(frame, form);
        return;
    }
    let bottom = all.bottom() - 2 - errors.len() as u16;
    let body = Rect::new(work.x, body_y, work.width, bottom - body_y);
    let label = match form.editing.as_ref().unwrap() {
        Editing::Text { field, .. } => LABELS[*field],
        Editing::Choice(_) => "Restart policy",
        Editing::Env { .. } => "Environment",
    };
    let suffix = if form.dirty() { " · modified" } else { "" };
    let prefix = clip(
        &format!("served / edit · {label}"),
        usize::from(work.width).saturating_sub(cells(suffix)),
        false,
    );
    let title = format!("{prefix}{suffix}");
    put(
        frame,
        row(work, title_y),
        &title,
        color(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    put(
        frame,
        row(work, title_y + 1),
        &clip(
            &form.document.path.to_string_lossy(),
            work.width as usize,
            true,
        ),
        dim(),
    );
    match form.editing.as_mut().unwrap() {
        Editing::Text { field, input, .. } => {
            if *field == 1 {
                draw_wrapped_input(frame, body, input, true);
            } else {
                draw_single_input(frame, body, input, true);
            }
        }
        Editing::Env {
            key,
            value,
            value_focus,
            delete_available,
            delete_focus,
            ..
        } => {
            put(
                frame,
                row(body, body.y),
                "Name",
                if *value_focus {
                    dim()
                } else {
                    color(Color::Cyan)
                },
            );
            draw_single_input(
                frame,
                Rect::new(body.x + 2, body.y + 1, body.width - 2, 1),
                key,
                !*value_focus,
            );
            put(
                frame,
                row(body, body.y + 2),
                "Value",
                if *value_focus && !*delete_focus {
                    color(Color::Cyan)
                } else {
                    dim()
                },
            );
            draw_wrapped_input(
                frame,
                Rect::new(
                    body.x + 2,
                    body.y + 3,
                    body.width - 2,
                    body.height.saturating_sub(3 + u16::from(*delete_available)),
                ),
                value,
                *value_focus && !*delete_focus,
            );
            if *delete_available {
                put(
                    frame,
                    row(body, body.bottom() - 1),
                    "[ Delete ]",
                    danger_style(*delete_focus),
                );
            }
        }
        Editing::Choice(index) => {
            let start = (*index + 1).saturating_sub(body.height as usize);
            for (i, option) in ["never", "on-failure", "always"]
                .iter()
                .enumerate()
                .skip(start)
                .take(body.height as usize)
            {
                put(
                    frame,
                    row(body, body.y + (i - start) as u16),
                    option,
                    if i == *index {
                        selected()
                    } else {
                        Style::default()
                    },
                );
            }
        }
    }
    draw_errors(frame, work, bottom, errors);
    put(
        frame,
        row(work, all.bottom() - 2),
        &"─".repeat(work.width as usize),
        dim(),
    );
    hints(frame, row(work, all.bottom() - 1), &[("esc", "back")]);
}

fn draw_wrapped_input(frame: &mut Frame<'_>, area: Rect, input: &mut Buffer, focused: bool) {
    if area.height == 0 || area.width < 2 {
        return;
    }
    input.viewport_width = usize::from(area.width - 1);
    input.viewport_height = usize::from(area.height);
    let layout = input.layout();
    let height = input.viewport_height;
    input.scroll = input
        .scroll
        .min(layout.row)
        .max((layout.row + 1).saturating_sub(height));
    input.scroll = input.scroll.min(layout.lines.len().saturating_sub(height));
    for (i, line) in layout
        .lines
        .iter()
        .skip(input.scroll)
        .take(height)
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(Line::from({
                let mut spans = Vec::new();
                let mut at = 0;
                for mark in &line.marks {
                    if at < mark.start {
                        spans.push(Span::raw(&line.text[at..mark.start]));
                    }
                    spans.push(Span::styled(
                        &line.text[mark.clone()],
                        whitespace(Style::default()),
                    ));
                    at = mark.end;
                }
                if at < line.text.len() {
                    spans.push(Span::raw(&line.text[at..]));
                }
                spans
            })),
            Rect::new(area.x, area.y + i as u16, area.width - 1, 1),
        );
    }
    if layout.lines.len() > height {
        let thumb = (height * height / layout.lines.len()).max(1);
        let offset = input.scroll * (height - thumb) / (layout.lines.len() - height);
        for i in 0..height {
            let active = (offset..offset + thumb).contains(&i);
            put(
                frame,
                Rect::new(area.right() - 1, area.y + i as u16, 1, 1),
                if active { "┃" } else { "│" },
                if active { color(Color::Cyan) } else { dim() },
            );
        }
    }
    if focused {
        frame.set_cursor_position((
            area.x + layout.column as u16,
            area.y + (layout.row - input.scroll) as u16,
        ));
    }
}

fn put_preview(frame: &mut Frame<'_>, area: Rect, text: &str, base: Style) {
    let mut spans = Vec::new();
    let mut used = 0;
    let glyphs: Vec<_> = text.graphemes(true).map(visible_grapheme).collect();
    let total: usize = glyphs.iter().map(|(text, _)| cells(text)).sum();
    let limit = area.width as usize - usize::from(total > area.width as usize && area.width > 0);
    for (glyph, marked) in glyphs {
        if used + cells(&glyph) > limit {
            break;
        }
        used += cells(&glyph);
        spans.push(Span::styled(
            glyph,
            if marked { whitespace(base) } else { base },
        ));
    }
    if total > area.width as usize {
        spans.push(Span::styled("…", base));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_single_input(frame: &mut Frame<'_>, area: Rect, input: &Buffer, focused: bool) {
    if area.height == 0 {
        return;
    }
    let lines: Vec<_> = input.text.split('\n').collect();
    let row_index = input.row();
    let top = (row_index + 1).saturating_sub(area.height as usize);
    let left = (input.column() + 1).saturating_sub(area.width as usize);
    for (index, line) in lines
        .iter()
        .enumerate()
        .skip(top)
        .take(area.height as usize)
    {
        let mut at = 0;
        let mut text = String::new();
        for g in line.graphemes(true) {
            let display = if g == "\t" {
                "    ".into()
            } else if g.chars().any(char::is_control) {
                "�".into()
            } else {
                g.to_owned()
            };
            let end = at + cells(&display);
            if at >= left && end <= left + area.width as usize {
                text.push_str(&display);
            } else if end > left && at < left + area.width as usize {
                text.push_str(&" ".repeat(end.min(left + area.width as usize) - at.max(left)));
            }
            at = end;
        }
        frame.render_widget(
            Paragraph::new(text),
            row(area, area.y + (index - top) as u16),
        );
    }
    if focused {
        frame.set_cursor_position((
            area.x + (input.column() - left) as u16,
            area.y + (row_index - top) as u16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn pages_resize_and_export_actual_cells() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".served.json5");
        std::fs::write(&path, "{name:'pixiv-api',command:'uv run python -m app',cwd:'/srv/pixiv-api',restart:'on-failure',persist_logs:true,env:{API_BASE:'https://example.com',LANG:'zh_CN.UTF-8',PORT:'8080'}}").unwrap();
        export_readable_editors(&path);
        let mut form = Form::open(&path).unwrap();
        for (width, height) in [(40, 10), (59, 18), (80, 24), (120, 40), (25, 6)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for tab in 0..4 {
                form.tab = tab;
                for selected in 0..3 {
                    form.selected = selected;
                    terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                    let buffer = terminal.backend().buffer();
                    let plain = buffer
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>();
                    if width >= 40 {
                        assert!(plain.contains("ctrl+s save"));
                        assert!(plain.contains("? help"));
                        assert_eq!(plain.contains("a add"), tab == 3);
                        assert!(plain.contains(if tab == 3 {
                            ["API_BASE", "LANG", "PORT"][selected]
                        } else {
                            LABELS[tab * 3 + selected]
                        }));
                    }
                    if width == 80 && selected == 0 {
                        export(buffer, &TABS[tab].to_lowercase());
                    }
                }
            }
        }
    }

    #[test]
    fn modified_title_and_colored_keys_survive_narrow_windows() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "{name:'api',command:'true'}").unwrap();
        for (width, height) in [(40, 10), (80, 24), (120, 40)] {
            let mut form = Form::open(&path).unwrap();
            form.selected = 2;
            form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 2);
            form.paste("/a/long/working/directory");
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut form)).unwrap();
            let buf = terminal.backend().buffer();
            let title_y = u16::from(height >= 18);
            let title: String = (0..width).map(|x| buf[(x, title_y)].symbol()).collect();
            assert!(title.trim_end().ends_with(" · modified"));
            let x = if width >= 80 { 2 } else { 1 };
            assert_eq!(
                buf[(x, height - 1)].fg,
                if colors_enabled() {
                    Color::Cyan
                } else {
                    Color::Reset
                }
            );
            assert!(!buf[(x, height - 1)].modifier.contains(Modifier::DIM));
            assert!(buf[(x + 4, height - 1)].modifier.contains(Modifier::DIM));
            export(buf, &format!("modified-title-{width}"));
        }
    }

    #[test]
    fn validation_errors_wrap_or_open_reader_without_losing_drafts() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let source = "{name:'api',command:'true',env:{KEY:'value'}}";
        std::fs::write(&path, source).unwrap();
        let mut form = Form::open(&path).unwrap();
        form.tab = 3;
        form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 2);
        form.paste("=");
        form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 2);
        form.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL), 2);
        let original = match form.editing.as_ref().unwrap() {
            Editing::Env { key, value, .. } => (key.text.clone(), key.cursor, value.text.clone()),
            _ => unreachable!(),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(form.error.is_some());
        export(terminal.backend().buffer(), "validation-reader-40");
        form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 2);
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(form.error.is_none());
        assert!(form.inline_error_acknowledged);
        match form.editing.as_ref().unwrap() {
            Editing::Env { key, value, .. } => {
                assert_eq!((key.text.clone(), key.cursor, value.text.clone()), original)
            }
            _ => unreachable!(),
        }
        export(terminal.backend().buffer(), "validation-return-40");
        form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 2);
        form.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL), 2);
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(form.error.is_some(), "retry must show the full error again");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);

        let mut form = Form::open(&path).unwrap();
        form.inline_error = Some("Use letters, digits, '.', '_' or '-' for the name.".into());
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(form.error.is_none());
        let buf = terminal.backend().buffer();
        let message: String = (6..8)
            .flat_map(|y| (1..39).map(move |x| buf[(x, y)].symbol()))
            .collect();
        assert!(message.trim_end().ends_with("for the name."));
        export(buf, "validation-inline-40");
        form.inline_error = Some(format!("{}TAIL", "Long validation message. ".repeat(20)));
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(form.error.is_some());
        form.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), 2);
        terminal.draw(|frame| draw(frame, &mut form)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .contains("TAIL")
        );
    }

    #[test]
    fn compact_confirmations_keep_options_visible_while_message_scrolls() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "{name:'api',command:'true'}").unwrap();
        for (width, height) in [(40, 10), (80, 24), (120, 40)] {
            let mut form = Form::open(&path).unwrap();
            form.confirm = Some(Confirm::Delete(format!("{}END", "LONG_NAME_".repeat(300))));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for key in [KeyCode::Home, KeyCode::PageDown, KeyCode::End] {
                form.key(KeyEvent::new(key, KeyModifiers::NONE), 2);
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let buf = terminal.backend().buffer();
                let text: String = buf.content.iter().map(|c| c.symbol()).collect();
                assert!(!text.contains("Basic"));
                assert!(text.contains("Cancel"));
                assert!(text.contains("enter confirm  esc back"));
                assert_eq!(form.confirm_selected, 0);
                if key == KeyCode::End {
                    assert!(text.contains("END?"));
                }
                export(buf, &format!("long-confirmation-{width}"));
            }
            form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 2);
            assert!(form.confirm.is_none());
        }
    }

    #[test]
    fn destructive_confirmations_keep_semantic_colors_when_selected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "{name:'api',command:'true'}").unwrap();
        for (name, confirm, label, danger_index) in [
            ("discard", Confirm::Exit, "Discard changes", 2),
            ("reload", Confirm::Reload, "Reload", 1),
            ("delete", Confirm::Delete("PORT".into()), "Delete", 1),
            ("delete-new", Confirm::DeleteNew, "Delete", 1),
        ] {
            let mut form = Form::open(&path).unwrap();
            form.confirm = Some(confirm);
            for (width, height) in [(40, 10), (80, 24)] {
                for selected_index in [0, danger_index] {
                    form.confirm_selected = selected_index;
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                    let buf = terminal.backend().buffer();
                    let row = buf
                        .content
                        .chunks(width as usize)
                        .find(|row| {
                            row.iter().map(|c| c.symbol()).collect::<String>().trim() == label
                        })
                        .unwrap();
                    let cell = row.iter().find(|c| c.symbol() != " ").unwrap();
                    if colors_enabled() {
                        assert_eq!(
                            cell.fg,
                            if selected_index == danger_index {
                                Color::Black
                            } else {
                                Color::Red
                            }
                        );
                        assert_eq!(
                            cell.bg,
                            if selected_index == danger_index {
                                Color::Red
                            } else {
                                Color::Reset
                            }
                        );
                        assert!(!cell.modifier.contains(Modifier::REVERSED));
                    } else {
                        assert_eq!(cell.fg, Color::Reset);
                        assert_eq!(cell.bg, Color::Reset);
                        assert_eq!(
                            cell.modifier.contains(Modifier::REVERSED),
                            selected_index == danger_index
                        );
                    }
                    let message_y = if height < 18 { 3 } else { 4 };
                    assert_eq!(
                        buf[(if width < 80 { 1 } else { 2 }, message_y)].fg,
                        if !colors_enabled() {
                            Color::Reset
                        } else if name == "discard" {
                            Color::Yellow
                        } else {
                            Color::Red
                        }
                    );
                    export(buf, &format!("{name}-{selected_index}-{width}"));
                }
            }
        }
    }

    #[test]
    fn contextual_help_preserves_editor_and_uses_shared_reader() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".served.json5");
        std::fs::write(
            &path,
            "{name:'api',command:'echo hello',env:{KEY:'first\\nsecond'}}",
        )
        .unwrap();
        let press = |form: &mut Form, code| {
            form.key(KeyEvent::new(code, KeyModifiers::NONE), 2);
        };
        let state = |form: &Form| {
            let buffer = |b: &Buffer| format!("{:?}:{}:{}", b.text, b.cursor, b.scroll);
            match form.editing.as_ref() {
                None => format!("{}:{}", form.tab, form.selected),
                Some(Editing::Text { input, .. }) => buffer(input),
                Some(Editing::Choice(i)) => i.to_string(),
                Some(Editing::Env {
                    key,
                    value,
                    value_focus,
                    delete_focus,
                    ..
                }) => {
                    format!(
                        "{}:{}:{value_focus}:{delete_focus}",
                        buffer(key),
                        buffer(value)
                    )
                }
            }
        };
        for context in [
            "overview",
            "environment",
            "single",
            "command",
            "name",
            "value",
            "delete",
            "choice",
        ] {
            for (width, height) in [(40, 10), (80, 24)] {
                let mut form = Form::open(&path).unwrap();
                form.tab = match context {
                    "environment" | "name" | "value" | "delete" => 3,
                    "choice" => 1,
                    _ => 0,
                };
                form.selected = usize::from(context == "command");
                if !matches!(context, "overview" | "environment") {
                    press(&mut form, KeyCode::Enter);
                }
                if matches!(context, "value" | "delete") {
                    press(&mut form, KeyCode::Down);
                }
                if context == "delete" {
                    form.key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), 2);
                    press(&mut form, KeyCode::Down);
                } else if matches!(context, "single" | "command" | "name" | "value") {
                    form.paste(" draft");
                }
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let before = terminal.backend().buffer().clone();
                if context == "delete" {
                    let cell = before.content.iter().find(|c| c.symbol() == "[").unwrap();
                    assert_eq!(
                        cell.bg,
                        if colors_enabled() {
                            Color::Red
                        } else {
                            Color::Reset
                        }
                    );
                    assert_eq!(
                        cell.fg,
                        if colors_enabled() {
                            Color::Black
                        } else {
                            Color::Reset
                        }
                    );
                    assert_eq!(
                        cell.modifier.contains(Modifier::REVERSED),
                        !colors_enabled()
                    );
                    export(&before, &format!("delete-button-{width}"));
                }
                let input_before = state(&form);
                press(&mut form, KeyCode::F(1));
                let (_, content) = form.help_content();
                assert_eq!(
                    content.contains("Newline"),
                    matches!(context, "command" | "value")
                );
                assert_eq!(content.contains("Request deletion"), context == "delete");
                assert_eq!(content.contains("Add variable"), context == "environment");
                assert_eq!(
                    content.contains("ctrl+s"),
                    matches!(context, "overview" | "environment")
                );
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let buf = terminal.backend().buffer();
                let text: String = buf.content.iter().map(|c| c.symbol()).collect();
                assert!(text.contains("Help /"));
                assert!(text.contains("esc back"));
                assert!(!text.contains(".served.json5"));
                assert!(!text.contains("Basic   "));
                assert_eq!(
                    buf.content.iter().any(|c| c.fg == Color::Cyan),
                    colors_enabled()
                );
                export(buf, &format!("help-{context}-{width}"));
                press(&mut form, KeyCode::PageDown);
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                press(&mut form, KeyCode::Esc);
                assert!(!form.help);
                assert_eq!(state(&form), input_before);
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                assert_eq!(terminal.backend().buffer(), &before);
            }
        }
    }

    fn export_readable_editors(path: &std::path::Path) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        if std::env::var_os("SERVED_FORM_PREVIEWS").is_none() {
            return;
        }
        let command = "uv run python -m app --host 0.0.0.0 --port 8080 --workers 4 --cache-directory /srv/pixiv-api/cache --upstream https://api.example.com/v1 --log-level info\n".to_owned()
            + &(1..20).map(|n|format!("echo 'Warm cache: collection {n:02}'\n")).collect::<String>();
        let value = "{\n  \"endpoint\": \"https://api.example.com/v1/illustrations?language=zh-CN&include=tags,author,statistics&sort=updated_at\",\n  \"cache\": {\n    \"directory\": \"/srv/pixiv-api/cache\",\n    \"ttl\": 3600\n  },\n  \"collections\": [\n".to_owned()
            + &(1..18).map(|n|format!("    \"collection-{n:02}\",\n")).collect::<String>() + "    \"favorites\"\n  ]\n}";
        for env in [false, true] {
            let mut form = Form::open(path).unwrap();
            form.tab = if env { 3 } else { 0 };
            form.selected = usize::from(!env);
            if env {
                form.config.env.clear();
                form.config
                    .env
                    .insert("APPLICATION_CONFIG".into(), value.clone());
            }
            form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 16);
            match form.editing.as_mut().unwrap() {
                Editing::Text { input, .. } => *input = Buffer::new(command.clone()),
                Editing::Env {
                    key,
                    value: input,
                    value_focus,
                    ..
                } => {
                    *key = Buffer::new("APPLICATION_CONFIG".into());
                    *input = Buffer::new(value.clone());
                    *value_focus = true;
                }
                _ => unreachable!(),
            }
            for (width, height) in [(100, 24), (40, 10)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                export(
                    terminal.backend().buffer(),
                    &format!(
                        "preview-{}-{width}",
                        if env { "environment" } else { "command" }
                    ),
                );
            }
        }
    }
    fn export(buffer: &ratatui::buffer::Buffer, name: &str) {
        if let Ok(output) = std::env::var("SERVED_FORM_PREVIEWS") {
            std::fs::create_dir_all(&output).unwrap();
            let cells = buffer.content.iter().map(|cell| serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),"reverse":cell.modifier.contains(Modifier::REVERSED),"bold":cell.modifier.contains(Modifier::BOLD),"dim":cell.modifier.contains(Modifier::DIM),"underline":cell.modifier.contains(Modifier::UNDERLINED)})).collect::<Vec<_>>();
            std::fs::write(std::path::Path::new(&output).join(format!("{name}.json")),serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
        }
    }
    #[test]
    fn input_and_confirmation_remain_visible_at_minimum_size() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "{name:'api',command:'echo hello'}").unwrap();
        for (width, height) in [(40, 10), (80, 24)] {
            for enhanced in [false, true] {
                let mut form = Form::open(&path).unwrap();
                form.enhanced_keyboard = enhanced;
                form.selected = 1;
                form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 16);
                form.paste("\necho second");
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let plain = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(plain.contains("esc back"));
                for unwanted in ["enter done", "ctrl+s", "newline", "Save writes", "tab next"] {
                    assert!(!plain.contains(unwanted));
                }
                if width == 80 && enhanced {
                    export(terminal.backend().buffer(), "editing");
                }
                form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 16);
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let buffer = terminal.backend().buffer();
                let plain = buffer
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(plain.contains("ctrl+s save  esc quit"));
                assert!(!plain.contains("[ Save ]"));
                assert!(!plain.contains("[ Exit ]"));
                form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 16);
                for choice in 0..3 {
                    form.confirm_selected = choice;
                    terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                    let buffer = terminal.backend().buffer();
                    let plain = buffer
                        .content
                        .iter()
                        .map(|c| c.symbol())
                        .collect::<String>();
                    assert!(plain.contains(
                        ["Continue editing", "Save and exit", "Discard changes"][choice]
                    ));
                    assert!(!plain.contains("ctrl+s save"));
                    assert!(plain.contains("enter confirm  esc back"));
                    if width == 80 && enhanced && choice == 0 {
                        export(buffer, "exit-confirmation");
                    }
                }
            }
        }
    }
    #[test]
    fn environment_and_command_wrap_scroll_and_resize_with_visible_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let long = "echo 中文 👩‍💻 e\u{301} --argument ".repeat(100);
        std::fs::write(
            &path,
            serde_json::to_string(
                &serde_json::json!({"name":"api","command":long,"env":{"LONG_VALUE":long}}),
            )
            .unwrap(),
        )
        .unwrap();
        for env in [false, true] {
            let mut form = Form::open(&path).unwrap();
            form.tab = if env { 3 } else { 0 };
            form.selected = usize::from(!env);
            form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 16);
            if env {
                form.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 16);
            }
            form.key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), 16);
            for (width, height, error) in [
                (120, 24, false),
                (40, 10, false),
                (80, 24, false),
                (40, 10, true),
            ] {
                form.inline_error = error.then(|| "Invalid value".into());
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let buffer = terminal.backend().buffer();
                let plain = buffer
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                if env {
                    assert!(plain.contains("Name"));
                    assert!(plain.contains("Value"));
                    assert!(plain.contains("LONG_VALUE"));
                }
                assert!(plain.contains('┃'));
                assert!(plain.contains("esc back"));
                if error {
                    assert!(plain.contains("Invalid value"));
                }
                if env {
                    assert!(plain.contains("[ Delete ]"));
                }
                let input = match form.editing.as_ref().unwrap() {
                    Editing::Text { input, .. } => input,
                    Editing::Env { value, .. } => value,
                    _ => unreachable!(),
                };
                assert_eq!(input.text, long);
                assert!(input.scroll > 0);
                let layout = input.layout();
                assert!(
                    layout.row >= input.scroll && layout.row < input.scroll + input.viewport_height
                );
                if width == 120 {
                    assert!(input.viewport_width > 100);
                }
                export(
                    buffer,
                    &format!(
                        "{}-{width}",
                        if env {
                            "environment-form"
                        } else {
                            "command-wrap"
                        }
                    ),
                );
                form.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), 16);
                terminal.draw(|frame| draw(frame, &mut form)).unwrap();
                let input = match form.editing.as_ref().unwrap() {
                    Editing::Text { input, .. } => input,
                    Editing::Env { value, .. } => value,
                    _ => unreachable!(),
                };
                assert!(input.layout().row < layout.row);
                assert_eq!(input.text, long);
                form.key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), 16);
            }
        }
    }
    #[test]
    fn overview_stacks_entries_and_marks_only_actual_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "{name:'api',command:'true',env:{A:'a ·\\t\\nb',B:'next'}}",
        )
        .unwrap();
        for (width, height) in [(40, 10), (100, 24)] {
            let mut form = Form::open(&path).unwrap();
            form.tab = 3;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut form)).unwrap();
            let buffer = terminal.backend().buffer();
            let lines: Vec<String> = buffer
                .content
                .chunks(width as usize)
                .map(|row| row.iter().map(|c| c.symbol()).collect())
                .collect();
            let name = lines.iter().position(|line| line.trim() == "A").unwrap();
            assert!(lines[name + 1].contains("a··→···↵b"));
            let dots: Vec<_> = buffer
                .content
                .chunks(width as usize)
                .nth(name + 1)
                .unwrap()
                .iter()
                .filter(|c| c.symbol() == "·")
                .collect();
            if colors_enabled() {
                assert_eq!(dots[0].bg, Color::Indexed(242));
                assert_eq!(dots[0].fg, dots[1].fg);
                assert!(!dots[0].modifier.contains(Modifier::DIM));
            } else {
                assert_eq!(dots[0].bg, dots[1].bg);
                assert!(dots[0].modifier.contains(Modifier::DIM));
            }
            assert!(dots[0].modifier.contains(Modifier::REVERSED));
            assert!(!dots[1].modifier.contains(Modifier::DIM));
            assert_eq!(dots[1].bg, Color::Reset);
            if width == 100 {
                export(buffer, "whitespace-selected");
            }
            assert!(lines[lines.len() - 2].trim().chars().all(|c| c == '─'));
            assert_eq!(
                lines.last().unwrap().trim(),
                "ctrl+s save  esc quit  a add  ? help"
            );
            form.selected = 1;
            terminal.draw(|frame| draw(frame, &mut form)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("next"));
            if width == 100 {
                export(terminal.backend().buffer(), "whitespace-unselected");
            }
            assert!(!text.contains("Overrides"));
            assert!(text.contains("ctrl+s save"));
        }
    }
}
