//! JSON value model, parser, and serializer. Stdlib only.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Arr(_) => "array",
            Value::Obj(_) => "object",
        }
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Null | Value::Bool(false))
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self { Value::Num(n) => Some(*n), _ => None }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self { Value::Str(s) => Some(s), _ => None }
    }

    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self { Value::Arr(a) => Some(a), _ => None }
    }

    pub fn as_obj(&self) -> Option<&BTreeMap<String, Value>> {
        match self { Value::Obj(o) => Some(o), _ => None }
    }

    pub fn get_key(&self, k: &str) -> Value {
        match self {
            Value::Obj(o) => o.get(k).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    pub fn get_index(&self, i: i64) -> Value {
        match self {
            Value::Arr(a) => {
                let len = a.len() as i64;
                let idx = if i < 0 { len + i } else { i };
                if idx >= 0 && idx < len { a[idx as usize].clone() } else { Value::Null }
            }
            _ => Value::Null,
        }
    }
}

pub fn format_number(n: f64) -> String {
    if n.is_nan() { return "null".to_string(); }
    if n.is_infinite() {
        return if n > 0.0 { "1.7976931348623157e308".to_string() }
               else { "-1.7976931348623157e308".to_string() };
    }
    if n == n.trunc() && n.abs() < 1e17 {
        format!("{}", n as i64)
    } else {
        let s = format!("{}", n);
        s.replace('E', "e")
    }
}

pub struct Serializer {
    pub indent: Option<usize>,
}

impl Serializer {
    pub fn compact() -> Self { Serializer { indent: None } }
    pub fn pretty(n: usize) -> Self { Serializer { indent: Some(n) } }

    pub fn to_string(&self, v: &Value) -> String {
        let mut out = String::new();
        self.write(&mut out, v, 0);
        out
    }

    fn write(&self, out: &mut String, v: &Value, depth: usize) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Num(n) => out.push_str(&format_number(*n)),
            Value::Str(s) => write_json_string(out, s),
            Value::Arr(a) => {
                if a.is_empty() { out.push_str("[]"); return; }
                out.push('[');
                for (i, item) in a.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    self.newline(out, depth + 1);
                    self.write(out, item, depth + 1);
                }
                self.newline(out, depth);
                out.push(']');
            }
            Value::Obj(o) => {
                if o.is_empty() { out.push_str("{}"); return; }
                out.push('{');
                for (i, (k, val)) in o.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    self.newline(out, depth + 1);
                    write_json_string(out, k);
                    out.push(':');
                    if self.indent.is_some() { out.push(' '); }
                    self.write(out, val, depth + 1);
                }
                self.newline(out, depth);
                out.push('}');
            }
        }
    }

    fn newline(&self, out: &mut String, depth: usize) {
        if let Some(n) = self.indent {
            out.push('\n');
            for _ in 0..(n * depth) { out.push(' '); }
        }
    }
}

pub fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------- Parser ----------

pub struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

pub type PResult<T> = Result<T, String>;

impl<'a> Parser<'a> {
    pub fn new(s: &'a str) -> Self {
        Parser { s: s.as_bytes(), pos: 0 }
    }

    pub fn parse(s: &str) -> PResult<Value> {
        let mut p = Parser::new(s);
        p.skip_ws();
        let v = p.parse_value()?;
        p.skip_ws();
        if p.pos < p.s.len() {
            return Err(format!("trailing characters at position {}", p.pos));
        }
        Ok(v)
    }

