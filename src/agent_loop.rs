//! The session loop (section 5 of the PRD). One task per running session.
//!
//! ```text
//! load store + state.lua
//! system = prompt.md + protocol + index(store) + lua state + budget + verify
//! loop: drain bus -> compile ctx -> llm -> parse -> exec/done/text -> effects
//!       -> compact at 70 % -> persist -> exhausted?
//! ```
//!
//! The module is `agent_loop` because `loop` is a keyword.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::budget::Budget;
use crate::compact;
use crate::config::Harness;
use crate::llm::{ChatMessage, ChatRequest, LlmError, LlmRegistry, Role};
use crate::parse::{self, Action};
use crate::repl::{bindings::REFERENCE, Effects, Repl, ReplCtx};
use crate::sandbox::{Policy, Sandbox};
use crate::session::state::Session;
use crate::session::{now_rfc3339, Ctl, Outcome, SessionInfo, SessionState, TreeHandle};
use crate::store::Store;

/// Shared, immutable dependencies of every loop.
pub struct LoopDeps {
    pub store: Arc<Store>,
    pub llm: Arc<LlmRegistry>,
    pub harness: Arc<Harness>,
    pub sandbox: Arc<dyn Sandbox>,
    pub tree: TreeHandle,
}

impl std::fmt::Debug for LoopDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopDeps").finish_non_exhaustive()
    }
}

