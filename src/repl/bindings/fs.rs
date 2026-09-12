//! `fs.*`: confined to the session workspace, symlink escapes included.

use std::path::{Component, Path, PathBuf};

use mlua::Lua;

use super::{ctx, rt_err};

/// Resolve `p` under `ws`, refusing anything that leaves it.
pub fn resolve(ws: &Path, p: &str) -> Result<PathBuf, String> {
    let raw = Path::new(p);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        ws.join(raw)
    };
    let mut norm = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                norm.pop();
            }
            Component::CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    let ws_canon = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
    if !norm.starts_with(ws) && !norm.starts_with(&ws_canon) {
        return Err(format!("path `{p}` is outside the workspace"));
    }
    // Follow symlinks in the existing prefix so a link cannot point outside.
    let mut existing = norm.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(n) => {
                rest.push(n.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    if let Ok(canon) = std::fs::canonicalize(&existing) {
        if !canon.starts_with(&ws_canon) {
            return Err(format!("path `{p}` resolves outside the workspace"));
        }
    }
    let _ = rest;
    Ok(norm)
}

/// Lines shown by `fs.lines` when `to` is omitted.
const LINES_SPAN: usize = 200;

/// Matches shown by `fs.grep` before it stops.
const GREP_MAX: usize = 200;

/// Numbered lines `from..=to` (1-based) of `text`, then `[lines a-b of n]`.
pub fn lines_view(text: &str, from: usize, to: Option<usize>) -> Result<String, String> {
    if from == 0 {
        return Err("lines are 1-based: from must be at least 1".into());
    }
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if from > total {
        return Err(format!(
            "from {from} is past the end: the file has {total} lines"
        ));
    }
    let to = to
        .unwrap_or_else(|| from.saturating_add(LINES_SPAN - 1))
        .min(total);
    if to < from {
        return Err(format!("to {to} is before from {from}"));
    }
    let mut out = String::with_capacity((to - from + 1) * 64);
    for (i, line) in lines[from - 1..to].iter().enumerate() {
        out.push_str(&format!("{:>5}│{line}\n", from + i));
    }
    out.push_str(&format!("[lines {from}-{to} of {total}]"));
    Ok(out)
}

/// Regular files under `p` (or `p` itself), skipping hidden entries, build
/// directories and symlinks, so a link cannot lead outside the workspace.
fn collect_files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        out.push(p.to_path_buf());
        return;
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if e.file_type().map_or(true, |t| t.is_symlink()) {
            continue;
        }
        collect_files(&e.path(), out);
    }
}

/// `path:line:text` for every line containing `pattern` (literal), at most
/// `GREP_MAX` of them, files in path order. Non-UTF-8 files are skipped.
pub fn grep(ws: &Path, root: &Path, pattern: &str) -> String {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files.sort();
    let mut out: Vec<String> = Vec::new();
    'files: for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f.strip_prefix(ws).unwrap_or(f).display();
        for (i, line) in text.lines().enumerate() {
            if !line.contains(pattern) {
                continue;
            }
            if out.len() == GREP_MAX {
                out.push(format!(
                    "[{GREP_MAX}+ matches; narrow the pattern or the path]"
                ));
                break 'files;
            }
            out.push(format!("{rel}:{}:{line}", i + 1));
        }
    }
    if out.is_empty() {
        "(no match)".into()
    } else {
        out.join("\n")
    }
}

