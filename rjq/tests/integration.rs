//! Integration tests for rjq CLI.

use std::io::Write;
use std::process::{Command, Stdio};

fn rjq(filter: &str, input: &str, opts: &[&str]) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_rjq");
    let mut cmd = Command::new(bin);
    for o in opts { cmd.arg(o); }
    cmd.arg(filter);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn out(filter: &str, input: &str, opts: &[&str]) -> String {
    let (o, e, c) = rjq(filter, input, opts);
    assert_eq!(c, 0, "exit {} stderr {}", c, e);
    o
}

#[test]
fn identity() {
    assert_eq!(out(".", "{\"a\":1}", &["-c"]), "{\"a\":1}\n");
    assert_eq!(out(".", "42", &["-c"]), "42\n");
    assert_eq!(out(".", "null", &["-c"]), "null\n");
    assert_eq!(out(".", "true", &["-c"]), "true\n");
}

#[test]
fn field_access() {
    assert_eq!(out(".foo", "{\"foo\":\"bar\"}", &["-r"]), "bar\n");
    assert_eq!(out(".foo.bar", "{\"foo\":{\"bar\":42}}", &["-c"]), "42\n");
    assert_eq!(out(".missing", "{\"a\":1}", &["-c"]), "null\n");
    assert_eq!(out(".foo", "[1,2]", &["-c"]), "null\n");
}

#[test]
fn iterate() {
    assert_eq!(out(".[]", "[1,2,3]", &["-c"]), "1\n2\n3\n");
    assert_eq!(out(".[]", "{\"a\":1,\"b\":2}", &["-c"]), "1\n2\n");
    assert_eq!(out(".[]", "[]", &["-c"]), "");
}

#[test]
fn index() {
    assert_eq!(out(".[0]", "[10,20,30]", &["-c"]), "10\n");
    assert_eq!(out(".[-1]", "[10,20,30]", &["-c"]), "30\n");
    assert_eq!(out(".[5]", "[10,20,30]", &["-c"]), "null\n");
    assert_eq!(out(".[\"a\"]", "{\"a\":7}", &["-c"]), "7\n");
}

#[test]
fn pipe_and_comma() {
    assert_eq!(out(".a | .b", "{\"a\":{\"b\":\"x\"}}", &["-r"]), "x\n");
    assert_eq!(out(".a, .b", "{\"a\":1,\"b\":2}", &["-c"]), "1\n2\n");
    assert_eq!(out(".[] | .x", "[{\"x\":1},{\"x\":2}]", &["-c"]), "1\n2\n");
}

#[test]
fn length() {
    assert_eq!(out("length", "\"hello\"", &["-c"]), "5\n");
    assert_eq!(out("length", "[1,2,3]", &["-c"]), "3\n");
    assert_eq!(out("length", "{\"a\":1,\"b\":2}", &["-c"]), "2\n");
    assert_eq!(out("length", "null", &["-c"]), "0\n");
    assert_eq!(out("length", "-5", &["-c"]), "5\n");
}

#[test]
fn keys() {
    assert_eq!(out("keys", "{\"b\":1,\"a\":2}", &["-c"]), "[\"a\",\"b\"]\n");
    assert_eq!(out("keys", "[5,6,7]", &["-c"]), "[0,1,2]\n");
}

#[test]
fn has() {
    assert_eq!(out("has(\"a\")", "{\"a\":1}", &["-c"]), "true\n");
    assert_eq!(out("has(\"z\")", "{\"a\":1}", &["-c"]), "false\n");
    assert_eq!(out("has(0)", "[5,6,7]", &["-c"]), "true\n");
    assert_eq!(out("has(9)", "[5,6,7]", &["-c"]), "false\n");
}

#[test]
fn tostring_tonumber() {
    assert_eq!(out("tostring", "42", &["-r"]), "42\n");
    assert_eq!(out("tostring", "\"hi\"", &["-r"]), "hi\n");
    assert_eq!(out("tostring", "{\"a\":1}", &["-r"]), "{\"a\":1}\n");
    assert_eq!(out("tonumber", "\"42\"", &["-c"]), "42\n");
    assert_eq!(out("tonumber", "\"3.14\"", &["-c"]), "3.14\n");
    assert_eq!(out("tonumber", "7", &["-c"]), "7\n");
}

