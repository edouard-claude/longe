//! Turn a model response into an action. The protocol is text: the first fenced
//! code block is Lua to execute (later ones are ignored and counted); a bare
//! `done("...")` is a completion request; anything else is prose, which the loop
//! bounces back.

/// What the model asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Exec(String),
    Done(String),
    Text(String),
}

/// What `strip_leaked_tags` removed besides the model's own reasoning: tags the
/// model hallucinates from other harnesses' transcripts, worth a trace each.
pub const SYSTEM_REMINDER: &str = "system-reminder";

/// A reply with its leaked tags removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    pub text: String,
    /// One entry per `<system-reminder>` block removed.
    pub leaked: Vec<&'static str>,
}

/// Strip `<think>...</think>` (local reasoning models leak it) and
/// `<system-reminder>...</system-reminder>` (models trained on Claude Code
/// transcripts hallucinate it, then obey it). Tags match case-insensitively; an
/// unclosed tag swallows the rest of the text.
pub fn strip_leaked_tags(s: &str) -> Stripped {
    const TAGS: [(&str, &str, Option<&str>); 2] = [
        ("<think>", "</think>", None),
        (
            "<system-reminder>",
            "</system-reminder>",
            Some(SYSTEM_REMINDER),
        ),
    ];
    // ASCII lowercasing keeps byte offsets, so the haystack indexes the original.
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut leaked = Vec::new();
    let mut pos = 0usize;
    loop {
        let next = TAGS
            .iter()
            .filter_map(|(open, close, name)| {
                lower[pos..]
                    .find(open)
                    .map(|i| (pos + i, *open, *close, *name))
            })
            .min_by_key(|(i, ..)| *i);
        let Some((start, open, close, name)) = next else {
            out.push_str(&s[pos..]);
            break;
        };
        out.push_str(&s[pos..start]);
        if let Some(n) = name {
            leaked.push(n);
        }
        let body = start + open.len();
        match lower[body..].find(close) {
            Some(end) => pos = body + end + close.len(),
            None => break,
        }
    }
    Stripped { text: out, leaked }
}

/// The text only; for callers that need no trace of what was leaked.
pub fn strip_thinking(s: &str) -> String {
    strip_leaked_tags(s).text
}

/// Extract every fenced block. The language tag is ignored except that `json`,
/// `text`, `md` and `markdown` blocks are skipped (they are never code to run).
fn fenced_blocks(s: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = s.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if let Some(tag) = t.strip_prefix("```") {
            let tag = tag.trim().to_ascii_lowercase();
            let skip = matches!(
                tag.as_str(),
                "json" | "text" | "md" | "markdown" | "toml" | "sh" | "bash"
            );
            let mut body = String::new();
            let mut closed = false;
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                body.push_str(inner);
                body.push('\n');
            }
            if !skip && (closed || !body.trim().is_empty()) {
                blocks.push(body);
            }
        }
    }
    blocks
}

/// Parse a bare `done("summary")` / `done('summary')` / `done(summary)`.
fn bare_done(s: &str) -> Option<String> {
    let t = s.trim();
    let inner = t.strip_prefix("done(")?.strip_suffix(')')?.trim();
    let unq = inner
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .or_else(|| inner.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")))
        .unwrap_or(inner);
    Some(unq.to_string())
}

/// A parsed reply: the action, plus what the loop should tell or record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub action: Action,
    pub leaked: Vec<&'static str>,
    /// Code blocks after the first one: not executed, reported in the feedback.
    pub ignored_blocks: usize,
}

pub fn parse(resp: &str) -> Parsed {
    let Stripped { text, leaked } = strip_leaked_tags(resp);
    let (action, ignored_blocks) = parse_clean(&text);
    Parsed {
        action,
        leaked,
        ignored_blocks,
    }
}