/// A path the verifier most likely covers: a source extension, or anything
/// under `src/`. Counted so the loop can nag when writes pile up unverified.
pub fn is_source_path(p: &str) -> bool {
    let p = p.trim_start_matches("./");
    let ext = Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(ext, "rs" | "py" | "go" | "ts" | "js") || p.starts_with("src/")
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "read",
        lua.create_function(|lua, p: String| {
            let c = ctx(lua)?;
            let path = resolve(&c.workspace, &p).map_err(rt_err)?;
            std::fs::read_to_string(&path).map_err(|e| rt_err(format!("{p}: {e}")))
        })?,
    )?;
    t.set(
        "lines",
        lua.create_function(
            |lua, (p, from, to): (String, Option<usize>, Option<usize>)| {
                let c = ctx(lua)?;
                let path = resolve(&c.workspace, &p).map_err(rt_err)?;
                let text =
                    std::fs::read_to_string(&path).map_err(|e| rt_err(format!("{p}: {e}")))?;
                lines_view(&text, from.unwrap_or(1), to).map_err(|e| rt_err(format!("{p}: {e}")))
            },
        )?,
    )?;
    t.set(
        "grep",
        lua.create_function(|lua, (pattern, p): (String, Option<String>)| {
            if pattern.is_empty() {
                return Err(rt_err("fs.grep: empty pattern"));
            }
            let c = ctx(lua)?;
            let p = p.unwrap_or_else(|| ".".into());
            let path = resolve(&c.workspace, &p).map_err(rt_err)?;
            if !path.exists() {
                return Err(rt_err(format!("{p}: no such file or directory")));
            }
            Ok(grep(&c.workspace, &path, &pattern))
        })?,
    )?;
    t.set(
        "write",
        lua.create_function(|lua, (p, text): (String, String)| {
            let c = ctx(lua)?;
            let path = resolve(&c.workspace, &p).map_err(rt_err)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| rt_err(format!("{p}: {e}")))?;
            }
            std::fs::write(&path, text).map_err(|e| rt_err(format!("{p}: {e}")))?;
            if is_source_path(&p) {
                c.effects.lock().source_writes += 1;
            }
            Ok(true)
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(|lua, d: Option<String>| {
            let c = ctx(lua)?;
            let d = d.unwrap_or_else(|| ".".into());
            let path = resolve(&c.workspace, &d).map_err(rt_err)?;
            let rd = std::fs::read_dir(&path).map_err(|e| rt_err(format!("{d}: {e}")))?;
            let mut names: Vec<String> = rd
                .flatten()
                .map(|e| {
                    let mut n = e.file_name().to_string_lossy().into_owned();
                    if e.path().is_dir() {
                        n.push('/');
                    }
                    n
                })
                .collect();
            names.sort();
            lua.create_sequence_from(names)
        })?,
    )?;
    t.set(
        "rm",
        lua.create_function(|lua, p: String| {
            let c = ctx(lua)?;
            let path = resolve(&c.workspace, &p).map_err(rt_err)?;
            if path == c.workspace
                || std::fs::canonicalize(&path).ok() == std::fs::canonicalize(&c.workspace).ok()
            {
                return Err(rt_err("refusing to remove the workspace itself"));
            }
            let r = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match r {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(rt_err(format!("{p}: {e}"))),
            }
        })?,
    )?;
    lua.globals().set("fs", t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::testkit::fixture;

    #[test]
    fn resolve_confines() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        assert!(resolve(&ws, "a/b.txt").unwrap().ends_with("ws/a/b.txt"));
        assert!(resolve(&ws, "sub/../c").unwrap().ends_with("ws/c"));
        assert!(resolve(&ws, "../../etc/passwd").is_err());
        assert!(resolve(&ws, "/etc/passwd").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", ws.join("link")).unwrap();
            assert!(resolve(&ws, "link/passwd").is_err());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fs_bindings_round_trip() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        assert_eq!(run("fs.write('dir/a.txt', 'hello')").output, "true");
        assert_eq!(run("fs.read('dir/a.txt')").output, "\"hello\"");
        assert_eq!(run("fs.list('.')").output, "{\"dir/\"}");
        assert_eq!(run("fs.list('dir')").output, "{\"a.txt\"}");
        let r = run("fs.read('../store/prompt.md')");
        assert!(r.error.as_deref().unwrap().contains("outside"), "{r:?}");
        assert_eq!(run("fs.rm('dir')").output, "true");
        assert_eq!(run("fs.rm('dir')").output, "false");
        assert!(run("fs.rm('.')").error.is_some());
        assert!(f.workspace.exists());
        // Source writes are counted for the verify reminder; other files are not.
        run("fs.write('src/m.rs', ''); fs.write('notes.md', '')");
        assert_eq!(f.repl.take_effects().source_writes, 1);
    }

    #[test]
    fn source_paths_are_recognised() {
        for p in [
            "src/a.rs",
            "./src/x/y.txt",
            "lib/b.py",
            "cmd/main.go",
            "web/app.ts",
            "a.js",
        ] {
            assert!(is_source_path(p), "{p}");
        }
        for p in [
            "README.md",
            "Cargo.toml",
            "docs/src/a.md",
            "tests/data.json",
            "srcs/a.c",
        ] {
            assert!(!is_source_path(p), "{p}");
        }
    }

    #[test]
    fn lines_view_numbers_and_bounds() {
        let text = (1..=668)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let v = lines_view(&text, 1, None).unwrap();
        assert!(v.starts_with("    1│line 1\n    2│line 2\n"), "{v}");
        assert!(v.ends_with("  200│line 200\n[lines 1-200 of 668]"), "{v}");
        let v = lines_view(&text, 660, Some(9000)).unwrap();
        assert!(v.ends_with("  668│line 668\n[lines 660-668 of 668]"), "{v}");
        let v = lines_view(&text, 12, Some(12)).unwrap();
        assert_eq!(v, "   12│line 12\n[lines 12-12 of 668]");
        assert!(lines_view(&text, 0, None).unwrap_err().contains("1-based"));
        assert!(lines_view(&text, 669, None)
            .unwrap_err()
            .contains("past the end"));
        assert!(lines_view(&text, 10, Some(5))
            .unwrap_err()
            .contains("before from"));
        assert!(
            lines_view("", 1, None).is_err(),
            "an empty file has no line 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lines_and_grep_bindings() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        run("fs.write('src/a.rs', 'fn a() {}\\n// TODO one\\nfn b() {}\\n')");
        run("fs.write('src/sub/b.rs', '// TODO two\\nfn c() {}\\n')");
        run("fs.write('.hidden/c.rs', '// TODO hidden\\n')");
        run("fs.write('target/d.rs', '// TODO built\\n')");
        assert_eq!(
            run("fs.lines('src/a.rs', 2, 3)").output,
            "\"    2│// TODO one\\n    3│fn b() {}\\n[lines 2-3 of 3]\""
        );
        let r = run("fs.lines('src/a.rs', 0)");
        assert!(r.error.as_deref().unwrap().contains("1-based"), "{r:?}");
        let r = run("fs.lines('nope.rs')");
        assert!(r.error.is_some());
        // A directory walk: path order, hidden and build directories skipped.
        assert_eq!(
            run("fs.grep('TODO', 'src')").output,
            "\"src/a.rs:2:// TODO one\\nsrc/sub/b.rs:1:// TODO two\""
        );
        assert_eq!(
            run("fs.grep('TODO')").output,
            "\"src/a.rs:2:// TODO one\\nsrc/sub/b.rs:1:// TODO two\""
        );
        // A single file, and a literal pattern (no regex).
        assert_eq!(
            run("fs.grep('fn c()', 'src/sub/b.rs')").output,
            "\"src/sub/b.rs:2:fn c() {}\""
        );
        assert_eq!(run("fs.grep('fn .()', 'src')").output, "\"(no match)\"");
        assert!(run("fs.grep('', 'src')").error.is_some());
        assert!(run("fs.grep('x', '../store')").error.is_some());
    }
}