#[test]
fn map_select() {
    assert_eq!(out("map(.+1)", "[1,2,3]", &["-c"]), "[2,3,4]\n");
    assert_eq!(out("map(.x)", "[{\"x\":1},{\"x\":2}]", &["-c"]), "[1,2]\n");
    assert_eq!(out(".[] | select(. > 2)", "[1,2,3,4]", &["-c"]), "3\n4\n");
    assert_eq!(out("map(select(. > 1))", "[1,2,3]", &["-c"]), "[2,3]\n");
}

#[test]
fn raw_and_compact() {
    assert_eq!(out(".", "\"hello\"", &["-r"]), "hello\n");
    assert_eq!(out(".", "\"hello\"", &["-c"]), "\"hello\"\n");
    assert_eq!(out(".", "{\"a\":1}", &["-c"]), "{\"a\":1}\n");
    assert_eq!(out(".", "{\"a\":1}", &[]), "{\n  \"a\": 1\n}\n");
}

#[test]
fn json_unicode() {
    assert_eq!(out(".", "\"\\u00e9\"", &["-r"]), "é\n");
    assert_eq!(out(".", "\"\\u0041\\u0042\"", &["-r"]), "AB\n");
    // surrogate pair for emoji U+1F600
    assert_eq!(out(".", "\"\\ud83d\\ude00\"", &["-r"]), "😀\n");
    // raw UTF-8 passthrough
    assert_eq!(out(".", "\"héllo\"", &["-r"]), "héllo\n");
}

#[test]
fn json_escapes() {
    assert_eq!(out(".", "\"a\\nb\"", &["-r"]), "a\nb\n");
    assert_eq!(out(".", "\"a\\tb\"", &["-r"]), "a\tb\n");
    assert_eq!(out(".", "\"q\\\"q\"", &["-r"]), "q\"q\n");
    assert_eq!(out(".", "\"back\\\\slash\"", &["-r"]), "back\\slash\n");
    assert_eq!(out(".", "\"\\u0000\"", &["-c"]), "\"\\u0000\"\n");
}

#[test]
fn json_exponential_numbers() {
    assert_eq!(out(".", "1e3", &["-c"]), "1000\n");
    assert_eq!(out(".", "1.5e2", &["-c"]), "150\n");
    assert_eq!(out(".", "1E-2", &["-c"]), "0.01\n");
    assert_eq!(out(".", "-2.5e1", &["-c"]), "-25\n");
    assert_eq!(out(".", "0.0", &["-c"]), "0\n");
}

#[test]
fn json_nested_roundtrip() {
    let input = "{\"a\":[1,2,{\"b\":\"x\\ny\"}],\"c\":null,\"d\":true}";
    let o = out(".", input, &["-c"]);
    assert_eq!(o, "{\"a\":[1,2,{\"b\":\"x\\ny\"}],\"c\":null,\"d\":true}\n");
}

#[test]
fn multiple_documents() {
    assert_eq!(out(".", "{\"a\":1} {\"b\":2}", &["-c"]), "{\"a\":1}\n{\"b\":2}\n");
    assert_eq!(out(".", "1\n2\n3\n", &["-c"]), "1\n2\n3\n");
}

#[test]
fn null_input() {
    assert_eq!(out(".", "", &["-n", "-c"]), "null\n");
    assert_eq!(out("1+2", "", &["-n", "-c"]), "3\n");
}

#[test]
fn slurp() {
    assert_eq!(out(".", "1 2 3", &["-s", "-c"]), "[1,2,3]\n");
}

#[test]
fn error_exit_code() {
    let (_, e, c) = rjq(".foo | error", "{}", &[]);
    assert_ne!(c, 0);
    assert!(e.contains("error"));
}

#[test]
fn variables_and_as() {
    assert_eq!(out(". as $x | $x + 1", "5", &["-c"]), "6\n");
    assert_eq!(out(".[] as $x | $x * 2", "[1,2,3]", &["-c"]), "2\n4\n6\n");
    assert_eq!(out(".a as $x | .b + $x", "{\"a\":10,\"b\":5}", &["-c"]), "15\n");
}

#[test]
fn reduce_foreach() {
    assert_eq!(out("reduce .[] as $x (0; . + $x)", "[1,2,3,4]", &["-c"]), "10\n");
    assert_eq!(out("reduce .[] as $x (1; . * $x)", "[1,2,3,4]", &["-c"]), "24\n");
    assert_eq!(out("foreach .[] as $x (0; . + $x)", "[1,2,3]", &["-c"]), "1\n3\n6\n");
}

