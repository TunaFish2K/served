use std::collections::VecDeque;

use super::{
    ATTACH_CACHE_LINES, ATTACH_CACHE_MAX_BYTES, OUTPUT_TAIL_CACHE_BYTES, OUTPUT_TAIL_CHARS,
};

#[derive(Debug, Default)]
pub(super) struct LineCounter {
    lines: u64,
    has_content: bool,
    pending_cr: bool,
    escape: EscapeState,
}

#[derive(Debug, Default)]
enum EscapeState {
    #[default]
    None,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

impl LineCounter {
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.feed_byte(byte);
        }
    }

    fn feed_byte(&mut self, byte: u8) {
        match self.escape {
            EscapeState::None => match byte {
                0x1b => self.escape = EscapeState::Escape,
                0x9b => self.escape = EscapeState::Csi,
                _ => self.feed_clean_byte(byte),
            },
            EscapeState::Escape => {
                self.escape = match byte {
                    b'[' => EscapeState::Csi,
                    b']' => EscapeState::Osc,
                    _ => EscapeState::None,
                };
            }
            EscapeState::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = EscapeState::None;
                }
            }
            EscapeState::Osc => match byte {
                0x07 => self.escape = EscapeState::None,
                0x1b => self.escape = EscapeState::OscEscape,
                _ => {}
            },
            EscapeState::OscEscape => {
                self.escape = if byte == b'\\' {
                    EscapeState::None
                } else {
                    EscapeState::Osc
                };
            }
        }
    }

    fn feed_clean_byte(&mut self, byte: u8) {
        if self.pending_cr {
            self.pending_cr = false;
            if byte == b'\n' {
                return;
            }
        }

        match byte {
            b'\r' => {
                self.lines = self.lines.saturating_add(1);
                self.pending_cr = true;
                self.has_content = false;
            }
            b'\n' => {
                self.lines = self.lines.saturating_add(1);
                self.has_content = false;
            }
            b'\t' | 0x20..=0x7e | 0x80..=0xff => {
                self.has_content = true;
            }
            _ => {}
        }
    }

    pub(super) fn total_lines(&self) -> u64 {
        self.lines.saturating_add(u64::from(self.has_content))
    }
}

#[derive(Debug, Default)]
pub(super) struct SanitizedTail {
    bytes: VecDeque<u8>,
    escape: EscapeState,
}

impl SanitizedTail {
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.escape {
                EscapeState::None => match byte {
                    0x1b => self.escape = EscapeState::Escape,
                    0x9b => self.escape = EscapeState::Csi,
                    b'\n' | b'\r' | b'\t' | 0x20..=0x7e | 0x80..=0xff => {
                        self.bytes.push_back(byte);
                    }
                    _ => {}
                },
                EscapeState::Escape => {
                    self.escape = match byte {
                        b'[' => EscapeState::Csi,
                        b']' => EscapeState::Osc,
                        _ => EscapeState::None,
                    };
                }
                EscapeState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = EscapeState::None;
                    }
                }
                EscapeState::Osc => match byte {
                    0x07 => self.escape = EscapeState::None,
                    0x1b => self.escape = EscapeState::OscEscape,
                    _ => {}
                },
                EscapeState::OscEscape => {
                    self.escape = if byte == b'\\' {
                        EscapeState::None
                    } else {
                        EscapeState::Osc
                    };
                }
            }
        }
        while self.bytes.len() > OUTPUT_TAIL_CACHE_BYTES {
            self.bytes.pop_front();
        }
    }

    pub(super) fn render(&self) -> String {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        let cleaned = String::from_utf8_lossy(&bytes);
        let length = cleaned.chars().count();
        if length > OUTPUT_TAIL_CHARS {
            cleaned.chars().skip(length - OUTPUT_TAIL_CHARS).collect()
        } else {
            cleaned.into_owned()
        }
    }
}

pub fn sanitize_log(bytes: &[u8]) -> String {
    let mut clean = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == 0x1b {
            index += 1;
            if index < bytes.len() && bytes[index] == b']' {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            } else if index < bytes.len() && bytes[index] == b'[' {
                index += 1;
                while index < bytes.len() {
                    let final_byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&final_byte) {
                        break;
                    }
                }
            } else if index < bytes.len() {
                index += 1;
            }
            continue;
        }
        if byte == 0x9b {
            index += 1;
            while index < bytes.len() {
                let final_byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&final_byte) {
                    break;
                }
            }
            continue;
        }
        if byte == b'\n' || byte == b'\r' || byte == b'\t' || (byte >= 0x20 && byte != 0x7f) {
            clean.push(byte);
        }
        index += 1;
    }
    String::from_utf8_lossy(&clean).into_owned()
}

pub fn attach_snapshot_from_bytes(bytes: &[u8]) -> Vec<u8> {
    let cleaned = normalize_line_endings(&sanitize_log(bytes));
    let ended_with_newline = cleaned.ends_with('\n');
    let mut lines: Vec<&str> = cleaned.split('\n').collect();
    if ended_with_newline {
        lines.pop();
    }
    let start = lines.len().saturating_sub(ATTACH_CACHE_LINES);
    let mut snapshot = lines[start..].join("\r\n");
    snapshot = tail_chars(&snapshot, ATTACH_CACHE_MAX_BYTES);
    if !snapshot.is_empty() {
        snapshot.push_str("\r\n");
    }
    snapshot.into_bytes()
}

pub(super) fn logical_line_count(bytes: &[u8]) -> u64 {
    let mut counter = LineCounter::default();
    counter.feed(bytes);
    counter.total_lines()
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn tail_chars(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut used = 0_usize;
    let mut start = value.len();
    for (index, character) in value.char_indices().rev() {
        let length = character.len_utf8();
        if used.saturating_add(length) > max_bytes {
            break;
        }
        used += length;
        start = index;
    }
    value[start..].to_owned()
}
