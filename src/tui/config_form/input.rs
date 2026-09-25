use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
}

pub(super) struct Buffer {
    pub text: String,
    pub cursor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    preferred_column: Option<usize>,
    pub viewport_width: usize,
    pub viewport_height: usize,
    pub scroll: usize,
}

impl Buffer {
    pub fn new(text: String) -> Self {
        Self {
            text,
            cursor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            preferred_column: None,
            viewport_width: 72,
            viewport_height: 10,
            scroll: 0,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        while self.undo.len() > 64
            || self.undo.iter().map(|s| s.text.len()).sum::<usize>() > 8 * 1024 * 1024
        {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.preferred_column = None;
    }

    pub fn undo(&mut self, redo: bool) {
        let snapshot = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        };
        if let Some(snapshot) = snapshot {
            let current = self.snapshot();
            if redo {
                self.undo.push(current);
            } else {
                self.redo.push(current);
            }
            self.text = snapshot.text;
            self.cursor = snapshot.cursor;
            self.preferred_column = None;
        }
    }

    pub fn insert(&mut self, text: &str) {
        // A paste is data, never a sequence of editor shortcuts.
        let text: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        if text.is_empty() {
            return;
        }
        self.checkpoint();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.snap_cursor();
    }

    fn snap_cursor(&mut self) {
        // Deleting a newline or inserting a combining mark can join graphemes.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i >= self.cursor)
            .unwrap_or(self.text.len());
    }

    pub fn newline(&mut self) {
        let start = self.line_start();
        let indent: String = self.text[start..self.cursor]
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect();
        self.insert(&format!("\n{indent}"));
    }

    pub fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        self.preferred_column = None;
    }

    pub fn right(&mut self) {
        if let Some(g) = self.text[self.cursor..].graphemes(true).next() {
            self.cursor += g.len();
        }
        self.preferred_column = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.checkpoint();
        let end = self.cursor;
        self.left();
        self.text.replace_range(self.cursor..end, "");
        self.snap_cursor();
    }

    pub fn delete(&mut self) {
        if let Some(g) = self.text[self.cursor..].graphemes(true).next() {
            let end = self.cursor + g.len();
            self.checkpoint();
            self.text.replace_range(self.cursor..end, "");
            self.snap_cursor();
        }
    }

    pub fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    pub fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| {
                let end = self.cursor + i;
                if end > 0 && self.text.as_bytes()[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                }
            })
    }

    pub fn row(&self) -> usize {
        self.text[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    }
    pub fn column(&self) -> usize {
        cells(&self.text[self.line_start()..self.cursor])
    }

    pub fn home(&mut self, document: bool) {
        self.cursor = if document { 0 } else { self.line_start() };
        self.preferred_column = None;
    }
    pub fn end(&mut self, document: bool) {
        self.cursor = if document {
            self.text.len()
        } else {
            self.line_end()
        };
        self.preferred_column = None;
    }

    pub fn layout(&self) -> Wrapped {
        Wrapped::new(&self.text, self.viewport_width.max(1), self.cursor)
    }

    pub fn vertical(&mut self, delta: isize) {
        let layout = self.layout();
        let column = self.preferred_column.unwrap_or(layout.column);
        self.preferred_column = Some(column);
        let row = layout
            .row
            .saturating_add_signed(delta)
            .min(layout.lines.len() - 1);
        let line = &layout.lines[row];
        self.cursor = line.start;
        let mut width = 0;
        for (offset, g) in self.text[line.start..line.end].grapheme_indices(true) {
            let next = width + cells(g).max(1).min(self.viewport_width.max(1));
            if next > column {
                break;
            }
            let end = line.start + offset + g.len();
            // A soft-wrap boundary belongs to the following visual row.
            if layout
                .lines
                .get(row + 1)
                .is_some_and(|next| next.start == end)
            {
                break;
            }
            self.cursor = end;
            width = next;
        }
    }
}

/// Source byte ranges and terminal-cell positions share one layout for drawing and movement.
pub(super) struct VisualLine {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub marks: Vec<std::ops::Range<usize>>,
}
impl VisualLine {
    fn new(start: usize) -> Self {
        Self {
            start,
            end: start,
            text: String::new(),
            marks: Vec::new(),
        }
    }
    fn append(&mut self, text: &str, marked: bool) {
        let start = self.text.len();
        self.text.push_str(text);
        if marked {
            self.marks.push(start..self.text.len());
        }
    }
}
pub(super) struct Wrapped {
    pub lines: Vec<VisualLine>,
    pub row: usize,
    pub column: usize,
}
impl Wrapped {
    fn new(source: &str, width: usize, cursor: usize) -> Self {
        let mut lines = Vec::new();
        let mut line = VisualLine::new(0);
        let mut used = 0;
        for (at, g) in source.grapheme_indices(true) {
            let (display, marked) = visible_grapheme(g);
            let display = if cells(&display) > width {
                "�".to_owned()
            } else {
                display
            };
            let size = cells(&display).max(1);
            if used + size > width {
                line.end = at;
                lines.push(std::mem::replace(&mut line, VisualLine::new(at)));
                used = 0;
            }
            line.append(&display, marked);
            if matches!(g, "\n" | "\r\n") {
                line.end = at;
                lines.push(std::mem::replace(&mut line, VisualLine::new(at + g.len())));
                used = 0;
            } else {
                line.end = at + g.len();
                used += size;
            }
        }
        lines.push(line);
        if used == width {
            lines.push(VisualLine::new(source.len()));
        }
        let row = lines
            .iter()
            .rposition(|line| line.start <= cursor)
            .unwrap_or(0);
        let column = source[lines[row].start..cursor]
            .graphemes(true)
            .map(|g| cells(&visible_grapheme(g).0))
            .sum::<usize>()
            .min(width - 1);
        Self { lines, row, column }
    }
}

