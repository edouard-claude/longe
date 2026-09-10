//! Builtin functions.

use crate::ast::Expr;
use crate::eval::{eval, Env, EResult};
use crate::json::{format_number, Serializer, Value};
use std::collections::BTreeMap;

pub fn call(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "empty" => Ok(vec![]),
        "error" => {
            let msg = if args.is_empty() {
                Serializer::compact().to_string(input)
            } else {
                let v = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
                match v { Value::Str(s) => s, other => Serializer::compact().to_string(&other) }
            };
            Err(msg)
        }
        "not" => Ok(vec![Value::Bool(!input.is_truthy())]),
        "length" => builtin_length(input),
        "utf8bytelength" => match input {
            Value::Str(s) => Ok(vec![Value::Num(s.len() as f64)]),
            _ => Err(format!("{} has no UTF-8 byte length", input.type_name())),
        },
        "keys" => builtin_keys(input),
        "keys_unsorted" => builtin_keys(input),
        "values" => match input {
            Value::Obj(o) => Ok(o.values().cloned().collect()),
            Value::Arr(a) => Ok(a.clone()),
            _ => Err(format!("{} has no values", input.type_name())),
        },
        "has" => {
            let k = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, k) {
                (Value::Obj(o), Value::Str(s)) => Ok(vec![Value::Bool(o.contains_key(&s))]),
                (Value::Arr(a), Value::Num(n)) => {
                    let i = n as i64;
                    Ok(vec![Value::Bool(i >= 0 && (i as usize) < a.len())])
                }
                _ => Err("has() requires object+string or array+number".to_string()),
            }
        }
        "type" => Ok(vec![Value::Str(input.type_name().to_string())]),
        "tostring" => Ok(vec![Value::Str(to_string_val(input))]),
        "tojson" => Ok(vec![Value::Str(Serializer::compact().to_string(input))]),
        "tonumber" => builtin_tonumber(input),
        "toboolean" => match input {
            Value::Bool(b) => Ok(vec![Value::Bool(*b)]),
            _ => Err(format!("{} cannot be parsed as a boolean", input.type_name())),
        },
        "ascii_downcase" => match input {
            Value::Str(s) => Ok(vec![Value::Str(s.to_lowercase())]),
            _ => Err("ascii_downcase requires a string".to_string()),
        },
        "ascii_upcase" => match input {
            Value::Str(s) => Ok(vec![Value::Str(s.to_uppercase())]),
            _ => Err("ascii_upcase requires a string".to_string()),
        },
        _ => call2(name, args, input, env),
    }
}

fn to_string_val(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        other => Serializer::compact().to_string(other),
    }
}

fn builtin_length(input: &Value) -> EResult {
    match input {
        Value::Null => Ok(vec![Value::Num(0.0)]),
        Value::Bool(_) => Err("boolean has no length".to_string()),
        Value::Num(n) => Ok(vec![Value::Num(n.abs())]),
        Value::Str(s) => Ok(vec![Value::Num(s.chars().count() as f64)]),
        Value::Arr(a) => Ok(vec![Value::Num(a.len() as f64)]),
        Value::Obj(o) => Ok(vec![Value::Num(o.len() as f64)]),
    }
}

fn builtin_keys(input: &Value) -> EResult {
    match input {
        Value::Obj(o) => Ok(vec![Value::Arr(o.keys().map(|k| Value::Str(k.clone())).collect())]),
        Value::Arr(a) => Ok(vec![Value::Arr((0..a.len()).map(|i| Value::Num(i as f64)).collect())]),
        _ => Err(format!("{} has no keys", input.type_name())),
    }
}

fn builtin_tonumber(input: &Value) -> EResult {
    match input {
        Value::Num(n) => Ok(vec![Value::Num(*n)]),
        Value::Str(s) => {
            let t = s.trim();
            match t.parse::<f64>() {
                Ok(n) => Ok(vec![Value::Num(n)]),
                Err(_) => Err(format!("Invalid numeric literal at EOF at line 1, column {} (while parsing '{}')", t.len(), t)),
            }
        }
        _ => Err(format!("{} cannot be parsed as a number", input.type_name())),
    }
}

