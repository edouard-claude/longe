//! Protocol A: the baseline harness. A plain loop with JSON tools (read, write, sh).
//! No budget, no memory, no sub-agents, no verifier gate. Same model, same sandbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Harness, ModelRef};
use crate::llm::{ChatMessage, ChatRequest, LlmRegistry, Role};
use crate::sandbox::{Policy, Sandbox};
use crate::store::Store;

const SYSTEM: &str = r#"You are an autonomous senior engineer. You work by calling tools.
Reply with exactly ONE JSON object per turn, inside a ```json block, one of:
{"tool":"read","path":"relative/path"}
{"tool":"write","path":"relative/path","content":"full file content"}
{"tool":"sh","cmd":"shell command run in the workspace"}
{"tool":"done","summary":"what you built and how you verified it"}
The tool result comes back as the next message. Paths are relative to the workspace. Think briefly before the block if useful."#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineReport {
    pub label: String,
    pub model: ModelRef,
    pub turns: u32,
    pub tokens: u64,
    pub seconds: u64,
    pub finished: bool,
    pub summary: Option<String>,
    pub tool_calls: u32,
    pub bad_turns: u32,
}

fn extract_tool(text: &str) -> Option<Value> {
    crate::reflect::extract_json(text)
}

fn resolve(ws: &Path, p: &str) -> Result<PathBuf, String> {
    crate::repl::bindings::fs_resolve(ws, p)
}

/// What a baseline run needs.
pub struct BaselineArgs<'a> {
    pub task: &'a str,
    pub workspace: &'a Path,
    pub model: ModelRef,
    pub max_turns: u32,
    pub label: &'a str,
}

pub async fn run(
    store: &Store,
    harness: &Harness,
    llm: Arc<LlmRegistry>,
    sandbox: Arc<dyn Sandbox>,
    args: BaselineArgs<'_>,
) -> anyhow::Result<BaselineReport> {
    let BaselineArgs {
        task,
        workspace,
        model,
        max_turns,
        label,
    } = args;
    let start = Instant::now();
    let policy = Policy::for_session(&harness.sandbox, workspace, store.root());
    let mut history: Vec<ChatMessage> =
        vec![ChatMessage::user(format!("Task:\n{task}\n\nStart now."))];
    let mut report = BaselineReport {
        label: label.to_string(),
        model: model.clone(),
        turns: 0,
        tokens: 0,
        seconds: 0,
        finished: false,
        summary: None,
        tool_calls: 0,
        bad_turns: 0,
    };
    let traj = format!("baseline-{label}");
    store.append_trajectory(
        &traj,
        &json!({"kind": "start", "task": task, "model": model.to_string()}),
    )?;
    while report.turns < max_turns {
        let req = ChatRequest {
            model: model.name.clone(),
            system: SYSTEM.into(),
            messages: history.clone(),
            temperature: harness.model.temperature,
            max_tokens: harness.model.max_output_tokens,
        };
        let resp = llm.complete(&model, &req).await?;
        report.turns += 1;
        report.tokens += resp.usage.total();
        history.push(ChatMessage::assistant(resp.text.clone()));
        store.append_trajectory(&traj, &json!({"kind": "assistant", "turn": report.turns, "text": resp.text, "usage": resp.usage}))?;
        let feedback = match extract_tool(&resp.text) {
            None => {
                report.bad_turns += 1;
                "No tool call found. Reply with one JSON object in a ```json block.".to_string()
            }
            Some(call) => {
                report.tool_calls += 1;
                let tool = call["tool"].as_str().unwrap_or("");
                match tool {
                    "read" => match resolve(workspace, call["path"].as_str().unwrap_or("")) {
                        Ok(p) => {
                            std::fs::read_to_string(&p).unwrap_or_else(|e| format!("error: {e}"))
                        }
                        Err(e) => format!("error: {e}"),
                    },
                    "write" => match resolve(workspace, call["path"].as_str().unwrap_or("")) {
                        Ok(p) => {
                            if let Some(parent) = p.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&p, call["content"].as_str().unwrap_or("")) {
                                Ok(()) => "written".into(),
                                Err(e) => format!("error: {e}"),
                            }
                        }
                        Err(e) => format!("error: {e}"),
                    },
                    "sh" => {
                        let cmd = call["cmd"].as_str().unwrap_or("").to_string();
                        let sb = sandbox.clone();
                        let pol = policy.clone();
                        match tokio::task::spawn_blocking(move || sb.run(&pol, &cmd)).await? {
                            Ok(o) => format!(
                                "exit {}\n{}{}",
                                o.code,
                                o.stdout,
                                if o.stderr.is_empty() {
                                    String::new()
                                } else {
                                    format!("\n--- stderr ---\n{}", o.stderr)
                                }
                            ),
                            Err(e) => format!("error: {e}"),
                        }
                    }
                    "done" => {
                        report.finished = true;
                        report.summary = Some(call["summary"].as_str().unwrap_or("").to_string());
                        break;
                    }
                    other => {
                        report.bad_turns += 1;
                        format!("unknown tool `{other}`")
                    }
                }
            }
        };
        let feedback = crate::verify::tail_bytes(&feedback, 8 * 1024);
        store.append_trajectory(
            &traj,
            &json!({"kind": "tool_result", "turn": report.turns, "text": feedback}),
        )?;
        history.push(ChatMessage::user(feedback));
        // Keep the context bounded the crude way: drop the oldest tool exchanges.
        let est: u64 = history
            .iter()
            .map(|m| crate::llm::provider::estimate_tokens(&m.content))
            .sum();
        let limit = u64::from(harness.model.context_window)
            .saturating_mul(7)
            .div_euclid(10);
        if est > limit && history.len() > 6 {
            history.drain(1..3);
        }
        debug_assert!(history.first().is_some_and(|m| m.role == Role::User));
    }
    report.seconds = start.elapsed().as_secs();
    store.append_trajectory(&traj, &json!({"kind": "finish", "report": report}))?;
    Ok(report)
}
