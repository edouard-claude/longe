//! Post-run reflection (continual harness). The runtime reads a finished trajectory,
//! asks a model for diffs on its own store (skills, memory, prompt, subagents,
//! harness.toml), applies them on a git branch, scores them with the fitness command,
//! and keeps or rolls back. A human can force either from the cockpit.

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Harness, ModelRef};
use crate::llm::{ChatMessage, ChatRequest, LlmRegistry};
use crate::session::SessionId;
use crate::store::{safe_name, write_atomic, Store, StoreError};

pub const BRANCH_PREFIX: &str = "reflect/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectReport {
    pub session: SessionId,
    pub branch: String,
    pub analysis: String,
    pub changes: Vec<Change>,
    pub fitness_before: Option<f64>,
    pub fitness_after: Option<f64>,
    /// `Some(true)` merged, `Some(false)` rolled back, `None` pending human review.
    pub accepted: Option<bool>,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDiff {
    pub branch: String,
    pub diff: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ReflectError {
    #[error("no trajectory for session {0}")]
    NoTrajectory(SessionId),
    #[error("model returned no usable JSON: {0}")]
    BadJson(String),
    #[error("refused change to {0}: {1}")]
    Refused(String, String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Llm(#[from] crate::llm::LlmError),
    #[error("fitness command failed: {0}")]
    Fitness(String),
}

/// Condense a trajectory for the reflection prompt.
pub fn condense(events: &[Value], max_bytes: usize) -> String {
    let mut s = String::new();
    for e in events {
        let kind = e["kind"].as_str().unwrap_or("?");
        let turn = e["turn"].as_u64().unwrap_or(0);
        let line = match kind {
            "start" => format!("[start] task: {}\n", e["task"].as_str().unwrap_or("")),
            "exec" => {
                let code = e["code"].as_str().unwrap_or("");
                let out = e["output"].as_str().unwrap_or("");
                let err = e["error"].as_str().unwrap_or("");
                format!(
                    "[t{turn} exec]\n{}\n-> {}{}\n",
                    head(code, 600),
                    head(out, 300),
                    if err.is_empty() {
                        String::new()
                    } else {
                        format!("\nERROR: {}", head(err, 200))
                    }
                )
            }
            "note" => format!("[t{turn} note] {}\n", e["text"].as_str().unwrap_or("")),
            "verify" => format!(
                "[t{turn} verify] ok={} code={} {}s\n",
                e["ok"], e["code"], e["seconds"]
            ),
            "done_refused" => format!(
                "[t{turn} done refused] {}\n",
                head(e["reason"].as_str().unwrap_or(""), 200)
            ),
            "message" => format!(
                "[t{turn} message from {}] {}\n",
                e["from"].as_str().unwrap_or("?"),
                head(e["body"].as_str().unwrap_or(""), 200)
            ),
            "compact" => format!("[t{turn} compaction]\n"),
            "model_switch" => format!("[t{turn} model {} -> {}]\n", e["from"], e["to"]),
            "llm_error" => format!("[t{turn} llm error] {}\n", e["error"]),
            "exhausted" => format!("[t{turn} exhausted] {}\n", e["reason"]),
            "finish" => format!("[finish] {}\n", e["outcome"]),
            _ => String::new(),
        };
        s.push_str(&line);
    }
    if s.len() > max_bytes {
        // Keep the start and the end; the middle is the least informative.
        let head_len = max_bytes.div_euclid(3);
        let tail_len = max_bytes - head_len;
        let mut h = head_len;
        while !s.is_char_boundary(h) {
            h -= 1;
        }
        let mut t = s.len() - tail_len;
        while !s.is_char_boundary(t) {
            t += 1;
        }
        format!("{}\n[... {} bytes elided ...]\n{}", &s[..h], t - h, &s[t..])
    } else {
        s
    }
}

fn head(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// Files reflection may touch, and the sections of harness.toml it may not.
pub fn validate_change(store: &Store, c: &Change) -> Result<(), ReflectError> {
    let p = c.path.trim_start_matches("./");
    let refuse = |why: &str| Err(ReflectError::Refused(c.path.clone(), why.into()));
    if p == "prompt.md" || p == "harness.toml" {
        if c.delete {
            return refuse("cannot delete");
        }
        if p == "harness.toml" {
            let text = c.content.as_deref().unwrap_or("");
            let new = Harness::parse(text)
                .map_err(|e| ReflectError::Refused(c.path.clone(), e.to_string()))?;
            let old = store.harness()?;
            fn same<T: Serialize>(a: &T, b: &T) -> bool {
                serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
            }
            if !same(&new.verify, &old.verify)
                || !same(&new.sandbox, &old.sandbox)
                || !same(&new.reflect, &old.reflect)
                || !same(&new.daemon, &old.daemon)
                || (!new.providers.is_empty() && !same(&new.providers, &old.providers))
            {
                return refuse("[verify], [sandbox], [reflect], [daemon] and [providers] are immutable to reflection");
            }
        }
        return Ok(());
    }
    let Some((dir, file)) = p.split_once('/') else {
        return refuse("unknown path");
    };
    if !matches!(dir, "skills" | "memory" | "subagents") {
        return refuse("only skills/, memory/, subagents/, prompt.md, harness.toml");
    }
    let stem = file
        .strip_suffix(".md")
        .ok_or_else(|| ReflectError::Refused(c.path.clone(), "must end in .md".into()))?;
    safe_name(stem).map_err(|e| ReflectError::Refused(c.path.clone(), e.to_string()))?;
    if !c.delete && c.content.is_none() {
        return refuse("missing content");
    }
    Ok(())
}

fn apply_change(store: &Store, c: &Change) -> Result<(), StoreError> {
    let path = store.root().join(c.path.trim_start_matches("./"));
    if c.delete {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    write_atomic(&path, c.content.as_deref().unwrap_or("").as_bytes())
}

/// Extract the first JSON object from a model reply.
pub fn extract_json(text: &str) -> Option<Value> {
    let clean = crate::parse::strip_thinking(text);
    let start = clean.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in clean[start..].char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&clean[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Run the fitness command in the store root; the last stdout line is the score.
pub fn fitness(store: &Store, cmd: &str, timeout: Duration) -> Result<f64, ReflectError> {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(cmd).current_dir(store.root());
    for (k, _) in std::env::vars_os() {
        if k.to_str()
            .is_some_and(crate::sandbox::Policy::env_is_secret)
        {
            c.env_remove(k);
        }
    }
    let policy = crate::sandbox::Policy {
        mode: crate::config::SandboxMode::FullAccess,
        workspace: store.root().to_path_buf(),
        read_extra: Vec::new(),
        network: true,
        timeout,
        hidden: Vec::new(),
        agent_user: None,
    };
    let out = crate::sandbox::run_prepared(c, &policy, cmd)
        .map_err(|e| ReflectError::Fitness(e.to_string()))?;
    if out.timed_out {
        return Err(ReflectError::Fitness("timed out".into()));
    }
    let last = out
        .stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    last.split_whitespace()
        .next()
        .and_then(|w| w.trim_end_matches('%').parse::<f64>().ok())
        .ok_or_else(|| {
            ReflectError::Fitness(format!("no numeric score in `{last}` (exit {})", out.code))
        })
}

fn log_event(store: &Store, ev: &Value) {
    use std::io::Write;
    let path = store.root().join("reflect.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ev}");
    }
}

/// Sessions already reflected (from reflect.log).
pub fn reflected_ids(store: &Store) -> HashSet<SessionId> {
    let mut set = HashSet::new();
    if let Ok(text) = std::fs::read_to_string(store.root().join("reflect.log")) {
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(id) = v["session"].as_str().and_then(|s| SessionId::parse(s).ok()) {
                    set.insert(id);
                }
            }
        }
    }
    set
}

const REFLECT_SYSTEM: &str = "You improve the harness of an autonomous coding agent by editing its store: \
skills (reusable procedures), memory (durable facts), subagents (reusable sub-agent task specs), prompt.md \
(its system prompt) and harness.toml ([model], [budget], [compact] and [[cron]] only). Reply with ONE JSON object and nothing else.";

/// Reflect over one finished session.
pub async fn run(
    store: &Store,
    llm: &LlmRegistry,
    harness: &Harness,
    id: &SessionId,
) -> Result<ReflectReport, ReflectError> {
    let events = store.trajectory_all(id.as_str())?;
    if events.is_empty() {
        return Err(ReflectError::NoTrajectory(id.clone()));
    }
    let model: ModelRef = harness
        .reflect
        .model
        .clone()
        .unwrap_or_else(|| harness.model.model_ref());
    let transcript = condense(&events, 60_000);
    let index = store.index()?.render();
    let prompt_md = store.prompt_get()?;
    let harness_toml = std::fs::read_to_string(store.harness_path()).unwrap_or_default();
    let prompt = format!(
        "A session just finished. Study its trajectory and propose changes to the store so the NEXT session \
         on a similar task is faster, cheaper and more reliable.\n\n\
         Answer these questions in `analysis` (short): what worked, what failed or looped, what cost the most turns, \
         what a skill or memory would have prevented.\n\n\
         Then list `changes`: an array of {{\"path\": ..., \"content\": ...}} or {{\"path\": ..., \"delete\": true}}. \
         Allowed paths: skills/<name>.md, memory/<name>.md, subagents/<name>.md, prompt.md, harness.toml. \
         Skills are procedures with exact commands and code snippets. Memory entries are facts. Keep prompt.md changes minimal. \
         Full file contents, not diffs. Propose at least one change unless the store is already perfect.\n\n\
         ## Current store index\n{index}\n\n## Current prompt.md\n{prompt_md}\n\n## Current harness.toml\n{harness_toml}\n\n\
         ## Trajectory\n{transcript}\n\n\
         Reply with JSON only: {{\"analysis\": \"...\", \"changes\": [...]}}"
    );
    let req = ChatRequest {
        model: model.name.clone(),
        system: REFLECT_SYSTEM.into(),
        messages: vec![ChatMessage::user(prompt)],
        temperature: 0.1,
        max_tokens: harness.model.max_output_tokens.max(4096),
    };
    let resp = llm.complete(&model, &req).await?;
    let json =
        extract_json(&resp.text).ok_or_else(|| ReflectError::BadJson(head(&resp.text, 300)))?;
    let analysis = json["analysis"].as_str().unwrap_or("").to_string();
    let changes: Vec<Change> = serde_json::from_value(json["changes"].clone()).unwrap_or_default();
    let mut valid = Vec::new();
    for c in changes {
        match validate_change(store, &c) {
            Ok(()) => valid.push(c),
            Err(e) => tracing::warn!("reflect: {e}"),
        }
    }
    apply(store, llm_free(harness), id, analysis, valid).await
}

fn llm_free(h: &Harness) -> &Harness {
    h
}

/// Apply validated changes on a branch and decide.
pub async fn apply(
    store: &Store,
    harness: &Harness,
    id: &SessionId,
    analysis: String,
    changes: Vec<Change>,
) -> Result<ReflectReport, ReflectError> {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let branch = format!("{BRANCH_PREFIX}{id}-{ts}");
    // Leave main clean before branching.
    store.git_commit_all(&format!("store: changes during session {id}"))?;
    let base = store.git_current_branch()?;
    let mut report = ReflectReport {
        session: id.clone(),
        branch: branch.clone(),
        analysis,
        changes: changes.clone(),
        fitness_before: None,
        fitness_after: None,
        accepted: None,
        commit: None,
    };
    if changes.is_empty() {
        log_event(
            store,
            &json!({"session": id, "branch": branch, "changes": 0, "accepted": false, "reason": "no changes"}),
        );
        report.accepted = Some(false);
        return Ok(report);
    }
    let fitness_cmd = harness.reflect.fitness.clone();
    let timeout = Duration::from_secs(harness.reflect.fitness_timeout_seconds);
    if let Some(cmd) = &fitness_cmd {
        report.fitness_before = Some(tokio::task::block_in_place(|| {
            fitness(store, cmd, timeout)
        })?);
    }
    store.git_checkout_new(&branch)?;
    let applied: Result<(), StoreError> = changes.iter().try_for_each(|c| apply_change(store, c));
    if let Err(e) = applied {
        store.git_reset_hard()?;
        store.git_checkout(&base)?;
        store.git_delete_branch(&branch)?;
        return Err(e.into());
    }
    report.commit = store.git_commit_all(&format!(
        "reflect: {} change(s) from session {id}",
        changes.len()
    ))?;
    match &fitness_cmd {
        Some(cmd) => {
            let after = tokio::task::block_in_place(|| fitness(store, cmd, timeout));
            store.git_checkout(&base)?;
            match after {
                Ok(score) => {
                    report.fitness_after = Some(score);
                    let keep = score >= report.fitness_before.unwrap_or(f64::MIN);
                    if keep {
                        store.git_merge(
                            &branch,
                            &format!(
                                "accept {branch} (fitness {score} >= {:?})",
                                report.fitness_before
                            ),
                        )?;
                        store.git_delete_branch(&branch)?;
                    } else {
                        store.git_delete_branch(&branch)?;
                    }
                    report.accepted = Some(keep);
                }
                Err(e) => {
                    tracing::warn!("fitness on branch failed, leaving {branch} pending: {e}");
                }
            }
        }
        None => {
            store.git_checkout(&base)?;
        }
    }
    log_event(
        store,
        &json!({"session": id, "branch": branch, "changes": changes.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
                "fitness_before": report.fitness_before, "fitness_after": report.fitness_after, "accepted": report.accepted, "commit": report.commit, "ts": crate::session::now_rfc3339()}),
    );
    Ok(report)
}

/// Branches awaiting a human decision, with their diff against main.
pub fn pending(store: &Store) -> Result<Vec<PendingDiff>, StoreError> {
    let base = store.git_current_branch()?;
    store
        .git_branches(BRANCH_PREFIX)?
        .into_iter()
        .map(|b| {
            Ok(PendingDiff {
                diff: store.git_diff(&base, &b)?,
                branch: b,
            })
        })
        .collect()
}

pub fn accept(store: &Store, branch: &str) -> Result<(), StoreError> {
    store.git_merge(branch, &format!("accept {branch} (human)"))?;
    store.git_delete_branch(branch)?;
    log_event(
        store,
        &json!({"branch": branch, "accepted": true, "by": "human", "ts": crate::session::now_rfc3339()}),
    );
    Ok(())
}

pub fn reject(store: &Store, branch: &str) -> Result<(), StoreError> {
    store.git_delete_branch(branch)?;
    log_event(
        store,
        &json!({"branch": branch, "accepted": false, "by": "human", "ts": crate::session::now_rfc3339()}),
    );
    Ok(())
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
    fn json_extraction_survives_prose_and_fences() {
        let v = extract_json("Sure!\n```json\n{\"analysis\": \"a {b}\", \"changes\": [{\"path\": \"skills/x.md\", \"content\": \"c\\\"q\"}]}\n```").unwrap();
        assert_eq!(v["changes"][0]["path"], "skills/x.md");
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn validation_rules() {
        let (_d, s) = store();
        let ok = Change {
            path: "skills/parse.md".into(),
            content: Some("x".into()),
            delete: false,
        };
        assert!(validate_change(&s, &ok).is_ok());
        let bad_dir = Change {
            path: "evals/run.sh".into(),
            content: Some("x".into()),
            delete: false,
        };
        assert!(validate_change(&s, &bad_dir).is_err());
        let traversal = Change {
            path: "skills/../x.md".into(),
            content: Some("x".into()),
            delete: false,
        };
        assert!(validate_change(&s, &traversal).is_err());
        let del_prompt = Change {
            path: "prompt.md".into(),
            content: None,
            delete: true,
        };
        assert!(validate_change(&s, &del_prompt).is_err());
        let verify_edit = Change {
            path: "harness.toml".into(),
            content: Some("[verify]\ncommand = \"cat $LONGE_EVALS/cases/*/expected\"\n".into()),
            delete: false,
        };
        assert!(matches!(
            validate_change(&s, &verify_edit),
            Err(ReflectError::Refused(_, _))
        ));
        let mut h = s.harness().unwrap();
        h.budget.min_turns = 5;
        h.providers.clear();
        let budget_edit = Change {
            path: "harness.toml".into(),
            content: Some(h.to_toml().unwrap()),
            delete: false,
        };
        assert!(
            validate_change(&s, &budget_edit).is_ok(),
            "{:?}",
            validate_change(&s, &budget_edit).err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_with_fitness_accepts_when_not_worse() {
        let (_d, s) = store();
        let mut h = s.harness().unwrap();
        h.reflect.fitness = Some("cat fitness.txt 2>/dev/null || echo 1".into());
        std::fs::write(s.root().join("fitness.txt"), "1\n").unwrap();
        let id = SessionId::parse("cafe0001").unwrap();
        s.append_trajectory(id.as_str(), &json!({"kind": "start", "task": "t"}))
            .unwrap();
        let changes = vec![Change {
            path: "skills/lesson.md".into(),
            content: Some("# Lesson\nrun clippy".into()),
            delete: false,
        }];
        let r = apply(&s, &h, &id, "a".into(), changes).await.unwrap();
        assert_eq!(r.accepted, Some(true));
        assert_eq!(r.fitness_before, Some(1.0));
        assert_eq!(r.fitness_after, Some(1.0));
        assert!(s.skill_get("lesson").is_ok());
        assert!(s.git_branches(BRANCH_PREFIX).unwrap().is_empty());
        assert!(s
            .git_log(5)
            .unwrap()
            .iter()
            .any(|l| l.contains("accept reflect/")));
        assert!(reflected_ids(&s).contains(&id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_with_fitness_rolls_back_when_worse() {
        let (_d, s) = store();
        let mut h = s.harness().unwrap();
        // Score = number of skills negated: adding a skill makes it worse.
        h.reflect.fitness = Some("echo $(( 10 - $(ls skills | wc -l) ))".into());
        let id = SessionId::parse("cafe0002").unwrap();
        let changes = vec![Change {
            path: "skills/bad.md".into(),
            content: Some("bad".into()),
            delete: false,
        }];
        let r = apply(&s, &h, &id, "a".into(), changes).await.unwrap();
        assert_eq!(r.accepted, Some(false));
        assert!(r.fitness_after.unwrap() < r.fitness_before.unwrap());
        assert!(s.skill_get("bad").is_err());
        assert_eq!(s.git_current_branch().unwrap(), "main");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_fitness_the_branch_waits_for_a_human() {
        let (_d, s) = store();
        let h = s.harness().unwrap();
        let id = SessionId::parse("cafe0003").unwrap();
        let changes = vec![Change {
            path: "memory/fact.md".into(),
            content: Some("jq exit codes: 5 on filter error".into()),
            delete: false,
        }];
        let r = apply(&s, &h, &id, "a".into(), changes).await.unwrap();
        assert_eq!(r.accepted, None);
        let p = pending(&s).unwrap();
        assert_eq!(p.len(), 1);
        assert!(p[0].diff.contains("fact.md"));
        assert!(s.mem_get("fact").is_err());
        accept(&s, &p[0].branch).unwrap();
        assert!(s.mem_get("fact").is_ok());
        assert!(pending(&s).unwrap().is_empty());
    }

    #[test]
    fn condense_keeps_start_and_end() {
        let mut ev = vec![json!({"kind": "start", "task": "T", "turn": 0})];
        for i in 1..200 {
            ev.push(json!({"kind": "exec", "turn": i, "code": "x".repeat(500), "output": "o"}));
        }
        ev.push(json!({"kind": "finish", "outcome": {"kind": "done"}}));
        let c = condense(&ev, 5000);
        assert!(c.starts_with("[start] task: T"));
        assert!(c.trim_end().ends_with("[finish] {\"kind\":\"done\"}"));
        assert!(c.contains("elided"));
        assert!(c.len() < 6000);
    }
}
