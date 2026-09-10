//! Evaluator: each expression maps an input Value to a stream (Vec) of Values.

use crate::ast::*;
use crate::json::Value;
use std::collections::HashMap;

pub type EResult = Result<Vec<Value>, String>;

pub struct Env {
    pub vars: HashMap<String, Value>,
    pub funcs: HashMap<String, (Vec<String>, Expr)>,
}

impl Env {
    pub fn new() -> Self {
        Env { vars: HashMap::new(), funcs: HashMap::new() }
    }
    pub fn child(&self) -> Self {
        Env { vars: self.vars.clone(), funcs: self.funcs.clone() }
    }
}

pub fn eval(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Identity => Ok(vec![input.clone()]),
        Expr::Recurse => {
            let mut out = Vec::new();
            recurse(input, &mut out);
            Ok(out)
        }
        Expr::Field(name) => Ok(vec![input.get_key(name)]),
        Expr::Literal(v) => Ok(vec![v.clone()]),
        Expr::Var(name) => {
            match env.vars.get(name) {
                Some(v) => Ok(vec![v.clone()]),
                None => Err(format!("${} is not defined", name)),
            }
        }
        _ => eval_rest(expr, input, env),
    }
}

fn recurse(v: &Value, out: &mut Vec<Value>) {
    out.push(v.clone());
    match v {
        Value::Arr(a) => { for x in a { recurse(x, out); } }
        Value::Obj(o) => { for (_, x) in o { recurse(x, out); } }
        _ => {}
    }
}

fn eval_rest(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Pipe(a, b) => {
            let mut out = Vec::new();
            for v in eval(a, input, env)? {
                out.extend(eval(b, &v, env)?);
            }
            Ok(out)
        }
        Expr::Comma(a, b) => {
            let mut out = eval(a, input, env)?;
            out.extend(eval(b, input, env)?);
            Ok(out)
        }
        Expr::Array(inner) => {
            let vals = eval(inner, input, env)?;
            Ok(vec![Value::Arr(vals)])
        }
        Expr::Object(entries) => {
            let mut obj = std::collections::BTreeMap::new();
            for (k, vexpr) in entries {
                let key = match k {
                    ObjKey::Ident(s) => s.clone(),
                    ObjKey::Str(s) => s.clone(),
                    ObjKey::Expr(e) => {
                        let ks = eval(e, input, env)?;
                        match ks.into_iter().next() {
                            Some(Value::Str(s)) => s,
                            Some(other) => crate::json::Serializer::compact().to_string(&other),
                            None => continue,
                        }
                    }
                };
                let vals = eval(vexpr, input, env)?;
                let val = vals.into_iter().next().unwrap_or(Value::Null);
                obj.insert(key, val);
            }
            Ok(vec![Value::Obj(obj)])
        }
        Expr::Iterate => {
            match input {
                Value::Arr(a) => Ok(a.clone()),
                Value::Obj(o) => Ok(o.values().cloned().collect()),
                _ => Err(format!("Cannot iterate over {}", input.type_name())),
            }
        }
        _ => eval_ops(expr, input, env),
    }
}

