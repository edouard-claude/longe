//! Recursive-descent parser for jq programs.

use crate::ast::*;
use crate::json::Value;
use crate::lexer::Tok;

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

pub type PResult<T> = Result<T, String>;

impl Parser {
    pub fn new(toks: Vec<Tok>) -> Self { Parser { toks, pos: 0 } }

    pub fn parse_program(src: &str) -> PResult<Expr> {
        let toks = crate::lexer::Lexer::tokenize(src)?;
        let mut p = Parser::new(toks);
        let e = p.parse_expr()?;
        if p.peek() != &Tok::Eof {
            return Err(format!("unexpected token {:?}", p.peek()));
        }
        Ok(e)
    }

    fn peek(&self) -> &Tok { self.toks.get(self.pos).unwrap_or(&Tok::Eof) }
    fn peek_at(&self, n: usize) -> &Tok { self.toks.get(self.pos + n).unwrap_or(&Tok::Eof) }
    fn advance(&mut self) -> Tok {
        let t = self.toks.get(self.pos).cloned().unwrap_or(Tok::Eof);
        self.pos += 1;
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t { self.pos += 1; true } else { false }
    }
    fn expect(&mut self, t: &Tok) -> PResult<()> {
        if self.eat(t) { Ok(()) } else { Err(format!("expected {:?}, got {:?}", t, self.peek())) }
    }
    fn is_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == name)
    }
    fn eat_ident(&mut self, name: &str) -> bool {
        if self.is_ident(name) { self.pos += 1; true } else { false }
    }

    // expr := pipe (',' pipe)*
    fn parse_expr(&mut self) -> PResult<Expr> {
        let mut left = self.parse_pipe()?;
        while self.eat(&Tok::Comma) {
            let right = self.parse_pipe()?;
            left = Expr::Comma(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // pipe := assign ('|' assign)*
    fn parse_pipe(&mut self) -> PResult<Expr> {
        let mut left = self.parse_assign()?;
        while self.eat(&Tok::Pipe) {
            let right = self.parse_assign()?;
            left = Expr::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // assign := 'def' ... | 'reduce' ... | 'if' ... | alt ('=' | '|=' | '//=' alt)?
    fn parse_assign(&mut self) -> PResult<Expr> {
        // def
        if self.is_ident("def") {
            return self.parse_def();
        }
        if self.is_ident("reduce") {
            return self.parse_reduce();
        }
        if self.is_ident("foreach") {
            return self.parse_foreach();
        }
        if self.is_ident("label") {
            self.advance();
            let name = match self.advance() {
                Tok::Ident(s) => s.trim_start_matches('$').to_string(),
                t => return Err(format!("expected label name, got {:?}", t)),
            };
            self.expect(&Tok::Pipe)?;
            let body = self.parse_expr()?;
            return Ok(Expr::Label(name, Box::new(body)));
        }
        if self.is_ident("break") {
            self.advance();
            let name = match self.advance() {
                Tok::Ident(s) => s.trim_start_matches('$').to_string(),
                t => return Err(format!("expected label name, got {:?}", t)),
            };
            return Ok(Expr::Break(name));
        }
        let left = self.parse_alt()?;
        if self.is_ident("as") {
            self.advance();
            let var = match self.advance() {
                Tok::Ident(s) if s.starts_with('$') => s[1..].to_string(),
                t => return Err(format!("expected $var after 'as', got {:?}", t)),
            };
            self.expect(&Tok::Pipe)?;
            let body = self.parse_assign()?;
            return Ok(Expr::As(Box::new(left), var, Box::new(body)));
        }
        if self.eat(&Tok::Assign) {
            let right = self.parse_assign()?;
            return Ok(match left {
                Expr::Var(name) => Expr::Assign(name, Box::new(right)),
                _ => Expr::PathAssign(Box::new(left), Box::new(right)),
            });
        }
        if self.eat(&Tok::Update) {
            let right = self.parse_assign()?;
            return Ok(Expr::PathUpdate(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn parse_def(&mut self) -> PResult<Expr> {
        self.advance(); // def
        let name = match self.advance() {
            Tok::Ident(s) => s.trim_start_matches('$').to_string(),
            t => return Err(format!("expected function name, got {:?}", t)),
        };
        let mut params = Vec::new();
        if self.eat(&Tok::LParen) {
            if !self.eat(&Tok::RParen) {
                loop {
                    match self.advance() {
                        Tok::Ident(s) => params.push(s),
                        t => return Err(format!("expected param name, got {:?}", t)),
                    }
                    if self.eat(&Tok::Semi) { continue; }
                    self.expect(&Tok::RParen)?;
                    break;
                }
            }
        }
        self.expect(&Tok::Colon)?;
        let body = self.parse_expr()?;
        self.expect(&Tok::Semi)?;
        let rest = self.parse_expr()?;
        // Represent def as a Call to a special builtin? Simpler: store in a Def node.
        Ok(Expr::Call(format!("__def__{}", name), vec![
            Expr::Literal(Value::Str(params.join(","))),
            body,
            rest,
        ]))
    }

    fn parse_reduce(&mut self) -> PResult<Expr> {
        self.advance(); // reduce
        let src = self.parse_alt()?;
        if !self.eat_ident("as") {
            return Err("expected 'as' in reduce".to_string());
        }
        let var = match self.advance() {
            Tok::Ident(s) => s.trim_start_matches('$').to_string(),
            t => return Err(format!("expected var, got {:?}", t)),
        };
        self.expect(&Tok::LParen)?;
        let init = self.parse_expr()?;
        self.expect(&Tok::Semi)?;
        let update = self.parse_expr()?;
        self.expect(&Tok::RParen)?;
        Ok(Expr::Reduce(Box::new(src), Box::new(Expr::Var(var)), Box::new(init), Box::new(update)))
    }

    fn parse_foreach(&mut self) -> PResult<Expr> {
        self.advance(); // foreach
        let src = self.parse_alt()?;
        if !self.eat_ident("as") {
            return Err("expected 'as' in foreach".to_string());
        }
        let var = match self.advance() {
            Tok::Ident(s) => s.trim_start_matches('$').to_string(),
            t => return Err(format!("expected var, got {:?}", t)),
        };
        self.expect(&Tok::LParen)?;
        let init = self.parse_expr()?;
        self.expect(&Tok::Semi)?;
        let update = self.parse_expr()?;
        let extract = if self.eat(&Tok::Semi) {
            Some(Box::new(self.parse_expr()?))
        } else { None };
        self.expect(&Tok::RParen)?;
        Ok(Expr::Foreach(Box::new(src), Box::new(Expr::Var(var)),
                         Box::new(init), Box::new(update), extract))
    }

    // alt := or ('//' or)*
    fn parse_alt(&mut self) -> PResult<Expr> {
        let mut left = self.parse_or()?;
        while self.eat(&Tok::Alt) {
            let right = self.parse_or()?;
            left = Expr::BinOp(BinOp::Alt, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut left = self.parse_and()?;
        while self.eat_ident("or") {
            let right = self.parse_and()?;
            left = Expr::BinOp(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut left = self.parse_cmp()?;
        while self.eat_ident("and") {
            let right = self.parse_cmp()?;
            left = Expr::BinOp(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> PResult<Expr> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek() {
                Tok::Eq => BinOp::Eq,
                Tok::Ne => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_add()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> PResult<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> PResult<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.eat(&Tok::Minus) {
            let e = self.parse_unary()?;
            return Ok(Expr::Neg(Box::new(e)));
        }
        if self.is_ident("not") {
            self.advance();
            return Ok(Expr::Call("not".to_string(), vec![]));
        }
        if self.is_ident("try") {
            self.advance();
            let e = self.parse_postfix()?;
            let handler = if self.eat_ident("catch") {
                Some(Box::new(self.parse_postfix()?))
            } else { None };
            return Ok(Expr::Try(Box::new(e), handler));
        }
        self.parse_postfix()
    }

    // postfix := primary ('?' | '.' field | '[' index ']' | '(' args ')')*
    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::Question => { self.advance(); e = Expr::Optional(Box::new(e)); }
                Tok::Dot => {
                    self.advance();
                    match self.peek().clone() {
                        Tok::Ident(name) => {
                            self.advance();
                            e = Expr::Pipe(Box::new(e), Box::new(Expr::Field(name)));
                        }
                        Tok::Str(name) => {
                            self.advance();
                            e = Expr::Pipe(Box::new(e), Box::new(Expr::Field(name)));
                        }
                        Tok::LBracket => {
                            // .[ ... ] on e
                            self.advance();
                            let idx = self.parse_index_body()?;
                            e = Expr::Pipe(Box::new(e), Box::new(idx));
                        }
                        _ => {
                            // bare '.' after expr is identity pipe
                            e = Expr::Pipe(Box::new(e), Box::new(Expr::Identity));
                        }
                    }
                }
                Tok::LBracket => {
                    self.advance();
                    let idx = self.parse_index_body()?;
                    e = Expr::Pipe(Box::new(e), Box::new(idx));
                }
                _ => break,
            }
        }
        Ok(e)
    }

    // after '[' consumed: parse index/slice/iterate, then ']'
    fn parse_index_body(&mut self) -> PResult<Expr> {
        if self.eat(&Tok::RBracket) {
            return Ok(Expr::Iterate);
        }
        // slice with leading colon
        if self.eat(&Tok::Colon) {
            let hi = if self.peek() == &Tok::RBracket { None } else { Some(Box::new(self.parse_expr()?)) };
            self.expect(&Tok::RBracket)?;
            return Ok(Expr::Slice(None, hi));
        }
        let first = self.parse_expr()?;
        if self.eat(&Tok::Colon) {
            let hi = if self.peek() == &Tok::RBracket { None } else { Some(Box::new(self.parse_expr()?)) };
            self.expect(&Tok::RBracket)?;
            return Ok(Expr::Slice(Some(Box::new(first)), hi));
        }
        self.expect(&Tok::RBracket)?;
        Ok(Expr::Index(Box::new(first)))
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Dot => {
                self.advance();
                // .foo or .["x"] or .[e] or .[] or just .
                match self.peek().clone() {
                    Tok::Ident(name) if !is_keyword(&name) => {
                        self.advance();
                        Ok(Expr::Field(name))
                    }
                    Tok::Str(name) => {
                        self.advance();
                        Ok(Expr::Field(name))
                    }
                    Tok::LBracket => {
                        self.advance();
                        self.parse_index_body()
                    }
                    _ => Ok(Expr::Identity),
                }
            }
            Tok::DotDot => { self.advance(); Ok(Expr::Recurse) }
            Tok::Num(n) => { self.advance(); Ok(Expr::Literal(Value::Num(n))) }
            Tok::Str(s) => { self.advance(); Ok(Expr::Literal(Value::Str(s))) }
            Tok::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                self.advance();
                if self.eat(&Tok::RBracket) {
                    return Ok(Expr::Literal(Value::Arr(vec![])));
                }
                let e = self.parse_expr()?;
                self.expect(&Tok::RBracket)?;
                Ok(Expr::Array(Box::new(e)))
            }
            Tok::LBrace => self.parse_object(),
            Tok::Ident(name) => self.parse_ident_expr(name),
            t => Err(format!("unexpected token {:?}", t)),
        }
    }

    fn parse_ident_expr(&mut self, name: String) -> PResult<Expr> {
        if let Some(v) = name.strip_prefix('$') {
            self.advance();
            return Ok(Expr::Var(v.to_string()));
        }
        match name.as_str() {
            "true" => { self.advance(); return Ok(Expr::Literal(Value::Bool(true))); }
            "false" => { self.advance(); return Ok(Expr::Literal(Value::Bool(false))); }
            "null" => { self.advance(); return Ok(Expr::Literal(Value::Null)); }
            "if" => return self.parse_if(),
            _ => {}
        }
        self.advance();
        // function call with args?
        if self.peek() == &Tok::LParen {
            self.advance();
            let mut args = Vec::new();
            if !self.eat(&Tok::RParen) {
                loop {
                    args.push(self.parse_expr()?);
                    if self.eat(&Tok::Semi) { continue; }
                    self.expect(&Tok::RParen)?;
                    break;
                }
            }
            return Ok(Expr::Call(name, args));
        }
        Ok(Expr::Call(name, vec![]))
    }

    fn parse_if(&mut self) -> PResult<Expr> {
        self.advance(); // if
        let cond = self.parse_expr()?;
        if !self.eat_ident("then") {
            return Err("expected 'then'".to_string());
        }
        let then = self.parse_expr()?;
        let els = if self.eat_ident("elif") {
            // rewrite elif as nested if
            let nested = self.parse_if_after_elif()?;
            Some(Box::new(nested))
        } else if self.eat_ident("else") {
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };
        if !self.eat_ident("end") {
            return Err("expected 'end'".to_string());
        }
        Ok(Expr::If(Box::new(cond), Box::new(then), els))
    }

    fn parse_if_after_elif(&mut self) -> PResult<Expr> {
        // we've consumed 'elif'; parse cond then ... end
        let cond = self.parse_expr()?;
        if !self.eat_ident("then") {
            return Err("expected 'then'".to_string());
        }
        let then = self.parse_expr()?;
        let els = if self.eat_ident("elif") {
            Some(Box::new(self.parse_if_after_elif()?))
        } else if self.eat_ident("else") {
            Some(Box::new(self.parse_expr()?))
        } else { None };
        if !self.eat_ident("end") {
            return Err("expected 'end'".to_string());
        }
        Ok(Expr::If(Box::new(cond), Box::new(then), els))
    }

    fn parse_object(&mut self) -> PResult<Expr> {
        self.advance(); // {
        let mut entries = Vec::new();
        if self.eat(&Tok::RBrace) {
            return Ok(Expr::Object(entries));
        }
        loop {
            let key = match self.peek().clone() {
                Tok::Ident(name) => { self.advance(); ObjKey::Ident(name) }
                Tok::Str(s) => { self.advance(); ObjKey::Str(s) }
                Tok::LParen => {
                    self.advance();
                    let e = self.parse_expr()?;
                    self.expect(&Tok::RParen)?;
                    ObjKey::Expr(e)
                }
                t => return Err(format!("bad object key {:?}", t)),
            };
            let val = if self.eat(&Tok::Colon) {
                self.parse_pipe()?
            } else {
                // shorthand {foo} means {foo: .foo}
                match &key {
                    ObjKey::Ident(n) => Expr::Field(n.clone()),
                    ObjKey::Str(s) => Expr::Field(s.clone()),
                    ObjKey::Expr(_) => return Err("expected ':'".to_string()),
                }
            };
            entries.push((key, val));
            if self.eat(&Tok::Comma) { continue; }
            self.expect(&Tok::RBrace)?;
            break;
        }
        Ok(Expr::Object(entries))
    }
}

pub fn parse(src: &str) -> PResult<Expr> {
    Parser::parse_program(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr { parse(s).unwrap() }

    #[test]
    fn test_basic() {
        assert_eq!(p("."), Expr::Identity);
        assert_eq!(p(".foo"), Expr::Field("foo".to_string()));
        assert_eq!(p(".foo.bar"),
            Expr::Pipe(Box::new(Expr::Field("foo".to_string())),
                       Box::new(Expr::Field("bar".to_string()))));
    }

    #[test]
    fn test_pipe_comma() {
        assert!(matches!(p(".a | .b"), Expr::Pipe(_, _)));
        assert!(matches!(p(".a, .b"), Expr::Comma(_, _)));
    }

    #[test]
    fn test_arith() {
        assert!(matches!(p("1 + 2 * 3"), Expr::BinOp(BinOp::Add, _, _)));
        assert!(matches!(p("1 + 2 * 3"),
            Expr::BinOp(BinOp::Add, _, ref r) if matches!(**r, Expr::BinOp(BinOp::Mul, _, _))));
    }

    #[test]
    fn test_array_object() {
        assert!(matches!(p("[1,2]"), Expr::Array(_)));
        assert!(matches!(p("{a: 1, b: 2}"), Expr::Object(_)));
        assert!(matches!(p("{a}"), Expr::Object(_)));
    }

    #[test]
    fn test_if() {
        assert!(matches!(p("if .a then 1 else 2 end"), Expr::If(_, _, Some(_))));
        assert!(matches!(p("if .a then 1 end"), Expr::If(_, _, None)));
    }

    #[test]
    fn test_index_slice() {
        assert!(matches!(p(".[0]"), Expr::Index(_)));
        assert!(matches!(p(".[1:2]"), Expr::Slice(Some(_), Some(_))));
        assert!(matches!(p(".[:2]"), Expr::Slice(None, Some(_))));
        assert!(matches!(p(".[]"), Expr::Iterate));
    }

    #[test]
    fn test_call() {
        assert!(matches!(p("length"), Expr::Call(_, _)));
        assert!(matches!(p("map(.+1)"), Expr::Call(_, ref a) if a.len() == 1));
    }

    #[test]
    fn test_reduce() {
        assert!(matches!(p("reduce .[] as $x (0; . + $x)"), Expr::Reduce(_, _, _, _)));
    }
}

fn is_keyword(s: &str) -> bool {
    matches!(s, "as" | "then" | "else" | "elif" | "end" | "and" | "or" | "catch")
}