const PROTOCOL: &str = r#"## Runtime protocol (fixed)
You drive a persistent Lua 5.4 REPL. Every turn, reply with exactly ONE fenced ```lua block; it is executed and its
output (prints and returned values, max 8 KB) comes back as the next message. Prose outside the block is ignored.
Your Lua globals persist across turns and across restarts. Define helpers once and reuse them; keep big data in
variables instead of re-reading it. Output over 8 KB is truncated and kept in `_last`.
Finish with `done("summary")` inside the block: it is accepted only after the minimum budget AND a passing
`verify()`. If refused, keep improving: more tests, edge cases, refactors, notes for your future self.
Messages from the human, your parent, your children or siblings are injected between turns; read them.
Split parallelizable work across sub-agents with `agent.spawn`; they report back when done.

## Bindings
"#;

fn build_system(deps: &LoopDeps, session: &Session, budget: &Budget, repl: &Repl) -> String {
    let store = &deps.store;
    let prompt = store.prompt_get().unwrap_or_default();
    let index = store.index().map(|i| i.render()).unwrap_or_default();
    let mut s = String::with_capacity(8 * 1024);
    s.push_str(prompt.trim());
    s.push_str("\n\n");
    s.push_str(PROTOCOL);
    s.push_str(REFERENCE);
    s.push_str("\n\n## Session\n");
    s.push_str(&format!(
        "id: {} | name: {} | parent: {} | model: {} | workspace: {}\n",
        session.meta.id,
        session.meta.name,
        session
            .meta
            .parent
            .as_ref()
            .map_or("none (root)".to_string(), ToString::to_string),
        session.meta.model,
        session.meta.workspace.display()
    ));
    s.push_str(&format!("### Task\n{}\n\n", session.meta.task.trim()));
    s.push_str("## Store\n");
    s.push_str(&index);
    let size = repl.state_size();
    s.push_str(&format!("\n## Lua state: {size} bytes serialized"));
    let globals = repl.globals_summary();
    if !globals.is_empty() {
        let top: Vec<String> = globals
            .iter()
            .take(12)
            .map(|(n, b)| format!("{n} ({b} B)"))
            .collect();
        s.push_str(&format!("; largest globals: {}", top.join(", ")));
    }
    if size > 256 * 1024 {
        s.push_str(
            "\nState is large: garbage-collect what you no longer need (set globals to nil).",
        );
    }
    s.push_str("\n\n## Budget\n");
    s.push_str(&budget.status_line());
    s.push('\n');
    match &session.meta.last_verify {
        Some(v) => s.push_str(&format!(
            "last verify: {}\n",
            if v.ok { "OK" } else { "FAILED" }
        )),
        None => s.push_str("last verify: never run\n"),
    }
    if !session.meta.children.is_empty() {
        s.push_str(&format!(
            "children: {}\n",
            session
                .meta
                .children
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    s
}

/// Alternate roles strictly (providers reject two consecutive user turns).
fn compile_messages(session: &Session) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(session.history.len());
    for t in &session.history {
        match out.last_mut() {
            Some(last) if last.role == t.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&t.content);
            }
            _ => out.push(ChatMessage {
                role: t.role,
                content: t.content.clone(),
            }),
        }
    }
    if out.first().map(|m| m.role) == Some(Role::Assistant) {
        out.insert(0, ChatMessage::user("(session resumed)"));
    }
    out
}

fn make_repl(deps: &Arc<LoopDeps>, session: &Session) -> Result<Repl, String> {
    let h = &deps.harness;
    let policy = Policy::for_session(&h.sandbox, &session.meta.workspace, deps.store.root());
    policy.validate(deps.store.root())?;
    let ctx = ReplCtx {
        session_id: session.meta.id.clone(),
        session_name: session.meta.name.clone(),
        session_budget: session.meta.budget,
        workspace: session.meta.workspace.clone(),
        store: deps.store.clone(),
        policy,
        sandbox: deps.sandbox.clone(),
        tree: deps.tree.clone(),
        llm: deps.llm.clone(),
        model: Mutex::new(session.meta.model.clone()),
        temperature: h.model.temperature,
        max_output_tokens: h.model.max_output_tokens,
        verify_cfg: h.verify.clone(),
        evals_dir: deps.store.evals_dir(),
        rt: tokio::runtime::Handle::current(),
        effects: Mutex::new(Effects::default()),
        out: Mutex::new(String::new()),
    };
    let repl = Repl::new(ctx).map_err(|e| e.to_string())?;
    if let Some(src) = Session::saved_state(&deps.store, &session.meta.id) {
        if let Err(e) = repl.restore(&src) {
            tracing::warn!(session = %session.meta.id, "state.lua restore failed: {e}");
        }
    }
    Ok(repl)
}

fn log(store: &Store, session: &Session, event: serde_json::Value) {
    let mut e = event;
    if let serde_json::Value::Object(m) = &mut e {
        m.insert("ts".into(), json!(now_rfc3339()));
        m.insert("turn".into(), json!(session.meta.counters.turns));
    }
    if let Err(err) = store.append_trajectory(session.meta.id.as_str(), &e) {
        tracing::warn!("trajectory write failed: {err}");
    }
}

/// Run a session until it finishes, is paused, killed, or exhausted.
pub async fn run(
    mut session: Box<Session>,
    deps: Arc<LoopDeps>,
    mut ctl: mpsc::Receiver<Ctl>,
    info_tx: watch::Sender<SessionInfo>,
) -> (Box<Session>, Outcome) {
    let outcome = run_inner(&mut session, &deps, &mut ctl, &info_tx).await;
    session.meta.outcome = Some(outcome.clone());
    log(
        &deps.store,
        &session,
        json!({"kind": "finish", "outcome": outcome}),
    );
    if let Err(e) = session.save(&deps.store) {
        tracing::error!(session = %session.meta.id, "final save failed: {e}");
    }
    let _ = info_tx.send(session.info(SessionState::Idle));
    (session, outcome)
}

async fn run_inner(
    session: &mut Session,
    deps: &Arc<LoopDeps>,
    ctl: &mut mpsc::Receiver<Ctl>,
    info_tx: &watch::Sender<SessionInfo>,
) -> Outcome {
    let store = deps.store.clone();
    if session.repl.is_none() {
        match make_repl(deps, session) {
            Ok(r) => session.repl = Some(r),
            Err(e) => return Outcome::Error { message: e },
        }
    }
    let mut budget = Budget::resume(session.meta.budget, session.meta.counters);
    if session.history.is_empty() {
        session.push_turn(
            Role::User,
            format!(
                "Task:\n{}\n\nStart now. First turn: inspect the workspace and plan.",
                session.meta.task
            ),
        );
        log(
            &store,
            session,
            json!({"kind": "start", "task": session.meta.task, "model": session.meta.model.to_string()}),
        );
    } else {
        log(&store, session, json!({"kind": "resume"}));
    }
    let mut consecutive_errors = 0u32;
    let window = f64::from(deps.harness.model.context_window);
    let threshold = f64::from(deps.harness.compact.threshold);

    loop {
        // 1. control
        match ctl.try_recv() {
            Ok(Ctl::Pause) => return Outcome::Paused,
            Ok(Ctl::Kill) => return Outcome::Killed,
            Err(_) => {}
        }
        // 2. bus
        if let Ok(msgs) = deps.tree.drain(session.meta.id.clone()).await {
            for m in msgs {
                log(
                    &store,
                    session,
                    json!({"kind": "message", "from": m.from_name, "body": m.body}),
                );
                session.push_turn(Role::User, m.render());
            }
        }
        if session.history.last().map(|t| t.role) == Some(Role::Assistant) {
            session.push_turn(Role::User, "(resumed; continue)");
        }
        // 3. context
        let Some(repl) = session.repl.as_ref() else {
            return Outcome::Error {
                message: "repl vanished".into(),
            };
        };
        let system = build_system(deps, session, &budget, repl);
        let req = ChatRequest {
            model: session.meta.model.name.clone(),
            system,
            messages: compile_messages(session),
            temperature: deps.harness.model.temperature,
            max_tokens: deps.harness.model.max_output_tokens,
        };
        // 4. llm
        let resp = tokio::select! {
            r = deps.llm.complete(&session.meta.model, &req) => r,
            Some(c) = ctl.recv() => {
                return match c { Ctl::Pause => Outcome::Paused, Ctl::Kill => Outcome::Killed };
            }
        };
        let resp = match resp {
            Ok(r) => {
                consecutive_errors = 0;
                r
            }
            Err(e) => {
                consecutive_errors += 1;
                tracing::error!(session = %session.meta.id, "llm error ({consecutive_errors}): {e}");
                log(
                    &store,
                    session,
                    json!({"kind": "llm_error", "error": e.to_string()}),
                );
                if consecutive_errors >= 3 || !recoverable(&e) {
                    return Outcome::Error {
                        message: e.to_string(),
                    };
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        budget.tick(resp.usage.total());
        session.meta.counters = budget.freeze();
        session.push_turn(Role::Assistant, resp.text.clone());
        log(
            &store,
            session,
            json!({"kind": "assistant", "text": resp.text, "usage": resp.usage, "estimated": resp.usage_estimated, "stop": resp.stop_reason}),
        );

        // 5. act
        let mut done_request: Option<String> = None;
        match parse::parse(&resp.text) {
            Action::Exec(code) => {
                let Some(repl) = session.repl.as_ref() else {
                    return Outcome::Error {
                        message: "repl vanished".into(),
                    };
                };
                let result = tokio::task::block_in_place(|| repl.exec(&code));
                log(
                    &store,
                    session,
                    json!({"kind": "exec", "code": code, "output": result.output, "error": result.error, "truncated": result.truncated}),
                );
                let mut feedback = result.render();
                let effects = repl.take_effects();
                feedback = apply_effects(
                    session,
                    deps,
                    &mut budget,
                    effects,
                    feedback,
                    &mut done_request,
                )
                .await;
                session.push_turn(Role::User, feedback);
            }
            Action::Done(summary) => done_request = Some(summary),
            Action::Text(_) => {
                session.push_turn(Role::User, "No code block found. Reply with exactly one ```lua block (or done(\"summary\") inside it).");
                log(&store, session, json!({"kind": "text_bounced"}));
            }
        }

        // 6. done?
        if let Some(summary) = done_request {
            match try_finish(session, deps, &mut budget, summary).await {
                Ok(summary) => return Outcome::Done { summary },
                Err(refusal) => {
                    log(
                        &store,
                        session,
                        json!({"kind": "done_refused", "reason": refusal}),
                    );
                    session.push_turn(Role::User, refusal);
                }
            }
        }

        // 7. compaction
        let used = f64::from(u32::try_from(resp.usage.input_tokens).unwrap_or(u32::MAX));
        if used > threshold * window {
            compact_now(session, deps, "").await;
        }

        // 8. persist + publish
        session.meta.counters = budget.freeze();
        if let Err(e) = session.save(&store) {
            tracing::error!(session = %session.meta.id, "save failed: {e}");
        }
        let _ = info_tx.send(session.info(SessionState::Running));

        // 9. exhausted?
        if let Some(reason) = budget.exhausted() {
            log(
                &store,
                session,
                json!({"kind": "exhausted", "reason": reason}),
            );
            return Outcome::Exhausted { reason };
        }
    }
}

