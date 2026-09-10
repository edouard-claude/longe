//! Path expression support for assignment (path = value, path |= f).

use crate::ast::Expr;
use crate::eval::{eval, Env};
use crate::json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum PathComp {
    Key(String),
    Index(i64),
}

pub type Path = Vec<PathComp>;

pub fn collect_paths(expr: &Expr, input: &Value, env: &Env) -> Result<Vec<Path>, String> {
    match expr {
        Expr::Identity => Ok(vec![vec![]]),
        Expr::Field(name) => Ok(vec![vec![PathComp::Key(name.clone())]]),
        Expr::Pipe(a, b) => {
            let mut out = Vec::new();
            for pa in collect_paths(a, input, env)? {
                let sub = get_path(input, &pa);
                for pb in collect_paths(b, &sub, env)? {
                    let mut p = pa.clone();
                    p.extend(pb);
                    out.push(p);
                }
            }
            Ok(out)
        }
        Expr::Comma(a, b) => {
            let mut out = collect_paths(a, input, env)?;
            out.extend(collect_paths(b, input, env)?);
            Ok(out)
        }
        Expr::Index(idx) => {
            let keys = eval(idx, input, env)?;
            let mut out = Vec::new();
            for k in keys {
                match k {
                    Value::Num(n) => out.push(vec![PathComp::Index(n as i64)]),
                    Value::Str(s) => out.push(vec![PathComp::Key(s)]),
                    _ => return Err("bad path index".to_string()),
                }
            }
            Ok(out)
        }
        Expr::Iterate => {
            match input {
                Value::Arr(a) => Ok((0..a.len()).map(|i| vec![PathComp::Index(i as i64)]).collect()),
                Value::Obj(o) => Ok(o.keys().map(|k| vec![PathComp::Key(k.clone())]).collect()),
                _ => Err("cannot iterate".to_string()),
            }
        }
        Expr::Optional(e) => collect_paths(e, input, env),
        _ => Err(format!("invalid path expression: {:?}", expr)),
    }
}

pub fn get_path(v: &Value, path: &[PathComp]) -> Value {
    let mut cur = v.clone();
    for c in path {
        cur = match c {
            PathComp::Key(k) => cur.get_key(k),
            PathComp::Index(i) => cur.get_index(*i),
        };
    }
    cur
}

pub fn set_path(v: &Value, path: &[PathComp], newv: &Value) -> Value {
    if path.is_empty() { return newv.clone(); }
    match &path[0] {
        PathComp::Key(k) => {
            let mut obj = match v {
                Value::Obj(o) => o.clone(),
                _ => std::collections::BTreeMap::new(),
            };
            let sub = obj.get(k).cloned().unwrap_or(Value::Null);
            obj.insert(k.clone(), set_path(&sub, &path[1..], newv));
            Value::Obj(obj)
        }
        PathComp::Index(i) => {
            let mut arr = match v {
                Value::Arr(a) => a.clone(),
                _ => Vec::new(),
            };
            let len = arr.len() as i64;
            let idx = if *i < 0 { len + *i } else { *i };
            if idx < 0 { return v.clone(); }
            let idx = idx as usize;
            while arr.len() <= idx { arr.push(Value::Null); }
            let sub = arr[idx].clone();
            arr[idx] = set_path(&sub, &path[1..], newv);
            Value::Arr(arr)
        }
    }
}

pub fn del_path(v: &Value, path: &[PathComp]) -> Value {
    if path.is_empty() { return Value::Null; }
    if path.len() == 1 {
        return match (&path[0], v) {
            (PathComp::Key(k), Value::Obj(o)) => {
                let mut o2 = o.clone(); o2.remove(k); Value::Obj(o2)
            }
            (PathComp::Index(i), Value::Arr(a)) => {
                let len = a.len() as i64;
                let idx = if *i < 0 { len + *i } else { *i };
                if idx < 0 || idx >= len { return v.clone(); }
                let mut a2 = a.clone(); a2.remove(idx as usize); Value::Arr(a2)
            }
            _ => v.clone(),
        };
    }
    match &path[0] {
        PathComp::Key(k) => {
            if let Value::Obj(o) = v {
                let mut o2 = o.clone();
                if let Some(sub) = o.get(k) {
                    o2.insert(k.clone(), del_path(sub, &path[1..]));
                }
                Value::Obj(o2)
            } else { v.clone() }
        }
        PathComp::Index(i) => {
            if let Value::Arr(a) = v {
                let len = a.len() as i64;
                let idx = if *i < 0 { len + *i } else { *i };
                if idx < 0 || idx >= len { return v.clone(); }
                let mut a2 = a.clone();
                a2[idx as usize] = del_path(&a2[idx as usize], &path[1..]);
                Value::Arr(a2)
            } else { v.clone() }
        }
    }
}