fn call2(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "select" => {
            let mut out = Vec::new();
            for v in eval(&args[0], input, env)? {
                if v.is_truthy() { out.push(input.clone()); }
            }
            Ok(out)
        }
        "map" => {
            let mut out = Vec::new();
            match input {
                Value::Arr(a) => {
                    for item in a { out.extend(eval(&args[0], item, env)?); }
                    Ok(vec![Value::Arr(out)])
                }
                Value::Obj(o) => {
                    for (_, item) in o { out.extend(eval(&args[0], item, env)?); }
                    Ok(vec![Value::Arr(out)])
                }
                _ => Err(format!("Cannot iterate over {}", input.type_name())),
            }
        }
        _ => call2b(name, args, input, env),
    }
}

fn call2b(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "map_values" => {
            match input {
                Value::Obj(o) => {
                    let mut res = BTreeMap::new();
                    for (k, v) in o {
                        let nv = eval(&args[0], v, env)?.into_iter().next().unwrap_or(Value::Null);
                        res.insert(k.clone(), nv);
                    }
                    Ok(vec![Value::Obj(res)])
                }
                Value::Arr(a) => {
                    let mut res = Vec::new();
                    for v in a {
                        let nv = eval(&args[0], v, env)?.into_iter().next().unwrap_or(Value::Null);
                        res.push(nv);
                    }
                    Ok(vec![Value::Arr(res)])
                }
                _ => Err(format!("Cannot iterate over {}", input.type_name())),
            }
        }
        "add" => {
            let items = match input {
                Value::Arr(a) => a.clone(),
                Value::Obj(o) => o.values().cloned().collect(),
                _ => return Err(format!("Cannot iterate over {}", input.type_name())),
            };
            let mut acc = Value::Null;
            for it in items {
                acc = crate::eval::apply_binop_pub(crate::ast::BinOp::Add, &acc, &it)?;
            }
            Ok(vec![acc])
        }
        _ => call3(name, args, input, env),
    }
}

fn call3(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "any" => {
            let items = match input {
                Value::Arr(a) => a.clone(),
                _ => return Err("any requires an array".to_string()),
            };
            if args.is_empty() {
                Ok(vec![Value::Bool(items.iter().any(|v| v.is_truthy()))])
            } else {
                for it in &items {
                    if eval(&args[0], it, env)?.into_iter().any(|v| v.is_truthy()) {
                        return Ok(vec![Value::Bool(true)]);
                    }
                }
                Ok(vec![Value::Bool(false)])
            }
        }
        "all" => {
            let items = match input {
                Value::Arr(a) => a.clone(),
                _ => return Err("all requires an array".to_string()),
            };
            if args.is_empty() {
                Ok(vec![Value::Bool(items.iter().all(|v| v.is_truthy()))])
            } else {
                for it in &items {
                    if !eval(&args[0], it, env)?.into_iter().any(|v| v.is_truthy()) {
                        return Ok(vec![Value::Bool(false)]);
                    }
                }
                Ok(vec![Value::Bool(true)])
            }
        }
        _ => call4(name, args, input, env),
    }
}

