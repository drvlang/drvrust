//! Focused zero-dependency TOML parser for Cargo.toml and Cargo.lock (v4).

use std::collections::BTreeMap;

pub type Table = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_table()?.get(key)
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(s)
    }

    fn skip_ws_inline(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(b) = self.peek() {
                if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.peek() == Some(b'#') {
                while let Some(b) = self.peek() {
                    self.pos += 1;
                    if b == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn skip_to_eol(&mut self) {
        self.skip_ws_inline();
        if self.peek() == Some(b'#') {
            while let Some(b) = self.peek() {
                self.pos += 1;
                if b == b'\n' {
                    return;
                }
            }
        }
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
    }

    fn parse_basic_string(&mut self) -> Result<String, String> {
        if self.starts_with(b"\"\"\"") {
            self.pos += 3;
            if self.peek() == Some(b'\r') {
                self.pos += 1;
            }
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
            let mut out = String::new();
            while self.pos < self.bytes.len() {
                if self.starts_with(b"\"\"\"") {
                    self.pos += 3;
                    return Ok(out);
                }
                let b = self.bytes[self.pos];
                self.pos += 1;
                if b == b'\\' {
                    if matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                        while let Some(c) = self.peek() {
                            if matches!(c, b' ' | b'\t' | b'\r' | b'\n') {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    } else {
                        self.push_escape(&mut out)?;
                    }
                } else {
                    out.push(b as char);
                }
            }
            return Err("unterminated multiline basic string".into());
        }
        self.pos += 1; // opening "
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => self.push_escape(&mut out)?,
                _ => out.push(b as char),
            }
        }
        Err("unterminated basic string".into())
    }

    fn push_escape(&mut self, out: &mut String) -> Result<(), String> {
        let esc = self.peek().ok_or("unexpected eof in string escape")?;
        self.pos += 1;
        match esc {
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'\\' => out.push('\\'),
            b'"' => out.push('"'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000C}'),
            b'u' => {
                let hex = std::str::from_utf8(
                    self.bytes
                        .get(self.pos..self.pos + 4)
                        .ok_or("short \\u escape")?,
                )
                .map_err(|_| "invalid \\u escape")?;
                self.pos += 4;
                let code = u32::from_str_radix(hex, 16).map_err(|_| "bad \\u hex")?;
                out.push(char::from_u32(code).ok_or("invalid unicode scalar")?);
            }
            b'U' => {
                let hex = std::str::from_utf8(
                    self.bytes
                        .get(self.pos..self.pos + 8)
                        .ok_or("short \\U escape")?,
                )
                .map_err(|_| "invalid \\U escape")?;
                self.pos += 8;
                let code = u32::from_str_radix(hex, 16).map_err(|_| "bad \\U hex")?;
                out.push(char::from_u32(code).ok_or("invalid unicode scalar")?);
            }
            _ => return Err(format!("unknown TOML escape \\{}", esc as char)),
        }
        Ok(())
    }

    fn parse_literal_string(&mut self) -> Result<String, String> {
        if self.starts_with(b"'''") {
            self.pos += 3;
            if self.peek() == Some(b'\r') {
                self.pos += 1;
            }
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
            let start = self.pos;
            while self.pos < self.bytes.len() {
                if self.starts_with(b"'''") {
                    let s = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|_| "invalid utf8")?
                        .to_owned();
                    self.pos += 3;
                    return Ok(s);
                }
                self.pos += 1;
            }
            return Err("unterminated multiline literal string".into());
        }
        self.pos += 1; // opening '
        let start = self.pos;
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'\'' {
                let s = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| "invalid utf8")?
                    .to_owned();
                self.pos += 1;
                return Ok(s);
            }
            self.pos += 1;
        }
        Err("unterminated literal string".into())
    }

    fn parse_key_part(&mut self) -> Result<String, String> {
        self.skip_ws_inline();
        match self.peek() {
            Some(b'"') => self.parse_basic_string(),
            Some(b'\'') => self.parse_literal_string(),
            Some(_) => {
                let start = self.pos;
                while let Some(b) = self.peek() {
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == start {
                    return Err(format!("expected TOML key at byte {}", self.pos));
                }
                Ok(std::str::from_utf8(&self.bytes[start..self.pos])
                    .unwrap()
                    .to_owned())
            }
            None => Err("unexpected EOF reading TOML key".into()),
        }
    }

    fn parse_dotted_key(&mut self) -> Result<Vec<String>, String> {
        let mut parts = vec![self.parse_key_part()?];
        loop {
            self.skip_ws_inline();
            if self.peek() == Some(b'.') {
                self.pos += 1;
                parts.push(self.parse_key_part()?);
            } else {
                break;
            }
        }
        Ok(parts)
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws_inline();
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.parse_basic_string()?)),
            Some(b'\'') => Ok(Value::Str(self.parse_literal_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_inline_table(),
            Some(_) => {
                let start = self.pos;
                while let Some(b) = self.peek() {
                    if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b'#') {
                        break;
                    }
                    self.pos += 1;
                }
                let tok = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| "invalid utf8 token")?;
                match tok {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => {
                        let cleaned = tok.replace('_', "");
                        if let Ok(i) = cleaned.parse::<i64>() {
                            Ok(Value::Int(i))
                        } else {
                            // Keep floats/dates as string so Cargo.toml metadata never fails parsing.
                            Ok(Value::Str(tok.to_owned()))
                        }
                    }
                }
            }
            None => Err("unexpected EOF reading TOML value".into()),
        }
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        self.pos += 1; // [
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            }
            items.push(self.parse_value()?);
            self.skip_ws_and_comments();
            if self.peek() == Some(b',') {
                self.pos += 1;
            } else if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            } else {
                return Err(format!("expected ',' or ']' in array at byte {}", self.pos));
            }
        }
    }

    fn parse_inline_table(&mut self) -> Result<Value, String> {
        self.pos += 1; // {
        let mut table = Table::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Value::Table(table));
            }
            let key = self.parse_dotted_key()?;
            self.skip_ws_inline();
            if self.peek() != Some(b'=') {
                return Err(format!("expected '=' in inline table at byte {}", self.pos));
            }
            self.pos += 1;
            let val = self.parse_value()?;
            insert_dotted(&mut table, &key, val)?;
            self.skip_ws_and_comments();
            if self.peek() == Some(b',') {
                self.pos += 1;
            } else if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Value::Table(table));
            } else {
                return Err(format!(
                    "expected ',' or '}}' in inline table at byte {}",
                    self.pos
                ));
            }
        }
    }
}