fn recoverable(e: &LlmError) -> bool {
    !matches!(
        e,
        LlmError::MissingApiKey { .. } | LlmError::UnknownProvider(_)
    )
}

/// Fold the effects of one exec into the session; returns the feedback text.
async fn apply_effects(
    session: &mut Session,
    deps: &Arc<LoopDeps>,
    budget: &mut Budget,
    effects: Effects,
    mut feedback: String,
    done_request: &mut Option<String>,
) -> String {
    let store = &deps.store;
    for n in effects.notes {
        log(store, session, json!({"kind": "note", "text": n}));
        session.meta.last_note = Some(n);
    }
    if effects.spawned > 0 {
        session.meta.spawned += effects.spawned;
        if let Ok(list) = deps.tree.list().await {
            session.meta.children = list
                .into_iter()
                .filter(|s| s.parent.as_ref() == Some(&session.meta.id))
                .map(|s| s.id)
                .collect();
        }
    }
    if effects.llm_tokens > 0 {
        budget.tick_tokens(effects.llm_tokens);
    }
    if let Some(m) = effects.model_switch {
        log(
            store,
            session,
            json!({"kind": "model_switch", "from": session.meta.model.to_string(), "to": m.to_string()}),
        );
        session.meta.model = m;
    }
    if let Some(v) = effects.verify {
        log(
            store,
            session,
            json!({"kind": "verify", "ok": v.ok, "code": v.code, "seconds": v.seconds}),
        );
        session.meta.last_verify = Some(v);
    }
    if let Some(hint) = effects.compact {
        compact_now(session, deps, &hint).await;
        feedback.push_str("\n[context compacted as requested]");
    }
    if let Some(s) = effects.done {
        *done_request = Some(s);
    }
    feedback
}

