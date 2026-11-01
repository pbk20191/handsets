// Minimal JSON parser, just enough for the daemon's dump output.
//
// Integral numbers collapse to i64 (rectangle coordinates, timestamps);
// fractional numbers come back as Float (lat/lon, accuracy in meters).
// Strings handle the escapes the daemon emits (\n, \r, \t, \b, \f, \" ,
// \\, \/, and \uXXXX). UTF-8 is passed through raw — the daemon emits
// well-formed UTF-8.

#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Num(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

pub fn parse(s: &str) -> Result<Value, String> {
    let mut p = Parser {
        src: s.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    Ok(v)
}

pub fn obj_get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if let Value::Obj(pairs) = v {
        for (k, val) in pairs {
            if k == key {
                return Some(val);
            }
        }
    }
    None
}

pub fn as_str(v: &Value) -> Option<&str> {
    if let Value::Str(s) = v {
        Some(s.as_str())
    } else {
        None
    }
}

pub fn as_num(v: &Value) -> Option<i64> {
    if let Value::Num(n) = v {
        Some(*n)
    } else {
        None
    }
}

pub fn as_arr(v: &Value) -> Option<&[Value]> {
    if let Value::Arr(a) = v {
        Some(a.as_slice())
    } else {
        None
    }
}

/// Every window root in a dump, in the daemon's order (topmost first).
/// Unwraps both envelopes: `dump_active` = `{"root": …}`, `dump` =
/// `{"windows": [{…, "root": …}, …]}`; a bare node passes through.
pub fn all_roots(dump: &Value) -> Vec<&Value> {
    if let Some(root) = obj_get(dump, "root") {
        return vec![root];
    }
    if let Some(windows) = obj_get(dump, "windows").and_then(as_arr) {
        return windows.iter().filter_map(|w| obj_get(w, "root")).collect();
    }
    if obj_get(dump, "cls").is_some() {
        return vec![dump];
    }
    Vec::new()
}

/// Roots of the app's own windows in a dump, bottom (activity) first.
/// `dump` lists every window; this keeps TYPE_APPLICATION ones (activity,
/// dialogs, popups, bottom sheets) and drops the status bar, IME and other
/// system windows. A `dump_active` payload (`{"root": …}`) passes through.
pub fn app_roots(dump: &Value) -> Vec<&Value> {
    const TYPE_APPLICATION: i64 = 1;
    if let Some(root) = obj_get(dump, "root") {
        return vec![root];
    }
    let mut apps: Vec<&Value> = obj_get(dump, "windows").and_then(as_arr).unwrap_or(&[])
        .iter()
        .filter(|w| obj_get(w, "type").and_then(as_num) == Some(TYPE_APPLICATION))
        .collect();
    apps.sort_by_key(|w| obj_get(w, "layer").and_then(as_num).unwrap_or(0));
    apps.into_iter().filter_map(|w| obj_get(w, "root")).collect()
}

// ---------- parser internals ----------

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        match self.peek() {
            Some(x) if x == c => {
                self.pos += 1;
                Ok(())
            }
            Some(x) => Err(format!(
                "expected '{}' got '{}' at byte {}",
                c as char, x as char, self.pos
            )),
            None => Err(format!("expected '{}' at eof", c as char)),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') | Some(b'f') => self.boolean(),
            Some(b'n') => self.null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected '{}' at byte {}", c as char, self.pos)),
            None => Err("unexpected eof".into()),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Obj(out));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.value()?;
            out.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Obj(out));
                }
                Some(c) => {
                    return Err(format!(
                        "expected ',' or '}}' got '{}' at byte {}",
                        c as char, self.pos
                    ))
                }
                None => return Err("eof in object".into()),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Arr(out));
        }
        loop {
            let v = self.value()?;
            out.push(v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Arr(out));
                }
                Some(c) => {
                    return Err(format!(
                        "expected ',' or ']' got '{}' at byte {}",
                        c as char, self.pos
                    ))
                }
                None => return Err("eof in array".into()),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            let c = self.peek().ok_or_else(|| "eof in string".to_string())?;
            self.pos += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    let esc = self.peek().ok_or_else(|| "eof in escape".to_string())?;
                    self.pos += 1;
                    match esc {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'b' => s.push('\u{08}'),
                        b'f' => s.push('\u{0c}'),
                        b'u' => {
                            if self.pos + 4 > self.src.len() {
                                return Err("eof in \\u escape".into());
                            }
                            let hex = std::str::from_utf8(&self.src[self.pos..self.pos + 4])
                                .map_err(|_| "bad \\u escape".to_string())?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| "bad \\u hex".to_string())?;
                            self.pos += 4;
                            if let Some(ch) = char::from_u32(cp) {
                                s.push(ch);
                            }
                            // Surrogate pairs not handled — the daemon doesn't
                            // emit them for the fields we read.
                        }
                        other => return Err(format!("bad escape \\{}", other as char)),
                    }
                }
                _ if c < 0x80 => s.push(c as char),
                _ => {
                    // Multi-byte UTF-8 — pass through raw bytes, trusting the
                    // daemon's output is well-formed UTF-8.
                    let start = self.pos - 1;
                    let extra = if c >= 0xF0 {
                        3
                    } else if c >= 0xE0 {
                        2
                    } else {
                        1
                    };
                    let end = (start + 1 + extra).min(self.src.len());
                    self.pos = end;
                    if let Ok(t) = std::str::from_utf8(&self.src[start..end]) {
                        s.push_str(t);
                    }
                }
            }
        }
    }

    fn boolean(&mut self) -> Result<Value, String> {
        if self.src[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(Value::Bool(true))
        } else if self.src[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(Value::Bool(false))
        } else {
            Err(format!("bad bool at byte {}", self.pos))
        }
    }

    fn null(&mut self) -> Result<Value, String> {
        if self.src[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Ok(Value::Null)
        } else {
            Err(format!("bad null at byte {}", self.pos))
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let s = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| "bad number utf8".to_string())?;
        if let Ok(n) = s.parse::<i64>() {
            Ok(Value::Num(n))
        } else if let Ok(f) = s.parse::<f64>() {
            Ok(Value::Float(f))
        } else {
            Err(format!("bad number '{s}' at byte {start}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_roots_keeps_app_windows_bottom_first() {
        let dump = parse(r#"{"windows":[
            {"type":3,"layer":9,"root":{"cls":"StatusBar"}},
            {"type":1,"layer":5,"root":{"cls":"Popup"}},
            {"type":1,"layer":2,"root":{"cls":"Activity"}}
        ]}"#).unwrap();
        let cls: Vec<&str> = app_roots(&dump).iter()
            .map(|r| obj_get(r, "cls").and_then(as_str).unwrap()).collect();
        assert_eq!(cls, ["Activity", "Popup"]);
        let active = parse(r#"{"root":{"cls":"Only"}}"#).unwrap();
        assert_eq!(app_roots(&active).len(), 1);
    }

    #[test]
    fn all_roots_unwraps_every_envelope() {
        let dump = parse(r#"{"windows":[{"type":3,"root":{"cls":"A"}},{"type":1,"root":{"cls":"B"}}]}"#).unwrap();
        assert_eq!(all_roots(&dump).len(), 2);
        assert_eq!(all_roots(&parse(r#"{"ts":1,"root":{"cls":"A"}}"#).unwrap()).len(), 1);
        assert_eq!(all_roots(&parse(r#"{"cls":"A"}"#).unwrap()).len(), 1);
        assert!(all_roots(&parse(r#"{"ts":1}"#).unwrap()).is_empty());
    }
}