fn ensure_table<'a>(root: &'a mut Table, path: &[String]) -> Result<&'a mut Table, String> {
    let mut cur = root;
    for part in path {
        let entry = cur
            .entry(part.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        cur = match entry {
            Value::Table(t) => t,
            Value::Array(arr) => match arr.last_mut() {
                Some(Value::Table(t)) => t,
                _ => return Err(format!("TOML path {part:?} is not a table")),
            },
            _ => return Err(format!("TOML path {part:?} is not a table")),
        };
    }
    Ok(cur)
}

fn insert_dotted(table: &mut Table, key: &[String], val: Value) -> Result<(), String> {
    let target = ensure_table(table, &key[..key.len() - 1])?;
    target.insert(key.last().unwrap().clone(), val);
    Ok(())
}

pub fn parse(input: &str) -> Result<Value, String> {
    let mut p = Parser::new(input);
    let mut root = Table::new();
    enum CurTarget {
        Root,
        Table(Vec<String>),
        ArrayTable(Vec<String>),
    }
    let mut current = CurTarget::Root;

    loop {
        p.skip_ws_and_comments();
        let Some(b) = p.peek() else { break };
        if b == b'[' {
            if p.starts_with(b"[[") {
                p.pos += 2;
                let path = p.parse_dotted_key()?;
                p.skip_ws_inline();
                if !p.starts_with(b"]]") {
                    return Err(format!("expected ']]' at byte {}", p.pos));
                }
                p.pos += 2;
                p.skip_to_eol();
                let parent = ensure_table(&mut root, &path[..path.len() - 1])?;
                let arr = parent
                    .entry(path.last().unwrap().clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                match arr {
                    Value::Array(vec) => vec.push(Value::Table(Table::new())),
                    _ => return Err("conflicting array table".into()),
                }
                current = CurTarget::ArrayTable(path);
            } else {
                p.pos += 1;
                let path = p.parse_dotted_key()?;
                p.skip_ws_inline();
                if p.peek() != Some(b']') {
                    return Err(format!("expected ']' at byte {}", p.pos));
                }
                p.pos += 1;
                p.skip_to_eol();
                let _ = ensure_table(&mut root, &path)?;
                current = CurTarget::Table(path);
            }
        } else {
            let key = p.parse_dotted_key()?;
            p.skip_ws_inline();
            if p.peek() != Some(b'=') {
                return Err(format!("expected '=' at byte {}", p.pos));
            }
            p.pos += 1;
            let val = p.parse_value()?;
            p.skip_to_eol();
            let target = match &current {
                CurTarget::Root => &mut root,
                CurTarget::Table(path) | CurTarget::ArrayTable(path) => {
                    ensure_table(&mut root, path)?
                }
            };
            insert_dotted(target, &key, val)?;
        }
    }
    Ok(Value::Table(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_lock_and_manifest() {
        let lock = r#"
# comment
version = 4

[[package]]
name = "hex"
version = "0.4.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "7f24254aa9a54b5c858eaee2f5bccdb46aaf0e486a595ed5fd8f86ba55232a70"

[[package]]
name = "hex-demo"
version = "0.1.0"
dependencies = [
 "hex",
]
"#;
        let v = parse(lock).unwrap();
        assert_eq!(v.get("version").and_then(Value::as_int), Some(4));
        let pkgs = v.get("package").and_then(Value::as_array).unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].get("name").and_then(Value::as_str), Some("hex"));

        let manifest = r#"
[package]
name = "demo"
edition = "2021"

[dependencies]
shared = { path = "../../pkg/shared", default-features = false, features = ["a", "b"] }
[dependencies.clap]
version = "4"
features = ["derive"]
"#;
        let m = parse(manifest).unwrap();
        let deps = m.get("dependencies").unwrap();
        assert_eq!(
            deps.get("shared")
                .unwrap()
                .get("default-features")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            deps.get("clap")
                .unwrap()
                .get("features")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
    }
}
