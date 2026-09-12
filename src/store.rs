//! L3 memory: the store. Flat files versioned by git.
//!
//! ```text
//! store/
//!   harness.toml  prompt.md  skills/*.md  memory/*.md  subagents/*.md
//!   sessions/<id>/  trajectories/<id>.jsonl  evals/
//! ```
//!
//! The model only ever touches these through the Lua bindings, hence through this
//! module. `sh()` cannot see them (sandbox fixed rules).

use std::path::{Path, PathBuf};
use std::process::Command;

use parking_lot::Mutex;
use serde::Serialize;

use crate::config::{ConfigError, Harness};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid name `{0}`: use [A-Za-z0-9_.-], no leading dot, max 100 chars")]
    BadName(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Validate a user-supplied name used as a file stem.
pub fn safe_name(s: &str) -> Result<&str, StoreError> {
    let ok = !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(s)
    } else {
        Err(StoreError::BadName(s.to_string()))
    }
}

/// Write atomically: temp file in the same directory, then rename.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io(parent))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, content).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io(path))?;
    Ok(())
}

/// One index entry: name plus first non-empty line.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexEntry {
    pub name: String,
    pub head: String,
    pub bytes: u64,
}

/// What gets injected in the system prompt.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StoreIndex {
    pub skills: Vec<IndexEntry>,
    pub memory: Vec<IndexEntry>,
    pub subagents: Vec<IndexEntry>,
}

impl StoreIndex {
    pub fn render(&self) -> String {
        fn section(title: &str, entries: &[IndexEntry], hint: &str) -> String {
            let mut s = format!("## {title} ({})\n", entries.len());
            if entries.is_empty() {
                s.push_str(&format!("(none) {hint}\n"));
            }
            for e in entries {
                s.push_str(&format!("- {} ({} B): {}\n", e.name, e.bytes, e.head));
            }
            s
        }
        let mut out = String::new();
        out.push_str(&section(
            "skills",
            &self.skills,
            "skill.set(name, text) to add one.",
        ));
        out.push_str(&section(
            "memory",
            &self.memory,
            "mem.set(key, text) to remember.",
        ));
        out.push_str(&section(
            "subagents",
            &self.subagents,
            "reusable sub-agent specs.",
        ));
        out
    }
}

pub const DEFAULT_PROMPT: &str = r#"You are Longe's agent: an autonomous senior engineer driving a persistent Lua REPL.
Work in small verified steps. Write code to files, run the verifier often, keep notes.
Prefer parallelism: spawn sub-agents for independent sub-tasks and integrate their reports.
Record durable lessons in memory (mem.set) and reusable procedures in skills (skill.set).
Never give up early: the budget is a floor, use it to harden, test and refactor.
"#;

pub const DEFAULT_HARNESS: &str = r#"# Longe harness configuration. Edit freely; reflection may also edit it.

[model]
provider = "ollama"
name = "qwen3-vl:8b"
temperature = 0.2
context_window = 32768
# Reasoning tokens count against this limit: a thinking model that spends it all on
# reasoning returns nothing, so keep it high (the runtime retries once at twice this).
max_output_tokens = 16384

[providers.ollama]
kind = "ollama"
base_url = "http://127.0.0.1:11434"
# Add `think = false` only for models that support it; qwen3-vl fails on it.

[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[providers.openrouter]
kind = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"

[budget]
min_seconds = 1800
min_turns = 20
max_turns = 400
max_tokens = 4000000

[verify]
command = "cargo test && cargo clippy -- -D warnings"

[sandbox]
mode = "workspace-write"
network = false
timeout_seconds = 300

[reflect]
enabled = true

[compact]
threshold = 0.7
keep_last = 5

[repl]
# Bytes of exec output shown to the model per turn (the head; the rest stays in
# `_last`). Reading a spec in 8 KB slices costs a turn per slice: 16 KB halves that.
max_output_bytes = 16384
"#;

const STORE_GITIGNORE: &str = "sessions/\ntrajectories/\nevals/cases/\n*.sock\n*.tmp-*\n";

/// The store handle. Cheap to share behind an `Arc`; git operations are serialized.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    git_lock: Mutex<()>,
}