fn call4(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "sort" => {
            let mut a = match input { Value::Arr(a) => a.clone(), _ => return Err("sort requires an array".to_string()) };
            a.sort_by(crate::eval::cmp);
            Ok(vec![Value::Arr(a)])
        }
        "sort_by" => {
            let a = match input { Value::Arr(a) => a.clone(), _ => return Err("sort_by requires an array".to_string()) };
            let mut keyed: Vec<(Value, Value)> = Vec::new();
            for it in a {
                let k = eval(&args[0], &it, env)?.into_iter().next().unwrap_or(Value::Null);
                keyed.push((k, it));
            }
            keyed.sort_by(|x, y| crate::eval::cmp(&x.0, &y.0));
            Ok(vec![Value::Arr(keyed.into_iter().map(|(_, v)| v).collect())])
        }
        "reverse" => match input {
            Value::Arr(a) => { let mut a2 = a.clone(); a2.reverse(); Ok(vec![Value::Arr(a2)]) }
            Value::Str(s) => Ok(vec![Value::Str(s.chars().rev().collect())]),
            _ => Err("reverse requires an array or string".to_string()),
        },
        "min" => {
            let a = match input { Value::Arr(a) => a.clone(), _ => return Err("min requires an array".to_string()) };
            Ok(vec![a.into_iter().min_by(crate::eval::cmp).unwrap_or(Value::Null)])
        }
        "max" => {
            let a = match input { Value::Arr(a) => a.clone(), _ => return Err("max requires an array".to_string()) };
            Ok(vec![a.into_iter().max_by(crate::eval::cmp).unwrap_or(Value::Null)])
        }
        "unique" => {
            let mut a = match input { Value::Arr(a) => a.clone(), _ => return Err("unique requires an array".to_string()) };
            a.sort_by(crate::eval::cmp);
            a.dedup();
            Ok(vec![Value::Arr(a)])
        }
        "flatten" => {
            let a = match input { Value::Arr(a) => a.clone(), _ => return Err("flatten requires an array".to_string()) };
            let mut out = Vec::new();
            flatten_into(&a, &mut out);
            Ok(vec![Value::Arr(out)])
        }
        "range" => builtin_range(args, input, env),
        _ => call5(name, args, input, env),
    }
}

fn call5(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "contains" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            Ok(vec![Value::Bool(contains(input, &b))])
        }
        "startswith" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, &b) {
                (Value::Str(s), Value::Str(p)) => Ok(vec![Value::Bool(s.starts_with(p.as_str()))]),
                (Value::Arr(a), Value::Arr(p)) => Ok(vec![Value::Bool(a.len() >= p.len() && a[..p.len()] == p[..])]),
                _ => Err("startswith requires string or array".to_string()),
            }
        }
        "endswith" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, &b) {
                (Value::Str(s), Value::Str(p)) => Ok(vec![Value::Bool(s.ends_with(p.as_str()))]),
                (Value::Arr(a), Value::Arr(p)) => Ok(vec![Value::Bool(a.len() >= p.len() && a[a.len()-p.len()..] == p[..])]),
                _ => Err("endswith requires string or array".to_string()),
            }
        }
        _ => call6(name, args, input, env),
    }
}

fn call6(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "ltrimstr" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, &b) {
                (Value::Str(s), Value::Str(p)) => Ok(vec![Value::Str(s.strip_prefix(p.as_str()).unwrap_or(s).to_string())]),
                _ => Ok(vec![input.clone()]),
            }
        }
        "rtrimstr" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, &b) {
                (Value::Str(s), Value::Str(p)) => Ok(vec![Value::Str(s.strip_suffix(p.as_str()).unwrap_or(s).to_string())]),
                _ => Ok(vec![input.clone()]),
            }
        }
        "split" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            match (input, &b) {
                (Value::Str(s), Value::Str(sep)) => {
                    Ok(vec![Value::Arr(s.split(sep.as_str()).map(|x| Value::Str(x.to_string())).collect())])
                }
                _ => Err("split requires strings".to_string()),
            }
        }
        "join" => {
            let b = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            let sep = match b { Value::Str(s) => s, _ => return Err("join requires a string separator".to_string()) };
            match input {
                Value::Arr(a) => {
                    let parts: Vec<String> = a.iter().map(|v| match v {
                        Value::Str(s) => s.clone(),
                        Value::Null => String::new(),
                        other => Serializer::compact().to_string(other),
                    }).collect();
                    Ok(vec![Value::Str(parts.join(&sep))])
                }
                _ => Err("join requires an array".to_string()),
            }
        }
        _ => call7(name, args, input, env),
    }
}