/// Display-only markers. Literal marker characters remain ordinary text.
pub(super) fn visible_grapheme(g: &str) -> (String, bool) {
    match g {
        " " => ("·".into(), true),
        "\t" => ("→···".into(), true),
        "\n" => ("↵".into(), true),
        "\r\n" => ("␍↵".into(), true),
        "\r" => ("␍".into(), true),
        _ if g.chars().any(char::is_control) || cells(g) == 0 => ("�".into(), false),
        _ => (g.into(), false),
    }
}

pub(super) fn cells(text: &str) -> usize {
    text.graphemes(true)
        .map(|g| if g == "\t" { 4 } else { g.width() })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_editing_and_undo_preserve_graphemes() {
        let mut b = Buffer::new("中e\u{301}🙂".into());
        b.right();
        b.right();
        assert_eq!(b.column(), 3);
        b.backspace();
        assert_eq!(b.text, "中🙂");
        b.undo(false);
        assert_eq!(b.text, "中e\u{301}🙂");
        b.delete();
        assert_eq!(b.text, "中e\u{301}");
        b.undo(false);
        b.undo(true);
        assert_eq!(b.text, "中e\u{301}");
    }

    #[test]
    fn vertical_motion_preserves_visual_column_and_newlines_join() {
        let mut b = Buffer::new("中ab\nx\n    z".into());
        b.end(false);
        b.vertical(1);
        b.vertical(1);
        assert_eq!((b.row(), b.column()), (2, 4));
        b.home(false);
        b.backspace();
        assert_eq!(b.text, "中ab\nx    z");
        b.home(true);
        b.end(false);
        b.delete();
        assert_eq!(b.text, "中abx    z");
    }

    #[test]
    fn paste_is_one_undo_step_and_newline_indents() {
        let mut b = Buffer::new("  name:".into());
        b.end(true);
        b.newline();
        assert_eq!(b.text, "  name:\n  ");
        b.insert("中文\r\n\u{1b}\u{3}next");
        assert!(b.text.ends_with("中文\nnext"));
        b.undo(false);
        assert_eq!(b.text, "  name:\n  ");
    }
    #[test]
    fn wrapping_preserves_source_graphemes_and_visual_vertical_motion() {
        let text = "abc中文e\u{301}🙂def\n\nend";
        let mut b = Buffer::new(text.into());
        b.viewport_width = 5;
        let layout = b.layout();
        assert_eq!(
            layout
                .lines
                .iter()
                .map(|l| &b.text[l.start..l.end])
                .collect::<String>(),
            text.replace('\n', "")
        );
        for line in &layout.lines {
            assert!(cells(&line.text) <= 5);
            assert!(b.text.is_char_boundary(line.start));
        }
        b.cursor = 2;
        b.vertical(1);
        assert_eq!(b.layout().row, 1);
        assert_eq!(b.layout().column, 2);
        b.vertical(-1);
        assert_eq!(b.cursor, 2);
        assert_eq!(b.text, text);
        b.end(true);
        b.viewport_width = 3;
        assert!(b.layout().lines.len() > layout.lines.len());
        let raw = "12345\r\n\n";
        let wrap = Wrapped::new(raw, 5, raw.len());
        assert_eq!(wrap.lines.len(), 4);
        assert_eq!(wrap.row, 3);
        let wrap = Wrapped::new("12345", 5, 5);
        assert_eq!(wrap.row, 1);
        assert_eq!(wrap.column, 0);
    }
    #[test]
    fn whitespace_markers_do_not_mutate_text_or_split_crlf_cursor() {
        let raw = "  a\t·\r\n\n中 e\u{301}🙂 ";
        let mut b = Buffer::new(raw.into());
        b.viewport_width = 8;
        b.end(true);
        let layout = b.layout();
        let display = layout
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<String>();
        assert!(display.contains("··a→····␍↵↵"));
        assert_eq!(b.text, raw);
        for line in &layout.lines {
            assert!(cells(&line.text) <= 8);
            for mark in &line.marks {
                assert!(!line.text[mark.clone()].is_empty());
            }
        }
        b.home(true);
        b.end(false);
        assert_eq!(b.cursor, raw.find('\r').unwrap());
        b.insert("x");
        assert!(b.text.contains("·x\r\n"));
        b.undo(false);
        assert_eq!(b.text, raw);
        b.viewport_width = 3;
        b.home(true);
        b.vertical(1);
        assert!(b.text.is_char_boundary(b.cursor));
    }
}