#[test]
fn boolean_ops() {
    assert_eq!(out("has(\"a\") and has(\"b\")", "{\"a\":1,\"b\":2}", &["-c"]), "true\n");
    assert_eq!(out("has(\"a\") and has(\"z\")", "{\"a\":1}", &["-c"]), "false\n");
    assert_eq!(out("true or false", "null", &["-c"]), "true\n");
    assert_eq!(out("false and true", "null", &["-c"]), "false\n");
    assert_eq!(out("not", "false", &["-c"]), "true\n");
}

#[test]
fn conditionals() {
    assert_eq!(out("if . > 2 then \"big\" else \"small\" end", "5", &["-r"]), "big\n");
    assert_eq!(out("if . > 2 then \"big\" else \"small\" end", "1", &["-r"]), "small\n");
    assert_eq!(out("if . then 1 else 2 end", "true", &["-c"]), "1\n");
}

#[test]
fn alt_operator() {
    assert_eq!(out(".a // \"default\"", "{}", &["-r"]), "default\n");
    assert_eq!(out(".a // \"default\"", "{\"a\":1}", &["-c"]), "1\n");
}

#[test]
fn slices() {
    assert_eq!(out(".[1:3]", "[1,2,3,4,5]", &["-c"]), "[2,3]\n");
    assert_eq!(out(".[-2:]", "[1,2,3,4,5]", &["-c"]), "[4,5]\n");
    assert_eq!(out(".[:2]", "[1,2,3]", &["-c"]), "[1,2]\n");
    assert_eq!(out(".[1:3]", "\"hello\"", &["-r"]), "el\n");
}

#[test]
fn array_builtins() {
    assert_eq!(out("add", "[1,2,3]", &["-c"]), "6\n");
    assert_eq!(out("add", "[]", &["-c"]), "null\n");
    assert_eq!(out("sort", "[3,1,2]", &["-c"]), "[1,2,3]\n");
    assert_eq!(out("unique", "[1,1,2,3,3]", &["-c"]), "[1,2,3]\n");
    assert_eq!(out("flatten", "[[1,[2]],3]", &["-c"]), "[1,2,3]\n");
    assert_eq!(out("min", "[3,1,2]", &["-c"]), "1\n");
    assert_eq!(out("max", "[3,1,2]", &["-c"]), "3\n");
    assert_eq!(out("reverse", "[1,2,3]", &["-c"]), "[3,2,1]\n");
}

#[test]
fn string_builtins() {
    assert_eq!(out("split(\",\")", "\"a,b,c\"", &["-c"]), "[\"a\",\"b\",\"c\"]\n");
    assert_eq!(out("join(\"-\")", "[\"a\",\"b\"]", &["-r"]), "a-b\n");
    assert_eq!(out("contains(\"ell\")", "\"hello\"", &["-c"]), "true\n");
    assert_eq!(out("startswith(\"he\")", "\"hello\"", &["-c"]), "true\n");
    assert_eq!(out("endswith(\"lo\")", "\"hello\"", &["-c"]), "true\n");
    assert_eq!(out("ascii_upcase", "\"abc\"", &["-r"]), "ABC\n");
}

#[test]
fn path_assignment() {
    assert_eq!(out(".a = 5", "{\"a\":1}", &["-c"]), "{\"a\":5}\n");
    assert_eq!(out(".a |= . + 1", "{\"a\":1}", &["-c"]), "{\"a\":2}\n");
    assert_eq!(out(".a.b = 9", "{\"a\":{\"b\":1}}", &["-c"]), "{\"a\":{\"b\":9}}\n");
    assert_eq!(out(".[0] = 9", "[1,2,3]", &["-c"]), "[9,2,3]\n");
    assert_eq!(out("del(.a)", "{\"a\":1,\"b\":2}", &["-c"]), "{\"b\":2}\n");
}

#[test]
fn try_catch() {
    assert_eq!(out("try error(\"x\") catch .", "null", &["-r"]), "x\n");
    assert_eq!(out("try .foo", "1", &["-c"]), "null\n");
}

#[test]
fn type_builtin() {
    assert_eq!(out("type", "null", &["-r"]), "null\n");
    assert_eq!(out("type", "true", &["-r"]), "boolean\n");
    assert_eq!(out("type", "1", &["-r"]), "number\n");
    assert_eq!(out("type", "\"s\"", &["-r"]), "string\n");
    assert_eq!(out("type", "[]", &["-r"]), "array\n");
    assert_eq!(out("type", "{}", &["-r"]), "object\n");
}
