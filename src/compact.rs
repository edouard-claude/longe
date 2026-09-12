//! L1 compaction: replace the older history with a structured summary, keep the last
//! turns verbatim. The original task never enters the summary path: it lives in the
//! system prompt.

use std::path::Path;

use crate::config::ModelRef;
use crate::llm::{ChatMessage, ChatRequest, LlmError, LlmRegistry, Role};
use crate::session::state::{Note, Turn};
use crate::verify::VerifyOutcome;

pub const SUMMARY_PREFIX: &str = "[compacted context]";

/// Output limit of the compaction call: a summary is short by construction.
pub const MAX_TOKENS: u32 = 4096;
/// Word targets of the first prompt and of the retry after a truncated summary.
const WORDS: u32 = 1200;
const WORDS_SHORT: u32 = 600;
/// Lines of the last verifier report and of `git status` handed to the compactor.
const VERIFY_LINES: usize = 20;
const STATUS_LINES: usize = 40;

/// What the runtime knows for certain at compaction time. Handed to the model so
/// the summary does not guess at it (a summary once claimed that Lua helpers do
/// not persist across turns, and the agent believed it).
#[derive(Debug, Clone, Default)]
pub struct RuntimeFacts {
    /// Live Lua globals with their serialized size.
    pub globals: Vec<(String, usize)>,
    pub last_verify: Option<VerifyOutcome>,
    pub notes: Vec<Note>,
    /// `git status --short` of the workspace; `None` when it is not a repository.
    pub changed_files: Option<String>,
}

/// `git status --short` of `ws`, capped, or `None` outside a repository.
pub fn git_status_short(ws: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &ws.to_string_lossy(), "status", "--short"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let mut s = lines
        .iter()
        .take(STATUS_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > STATUS_LINES {
        s.push_str(&format!("\n... and {} more", lines.len() - STATUS_LINES));
    }
    Some(s)
}

/// The facts as a prompt section.
pub fn facts_preamble(f: &RuntimeFacts) -> String {
    let mut s = String::from(
        "## Runtime facts (known to the runtime, they persist: do not describe them)\n",
    );
    if f.globals.is_empty() {
        s.push_str("Lua globals alive: none\n");
    } else {
        let list: Vec<String> = f
            .globals
            .iter()
            .map(|(n, b)| format!("{n} ({b} B)"))
            .collect();
        s.push_str(&format!(
            "Lua globals alive (they survive turns and restarts): {}\n",
            list.join(", ")
        ));
    }
    match &f.last_verify {
        None => s.push_str("Last verify(): never run\n"),
        Some(v) => {
            let head: Vec<&str> = v.report.lines().take(VERIFY_LINES).collect();
            s.push_str(&format!(
                "Last verify(): {} (exit {}, {}s); report head:\n{}\n",
                if v.ok { "OK" } else { "FAILED" },
                v.code,
                v.seconds,
                head.join("\n")
            ));
        }
    }
    if f.notes.is_empty() {
        s.push_str("Notes: none\n");
    } else {
        s.push_str("Notes (shown to the agent every turn):\n");
        for n in &f.notes {
            s.push_str(&format!("- t{}: {}\n", n.turn, n.text.replace('\n', " ")));
        }
    }
    match &f.changed_files {
        None => s.push_str("Workspace: not a git repository\n"),
        Some(st) if st.trim().is_empty() => s.push_str("Workspace (git status --short): clean\n"),
        Some(st) => s.push_str(&format!("Workspace (git status --short):\n{st}\n")),
    }
    s
}

fn render_turns(turns: &[Turn]) -> String {
    let mut s = String::new();
    for t in turns {
        let who = match t.role {
            Role::User => "RUNTIME",
            Role::Assistant => "AGENT",
        };
        s.push_str(&format!("### {who}\n{}\n\n", t.content));
    }
    s
}

fn prompt(task: &str, hint: &str, facts: &str, transcript: &str, words: u32) -> String {
    let hint_line = if hint.trim().is_empty() {
        String::new()
    } else {
        format!("\nThe agent asked to keep in mind: {hint}\n")
    };
    format!(
        "You are compacting the working memory of an autonomous coding agent so it can continue.\n\
         The task (already known to the agent, do not restate it in full): {task}\n{hint_line}\n\
         {facts}\n\
         Write a dense, factual summary under {words} words with these sections, markdown headings, no fluff:\n\
         1. Progress so far (what exists, what works, verified how)\n\
         2. Workspace state (files/modules created, their roles, key functions and data structures)\n\
         3. Open problems and failing cases (exact errors, failing test names)\n\
         4. Decisions and lessons (what was tried and rejected, gotchas)\n\
         5. Next steps (concrete, ordered)\n\
         Do not describe the runtime or the Lua state: they are given above and persist. \
         Summarize decisions, errors seen and next steps only.\n\
         Keep every identifier, path, command and number that would be costly to rediscover.\n\n\
         TRANSCRIPT:\n{transcript}"
    )
}

/// A compaction summary and how it was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    /// The first call hit `max_tokens`; this text comes from the shorter retry.
    pub shortened: bool,
}