impl Store {
    /// Open an existing store, creating the skeleton if the directory is empty or missing.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root: PathBuf = root.into();
        let s = Self {
            root,
            git_lock: Mutex::new(()),
        };
        s.ensure_skeleton()?;
        Ok(s)
    }

    fn ensure_skeleton(&self) -> Result<(), StoreError> {
        for d in [
            "skills",
            "memory",
            "subagents",
            "sessions",
            "trajectories",
            "evals",
        ] {
            let p = self.root.join(d);
            std::fs::create_dir_all(&p).map_err(io(&p))?;
        }
        let prompt = self.root.join("prompt.md");
        if !prompt.exists() {
            write_atomic(&prompt, DEFAULT_PROMPT.as_bytes())?;
        }
        let harness = self.root.join("harness.toml");
        if !harness.exists() {
            write_atomic(&harness, DEFAULT_HARNESS.as_bytes())?;
        }
        let gi = self.root.join(".gitignore");
        if !gi.exists() {
            write_atomic(&gi, STORE_GITIGNORE.as_bytes())?;
        }
        if !self.root.join(".git").exists() {
            self.git(&["init", "-q", "-b", "main"])?;
            self.git(&["config", "user.email", "longe@localhost"])?;
            self.git(&["config", "user.name", "longe"])?;
            self.git_commit_all("store: init")?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn harness_path(&self) -> PathBuf {
        self.root.join("harness.toml")
    }

    pub fn harness(&self) -> Result<Harness, StoreError> {
        Ok(Harness::load(&self.harness_path())?.with_default_providers())
    }

    pub fn evals_dir(&self) -> PathBuf {
        self.root.join("evals")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(id)
    }

    pub fn trajectories_dir(&self) -> PathBuf {
        self.root.join("trajectories")
    }

    pub fn trajectory_path(&self, id: &str) -> PathBuf {
        self.trajectories_dir().join(format!("{id}.jsonl"))
    }

    /// Append one JSON event to a trajectory.
    pub fn append_trajectory<T: Serialize>(&self, id: &str, event: &T) -> Result<(), StoreError> {
        use std::io::Write;
        let path = self.trajectory_path(id);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io(&path))?;
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        f.write_all(&line).map_err(io(&path))?;
        Ok(())
    }

    /// Read the last `n` trajectory events (whole file scan; trajectories are small).
    pub fn trajectory_tail(
        &self,
        id: &str,
        n: usize,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let path = self.trajectory_path(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(io(&path))?;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let start = lines.len().saturating_sub(n);
        lines[start..]
            .iter()
            .map(|l| Ok(serde_json::from_str(l)?))
            .collect()
    }

    pub fn trajectory_all(&self, id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        self.trajectory_tail(id, usize::MAX)
    }

    // ----- prompt -----

    pub fn prompt_get(&self) -> Result<String, StoreError> {
        let p = self.root.join("prompt.md");
        std::fs::read_to_string(&p).map_err(io(&p))
    }

    pub fn prompt_set(&self, text: &str) -> Result<(), StoreError> {
        write_atomic(&self.root.join("prompt.md"), text.as_bytes())
    }

    // ----- generic named collections -----

    fn coll_path(&self, coll: &str, name: &str) -> Result<PathBuf, StoreError> {
        Ok(self
            .root
            .join(coll)
            .join(format!("{}.md", safe_name(name)?)))
    }

    fn coll_get(&self, coll: &str, name: &str) -> Result<String, StoreError> {
        let p = self.coll_path(coll, name)?;
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(format!("{coll}/{name}")))
            }
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn coll_set(&self, coll: &str, name: &str, text: &str) -> Result<(), StoreError> {
        write_atomic(&self.coll_path(coll, name)?, text.as_bytes())
    }

    fn coll_del(&self, coll: &str, name: &str) -> Result<bool, StoreError> {
        let p = self.coll_path(coll, name)?;
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io { path: p, source }),
        }
    }

    fn coll_list(&self, coll: &str) -> Result<Vec<IndexEntry>, StoreError> {
        let dir = self.root.join(coll);
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(source) => return Err(StoreError::Io { path: dir, source }),
        };
        for entry in rd {
            let entry = entry.map_err(io(&dir))?;
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) != Some("md") || stem.starts_with('.') {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(io(&path))?;
            let head = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect();
            out.push(IndexEntry {
                name: stem.to_string(),
                head,
                bytes: text.len() as u64,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn coll_search(&self, coll: &str, query: &str) -> Result<Vec<(String, String)>, StoreError> {
        let q = query.to_lowercase();
        let mut hits = Vec::new();
        for e in self.coll_list(coll)? {
            let text = self.coll_get(coll, &e.name)?;
            let lower = text.to_lowercase();
            if e.name.to_lowercase().contains(&q) || lower.contains(&q) {
                let snippet = match lower.find(&q) {
                    Some(pos) => {
                        let start = text[..pos]
                            .char_indices()
                            .rev()
                            .nth(60)
                            .map_or(0, |(i, _)| i);
                        let end = text[pos..]
                            .char_indices()
                            .nth(160)
                            .map_or(text.len(), |(i, _)| pos + i);
                        text[start..end].replace('\n', " ")
                    }
                    None => e.head.clone(),
                };
                hits.push((e.name, snippet));
            }
        }
        Ok(hits)
    }

    pub fn skill_get(&self, name: &str) -> Result<String, StoreError> {
        self.coll_get("skills", name)
    }
    pub fn skill_set(&self, name: &str, text: &str) -> Result<(), StoreError> {
        self.coll_set("skills", name, text)
    }
    pub fn skill_del(&self, name: &str) -> Result<bool, StoreError> {
        self.coll_del("skills", name)
    }
    pub fn skill_list(&self) -> Result<Vec<IndexEntry>, StoreError> {
        self.coll_list("skills")
    }

    pub fn mem_get(&self, key: &str) -> Result<String, StoreError> {
        self.coll_get("memory", key)
    }
    pub fn mem_set(&self, key: &str, text: &str) -> Result<(), StoreError> {
        self.coll_set("memory", key, text)
    }
    pub fn mem_del(&self, key: &str) -> Result<bool, StoreError> {
        self.coll_del("memory", key)
    }
    pub fn mem_list(&self) -> Result<Vec<IndexEntry>, StoreError> {
        self.coll_list("memory")
    }
    pub fn mem_search(&self, query: &str) -> Result<Vec<(String, String)>, StoreError> {
        self.coll_search("memory", query)
    }

    pub fn subagent_get(&self, name: &str) -> Result<String, StoreError> {
        self.coll_get("subagents", name)
    }
    pub fn subagent_set(&self, name: &str, text: &str) -> Result<(), StoreError> {
        self.coll_set("subagents", name, text)
    }
    pub fn subagent_del(&self, name: &str) -> Result<bool, StoreError> {
        self.coll_del("subagents", name)
    }
    pub fn subagent_list(&self) -> Result<Vec<IndexEntry>, StoreError> {
        self.coll_list("subagents")
    }

    pub fn index(&self) -> Result<StoreIndex, StoreError> {
        Ok(StoreIndex {
            skills: self.skill_list()?,
            memory: self.mem_list()?,
            subagents: self.subagent_list()?,
        })
    }

    // ----- git -----

    fn git(&self, args: &[&str]) -> Result<String, StoreError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(StoreError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })
        }
    }

    /// Stage everything and commit. Returns the new commit hash, or `None` when clean.
    pub fn git_commit_all(&self, message: &str) -> Result<Option<String>, StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["add", "-A"])?;
        let status = self.git(&["status", "--porcelain"])?;
        if status.trim().is_empty() {
            return Ok(None);
        }
        self.git(&["commit", "-q", "-m", message])?;
        Ok(Some(
            self.git(&["rev-parse", "--short", "HEAD"])?
                .trim()
                .to_string(),
        ))
    }

    pub fn git_current_branch(&self) -> Result<String, StoreError> {
        let _g = self.git_lock.lock();
        Ok(self
            .git(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    pub fn git_checkout_new(&self, branch: &str) -> Result<(), StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["checkout", "-q", "-b", branch]).map(drop)
    }

    pub fn git_checkout(&self, branch: &str) -> Result<(), StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["checkout", "-q", branch]).map(drop)
    }

    /// Fast-forward or merge `branch` into the current branch.
    pub fn git_merge(&self, branch: &str, message: &str) -> Result<(), StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["merge", "-q", "--no-ff", "--no-edit", "-m", message, branch])
            .map(drop)
    }

    pub fn git_delete_branch(&self, branch: &str) -> Result<(), StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["branch", "-q", "-D", branch]).map(drop)
    }

    pub fn git_branches(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let _g = self.git_lock.lock();
        let out = self.git(&["branch", "--list", "--format=%(refname:short)"])?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|b| b.starts_with(prefix))
            .map(String::from)
            .collect())
    }

    pub fn git_log(&self, n: usize) -> Result<Vec<String>, StoreError> {
        let _g = self.git_lock.lock();
        let out = self.git(&[
            "log",
            "--all",
            &format!("-{n}"),
            "--format=%h %ad %s",
            "--date=iso-strict",
        ])?;
        Ok(out.lines().map(String::from).collect())
    }

    /// Diff between `base` and `branch` (files under version control only).
    pub fn git_diff(&self, base: &str, branch: &str) -> Result<String, StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["diff", "--stat", "-p", &format!("{base}...{branch}")])
    }

    /// Discard uncommitted changes to tracked files (used for rollback).
    pub fn git_reset_hard(&self) -> Result<(), StoreError> {
        let _g = self.git_lock.lock();
        self.git(&["reset", "-q", "--hard"]).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().join("store")).unwrap();
        (d, s)
    }

    #[test]
    fn open_creates_skeleton_and_git() {
        let (_d, s) = store();
        assert!(s.root().join("prompt.md").exists());
        assert!(s.root().join("harness.toml").exists());
        assert!(s.root().join(".git").exists());
        assert_eq!(s.git_current_branch().unwrap(), "main");
        assert_eq!(s.harness().unwrap().budget.min_turns, 20);
    }

    #[test]
    fn init_template_leaves_room_for_reasoning_tokens() {
        let (_d, s) = store();
        let h = s.harness().unwrap();
        assert_eq!(h.model.max_output_tokens, 16_384);
        assert_eq!(h.repl.max_output_bytes, 16_384);
        let text = std::fs::read_to_string(s.harness_path()).unwrap();
        assert!(
            text.contains("Reasoning tokens count against this limit"),
            "the template must say why the limit is high"
        );
    }

    #[test]
    fn crud_and_index() {
        let (_d, s) = store();
        s.skill_set(
            "parse-json",
            "# Parsing JSON\nUse a recursive descent parser.",
        )
        .unwrap();
        s.mem_set("lesson-1", "clippy hates integer division")
            .unwrap();
        s.subagent_set("tester", "Writes tests for a module.")
            .unwrap();
        let idx = s.index().unwrap();
        assert_eq!(idx.skills[0].name, "parse-json");
        assert_eq!(idx.skills[0].head, "# Parsing JSON");
        assert_eq!(idx.memory.len(), 1);
        assert_eq!(idx.subagents.len(), 1);
        assert!(idx.render().contains("parse-json"));
        assert_eq!(s.skill_get("parse-json").unwrap().lines().count(), 2);
        assert!(s.skill_del("parse-json").unwrap());
        assert!(!s.skill_del("parse-json").unwrap());
        assert!(matches!(
            s.skill_get("parse-json"),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn search_finds_substring_with_snippet() {
        let (_d, s) = store();
        s.mem_set(
            "a",
            "first line\nthe Borrow checker complained about lifetimes\n",
        )
        .unwrap();
        s.mem_set("b", "unrelated").unwrap();
        let hits = s.mem_search("borrow").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "a");
        assert!(hits[0].1.contains("Borrow checker"));
    }

    #[test]
    fn names_are_validated() {
        let (_d, s) = store();
        assert!(matches!(
            s.mem_set("../etc", "x"),
            Err(StoreError::BadName(_))
        ));
        assert!(matches!(
            s.mem_set(".hidden", "x"),
            Err(StoreError::BadName(_))
        ));
        assert!(matches!(s.mem_set("a/b", "x"), Err(StoreError::BadName(_))));
        assert!(s.mem_set("ok-name_1.v2", "x").is_ok());
    }

    #[test]
    fn trajectory_append_and_tail() {
        let (_d, s) = store();
        for i in 0..5 {
            s.append_trajectory("sess", &serde_json::json!({"i": i}))
                .unwrap();
        }
        let tail = s.trajectory_tail("sess", 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1]["i"], 4);
        assert!(s.trajectory_tail("nope", 3).unwrap().is_empty());
    }

    #[test]
    fn git_branch_merge_rollback() {
        let (_d, s) = store();
        assert!(s.git_commit_all("nothing").unwrap().is_none());
        s.git_checkout_new("reflect/x").unwrap();
        s.skill_set("new", "a new skill").unwrap();
        let h = s.git_commit_all("reflect: add skill").unwrap();
        assert!(h.is_some());
        s.git_checkout("main").unwrap();
        assert!(s.skill_get("new").is_err());
        s.git_merge("reflect/x", "accept reflect/x").unwrap();
        assert!(s.skill_get("new").is_ok());
        s.git_delete_branch("reflect/x").unwrap();
        assert!(s.git_branches("reflect/").unwrap().is_empty());
        let log = s.git_log(10).unwrap();
        assert!(
            log.iter().any(|l| l.contains("reflect: add skill")),
            "{log:?}"
        );
        let diff = s.git_diff("main~1", "main").unwrap();
        assert!(diff.contains("new.md"));
    }
}
