//! Locate JSON5 properties without reserializing unrelated source text.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::ops::Range;

#[derive(Debug)]
struct Member {
    key: String,
    start: usize,
    value: Node,
    comma: Option<usize>,
}
#[derive(Debug)]
struct Node {
    range: Range<usize>,
    members: Option<Vec<Member>>,
}
struct Parser<'a> {
    text: &'a str,
    pos: usize,
}
impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }
    fn step(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }
    fn trivia(&mut self) -> Result<()> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() || c == '\u{feff}' => self.step(),
                Some('/') if self.text[self.pos..].starts_with("//") => {
                    while self
                        .peek()
                        .is_some_and(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
                    {
                        self.step();
                    }
                }
                Some('/') if self.text[self.pos..].starts_with("/*") => {
                    let end = self.text[self.pos + 2..]
                        .find("*/")
                        .context("unterminated comment")?;
                    self.pos += end + 4;
                }
                _ => return Ok(()),
            }
        }
    }
    fn string(&mut self) -> Result<()> {
        let quote = self.peek().context("missing string")?;
        self.step();
        loop {
            let c = self.peek().context("unterminated string")?;
            self.step();
            if c == quote {
                return Ok(());
            }
            if c == '\\' {
                ensure!(self.peek().is_some(), "unterminated escape");
                self.step();
            }
        }
    }
    fn node(&mut self) -> Result<Node> {
        self.trivia()?;
        let start = self.pos;
        let mut members = None;
        match self.peek().context("missing value")? {
            '{' => {
                self.step();
                let mut entries: Vec<Member> = Vec::new();
                loop {
                    self.trivia()?;
                    if self.peek() == Some('}') {
                        self.step();
                        break;
                    }
                    let key_start = self.pos;
                    match self.peek() {
                        Some('\'' | '"') => self.string()?,
                        Some(_) => {
                            while self
                                .peek()
                                .is_some_and(|c| !c.is_whitespace() && !matches!(c, ':' | '/'))
                            {
                                self.step();
                            }
                        }
                        None => bail!("unterminated object"),
                    }
                    let raw = &self.text[key_start..self.pos];
                    let key_object: Value = json5::from_str(&format!("{{{raw}:null}}"))?;
                    let key = key_object
                        .as_object()
                        .and_then(|o| o.keys().next())
                        .context("invalid property name")?
                        .clone();
                    ensure!(
                        !entries.iter().any(|e| e.key == key),
                        "duplicate configuration key {key:?}; repair using --editor"
                    );
                    self.trivia()?;
                    ensure!(self.peek() == Some(':'), "missing colon");
                    self.step();
                    let value = self.node()?;
                    self.trivia()?;
                    let comma = if self.peek() == Some(',') {
                        let at = self.pos;
                        self.step();
                        Some(at)
                    } else {
                        None
                    };
                    entries.push(Member {
                        key,
                        start: key_start,
                        value,
                        comma,
                    });
                    if comma.is_none() {
                        ensure!(self.peek() == Some('}'), "missing comma");
                    }
                }
                members = Some(entries);
            }
            '[' => {
                self.step();
                loop {
                    self.trivia()?;
                    if self.peek() == Some(']') {
                        self.step();
                        break;
                    }
                    self.node()?;
                    self.trivia()?;
                    if self.peek() == Some(',') {
                        self.step();
                    } else {
                        ensure!(self.peek() == Some(']'), "missing array comma");
                    }
                }
            }
            '\'' | '"' => self.string()?,
            _ => {
                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !matches!(c, ',' | '}' | ']' | '/'))
                {
                    self.step();
                }
                ensure!(self.pos > start, "unsupported JSON5 value");
            }
        }
        Ok(Node {
            range: start..self.pos,
            members,
        })
    }
}
fn root(text: &str) -> Result<Node> {
    let mut parser = Parser { text, pos: 0 };
    let node = parser.node()?;
    parser.trivia()?;
    ensure!(
        parser.pos == text.len() && node.members.is_some(),
        "configuration must be a JSON5 object"
    );
    Ok(node)
}

pub(super) fn parse(text: &str) -> Result<Value> {
    let value = json5::from_str(text)
        .context("Invalid JSON5; repair using served edit --editor COMMAND")?;
    root(text)?; // serde_json alone would silently collapse duplicate properties.
    Ok(value)
}

