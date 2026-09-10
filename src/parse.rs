//! Turn a model response into an action. The protocol is text: one or more fenced
//! code blocks are Lua to execute; a bare `done("...")` is a completion request;
//! anything else is prose, which the loop bounces back.

/// What the model asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Exec(String),
    Done(String),
    Text(String),
}

/// Strip `<think>...</think>` blocks (local reasoning models leak them).
pub fn strip_thinking(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
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

pub fn parse(resp: &str) -> Action {
    let clean = strip_thinking(resp);
    let blocks = fenced_blocks(&clean);
    if !blocks.is_empty() {
        return Action::Exec(blocks.join("\n"));
    }
    if let Some(summary) = bare_done(&clean) {
        return Action::Done(summary);
    }
    Action::Text(clean.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_lua_is_exec() {
        let r = "Let me look.\n```lua\nprint(fs.list('.'))\n```\n";
        assert_eq!(parse(r), Action::Exec("print(fs.list('.'))\n".into()));
    }

    #[test]
    fn multiple_blocks_are_joined_in_order() {
        let r = "```lua\na = 1\n```\ntext\n```\nb = 2\n```";
        assert_eq!(parse(r), Action::Exec("a = 1\n\nb = 2\n".into()));
    }

    #[test]
    fn json_and_bash_blocks_are_not_code() {
        let r = "```json\n{\"a\":1}\n```";
        assert_eq!(parse(r), Action::Text("```json\n{\"a\":1}\n```".into()));
    }

    #[test]
    fn bare_done_is_done() {
        assert_eq!(
            parse("done(\"all green\")"),
            Action::Done("all green".into())
        );
        assert_eq!(parse("  done('x')  "), Action::Done("x".into()));
    }

    #[test]
    fn prose_is_text() {
        assert_eq!(
            parse("I think we should..."),
            Action::Text("I think we should...".into())
        );
    }

    #[test]
    fn thinking_is_stripped() {
        let r = "<think>hmm</think>```lua\nx=1\n```";
        assert_eq!(parse(r), Action::Exec("x=1\n".into()));
        assert_eq!(strip_thinking("<think>unterminated"), "");
    }

    #[test]
    fn unclosed_fence_is_still_code() {
        assert_eq!(parse("```lua\nx = 1"), Action::Exec("x = 1\n".into()));
    }
}
