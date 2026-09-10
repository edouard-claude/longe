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
        "write",
        lua.create_function(|lua, (p, text): (String, String)| {
            let c = ctx(lua)?;
            let path = resolve(&c.workspace, &p).map_err(rt_err)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| rt_err(format!("{p}: {e}")))?;
            }
            std::fs::write(&path, text).map_err(|e| rt_err(format!("{p}: {e}")))?;
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
    }
}
