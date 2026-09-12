//! The single tool: `exec(code)` in a persistent Lua 5.4 VM with native bindings.
//! State survives across turns (kept in RAM) and across daemon restarts (dumped to
//! `state.lua` every turn, reloaded on wake).

pub mod bindings;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Value};
use parking_lot::Mutex;

use crate::config::{ModelRef, VerifyCfg};
use crate::llm::LlmRegistry;
use crate::sandbox::{Policy, Sandbox};
use crate::session::{SessionId, TreeHandle};
use crate::store::Store;
use crate::verify::VerifyOutcome;

const PRELUDE: &str = include_str!("prelude.lua");

#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    #[error("lua: {0}")]
    Lua(#[from] mlua::Error),
}

/// Side effects requested by bindings during one `exec`, drained by the loop.
#[derive(Debug, Default)]
pub struct Effects {
    pub done: Option<String>,
    pub compact: Option<String>,
    pub model_switch: Option<ModelRef>,
    pub verify: Option<VerifyOutcome>,
    pub notes: Vec<String>,
    pub spawned: u32,
    pub sent: u32,
    pub llm_tokens: u64,
    /// `fs.write` calls on source files (see `bindings::fs::is_source_path`).
    pub source_writes: u32,
}

/// Everything the bindings need. Lives in Lua app data.
pub struct ReplCtx {
    pub session_id: SessionId,
    pub session_name: String,
    pub session_budget: crate::config::BudgetCfg,
    pub workspace: PathBuf,
    pub store: Arc<Store>,
    pub policy: Policy,
    pub sandbox: Arc<dyn Sandbox>,
    pub tree: TreeHandle,
    pub llm: Arc<LlmRegistry>,
    pub model: Mutex<ModelRef>,
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// Bytes of exec output returned to the model (the head); the rest stays in `_last`.
    pub max_output_bytes: usize,
    pub verify_cfg: VerifyCfg,
    pub evals_dir: PathBuf,
    pub rt: tokio::runtime::Handle,
    pub effects: Mutex<Effects>,
    pub out: Mutex<String>,
}

impl std::fmt::Debug for ReplCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplCtx")
            .field("session_id", &self.session_id)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// Result of one `exec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// What goes back to the model: the head of the output when truncated.
    pub output: String,
    pub full_len: usize,
    pub truncated: bool,
    pub error: Option<String>,
}

impl ExecResult {
    pub fn render(&self) -> String {
        let mut s = String::new();
        if !self.output.is_empty() {
            s.push_str(&self.output);
        }
        if self.truncated {
            s.push_str(&format!(
                "\n[output truncated: first {} of {} bytes shown; full text in _last]",
                self.output.len(),
                self.full_len
            ));
        }
        if let Some(e) = &self.error {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str("error: ");
            s.push_str(e);
        }
        if s.is_empty() {
            s.push_str("(no output)");
        }
        s
    }
}

/// A persistent Lua VM with the Longe bindings installed.
#[derive(Debug)]
pub struct Repl {
    lua: Lua,
}

impl Repl {
    pub fn new(ctx: ReplCtx) -> Result<Self, ReplError> {
        let lua = Lua::new();
        lua.set_app_data(ctx);
        lua.load(PRELUDE).set_name("=prelude").exec()?;
        bindings::install(&lua)?;
        lua.load("__longe_freeze()").exec()?;
        Ok(Self { lua })
    }