fn eval_ops(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Index(idx) => {
            let keys = eval(idx, input, env)?;
            let mut out = Vec::new();
            for k in keys {
                match k {
                    Value::Num(n) => out.push(input.get_index(n as i64)),
                    Value::Str(s) => out.push(input.get_key(&s)),
                    _ => return Err(format!("Cannot index {} with {}", input.type_name(), k.type_name())),
                }
            }
            Ok(out)
        }
        Expr::Slice(lo, hi) => {
            let lo_v = match lo { Some(e) => eval(e, input, env)?.into_iter().next(), None => None };
            let hi_v = match hi { Some(e) => eval(e, input, env)?.into_iter().next(), None => None };
            let lo_i = lo_v.and_then(|v| v.as_f64()).map(|n| n as i64);
            let hi_i = hi_v.and_then(|v| v.as_f64()).map(|n| n as i64);
            match input {
                Value::Arr(a) => {
                    let len = a.len() as i64;
                    let s = norm(lo_i.unwrap_or(0), len);
                    let e = norm(hi_i.unwrap_or(len), len);
                    let s = s.max(0); let e = e.min(len).max(s);
                    Ok(vec![Value::Arr(a[s as usize..e as usize].to_vec())])
                }
                Value::Str(st) => {
                    let chars: Vec<char> = st.chars().collect();
                    let len = chars.len() as i64;
                    let s = norm(lo_i.unwrap_or(0), len).max(0);
                    let e = norm(hi_i.unwrap_or(len), len).min(len).max(s);
                    Ok(vec![Value::Str(chars[s as usize..e as usize].iter().collect())])
                }
                _ => Err(format!("Cannot slice {}", input.type_name())),
            }
        }
        _ => eval_ops2(expr, input, env),
    }
}

fn norm(i: i64, len: i64) -> i64 {
    if i < 0 { (len + i).max(0) } else { i }
}

fn eval_ops2(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Neg(e) => {
            let vals = eval(e, input, env)?;
            let mut out = Vec::new();
            for v in vals {
                match v {
                    Value::Num(n) => out.push(Value::Num(-n)),
                    _ => return Err(format!("{} cannot be negated", v.type_name())),
                }
            }
            Ok(out)
        }
        Expr::Unary(UnOp::Not, e) => {
            let vals = eval(e, input, env)?;
            Ok(vals.into_iter().map(|v| Value::Bool(!v.is_truthy())).collect())
        }
        Expr::BinOp(op, a, b) => {
            if matches!(op, BinOp::And | BinOp::Or) {
                let avals = eval(a, input, env)?;
                let bvals = eval(b, input, env)?;
                let mut out = Vec::new();
                for av in &avals {
                    for bv in &bvals {
                        out.push(apply_binop(*op, av, bv)?);
                    }
                }
                return Ok(out);
            }
            let avals = eval(a, input, env)?;
            let mut out = Vec::new();
            for av in avals {
                let bvals = eval(b, &av, env)?;
                for bv in bvals {
                    out.push(apply_binop(*op, &av, &bv)?);
                }
            }
            Ok(out)
        }
        Expr::If(cond, then, els) => {
            let cvals = eval(cond, input, env)?;
            let mut out = Vec::new();
            for c in cvals {
                if c.is_truthy() {
                    out.extend(eval(then, input, env)?);
                } else if let Some(e) = els {
                    out.extend(eval(e, input, env)?);
                } else {
                    out.push(Value::Null);
                }
            }
            Ok(out)
        }
        Expr::Optional(e) => {
            match eval(e, input, env) {
                Ok(v) => Ok(v),
                Err(_) => Ok(vec![]),
            }
        }
        Expr::Try(e, handler) => {
            match eval(e, input, env) {
                Ok(v) => Ok(v),
                Err(msg) => match handler {
                    Some(h) => eval(h, &Value::Str(msg), env),
                    None => Ok(vec![]),
                },
            }
        }
        _ => eval_ops3(expr, input, env),
    }
}