    fn skip_ws(&mut self) {
        while self.pos < self.s.len() {
            match self.s[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> PResult<Value> {
        self.skip_ws();
        match self.peek() {
            None => Err("unexpected end of input".to_string()),
            Some(b'n') => { self.expect_lit("null")?; Ok(Value::Null) }
            Some(b't') => { self.expect_lit("true")?; Ok(Value::Bool(true)) }
            Some(b'f') => { self.expect_lit("false")?; Ok(Value::Bool(false)) }
            Some(b'"') => Ok(Value::Str(self.parse_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{}' at position {}", c as char, self.pos)),
        }
    }

    fn expect_lit(&mut self, lit: &str) -> PResult<()> {
        if self.s[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(format!("expected '{}' at position {}", lit, self.pos))
        }
    }

    fn parse_string(&mut self) -> PResult<String> {
        // assumes current char is '"'
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            match c {
                b'"' => { self.pos += 1; return Ok(out); }
                b'\\' => {
                    self.pos += 1;
                    let e = self.peek().ok_or("unterminated escape")?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // high surrogate; expect low
                                if self.peek() == Some(b'\\') {
                                    self.pos += 1;
                                    if self.peek() == Some(b'u') {
                                        self.pos += 1;
                                        let lo = self.parse_hex4()?;
                                        let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                        out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                    } else {
                                        out.push('\u{FFFD}');
                                    }
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return Err(format!("invalid escape '\\{}'", e as char)),
                    }
                }
                _ => {
                    // copy raw UTF-8 byte
                    let start = self.pos;
                    // advance one UTF-8 char
                    let b = self.s[self.pos];
                    let len = if b < 0x80 { 1 } else if b >> 5 == 0b110 { 2 }
                              else if b >> 4 == 0b1110 { 3 } else { 4 };
                    self.pos += len;
                    out.push_str(std::str::from_utf8(&self.s[start..self.pos]).map_err(|_| "invalid utf8")?);
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> PResult<u32> {
        if self.pos + 4 > self.s.len() { return Err("bad \\u escape".to_string()); }
        let hex = std::str::from_utf8(&self.s[self.pos..self.pos+4]).map_err(|_| "bad utf8")?;
        let n = u32::from_str_radix(hex, 16).map_err(|_| format!("bad hex '{}'", hex))?;
        self.pos += 4;
        Ok(n)
    }

    fn parse_number(&mut self) -> PResult<Value> {
        let start = self.pos;
        if self.peek() == Some(b'-') { self.pos += 1; }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() { self.pos += 1; } else { break; }
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() { self.pos += 1; } else { break; }
            }
        }
        if let Some(c) = self.peek() {
            if c == b'e' || c == b'E' {
                self.pos += 1;
                if let Some(s) = self.peek() {
                    if s == b'+' || s == b'-' { self.pos += 1; }
                }
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() { self.pos += 1; } else { break; }
                }
            }
        }
        let txt = std::str::from_utf8(&self.s[start..self.pos]).map_err(|_| "bad utf8")?;
        let n: f64 = txt.parse().map_err(|_| format!("invalid number '{}'", txt))?;
        Ok(Value::Num(n))
    }

    fn parse_array(&mut self) -> PResult<Value> {
        self.pos += 1; // [
        let mut arr = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') { self.pos += 1; return Ok(Value::Arr(arr)); }
        loop {
            let v = self.parse_value()?;
            arr.push(v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b']') => { self.pos += 1; return Ok(Value::Arr(arr)); }
                _ => return Err(format!("expected ',' or ']' at position {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> PResult<Value> {
        self.pos += 1; // {
        let mut obj = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') { self.pos += 1; return Ok(Value::Obj(obj)); }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected string key at position {}", self.pos));
            }
            let k = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at position {}", self.pos));
            }
            self.pos += 1;
            let v = self.parse_value()?;
            obj.insert(k, v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b'}') => { self.pos += 1; return Ok(Value::Obj(obj)); }
                _ => return Err(format!("expected ',' or '}}' at position {}", self.pos)),
            }
        }
    }
}

pub fn parse(s: &str) -> PResult<Value> {
    Parser::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("42").unwrap(), Value::Num(42.0));
        assert_eq!(parse("-3.5").unwrap(), Value::Num(-3.5));
        assert_eq!(parse("\"hi\"").unwrap(), Value::Str("hi".to_string()));
    }

    #[test]
    fn test_parse_containers() {
        let v = parse("[1,2,3]").unwrap();
        assert_eq!(v, Value::Arr(vec![Value::Num(1.0), Value::Num(2.0), Value::Num(3.0)]));
        let o = parse("{\"a\":1,\"b\":[true,null]}").unwrap();
        assert_eq!(o.get_key("a"), Value::Num(1.0));
    }

    #[test]
    fn test_roundtrip() {
        let s = "{\"a\":[1,2,{\"b\":\"x\\ny\"}],\"c\":null}";
        let v = parse(s).unwrap();
        let out = Serializer::compact().to_string(&v);
        let v2 = parse(&out).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn test_escapes() {
        let v = parse("\"a\\u0041b\"").unwrap();
        assert_eq!(v, Value::Str("aAb".to_string()));
    }

    #[test]
    fn test_number_fmt() {
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1.5), "1.5");
    }
}

impl<'a> Parser<'a> {
    pub fn skip_ws_pub(&mut self) { self.skip_ws(); }
    pub fn at_end(&self) -> bool { self.pos >= self.s.len() }
    pub fn parse_value_pub(&mut self) -> PResult<Value> { self.parse_value() }
}
