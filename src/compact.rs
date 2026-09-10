//! L1 compaction: replace the older history with a structured summary, keep the last
//! turns verbatim. The original task never enters the summary path: it lives in the
//! system prompt.

use crate::config::ModelRef;
use crate::llm::{ChatMessage, ChatRequest, LlmError, LlmRegistry, Role};
use crate::session::state::Turn;

pub const SUMMARY_PREFIX: &str = "[compacted context]";

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

/// Ask the model for a structured summary of `older`.
pub async fn summarize(
    llm: &LlmRegistry,
    model: &ModelRef,
    task: &str,
    older: &[Turn],
    hint: &str,
    max_tokens: u32,
) -> Result<String, LlmError> {
    let transcript = render_turns(older);
    let hint_line = if hint.trim().is_empty() {
        String::new()
    } else {
        format!("\nThe agent asked to keep in mind: {hint}\n")
    };
    let prompt = format!(
        "You are compacting the working memory of an autonomous coding agent so it can continue.\n\
         The task (already known to the agent, do not restate it in full): {task}\n{hint_line}\n\
         Write a dense, factual summary with these sections, markdown headings, no fluff:\n\
         1. Progress so far (what exists, what works, verified how)\n\
         2. Workspace state (files/modules created, their roles, key functions and data structures)\n\
         3. Lua state (globals and helpers the agent defined and relies on)\n\
         4. Open problems and failing cases (exact errors, failing test names)\n\
         5. Decisions and lessons (what was tried and rejected, gotchas)\n\
         6. Next steps (concrete, ordered)\n\
         Keep every identifier, path, command and number that would be costly to rediscover.\n\n\
         TRANSCRIPT:\n{transcript}"
    );
    let req = ChatRequest {
        model: model.name.clone(),
        system: "You compress agent transcripts into precise working notes.".into(),
        messages: vec![ChatMessage::user(prompt)],
        temperature: 0.0,
        max_tokens,
    };
    let resp = llm.complete(model, &req).await?;
    Ok(crate::parse::strip_thinking(&resp.text).trim().to_string())
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