pub(super) fn set(
    text: &str,
    object: Option<&str>,
    key: &str,
    value: Option<&Value>,
) -> Result<String> {
    let root = root(text)?;
    let node = if let Some(object) = object {
        &root
            .members
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.key == object)
            .context("missing object")?
            .value
    } else {
        &root
    };
    let entries = node.members.as_ref().context("expected object")?;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    if let Some((index, member)) = entries.iter().enumerate().find(|(_, e)| e.key == key) {
        if let Some(value) = value {
            edits.push((member.value.range.clone(), serde_json::to_string(value)?));
        } else {
            edits.push((member.start..member.value.range.end, String::new()));
            if let Some(comma) = member
                .comma
                .or_else(|| index.checked_sub(1).and_then(|i| entries[i].comma))
            {
                edits.push((comma..comma + 1, String::new()));
            }
        }
    } else if let Some(value) = value {
        let close = node.range.end - 1;
        let multiline = text[node.range.clone()].contains('\n');
        let trailing = entries.last().is_some_and(|e| e.comma.is_some());
        if let Some(last) = entries.last().filter(|e| e.comma.is_none()) {
            edits.push((last.value.range.end..last.value.range.end, ",".into()));
        }
        let property = format!(
            "{}: {}{}",
            serde_json::to_string(key)?,
            serde_json::to_string(value)?,
            if trailing { "," } else { "" }
        );
        if multiline {
            let line_start = text[..close].rfind('\n').map_or(0, |i| i + 1);
            let closing_indent = &text[line_start..close];
            let at = if closing_indent.chars().all(|c| matches!(c, ' ' | '\t')) {
                line_start
            } else {
                close
            };
            let base = if at == line_start { closing_indent } else { "" };
            let indent = entries
                .first()
                .and_then(|e| {
                    let start = text[..e.start].rfind('\n').map_or(0, |i| i + 1);
                    let prefix = &text[start..e.start];
                    prefix
                        .chars()
                        .all(|c| matches!(c, ' ' | '\t'))
                        .then(|| prefix.to_owned())
                })
                .unwrap_or_else(|| format!("{base}  "));
            let prefix = if at > 0 && text.as_bytes()[at - 1] == b'\n' {
                ""
            } else {
                "\n"
            };
            edits.push((at..at, format!("{prefix}{indent}{property}\n")));
        } else {
            edits.push((close..close, format!(" {property} ")));
        }
    }
    edits.reverse(); // At one offset, insert the new property before inserting its leading comma.
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));
    let mut output = text.to_owned();
    for (range, replacement) in edits {
        output.replace_range(range, &replacement);
    }
    parse(&output).context("cannot safely update configuration")?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn changes_only_target_value_and_keeps_nested_comments() {
        let text = "// top\n{ name: 'api', /* key */ command: 'old', env: { A: '1', // keep A\n B: '中', }, } // tail";
        let changed = set(text, None, "command", Some(&json!("echo 'hi'\nnext"))).unwrap();
        assert_eq!(changed, text.replace("'old'", "\"echo 'hi'\\nnext\""));
        let changed = set(&changed, Some("env"), "A", Some(&json!("2"))).unwrap();
        assert!(changed.contains("// keep A\n B: '中'"));
        assert_eq!(parse(&changed).unwrap()["env"]["A"], "2");
    }
    #[test]
    fn insert_and_remove_across_comma_and_comment_styles() {
        for text in [
            "{}",
            "{a:1}",
            "{a:1,}",
            "{\n  a:1 // hi\n}",
            "{ /*hi*/ }",
            "{\n a:1,\n b:2\n}",
        ] {
            let added = set(text, None, "c", Some(&json!(3))).unwrap();
            assert_eq!(parse(&added).unwrap()["c"], 3);
            let removed = set(&added, None, "c", None).unwrap();
            assert_eq!(parse(&removed).unwrap(), parse(text).unwrap());
            for key in ["a", "b"] {
                let removed = set(text, None, key, None).unwrap();
                assert!(parse(&removed).unwrap().get(key).is_none());
            }
        }
    }
    #[test]
    fn escaped_keys_strings_and_duplicate_keys() {
        let text = r#"{na\u006de:'a', env:{'x:y': '/*not comment*/', z:'it\'s }, fine'}}"#;
        let out = set(text, None, "name", Some(&json!("b"))).unwrap();
        assert!(out.contains(r#"na\u006de:"#));
        assert_eq!(parse(&out).unwrap()["name"], "b");
        for text in ["{name:'a', name:'b'}", "{env:{a:'1', 'a':'2'}}"] {
            assert!(parse(text).is_err());
        }
    }
}