/// Ask the model for a structured summary of `older`. A summary cut at
/// `MAX_TOKENS` is useless (it ends mid-list), so it is asked again, shorter.
pub async fn summarize(
    llm: &LlmRegistry,
    model: &ModelRef,
    task: &str,
    older: &[Turn],
    hint: &str,
    facts: &RuntimeFacts,
) -> Result<Summary, LlmError> {
    let transcript = render_turns(older);
    let preamble = facts_preamble(facts);
    let ask = |words: u32| ChatRequest {
        model: model.name.clone(),
        system: "You compress agent transcripts into precise working notes.".into(),
        messages: vec![ChatMessage::user(prompt(
            task,
            hint,
            &preamble,
            &transcript,
            words,
        ))],
        temperature: 0.0,
        max_tokens: MAX_TOKENS,
    };
    let clean = |text: &str| crate::parse::strip_thinking(text).trim().to_string();
    match llm.complete(model, &ask(WORDS)).await {
        Ok(r) => Ok(Summary {
            text: clean(&r.text),
            shortened: false,
        }),
        Err(LlmError::Truncated { .. }) => {
            let r = llm.complete(model, &ask(WORDS_SHORT)).await?;
            Ok(Summary {
                text: clean(&r.text),
                shortened: true,
            })
        }
        Err(e) => Err(e),
    }
}

/// Build the compacted history from a summary. Pure, so it is unit-testable.
pub fn rebuild(history: &[Turn], summary: &str, keep_last: usize) -> Vec<Turn> {
    let mut keep_from = history.len().saturating_sub(keep_last);
    // The kept tail must start with a runtime (user) turn so roles alternate.
    while keep_from < history.len() && history[keep_from].role != Role::User {
        keep_from += 1;
    }
    let mut out = Vec::with_capacity(history.len() - keep_from + 2);
    out.push(Turn {
        role: Role::User,
        content: format!("{SUMMARY_PREFIX}\n{summary}\n\nContinue from here."),
        ts: crate::session::now_rfc3339(),
    });
    if keep_from < history.len() {
        out.push(Turn {
            role: Role::Assistant,
            content: "Understood. Continuing with the recent turns below.".into(),
            ts: crate::session::now_rfc3339(),
        });
        out.extend_from_slice(&history[keep_from..]);
    }
    out
}

/// Split point: everything before the kept tail is summarized.
pub fn split_older(history: &[Turn], keep_last: usize) -> &[Turn] {
    let mut keep_from = history.len().saturating_sub(keep_last);
    while keep_from < history.len() && history[keep_from].role != Role::User {
        keep_from += 1;
    }
    &history[..keep_from]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: Role, s: &str) -> Turn {
        Turn {
            role,
            content: s.into(),
            ts: String::new(),
        }
    }

    fn history(n: usize) -> Vec<Turn> {
        (0..n)
            .map(|i| {
                turn(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    &format!("t{i}"),
                )
            })
            .collect()
    }

    #[test]
    fn rebuild_keeps_tail_and_alternation() {
        let h = history(12);
        let out = rebuild(&h, "SUMMARY", 5);
        assert!(out[0].content.starts_with(SUMMARY_PREFIX));
        assert_eq!(out[0].role, Role::User);
        assert_eq!(out[1].role, Role::Assistant);
        // keep_last=5 from index 7 (assistant) rounds to 8 (user): t8..t11 kept.
        assert_eq!(out[2].content, "t8");
        assert_eq!(out.last().unwrap().content, "t11");
        assert_eq!(out.len(), 6);
        assert_eq!(split_older(&h, 5).len(), 8);
        for w in out.windows(2) {
            assert_ne!(w[0].role, w[1].role);
        }
    }

    #[test]
    fn preamble_states_globals_verify_notes_and_workspace() {
        let empty = facts_preamble(&RuntimeFacts::default());
        assert!(empty.contains("Lua globals alive: none"));
        assert!(empty.contains("Last verify(): never run"));
        assert!(empty.contains("Notes: none"));
        assert!(empty.contains("not a git repository"));
        let f = RuntimeFacts {
            globals: vec![("walk".into(), 120), ("cache".into(), 4096)],
            last_verify: Some(VerifyOutcome {
                ok: false,
                code: 1,
                report: (1..=30)
                    .map(|i| format!("case {i} failed"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                seconds: 12,
                timed_out: false,
            }),
            notes: vec![Note {
                turn: 34,
                text: "tier1: xxtea\nthen aes".into(),
            }],
            changed_files: Some(" M src/lib.rs\n?? src/crypto/".into()),
        };
        let p = facts_preamble(&f);
        assert!(p.contains("walk (120 B), cache (4096 B)"), "{p}");
        assert!(p.contains("they survive turns and restarts"));
        assert!(p.contains("Last verify(): FAILED (exit 1, 12s)"));
        assert!(
            p.contains("case 20 failed") && !p.contains("case 21 failed"),
            "20 lines"
        );
        assert!(p.contains("- t34: tier1: xxtea then aes"));
        assert!(p.contains("?? src/crypto/"));
        let clean = facts_preamble(&RuntimeFacts {
            changed_files: Some("\n".into()),
            ..RuntimeFacts::default()
        });
        assert!(clean.contains("clean"));
        let text = prompt("T", "", &p, "X", 1200);
        assert!(text.contains("under 1200 words"));
        assert!(text.contains("Do not describe the runtime or the Lua state"));
        assert!(text.contains("walk (120 B)"));
    }

    #[test]
    fn git_status_is_none_outside_a_repository() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(git_status_short(d.path()), None);
    }

    #[test]
    fn rebuild_short_history_is_only_summary() {
        let h = history(2);
        assert_eq!(rebuild(&h, "S", 5).len(), 4, "short history is kept whole");
        assert_eq!(
            rebuild(&h, "S", 0).len(),
            1,
            "keep_last=0 leaves only the summary"
        );
    }
}