    /// Run a chunk. Expressions are evaluated and their values shown.
    pub fn exec(&self, code: &str) -> ExecResult {
        let limit = match self.lua.app_data_ref::<ReplCtx>() {
            Some(c) => {
                c.out.lock().clear();
                c.max_output_bytes
            }
            None => crate::config::ReplCfg::default().max_output_bytes,
        };
        let expr = format!("return {code}");
        let chunk = match self.lua.load(&expr).set_name("=exec").into_function() {
            Ok(f) => Ok(f),
            Err(_) => self.lua.load(code).set_name("=exec").into_function(),
        };
        let result: Result<MultiValue, mlua::Error> = chunk.and_then(|f| f.call(()));
        let mut text = self
            .lua
            .app_data_ref::<ReplCtx>()
            .map(|c| c.out.lock().clone())
            .unwrap_or_default();
        let mut error = None;
        match result {
            Ok(values) => {
                let rendered: Vec<String> = values
                    .iter()
                    .filter(|v| !matches!(v, Value::Nil))
                    .map(|v| self.format(v))
                    .collect();
                if !rendered.is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&rendered.join("\t"));
                }
            }
            // A compile error names a line the model no longer sees: quote it.
            Err(mlua::Error::SyntaxError { message, .. }) => {
                error = Some(syntax_report(code, &message));
            }
            Err(e) => error = Some(short_error(&e)),
        }
        let trimmed = text.trim_end_matches('\n').len();
        text.truncate(trimmed);
        let full_len = text.len();
        let _ = self.lua.globals().set("_last", text.as_str());
        let truncated = full_len > limit;
        let output = if truncated {
            // The head, not the tail: a read starts at the top of the file.
            crate::verify::head_bytes(&text, limit).to_string()
        } else {
            text
        };
        ExecResult {
            output,
            full_len,
            truncated,
            error,
        }
    }

    fn format(&self, v: &Value) -> String {
        match self.lua.globals().get::<mlua::Function>("__longe_fmt") {
            Ok(f) => f
                .call::<String>(v.clone())
                .unwrap_or_else(|e| format!("<{e}>")),
            Err(_) => format!("{v:?}"),
        }
    }

    /// User globals as Lua source.
    pub fn dump_state(&self) -> Result<String, ReplError> {
        let f: mlua::Function = self.lua.globals().get("__longe_dump")?;
        Ok(f.call(())?)
    }

    /// Reload globals produced by `dump_state`.
    pub fn restore(&self, src: &str) -> Result<(), ReplError> {
        self.lua.load(src).set_name("=state.lua").exec()?;
        Ok(())
    }

    pub fn state_size(&self) -> usize {
        self.dump_state().map(|s| s.len()).unwrap_or(0)
    }

    pub fn take_effects(&self) -> Effects {
        self.lua
            .app_data_ref::<ReplCtx>()
            .map(|c| std::mem::take(&mut *c.effects.lock()))
            .unwrap_or_default()
    }

    /// Names of user globals with their rendered size (for the agentic GC hint).
    pub fn globals_summary(&self) -> Vec<(String, usize)> {
        let Ok(dump) = self.dump_state() else {
            return Vec::new();
        };
        let mut sizes: HashMap<String, usize> = HashMap::new();
        for line in dump.lines() {
            if let Some((name, rest)) = line.split_once(" = ") {
                sizes.insert(name.to_string(), rest.len());
            }
        }
        let mut v: Vec<(String, usize)> = sizes.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

/// A Lua compile error as a compiler would print it: the message, the offending
/// line with a caret under the token Lua stopped at, and the `[[`/`]]` hint when
/// a long string was closed early by code such as `a[b[1]]`.
fn syntax_report(code: &str, message: &str) -> String {
    let line_no = message
        .strip_prefix("exec:")
        .and_then(|r| r.split(':').next())
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n >= 1);
    let Some((line_no, line)) = line_no.and_then(|n| code.lines().nth(n - 1).map(|l| (n, l)))
    else {
        return message.to_string();
    };
    let col = message
        .rsplit_once("near '")
        .and_then(|(_, t)| t.strip_suffix('\''))
        .and_then(|tok| line.find(tok))
        .unwrap_or(0);
    let mut s = format!("{message}\n{line_no:>5}│{line}\n     │{}^", " ".repeat(col));
    if long_string_closed_early(code, line_no) {
        s.push_str("\nhint: `]]` closes a `[[` long string; use `[==[ ... ]==]`");
    }
    s
}