fn apply_binop(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    use BinOp::*;
    match op {
        Add => match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok(Value::Num(x + y)),
            (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{}{}", x, y))),
            (Value::Arr(x), Value::Arr(y)) => {
                let mut v = x.clone(); v.extend(y.clone()); Ok(Value::Arr(v))
            }
            (Value::Obj(x), Value::Obj(y)) => {
                let mut o = x.clone();
                for (k, v) in y { o.insert(k.clone(), v.clone()); }
                Ok(Value::Obj(o))
            }
            (Value::Null, y) => Ok(y.clone()),
            (x, Value::Null) => Ok(x.clone()),
            _ => Err(format!("{} and {} cannot be added", a.type_name(), b.type_name())),
        },
        Sub => match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok(Value::Num(x - y)),
            (Value::Arr(x), Value::Arr(y)) => {
                Ok(Value::Arr(x.iter().filter(|e| !y.contains(e)).cloned().collect()))
            }
            _ => Err(format!("{} and {} cannot be subtracted", a.type_name(), b.type_name())),
        },
        Mul => match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok(Value::Num(x * y)),
            (Value::Str(x), Value::Num(y)) => {
                let n = *y as i64;
                if n <= 0 { Ok(Value::Null) } else { Ok(Value::Str(x.repeat(n as usize))) }
            }
            (Value::Num(x), Value::Str(y)) => {
                let n = *x as i64;
                if n <= 0 { Ok(Value::Null) } else { Ok(Value::Str(y.repeat(n as usize))) }
            }
            _ => Err(format!("{} and {} cannot be multiplied", a.type_name(), b.type_name())),
        },
        Div => match (a, b) {
            (Value::Num(x), Value::Num(y)) => {
                if *y == 0.0 { Err("division by zero".to_string()) }
                else { Ok(Value::Num(x / y)) }
            }
            (Value::Str(x), Value::Str(y)) => Ok(Value::Arr(x.split(y.as_str()).map(|s| Value::Str(s.to_string())).collect())),
            _ => Err(format!("{} and {} cannot be divided", a.type_name(), b.type_name())),
        },
        Mod => match (a, b) {
            (Value::Num(x), Value::Num(y)) => {
                if *y == 0.0 { Err("division by zero".to_string()) }
                else { Ok(Value::Num(x % y)) }
            }
            _ => Err(format!("{} and {} cannot be modulo'd", a.type_name(), b.type_name())),
        },
        _ => apply_cmp(op, a, b),
    }
}

fn apply_cmp(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(a == b)),
        Ne => Ok(Value::Bool(a != b)),
        Lt => Ok(Value::Bool(cmp(a, b) == std::cmp::Ordering::Less)),
        Le => Ok(Value::Bool(cmp(a, b) != std::cmp::Ordering::Greater)),
        Gt => Ok(Value::Bool(cmp(a, b) == std::cmp::Ordering::Greater)),
        Ge => Ok(Value::Bool(cmp(a, b) != std::cmp::Ordering::Less)),
        And => Ok(Value::Bool(a.is_truthy() && b.is_truthy())),
        Or => Ok(Value::Bool(a.is_truthy() || b.is_truthy())),
        Alt => {
            if a.is_truthy() { Ok(a.clone()) } else { Ok(b.clone()) }
        }
        _ => Err("bad op".to_string()),
    }
}

fn type_order(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Num(_) => 3,
        Value::Str(_) => 4,
        Value::Arr(_) => 5,
        Value::Obj(_) => 6,
    }
}

pub fn cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ta, tb) = (type_order(a), type_order(b));
    if ta != tb { return ta.cmp(&tb); }
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Arr(x), Value::Arr(y)) => {
            for (ea, eb) in x.iter().zip(y.iter()) {
                let c = cmp(ea, eb);
                if c != Ordering::Equal { return c; }
            }
            x.len().cmp(&y.len())
        }
        (Value::Obj(x), Value::Obj(y)) => {
            let mut xk: Vec<_> = x.keys().collect();
            let mut yk: Vec<_> = y.keys().collect();
            xk.sort(); yk.sort();
            for (ka, kb) in xk.iter().zip(yk.iter()) {
                let c = ka.cmp(kb);
                if c != Ordering::Equal { return c; }
                let c = cmp(&x[*ka], &y[*kb]);
                if c != Ordering::Equal { return c; }
            }
            xk.len().cmp(&yk.len())
        }
        _ => Ordering::Equal,
    }
}