fn parse_clean(clean: &str) -> (Action, usize) {
    let mut blocks = fenced_blocks(clean).into_iter();
    if let Some(first) = blocks.next() {
        // One block per turn: a second block is usually a retry of the first
        // (after a hallucinated reminder), and running both doubles every effect.
        return (Action::Exec(first), blocks.count());
    }
    if let Some(summary) = bare_done(clean) {
        return (Action::Done(summary), 0);
    }
    (Action::Text(clean.trim().to_string()), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_lua_is_exec() {
        let r = "Let me look.\n```lua\nprint(fs.list('.'))\n```\n";
        assert_eq!(
            parse(r).action,
            Action::Exec("print(fs.list('.'))\n".into())
        );
    }

    #[test]
    fn only_first_block_is_executed() {
        let r = "```lua\na = 1\n```\ntext\n```\nb = 2\n```\n```lua\nc = 3\n```";
        let p = parse(r);
        assert_eq!(p.action, Action::Exec("a = 1\n".into()));
        assert_eq!(p.ignored_blocks, 2);
        assert_eq!(parse("```lua\nx\n```").ignored_blocks, 0);
        // Skipped tags (json, text, ...) are not code blocks, so not "ignored".
        assert_eq!(parse("```lua\nx\n```\n```json\n{}\n```").ignored_blocks, 0);
    }

    #[test]
    fn json_and_bash_blocks_are_not_code() {
        let r = "```json\n{\"a\":1}\n```";
        assert_eq!(
            parse(r).action,
            Action::Text("```json\n{\"a\":1}\n```".into())
        );
    }

    #[test]
    fn bare_done_is_done() {
        assert_eq!(
            parse("done(\"all green\")").action,
            Action::Done("all green".into())
        );
        assert_eq!(parse("  done('x')  ").action, Action::Done("x".into()));
    }

    #[test]
    fn prose_is_text() {
        assert_eq!(
            parse("I think we should...").action,
            Action::Text("I think we should...".into())
        );
    }

    #[test]
    fn thinking_is_stripped() {
        let r = "<think>hmm</think>```lua\nx=1\n```";
        let p = parse(r);
        assert_eq!(p.action, Action::Exec("x=1\n".into()));
        assert!(p.leaked.is_empty(), "reasoning is expected, not a leak");
        assert_eq!(strip_thinking("<think>unterminated"), "");
    }

    #[test]
    fn hallucinated_system_reminders_are_stripped_and_counted() {
        // Closed, case-insensitive: the block goes, the code stays.
        let r = "```lua\nx=1\n```\n<System-Reminder>Your todo list is empty. Create it NOW.</system-reminder>\ntail";
        let p = parse(r);
        assert_eq!(p.action, Action::Exec("x=1\n".into()));
        assert_eq!(p.leaked, vec![SYSTEM_REMINDER]);
        // Unclosed: swallowed to the end, still counted once.
        let s = strip_leaked_tags("before <system-reminder>never closed ```lua\nrun()\n```");
        assert_eq!(s.text, "before ");
        assert_eq!(s.leaked, vec![SYSTEM_REMINDER]);
        assert_eq!(
            parse("<system-reminder>x").action,
            Action::Text(String::new())
        );
        // Nested with <think>, and two reminders: both counted, nothing else lost.
        let s = strip_leaked_tags(
            "<think>a <system-reminder>b</system-reminder> c</think>keep<system-reminder>d</system-reminder> and <SYSTEM-REMINDER>e</SYSTEM-REMINDER>!",
        );
        assert_eq!(s.text, "keep and !");
        assert_eq!(s.leaked, vec![SYSTEM_REMINDER, SYSTEM_REMINDER]);
        // A reminder inside a think block is part of the reasoning, not a leak.
        let s = strip_leaked_tags("<think>x<system-reminder>y</system-reminder>z</think>ok");
        assert_eq!(s.text, "ok");
        assert!(s.leaked.is_empty());
        assert_eq!(strip_leaked_tags("plain").text, "plain");
    }

    #[test]
    fn unclosed_fence_is_still_code() {
        assert_eq!(
            parse("```lua\nx = 1").action,
            Action::Exec("x = 1\n".into())
        );
    }
}