/// True when a `[[` long string was opened at or before the failing line and
/// either that line contains `]]` or a `]]` appears anywhere while no long string
/// is open: the signature of `[[ ... a[b[1]] ... ]]`, whose intended close comes
/// after the line Lua complains about.
fn long_string_closed_early(code: &str, line_no: usize) -> bool {
    let mut open = false;
    let mut opened_by_error_line = false;
    let mut error_line_closes = false;
    let mut stray = false;
    for (i, l) in code.lines().enumerate() {
        let mut rest = l;
        loop {
            match (open, rest.find("[["), rest.find("]]")) {
                (false, Some(o), Some(c)) if c < o => {
                    stray = true;
                    rest = &rest[c + 2..];
                }
                (false, Some(o), _) => {
                    open = true;
                    if i < line_no {
                        opened_by_error_line = true;
                    }
                    rest = &rest[o + 2..];
                }
                (false, None, Some(c)) => {
                    stray = true;
                    rest = &rest[c + 2..];
                }
                (true, _, Some(c)) => {
                    open = false;
                    rest = &rest[c + 2..];
                }
                _ => break,
            }
        }
        if i + 1 == line_no && l.contains("]]") {
            error_line_closes = true;
        }
    }
    opened_by_error_line && (stray || error_line_closes)
}