fn call7(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "floor" => num1(input, |n| n.floor()),
        "ceil" => num1(input, |n| n.ceil()),
        "round" => num1(input, |n| n.round()),
        "sqrt" => num1(input, |n| n.sqrt()),
        "fabs" => num1(input, |n| n.abs()),
        "abs" => num1(input, |n| n.abs()),
        "toarray" => match input {
            Value::Arr(_) => Ok(vec![input.clone()]),
            _ => Ok(vec![Value::Arr(vec![input.clone()])]),
        },
        "first" => {
            if args.is_empty() {
                match input {
                    Value::Arr(a) => Ok(a.first().cloned().into_iter().collect()),
                    _ => Err("first requires an array".to_string()),
                }
            } else {
                Ok(eval(&args[0], input, env)?.into_iter().take(1).collect())
            }
        }
        "last" => {
            if args.is_empty() {
                match input {
                    Value::Arr(a) => Ok(a.last().cloned().into_iter().collect()),
                    _ => Err("last requires an array".to_string()),
                }
            } else {
                Ok(eval(&args[0], input, env)?.into_iter().last().into_iter().collect())
            }
        }
        "nth" => {
            let n = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null).as_f64().unwrap_or(0.0) as i64;
            let src = if args.len() > 1 { eval(&args[1], input, env)? } else {
                match input { Value::Arr(a) => a.clone(), _ => return Err("nth requires an array".to_string()) }
            };
            if n < 0 || n as usize >= src.len() { Ok(vec![]) }
            else { Ok(vec![src[n as usize].clone()]) }
        }
        _ => call8(name, args, input, env),
    }
}

fn num1(input: &Value, f: impl Fn(f64) -> f64) -> EResult {
    match input {
        Value::Num(n) => Ok(vec![Value::Num(f(*n))]),
        _ => Err(format!("{} is not a number", input.type_name())),
    }
}

fn call8(name: &str, args: &[Expr], input: &Value, env: &Env) -> EResult {
    match name {
        "del" => {
            let paths = crate::paths::collect_paths(&args[0], input, env)?;
            let mut result = input.clone();
            for p in paths { result = crate::paths::del_path(&result, &p); }
            Ok(vec![result])
        }
        "getpath" => {
            let pv = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            let path = value_to_path(&pv)?;
            Ok(vec![crate::paths::get_path(input, &path)])
        }
        "setpath" => {
            let pv = eval(&args[0], input, env)?.into_iter().next().unwrap_or(Value::Null);
            let nv = eval(&args[1], input, env)?.into_iter().next().unwrap_or(Value::Null);
            let path = value_to_path(&pv)?;
            Ok(vec![crate::paths::set_path(input, &path, &nv)])
        }
        "paths" => {
            let mut out = Vec::new();
            collect_paths_rec(input, &mut Vec::new(), &mut out);
            Ok(out)
        }
        "leaf_paths" => {
            let mut out = Vec::new();
            collect_leaf_paths(input, &mut Vec::new(), &mut out);
            Ok(out)
        }
        "tostream" => {
            let mut out = Vec::new();
            to_stream(input, &mut Vec::new(), &mut out);
            Ok(out)
        }
        "env" => Ok(vec![Value::Obj(BTreeMap::new())]),
        "now" => Ok(vec![Value::Num(0.0)]),
        _ => call9(name, args, input, env),
    }
}

fn value_to_path(v: &Value) -> Result<crate::paths::Path, String> {
    match v {
        Value::Arr(a) => {
            let mut p = Vec::new();
            for c in a {
                match c {
                    Value::Str(s) => p.push(crate::paths::PathComp::Key(s.clone())),
                    Value::Num(n) => p.push(crate::paths::PathComp::Index(*n as i64)),
                    _ => return Err("bad path component".to_string()),
                }
            }
            Ok(p)
        }
        _ => Err("path must be an array".to_string()),
    }
}

