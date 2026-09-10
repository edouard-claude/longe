mod ast;
mod builtins;
mod eval;
mod json;
mod lexer;
mod parser;
mod paths;

use std::io::{self, Read, Write};

struct Opts {
    raw_output: bool,
    compact: bool,
    null_input: bool,
    slurp: bool,
    tab: bool,
    indent: usize,
    filter: String,
    files: Vec<String>,
}

fn parse_args() -> Result<Opts, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Opts {
        raw_output: false, compact: false, null_input: false,
        slurp: false, tab: false, indent: 2, filter: String::new(), files: Vec::new(),
    };
    let mut i = 0;
    let mut got_filter = false;
    while i < args.len() {
        let a = &args[i];
        if !got_filter && a.starts_with('-') && a.len() > 1 {
            match a.as_str() {
                "-r" => o.raw_output = true,
                "-c" => o.compact = true,
                "-n" => o.null_input = true,
                "-s" => o.slurp = true,
                "-j" => o.raw_output = true,
                "--tab" => o.tab = true,
                "--indent" => { i += 1; o.indent = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(2); }
                "-h" | "--help" => { print_help(); std::process::exit(0); }
                "-V" | "--version" => { println!("rjq-0.1.0"); std::process::exit(0); }
                _ => {
                    if let Some(rest) = a.strip_prefix("--indent=") {
                        o.indent = rest.parse().unwrap_or(2);
                    } else {
                        return Err(format!("unknown option {}", a));
                    }
                }
            }
        } else if !got_filter {
            o.filter = a.clone();
            got_filter = true;
        } else {
            o.files.push(a.clone());
        }
        i += 1;
    }
    if !got_filter { return Err("no filter given".to_string()); }
    Ok(o)
}

fn print_help() {
    println!("rjq - a jq clone in Rust");
    println!("Usage: rjq [OPTIONS] FILTER [FILES...]");
    println!("  -c  compact output");
    println!("  -r  raw string output");
    println!("  -n  use null as input");
    println!("  -s  slurp all inputs into an array");
}

fn read_inputs(o: &Opts) -> Result<Vec<json::Value>, String> {
    if o.null_input { return Ok(vec![json::Value::Null]); }
    let mut texts = Vec::new();
    if o.files.is_empty() {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        texts.push(s);
    } else {
        for f in &o.files {
            let s = std::fs::read_to_string(f).map_err(|e| format!("{}: {}", f, e))?;
            texts.push(s);
        }
    }
    let mut vals = Vec::new();
    for t in texts {
        let mut p = json::Parser::new(&t);
        loop {
            p.skip_ws_pub();
            if p.at_end() { break; }
            let v = p.parse_value_pub()?;
            vals.push(v);
        }
    }
    if o.slurp {
        Ok(vec![json::Value::Arr(vals)])
    } else {
        Ok(vals)
    }
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => { eprintln!("rjq: {}", e); std::process::exit(2); }
    };
    let prog = match parser::parse(&o.filter) {
        Ok(p) => p,
        Err(e) => { eprintln!("rjq: error: {}", e); std::process::exit(3); }
    };
    let inputs = match read_inputs(&o) {
        Ok(v) => v,
        Err(e) => { eprintln!("rjq: {}", e); std::process::exit(2); }
    };
    let env = eval::Env::new();
    let ser = if o.compact { json::Serializer::compact() }
              else if o.tab { json::Serializer::pretty(1) }
              else { json::Serializer::pretty(o.indent) };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut exit_code = 0;
    for input in &inputs {
        match eval::eval(&prog, input, &env) {
            Ok(results) => {
                for r in results {
                    let s = match (&r, o.raw_output) {
                        (json::Value::Str(s), true) => s.clone(),
                        _ => ser.to_string(&r),
                    };
                    let _ = writeln!(out, "{}", s);
                }
            }
            Err(e) => {
                eprintln!("rjq: error: {}", e);
                exit_code = 5;
            }
        }
    }
    std::process::exit(exit_code);
}