fn eval_ops3(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Call(name, args) => crate::builtins::call(name, args, input, env),
        Expr::Reduce(src, var, init, update) => {
            let var_name = match &**var { Expr::Var(n) => n.clone(), _ => return Err("bad reduce var".to_string()) };
            let items = eval(src, input, env)?;
            let mut accs = eval(init, input, env)?;
            for item in items {
                let mut next = Vec::new();
                for acc in &accs {
                    let mut e2 = env.child();
                    e2.vars.insert(var_name.clone(), item.clone());
                    next.extend(eval(update, acc, &e2)?);
                }
                accs = next;
            }
            Ok(accs)
        }
        Expr::Foreach(src, var, init, update, extract) => {
            let var_name = match &**var { Expr::Var(n) => n.clone(), _ => return Err("bad foreach var".to_string()) };
            let items = eval(src, input, env)?;
            let mut accs = eval(init, input, env)?;
            let mut out = Vec::new();
            for item in items {
                let mut next = Vec::new();
                for acc in &accs {
                    let mut e2 = env.child();
                    e2.vars.insert(var_name.clone(), item.clone());
                    for nv in eval(update, acc, &e2)? {
                        if let Some(ex) = extract {
                            let mut e3 = env.child();
                            e3.vars.insert(var_name.clone(), item.clone());
                            out.extend(eval(ex, &nv, &e3)?);
                        } else {
                            out.push(nv.clone());
                        }
                        next.push(nv);
                    }
                }
                accs = next;
            }
            Ok(out)
        }
        Expr::As(src, var, body) => {
            let vals = eval(src, input, env)?;
            let mut out = Vec::new();
            for v in vals {
                let mut e2 = env.child();
                e2.vars.insert(var.clone(), v);
                out.extend(eval(body, input, &e2)?);
            }
            Ok(out)
        }
        _ => eval_ops4(expr, input, env),
    }
}

fn eval_ops4(expr: &Expr, input: &Value, env: &Env) -> EResult {
    match expr {
        Expr::Assign(name, e) => {
            let vals = eval(e, input, env)?;
            let v = vals.into_iter().next().unwrap_or(Value::Null);
            let mut e2 = env.child();
            e2.vars.insert(name.clone(), v);
            // assignment returns the input unchanged (jq semantics: $x = e | .)
            Ok(vec![input.clone()])
        }
        Expr::PathAssign(path, e) => {
            let vals = eval(e, input, env)?;
            let v = vals.into_iter().next().unwrap_or(Value::Null);
            let paths = crate::paths::collect_paths(path, input, env)?;
            let mut result = input.clone();
            for p in paths {
                result = crate::paths::set_path(&result, &p, &v);
            }
            Ok(vec![result])
        }
        Expr::PathUpdate(path, e) => {
            let paths = crate::paths::collect_paths(path, input, env)?;
            let mut result = input.clone();
            for p in paths {
                let old = crate::paths::get_path(&result, &p);
                let newv = eval(e, &old, env)?.into_iter().next().unwrap_or(Value::Null);
                result = crate::paths::set_path(&result, &p, &newv);
            }
            Ok(vec![result])
        }
        Expr::Label(name, body) => {
            match eval(body, input, env) {
                Ok(v) => Ok(v),
                Err(msg) => {
                    if let Some(rest) = msg.strip_prefix("__break__:") {
                        if rest == name { Ok(vec![input.clone()]) }
                        else { Err(msg) }
                    } else { Err(msg) }
                }
            }
        }
        Expr::Break(name) => Err(format!("__break__:{}", name)),
        Expr::StringInterp(parts) => {
            let mut s = String::new();
            for p in parts {
                let vals = eval(p, input, env)?;
                for v in vals {
                    match v {
                        Value::Str(x) => s.push_str(&x),
                        other => s.push_str(&crate::json::Serializer::compact().to_string(&other)),
                    }
                }
            }
            Ok(vec![Value::Str(s)])
        }
        _ => Err(format!("unhandled expr: {:?}", expr)),
    }
}

pub fn apply_binop_pub(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    apply_binop(op, a, b)
}
