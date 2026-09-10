//! Tokenizer for jq programs.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    // literals
    Num(f64),
    Str(String),
    InterpStr(Vec<String>), // alternating literal/expr-source
    Ident(String),
    // punctuation
    Dot, DotDot, Pipe, Comma, Colon, Semi,
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Question, Plus, Minus, Star, Slash, Percent,
    Eq, Ne, Lt, Le, Gt, Ge,
    Assign, Update, Alt, // = |= //
    // keywords handled as idents: if then else elif end and or not as def reduce foreach
    Eof,
}

pub struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(s: &'a str) -> Self { Lexer { s: s.as_bytes(), pos: 0 } }

    pub fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
        let mut lx = Lexer::new(s);
        let mut toks = Vec::new();
        loop {
            let t = lx.next_token()?;
            let end = t == Tok::Eof;
            toks.push(t);
            if end { break; }
        }
        Ok(toks)
    }


    fn peek(&self) -> Option<u8> { self.s.get(self.pos).copied() }
    fn peek2(&self) -> Option<u8> { self.s.get(self.pos + 1).copied() }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => self.pos += 1,
                Some(b'#') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' { break; }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn next_token(&mut self) -> Result<Tok, String> {
        self.skip_ws_and_comments();
        let c = match self.peek() {
            None => return Ok(Tok::Eof),
            Some(c) => c,
        };
        match c {
            b'.' => {
                if self.peek2() == Some(b'.') { self.pos += 2; Ok(Tok::DotDot) }
                else { self.pos += 1; Ok(Tok::Dot) }
            }
            b'|' => {
                if self.peek2() == Some(b'=') { self.pos += 2; Ok(Tok::Update) }
                else { self.pos += 1; Ok(Tok::Pipe) }
            }
            b'/' => {
                if self.peek2() == Some(b'/') { self.pos += 2; Ok(Tok::Alt) }
                else { self.pos += 1; Ok(Tok::Slash) }
            }
            b'=' => {
                if self.peek2() == Some(b'=') { self.pos += 2; Ok(Tok::Eq) }
                else { self.pos += 1; Ok(Tok::Assign) }
            }
            b'!' => {
                if self.peek2() == Some(b'=') { self.pos += 2; Ok(Tok::Ne) }
                else { Err("unexpected '!'".to_string()) }
            }
            b'<' => {
                if self.peek2() == Some(b'=') { self.pos += 2; Ok(Tok::Le) }
                else { self.pos += 1; Ok(Tok::Lt) }
            }
            b'>' => {
                if self.peek2() == Some(b'=') { self.pos += 2; Ok(Tok::Ge) }
                else { self.pos += 1; Ok(Tok::Gt) }
            }
            b',' => { self.pos += 1; Ok(Tok::Comma) }
            b':' => { self.pos += 1; Ok(Tok::Colon) }
            b';' => { self.pos += 1; Ok(Tok::Semi) }
            b'(' => { self.pos += 1; Ok(Tok::LParen) }
            b')' => { self.pos += 1; Ok(Tok::RParen) }
            b'[' => { self.pos += 1; Ok(Tok::LBracket) }
            b']' => { self.pos += 1; Ok(Tok::RBracket) }
            b'{' => { self.pos += 1; Ok(Tok::LBrace) }
            b'}' => { self.pos += 1; Ok(Tok::RBrace) }
            b'?' => { self.pos += 1; Ok(Tok::Question) }
            b'$' => { self.pos += 1; let t = self.lex_ident()?; match t { Tok::Ident(s) => Ok(Tok::Ident(format!("${}", s))), _ => Ok(t) } }
            b'+' => { self.pos += 1; Ok(Tok::Plus) }
            b'-' => { self.pos += 1; Ok(Tok::Minus) }
            b'*' => { self.pos += 1; Ok(Tok::Star) }
            b'%' => { self.pos += 1; Ok(Tok::Percent) }
            b'"' => self.lex_string(),
            c if c.is_ascii_digit() => self.lex_number(),
            c if c == b'_' || c.is_ascii_alphabetic() => self.lex_ident(),
            c => Err(format!("unexpected character '{}'", c as char)),
        }
    }

    fn lex_ident(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == b'_' || c.is_ascii_alphanumeric() { self.pos += 1; } else { break; }
        }
        let s = std::str::from_utf8(&self.s[start..self.pos]).unwrap().to_string();
        Ok(Tok::Ident(s))
    }

    fn lex_number(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() { self.pos += 1; } else { break; }
        }
        if self.peek() == Some(b'.') && self.peek2().map_or(false, |c| c.is_ascii_digit()) {
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
        let txt = std::str::from_utf8(&self.s[start..self.pos]).unwrap();
        let n: f64 = txt.parse().map_err(|_| format!("bad number '{}'", txt))?;
        Ok(Tok::Num(n))
    }

    fn lex_string(&mut self) -> Result<Tok, String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            match c {
                b'"' => { self.pos += 1; return Ok(Tok::Str(out)); }
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
                            let cp = self.hex4()?;
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        b'(' => {
                            // jq string interpolation \(expr) - handle later; for now error
                            return Err("string interpolation not yet supported".to_string());
                        }
                        _ => return Err(format!("invalid escape '\\{}'", e as char)),
                    }
                }
                _ => {
                    let start = self.pos;
                    let b = self.s[self.pos];
                    let len = if b < 0x80 { 1 } else if b >> 5 == 0b110 { 2 }
                              else if b >> 4 == 0b1110 { 3 } else { 4 };
                    self.pos += len;
                    out.push_str(std::str::from_utf8(&self.s[start..self.pos]).map_err(|_| "bad utf8")?);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.s.len() { return Err("bad \\u".to_string()); }
        let hex = std::str::from_utf8(&self.s[self.pos..self.pos+4]).unwrap();
        let n = u32::from_str_radix(hex, 16).map_err(|_| "bad hex".to_string())?;
        self.pos += 4;
        Ok(n)
    }
}