fn short_error(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(s) | mlua::Error::SyntaxError { message: s, .. } => s.clone(),
        mlua::Error::CallbackError { traceback, cause } => {
            let c = short_error(cause);
            if traceback.is_empty() {
                c
            } else {
                format!("{c}\n{traceback}")
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    //! A REPL wired to a temp store and a tree actor, for binding tests.
    use super::*;
    use crate::config::{Harness, SandboxCfg};
    use crate::sandbox::Unconfined;
    use crate::session::tree::spawn_tree;
    use std::path::Path;

    pub struct Fixture {
        /// Kept alive for the workspace and store paths.
        pub _dir: tempfile::TempDir,
        pub store: Arc<Store>,
        pub repl: Repl,
        pub _tree: TreeHandle,
        pub workspace: PathBuf,
    }

    pub fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("store")).unwrap());
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let harness = Harness::default().with_default_providers();
        let llm = Arc::new(LlmRegistry::from_harness(&harness).unwrap());
        let tree = spawn_tree(store.clone(), llm.clone(), Arc::new(harness.clone()));
        let repl = build(&store, &workspace, &tree, &llm, &harness);
        Fixture {
            _dir: dir,
            store,
            repl,
            _tree: tree,
            workspace,
        }
    }

    pub fn build(
        store: &Arc<Store>,
        workspace: &Path,
        tree: &TreeHandle,
        llm: &Arc<LlmRegistry>,
        h: &Harness,
    ) -> Repl {
        let cfg = SandboxCfg {
            timeout_seconds: 10,
            ..SandboxCfg::default()
        };
        let policy = Policy::for_session(&cfg, workspace, store.root());
        Repl::new(ReplCtx {
            session_id: SessionId::parse("abcd0001").unwrap(),
            session_name: "test".into(),
            session_budget: h.budget,
            workspace: workspace.to_path_buf(),
            store: store.clone(),
            policy,
            sandbox: Arc::new(Unconfined),
            tree: tree.clone(),
            llm: llm.clone(),
            model: Mutex::new(h.model.model_ref()),
            temperature: 0.0,
            max_output_tokens: 256,
            max_output_bytes: h.repl.max_output_bytes,
            verify_cfg: VerifyCfg {
                command: Some("test -f ok.txt".into()),
                ..VerifyCfg::default()
            },
            evals_dir: store.evals_dir(),
            rt: tokio::runtime::Handle::current(),
            effects: Mutex::new(Effects::default()),
            out: Mutex::new(String::new()),
        })
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::long_string_closed_early;
    use super::testkit::fixture;

    #[tokio::test(flavor = "multi_thread")]
    async fn expressions_and_prints_are_captured() {
        let f = fixture();
        let r = tokio::task::block_in_place(|| f.repl.exec("print('hi', 1)"));
        assert_eq!(r.output, "hi\t1");
        let r = tokio::task::block_in_place(|| f.repl.exec("print('a'); return 2 + 3"));
        assert_eq!(r.output, "a\n5");
        let r = tokio::task::block_in_place(|| f.repl.exec("2 + 3"));
        assert_eq!(r.output, "5");
        assert!(r.error.is_none());
        let r = tokio::task::block_in_place(|| f.repl.exec("x = {a = 1, b = {'p', 'q'}}"));
        assert_eq!(r.output, "");
        let r = tokio::task::block_in_place(|| f.repl.exec("x"));
        assert_eq!(r.output, "{a = 1, b = {\"p\", \"q\"}}");
        assert_eq!(r.render(), "{a = 1, b = {\"p\", \"q\"}}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn errors_are_reported_not_fatal() {
        let f = fixture();
        let r = tokio::task::block_in_place(|| f.repl.exec("error('boom')"));
        assert!(r.error.as_deref().unwrap().contains("boom"), "{r:?}");
        assert!(r.render().starts_with("error: "));
        let r = tokio::task::block_in_place(|| f.repl.exec("this is not lua"));
        assert!(r.error.is_some());
        let r = tokio::task::block_in_place(|| f.repl.exec("1+1"));
        assert_eq!(r.output, "2");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compile_errors_quote_the_line_with_a_caret_and_the_long_string_hint() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        // Line 3 closes the long string at `chunk[3]]`, the `))` that follow are
        // parsed as code: the classic first-Rust-file failure.
        let r = run("x = 1\nlocal src = [[\nlet v = a[b[1]]);\n]]\nfs.write('a.rs', src)");
        let e = r.error.as_deref().unwrap();
        assert!(e.starts_with("exec:3: "), "{e}");
        assert!(e.contains("\n    3│let v = a[b[1]]);\n     │"), "{e}");
        assert!(e.contains("^"), "{e}");
        assert!(
            e.contains("hint: `]]` closes a `[[` long string; use `[==[ ... ]==]`"),
            "{e}"
        );
        // The same file through a level-1 long string compiles.
        let r = run("src = [==[\nlet v = a[b[1]]);\n]==]\nreturn #src");
        assert_eq!(r.output, "18", "{r:?}");
        // A plain syntax error: line and caret, no hint.
        let r = run("y = 2\nz = (1 +\nw = 3");
        let e = r.error.as_deref().unwrap();
        assert!(e.contains("\n    3│w = 3\n     │"), "{e}");
        assert!(!e.contains("hint:"), "{e}");
        // A runtime error keeps its plain message.
        let r = run("error('rt')");
        assert!(!r.error.as_deref().unwrap().contains("│"));
    }

    #[test]
    fn long_string_hint_heuristic() {
        // Error on the very line that holds the stray `]]`.
        assert!(long_string_closed_early("s = [[\na[b[1]] x\n]]", 2));
        // Error later than the stray `]]`.
        assert!(long_string_closed_early("s = [[\na[b[1]];\nlet y\n]]", 3));
        // Balanced long string, error elsewhere: no hint.
        assert!(!long_string_closed_early("s = [[ok]]\nx = (", 2));
        // No long string at all.
        assert!(!long_string_closed_early("t = a[b[1]]\nx = (", 2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn output_is_truncated_and_kept_in_last() {
        let f = fixture();
        let r = tokio::task::block_in_place(|| {
            f.repl
                .exec("print('HEAD' .. string.rep('x', 20000) .. 'TAIL')")
        });
        assert!(r.truncated);
        assert_eq!(r.full_len, 20008);
        assert_eq!(r.output.len(), 8192, "exactly the configured head");
        assert!(
            r.output.starts_with("HEADxxxx"),
            "the start is what is shown"
        );
        assert!(!r.output.contains("TAIL"));
        assert!(r
            .render()
            .ends_with("[output truncated: first 8192 of 20008 bytes shown; full text in _last]"));
        // `_last` is rewritten by every exec, so read both facts in one.
        let r = tokio::task::block_in_place(|| f.repl.exec("return #_last, _last:sub(-4)"));
        assert_eq!(r.output, "20008\t\"TAIL\"");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn state_dump_and_restore_round_trip() {
        let f = fixture();
        tokio::task::block_in_place(|| {
            f.repl
                .exec("counter = 41; names = {'a', 'b'}; cfg = {depth = 2, tags = {x = true}}");
            f.repl
                .exec("function bump() counter = counter + 1 return counter end");
        });
        let dump = f.repl.dump_state().unwrap();
        assert!(dump.contains("counter = 41"), "{dump}");
        assert!(dump.contains("names = {[1]=\"a\",[2]=\"b\",}"), "{dump}");
        assert!(dump.contains("bump = load("), "{dump}");
        assert!(!dump.contains("__longe_fmt"));
        assert!(!dump.contains("\nfs = "));
        let sizes = f.repl.globals_summary();
        assert!(sizes.iter().any(|(n, _)| n == "counter"));

        let g = fixture();
        g.repl.restore(&dump).unwrap();
        let r = tokio::task::block_in_place(|| g.repl.exec("bump()"));
        assert_eq!(r.output, "42");
        let r = tokio::task::block_in_place(|| g.repl.exec("cfg.tags.x and names[2]"));
        assert_eq!(r.output, "\"b\"");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn returned_strings_are_escaped_for_reading_not_reloading() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        // A returned multi-line string must stay on one readable line, with no stray
        // backslash: Lua's %q would emit a backslash before every real newline.
        let r = run(r#"return "    }\n\n    fn parse(&mut self) {\n""#);
        assert_eq!(r.output, r#""    }\n\n    fn parse(&mut self) {\n""#);
        assert!(
            !r.output.contains('\n'),
            "the value must render on one line"
        );
        // Tabs, quotes, backslashes and other control bytes are escaped too.
        let r = run(r#"return "a\tb\"c\\d\1e""#);
        assert_eq!(r.output, r#""a\tb\"c\\d\001e""#);
        // print() is unchanged: it uses tostring, so it keeps the raw text.
        let r = run(r#"print("x\ny")"#);
        assert_eq!(r.output, "x\ny");
        // Strings nested in a table go through the same escaping.
        let r = run(r#"return {k = "l1\nl2"}"#);
        assert_eq!(r.output, r#"{k = "l1\nl2"}"#);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn state_serialization_still_round_trips_multiline_strings() {
        let f = fixture();
        tokio::task::block_in_place(|| {
            f.repl.exec(r#"blob = "line1\nline2\ttab""#);
        });
        // The dump stays loadable Lua source, which is why it keeps %q.
        let dump = f.repl.dump_state().unwrap();
        let g = fixture();
        g.repl.restore(&dump).unwrap();
        let r = tokio::task::block_in_place(|| g.repl.exec("#blob"));
        assert_eq!(r.output, "15", "the reloaded string must be byte identical");
        let r = tokio::task::block_in_place(|| g.repl.exec(r#"blob:find("\n") ~= nil"#));
        assert_eq!(r.output, "true", "the newline survived the round trip");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cycles_do_not_break_dump() {
        let f = fixture();
        tokio::task::block_in_place(|| f.repl.exec("t = {}; t.self = t; t.v = 1"));
        let dump = f.repl.dump_state().unwrap();
        assert!(dump.contains("[\"v\"]=1"), "{dump}");
        let g = fixture();
        g.repl.restore(&dump).unwrap();
        let r = tokio::task::block_in_place(|| g.repl.exec("t.v"));
        assert_eq!(r.output, "1");
    }
}