fn collect_paths_rec(v: &Value, cur: &mut Vec<Value>, out: &mut Vec<Value>) {
    match v {
        Value::Arr(a) => {
            for (i, x) in a.iter().enumerate() {
                cur.push(Value::Num(i as f64));
                out.push(Value::Arr(cur.clone()));
                collect_paths_rec(x, cur, out);
                cur.pop();
            }
        }
        Value::Obj(o) => {
            for (k, x) in o {
                cur.push(Value::Str(k.clone()));
                out.push(Value::Arr(cur.clone()));
                collect_paths_rec(x, cur, out);
                cur.pop();
            }
        }
        _ => {}
    }
}

fn collect_leaf_paths(v: &Value, cur: &mut Vec<Value>, out: &mut Vec<Value>) {
    match v {
        Value::Arr(a) => {
            for (i, x) in a.iter().enumerate() {
                cur.push(Value::Num(i as f64));
                collect_leaf_paths(x, cur, out);
                cur.pop();
            }
        }
        Value::Obj(o) => {
            for (k, x) in o {
                cur.push(Value::Str(k.clone()));
                collect_leaf_paths(x, cur, out);
                cur.pop();
            }
        }
        _ => out.push(Value::Arr(cur.clone())),
    }
}

fn to_stream(v: &Value, cur: &mut Vec<Value>, out: &mut Vec<Value>) {
    match v {
        Value::Arr(a) => {
            for (i, x) in a.iter().enumerate() {
                cur.push(Value::Num(i as f64));
                to_stream(x, cur, out);
                cur.pop();
            }
            out.push(Value::Arr(vec![Value::Arr(cur.clone()), Value::Arr(vec![])]));
        }
        Value::Obj(o) => {
            for (k, x) in o {
                cur.push(Value::Str(k.clone()));
                to_stream(x, cur, out);
                cur.pop();
            }
            out.push(Value::Arr(vec![Value::Arr(cur.clone()), Value::Obj(BTreeMap::new())]));
        }
        _ => out.push(Value::Arr(vec![Value::Arr(cur.clone()), v.clone()])),
    }
}

fn contains(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.contains(y.as_str()),
        (Value::Arr(x), Value::Arr(y)) => y.iter().all(|yb| x.iter().any(|xb| contains(xb, yb))),
        (Value::Obj(x), Value::Obj(y)) => y.iter().all(|(k, yv)| x.get(k).map_or(false, |xv| contains(xv, yv))),
        _ => a == b,
    }
}

fn call9(name: &str, _args: &[Expr], _input: &Value, _env: &Env) -> EResult {
    Err(format!("{} is not defined", name))
}

fn flatten_into(a: &[Value], out: &mut Vec<Value>) {
    for v in a {
        match v {
            Value::Arr(inner) => flatten_into(inner, out),
            other => out.push(other.clone()),
        }
    }
}

fn builtin_range(args: &[Expr], input: &Value, env: &Env) -> EResult {
    let mut nums = Vec::new();
    for a in args {
        nums.push(eval(a, input, env)?.into_iter().next().unwrap_or(Value::Null).as_f64().unwrap_or(0.0));
    }
    let (start, end, step) = match nums.len() {
        1 => (0.0, nums[0], 1.0),
        2 => (nums[0], nums[1], 1.0),
        3 => (nums[0], nums[1], nums[2]),
        _ => return Err("range takes 1-3 args".to_string()),
    };
    let mut out = Vec::new();
    let mut i = start;
    if step > 0.0 { while i < end { out.push(Value::Num(i)); i += step; } }
    else if step < 0.0 { while i > end { out.push(Value::Num(i)); i += step; } }
    Ok(out)
}
