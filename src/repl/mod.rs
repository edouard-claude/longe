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

/// Bytes of exec output returned to the model; the rest stays in `_last`.
pub const OUTPUT_LIMIT: usize = 8 * 1024;

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
    /// What goes back to the model (truncated).
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
                "\n[output truncated: {} of {} bytes shown; full text in _last]",
                OUTPUT_LIMIT, self.full_len
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
        if let Some(c) = self.lua.app_data_ref::<ReplCtx>() {
            c.out.lock().clear();
        }
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
            Err(e) => error = Some(short_error(&e)),
        }
        let trimmed = text.trim_end_matches('\n').len();
        text.truncate(trimmed);
        let full_len = text.len();
        let _ = self.lua.globals().set("_last", text.as_str());
        let truncated = full_len > OUTPUT_LIMIT;
        let output = if truncated {
            crate::verify::tail_bytes(&text, OUTPUT_LIMIT)
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
    use super::testkit::fixture;
    use super::*;

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
    async fn output_is_truncated_and_kept_in_last() {
        let f = fixture();
        let r = tokio::task::block_in_place(|| f.repl.exec("print(string.rep('x', 20000))"));
        assert!(r.truncated);
        assert_eq!(r.full_len, 20000);
        assert!(r.output.len() <= OUTPUT_LIMIT + 64);
        let r = tokio::task::block_in_place(|| f.repl.exec("#_last"));
        assert_eq!(r.output, "20000");
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
