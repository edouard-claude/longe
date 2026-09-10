//! AST for jq programs.

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Identity,                       // .
    Recurse,                        // ..
    Field(String),                  // .foo  (applied to input)
    Index(Box<Expr>),               // .[e]
    Slice(Option<Box<Expr>>, Option<Box<Expr>>), // .[a:b]
    Iterate,                        // .[]
    Literal(crate::json::Value),    // 42, "x", true, null
    Pipe(Box<Expr>, Box<Expr>),     // a | b
    Comma(Box<Expr>, Box<Expr>),    // a, b
    Array(Box<Expr>),               // [e]
    Object(Vec<(ObjKey, Expr)>),    // {k: v, ...}
    Neg(Box<Expr>),                 // -e
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Call(String, Vec<Expr>),        // f(args)
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>), // if c then a else b end
    Try(Box<Expr>, Option<Box<Expr>>),           // try e catch h
    Reduce(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>), // reduce src as $x (init; update)
    Foreach(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    As(Box<Expr>, String, Box<Expr>),            // e as $x | body
    Var(String),                                 // $x
    Optional(Box<Expr>),                         // e?
    Assign(String, Box<Expr>),                   // $x = e  (var assign)
    PathAssign(Box<Expr>, Box<Expr>),            // path = e
    PathUpdate(Box<Expr>, Box<Expr>),            // path |= e
    PathAlt(Box<Expr>, Box<Expr>),               // path //= e
    Label(String, Box<Expr>),
    Break(String),
    StringInterp(Vec<Expr>),                     // "a\(e)b"
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObjKey {
    Ident(String),
    Str(String),
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp { Add, Sub, Mul, Div, Mod, Eq, Ne, Lt, Le, Gt, Ge, And, Or, Alt }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UnOp { Not }