/// Budget gate, then a fresh verifier run. Err carries the refusal text.
async fn try_finish(
    session: &mut Session,
    deps: &Arc<LoopDeps>,
    budget: &mut Budget,
    summary: String,
) -> Result<String, String> {
    if let Err(r) = budget.check_done() {
        session.meta.counters = budget.freeze();
        return Err(r.to_string());
    }
    let h = &deps.harness;
    let policy = Policy::for_session(&h.sandbox, &session.meta.workspace, deps.store.root());
    let evals = deps.store.evals_dir();
    let sandbox = deps.sandbox.clone();
    let cfg = h.verify.clone();
    let outcome =
        tokio::task::block_in_place(|| crate::verify::run(&cfg, sandbox.as_ref(), &policy, &evals));
    match outcome {
        Ok(v) => {
            log(
                &deps.store,
                session,
                json!({"kind": "verify", "ok": v.ok, "code": v.code, "seconds": v.seconds, "at_done": true}),
            );
            let ok = v.ok;
            let text = v.summary();
            session.meta.last_verify = Some(v);
            if ok {
                Ok(summary)
            } else {
                Err(format!("done() refused: the verifier failed.\n{text}"))
            }
        }
        Err(e) => Err(format!("done() refused: verifier could not run: {e}")),
    }
}

async fn compact_now(session: &mut Session, deps: &Arc<LoopDeps>, hint: &str) {
    let keep = deps.harness.compact.keep_last;
    let older = compact::split_older(&session.history, keep);
    if older.len() < 2 {
        return;
    }
    let model = session.meta.model.clone();
    let max = deps.harness.model.max_output_tokens.min(4096);
    match compact::summarize(&deps.llm, &model, &session.meta.task, older, hint, max).await {
        Ok(summary) => {
            let before = session.history.len();
            session.history = compact::rebuild(&session.history, &summary, keep);
            session.meta.compactions += 1;
            log(
                &deps.store,
                session,
                json!({"kind": "compact", "turns_before": before, "turns_after": session.history.len(), "summary": summary}),
            );
        }
        Err(e) => {
            tracing::warn!(session = %session.meta.id, "compaction failed: {e}");
            log(
                &deps.store,
                session,
                json!({"kind": "compact_failed", "error": e.to_string()}),
            );
        }
    }
}
