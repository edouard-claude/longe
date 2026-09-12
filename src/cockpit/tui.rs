//! The cockpit TUI: session tree, budget, last note, trajectory tail, message box.
//! Polls the daemon over the socket; never holds a session.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;

use crate::client;
use crate::daemon::proto::Request;
use crate::reflect::PendingDiff;
use crate::session::{SessionDetail, SessionId, SessionInfo, SessionState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Sessions,
    Diffs,
}

struct App {
    socket: PathBuf,
    sessions: Vec<SessionInfo>,
    order: Vec<usize>, // indices into `sessions`, tree order
    depth: Vec<usize>,
    selected: usize,
    detail: Option<SessionDetail>,
    diffs: Vec<PendingDiff>,
    diff_selected: usize,
    view: View,
    input: String,
    status: String,
    typing: bool,
}

impl App {
    fn selected_id(&self) -> Option<SessionId> {
        self.order
            .get(self.selected)
            .map(|&i| self.sessions[i].id.clone())
    }

    async fn refresh(&mut self) {
        match client::call(&self.socket, &Request::List).await {
            Ok(v) => {
                if let Ok(list) = serde_json::from_value::<Vec<SessionInfo>>(v) {
                    self.sessions = list;
                    self.rebuild_order();
                }
            }
            Err(e) => self.status = format!("daemon: {e}"),
        }
        if let Some(id) = self.selected_id() {
            if let Ok(v) = client::call(
                &self.socket,
                &Request::Get {
                    id: id.to_string(),
                    tail: 12,
                },
            )
            .await
            {
                self.detail = serde_json::from_value(v).ok();
            }
        } else {
            self.detail = None;
        }
        if self.view == View::Diffs {
            if let Ok(v) = client::call(&self.socket, &Request::Diffs).await {
                self.diffs = serde_json::from_value(v).unwrap_or_default();
                if self.diff_selected >= self.diffs.len() {
                    self.diff_selected = self.diffs.len().saturating_sub(1);
                }
            }
        }
    }

    fn rebuild_order(&mut self) {
        self.order.clear();
        self.depth.clear();
        fn walk(
            app: &App,
            parent: Option<&SessionId>,
            depth: usize,
            out: &mut Vec<(usize, usize)>,
        ) {
            for (i, s) in app.sessions.iter().enumerate() {
                if s.parent.as_ref() == parent {
                    out.push((i, depth));
                    walk(app, Some(&s.id), depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, None, 0, &mut out);
        // Orphans (parent unknown to the daemon) at the end.
        for (i, s) in self.sessions.iter().enumerate() {
            if !out.iter().any(|(j, _)| *j == i) {
                let _ = s;
                out.push((i, 0));
            }
        }
        for (i, d) in out {
            self.order.push(i);
            self.depth.push(d);
        }
        if self.selected >= self.order.len() {
            self.selected = self.order.len().saturating_sub(1);
        }
    }

    async fn act(&mut self, req: Request, label: &str) {
        match client::call(&self.socket, &req).await {
            Ok(_) => self.status = format!("{label}: ok"),
            Err(e) => self.status = format!("{label}: {e}"),
        }
    }
}

/// `41k`, or `41k (+3k reasoning)` when the provider reports reasoning tokens.
fn tokens_k(c: &crate::budget::BudgetCounters) -> String {
    let k = c.tokens.div_euclid(1000);
    if c.reasoning_tokens > 0 {
        format!("{k}k (+{}k reasoning)", c.reasoning_tokens.div_euclid(1000))
    } else {
        format!("{k}k")
    }
}

fn state_style(s: SessionState) -> Style {
    match s {
        SessionState::Running => Style::default().fg(Color::Green),
        SessionState::Idle => Style::default().fg(Color::Yellow),
        SessionState::Paused => Style::default().fg(Color::Magenta),
        SessionState::Offloaded => Style::default().fg(Color::DarkGray),
    }
}

fn draw(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());
    match app.view {
        View::Sessions => draw_sessions(f, app, outer[0]),
        View::Diffs => draw_diffs(f, app, outer[0]),
    }
    let input_title = if app.typing {
        " message (Enter to send, Esc to cancel) "
    } else {
        " message: press i to type "
    };
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(input_title));
    f.render_widget(input, outer[1]);
    let help = if app.view == View::Sessions {
        "↑↓ select  i message  p pause  r resume  k kill  o offload  R reflect  d diffs  q quit"
    } else {
        "↑↓ select  a accept  x reject  d sessions  q quit"
    };
    let bar = Line::from(vec![
        Span::styled(help, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(&app.status, Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(bar), outer[2]);
}

fn draw_sessions(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    let items: Vec<ListItem> = app
        .order
        .iter()
        .zip(&app.depth)
        .map(|(&i, &d)| {
            let s = &app.sessions[i];
            let pad = "  ".repeat(d);
            let outcome = s
                .outcome
                .as_ref()
                .map(|o| format!(" [{o}]"))
                .unwrap_or_default();
            let line = Line::from(vec![
                Span::raw(format!("{pad}{} ", s.id)),
                Span::styled(
                    format!("{:?}", s.state).to_lowercase(),
                    state_style(s.state),
                ),
                Span::raw(format!(
                    " {} t{} {}{}",
                    s.name,
                    s.counters.turns,
                    tokens_k(&s.counters),
                    outcome
                )),
            ]);
            ListItem::new(line)
        })
        .collect();
    let mut state = ListState::default();
    state.select(if app.order.is_empty() {
        None
    } else {
        Some(app.selected)
    });
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" sessions ({}) ", app.sessions.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, cols[0], &mut state);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(3)])
        .split(cols[1]);
    let (detail, tail) = match &app.detail {
        Some(d) => (detail_lines(d), d.tail.iter().map(event_line).collect()),
        None => (
            vec![Line::from(Span::styled(
                "no session selected",
                Style::default().fg(Color::DarkGray),
            ))],
            Vec::new(),
        ),
    };
    // Fixed-height header: truncate each line so none of them wraps out of view.
    let inner = right[0].width.saturating_sub(2);
    let detail: Vec<Line> = detail
        .into_iter()
        .map(|l| truncate_line(l, inner))
        .collect();
    f.render_widget(
        Paragraph::new(detail).block(Block::default().borders(Borders::ALL).title(" detail ")),
        right[0],
    );
    // Keep only the last events that fit once wrapped, so the newest is always the
    // last visible row. Counting logical lines here would undercount: one event can
    // wrap over several rows, which is what used to push the newest ones out of view.
    let tail = fit_tail(
        tail,
        right[1].width.saturating_sub(2),
        right[1].height.saturating_sub(2),
    );
    f.render_widget(
        Paragraph::new(tail).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" trajectory tail "),
        ),
        right[1],
    );
}

/// Rows a line occupies once wrapped into `width` columns.
fn wrapped_rows(width_chars: usize, width: u16) -> u16 {
    let w = usize::from(width.max(1));
    u16::try_from(width_chars.max(1).div_ceil(w)).unwrap_or(u16::MAX)
}

fn line_chars(l: &Line<'_>) -> usize {
    l.spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Cut a line to `width` columns, span by span, so a fixed-height pane cannot wrap.
fn truncate_line(l: Line<'static>, width: u16) -> Line<'static> {
    let max = usize::from(width);
    let mut used = 0usize;
    let mut out: Vec<Span<'static>> = Vec::with_capacity(l.spans.len());
    for span in l.spans {
        let n = span.content.chars().count();
        if used + n <= max {
            used += n;
            out.push(span);
        } else {
            let room = max.saturating_sub(used);
            if room > 1 {
                let cut: String = span.content.chars().take(room - 1).collect();
                out.push(Span::styled(format!("{cut}…"), span.style));
            }
            break;
        }
    }
    Line::from(out)
}

/// Keep the last lines that fit in `height` rows once wrapped into `width` columns.
fn fit_tail(lines: Vec<Line<'static>>, width: u16, height: u16) -> Vec<Line<'static>> {
    let mut used = 0u16;
    let mut kept: Vec<Line<'static>> = Vec::new();
    for l in lines.into_iter().rev() {
        let rows = wrapped_rows(line_chars(&l), width);
        if used.saturating_add(rows) > height && !kept.is_empty() {
            break;
        }
        used = used.saturating_add(rows);
        kept.push(l);
        if used >= height {
            break;
        }
    }
    kept.reverse();
    kept
}

const SINGLE_QUOTE: char = '\'';

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// The namespaces the runtime installs, highlighted so the eye finds them first.
const BINDINGS: &[&str] = &[
    "fs", "sh", "mem", "skill", "subagent", "prompt", "agent", "llm", "model", "compact", "verify",
    "note", "done", "print",
];

/// A deliberately small Lua tokenizer: comments, strings, numbers, keywords and the
/// runtime bindings. Enough to read a one-line snippet, not a full grammar.
fn lua_spans(code: &str) -> Vec<Span<'static>> {
    let kw = Style::default().fg(Color::Magenta);
    let string = Style::default().fg(Color::Green);
    let number = Style::default().fg(Color::Yellow);
    let comment = Style::default().fg(Color::DarkGray);
    let binding = Style::default().fg(Color::Cyan);
    let plain = Style::default().fg(Color::Gray);

    let mut out: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0usize;
    let push = |out: &mut Vec<Span<'static>>, text: String, style: Style| {
        if !text.is_empty() {
            out.push(Span::styled(text, style));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            push(&mut out, chars[i..].iter().collect(), comment);
            break;
        }
        if c == '"' || c == SINGLE_QUOTE {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(
                &mut out,
                chars[start..i.min(chars.len())].iter().collect(),
                string,
            );
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            push(&mut out, chars[start..i].iter().collect(), number);
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if KEYWORDS.contains(&word.as_str()) {
                kw
            } else if BINDINGS.contains(&word.as_str()) {
                binding
            } else {
                plain
            };
            push(&mut out, word, style);
            continue;
        }
        let start = i;
        while i < chars.len()
            && !chars[i].is_alphanumeric()
            && chars[i] != '_'
            && chars[i] != '"'
            && chars[i] != '\''
            && !(chars[i] == '-' && chars.get(i + 1) == Some(&'-'))
        {
            i += 1;
        }
        push(&mut out, chars[start..i].iter().collect(), plain);
    }
    out
}

fn one_line(s: &str, n: usize) -> String {
    let flat = s.replace('\n', " ⏎ ");
    let mut out: String = flat.chars().take(n).collect();
    if flat.chars().count() > n {
        out.push('…');
    }
    out
}

/// The header of the detail pane, one styled line per row.
fn detail_lines(d: &SessionDetail) -> Vec<Line<'static>> {
    let i = &d.info;
    let dim = Style::default().fg(Color::DarkGray);
    let val = Style::default().fg(Color::Gray);
    let field = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{k}: "), dim),
            Span::styled(v, val),
        ])
    };
    let verify = match i.last_verify_ok {
        None => Span::styled("never", dim),
        Some(true) => Span::styled("OK", Style::default().fg(Color::Green)),
        Some(false) => Span::styled("FAILED", Style::default().fg(Color::Red)),
    };
    vec![
        Line::from(vec![
            Span::styled(
                i.name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {} ", i.id), dim),
            Span::styled(
                format!("{:?}", i.state).to_lowercase(),
                state_style(i.state),
            ),
            Span::styled(
                i.outcome
                    .as_ref()
                    .map_or(String::new(), |o| format!("  [{o}]")),
                dim,
            ),
        ]),
        field(
            "parent",
            i.parent.as_ref().map_or("-".into(), ToString::to_string),
        ),
        field("model", i.model.to_string()),
        Line::from(vec![
            Span::styled("budget: ", dim),
            Span::styled(
                format!("turn {}/{}", i.counters.turns, i.budget.max_turns),
                val,
            ),
            Span::styled(format!(" (min {})", i.budget.min_turns), dim),
            Span::styled(format!("  tokens {}", i.counters.tokens), val),
            Span::styled(format!("/{}", i.budget.max_tokens), dim),
            Span::styled(
                if i.counters.reasoning_tokens > 0 {
                    format!(" (+{} reasoning)", i.counters.reasoning_tokens)
                } else {
                    String::new()
                },
                dim,
            ),
            Span::styled(format!("  refused {}", i.counters.done_refused), val),
        ]),
        Line::from(vec![
            Span::styled("verify: ", dim),
            verify,
            Span::styled("  pending msgs: ", dim),
            Span::styled(d.pending_messages.to_string(), val),
        ]),
        field("note", i.last_note.clone().unwrap_or_else(|| "-".into())),
        field("task", one_line(d.task.lines().next().unwrap_or(""), 200)),
    ]
}

/// One trajectory event as a styled line: a coloured kind, then its payload, with the
/// Lua of an `exec` lightly highlighted.
fn event_line(e: &Value) -> Line<'static> {
    let kind = e["kind"].as_str().unwrap_or("?");
    let turn = e["turn"].as_u64().unwrap_or(0);
    let dim = Style::default().fg(Color::DarkGray);
    let val = Style::default().fg(Color::Gray);
    let mut spans = vec![Span::styled(format!("t{turn} "), dim)];
    let label = |s: &str, c: Color| Span::styled(format!("{s} "), Style::default().fg(c));
    match kind {
        "exec" => {
            spans.push(label("exec", Color::Cyan));
            spans.extend(lua_spans(&one_line(e["code"].as_str().unwrap_or(""), 120)));
            spans.push(Span::styled(" → ", dim));
            let err = e["error"].as_str().unwrap_or("");
            if err.is_empty() {
                spans.push(Span::styled(
                    one_line(e["output"].as_str().unwrap_or(""), 120),
                    val,
                ));
            } else {
                spans.push(Span::styled(
                    one_line(err, 120),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        "assistant" => {
            spans.push(label("assistant", Color::Blue));
            let reasoning = e["usage"]["reasoning_tokens"].as_u64().unwrap_or(0);
            spans.push(Span::styled(
                format!(
                    "{} tok{}",
                    e["usage"]["output_tokens"].as_u64().unwrap_or(0),
                    if reasoning > 0 {
                        format!(" (+{reasoning} reasoning)")
                    } else {
                        String::new()
                    }
                ),
                dim,
            ));
        }
        "truncated" => {
            spans.push(label("truncated", Color::Red));
            let retry = e["retried_with"]
                .as_u64()
                .map_or_else(|| "fed back".to_string(), |m| format!("retry at {m}"));
            spans.push(Span::styled(
                format!(
                    "{} tok ({} reasoning), {retry}",
                    e["output_tokens"].as_u64().unwrap_or(0),
                    e["reasoning_tokens"].as_u64().unwrap_or(0)
                ),
                dim,
            ));
        }
        "note" => {
            spans.push(label("note", Color::Yellow));
            spans.push(Span::styled(
                one_line(e["text"].as_str().unwrap_or(""), 200),
                Style::default().fg(Color::Yellow),
            ));
        }
        "message" => {
            spans.push(label("message", Color::Magenta));
            spans.push(Span::styled(
                format!("from {} ", e["from"].as_str().unwrap_or("?")),
                dim,
            ));
            spans.push(Span::styled(
                one_line(e["body"].as_str().unwrap_or(""), 160),
                val,
            ));
        }
        "verify" => {
            let ok = e["ok"].as_bool().unwrap_or(false);
            spans.push(label("verify", if ok { Color::Green } else { Color::Red }));
            spans.push(Span::styled(
                format!(
                    "{} in {}s",
                    if ok { "OK" } else { "FAILED" },
                    e["seconds"].as_u64().unwrap_or(0)
                ),
                val,
            ));
        }
        "done_refused" => {
            spans.push(label("done refused", Color::Red));
            spans.push(Span::styled(
                one_line(e["reason"].as_str().unwrap_or(""), 160),
                val,
            ));
        }
        "compact_started" => {
            spans.push(label("compacting…", Color::Blue));
            spans.push(Span::styled(
                format!("{} turns, waiting for the summary", e["turns_before"]),
                dim,
            ));
        }
        "compact" => {
            spans.push(label("compact", Color::Blue));
            spans.push(Span::styled(
                format!("{} → {} turns", e["turns_before"], e["turns_after"]),
                dim,
            ));
        }
        "model_switch" => {
            spans.push(label("model", Color::Cyan));
            spans.push(Span::styled(format!("{} → {}", e["from"], e["to"]), val));
        }
        "llm_error" => {
            spans.push(label("llm error", Color::Red));
            spans.push(Span::styled(
                one_line(e["error"].as_str().unwrap_or(""), 160),
                val,
            ));
        }
        "finish" => {
            let o = &e["outcome"];
            let kind = o["kind"].as_str().unwrap_or("?");
            let c = if kind == "done" {
                Color::Green
            } else {
                Color::Red
            };
            spans.push(Span::styled(
                format!("finish {kind} "),
                Style::default().fg(c).add_modifier(Modifier::BOLD),
            ));
            // Show the one field that carries the reason, not the whole JSON object.
            let detail = ["summary", "reason", "message"]
                .iter()
                .find_map(|k| o[*k].as_str())
                .unwrap_or("");
            spans.push(Span::styled(one_line(detail, 160), val));
        }
        "exhausted" => {
            spans.push(label("exhausted", Color::Red));
            spans.push(Span::styled(
                e["reason"].as_str().unwrap_or("").to_string(),
                val,
            ));
        }
        "start" => {
            spans.push(label("start", Color::Green));
            spans.push(Span::styled(
                one_line(e["task"].as_str().unwrap_or(""), 160),
                val,
            ));
        }
        other => {
            spans.push(label(other, Color::DarkGray));
        }
    }
    Line::from(spans)
}

#[allow(dead_code)] // kept for `longe tail`, which prints plain text
fn render_event(e: &Value) -> String {
    let kind = e["kind"].as_str().unwrap_or("?");
    let turn = e["turn"].as_u64().unwrap_or(0);
    let short = |s: &str, n: usize| -> String {
        let one: String = s.chars().take(n).collect();
        one.replace('\n', " ⏎ ")
    };
    match kind {
        "exec" => format!(
            "t{turn} exec: {} → {}",
            short(e["code"].as_str().unwrap_or(""), 160),
            short(e["output"].as_str().unwrap_or(""), 160)
        ),
        "assistant" => format!(
            "t{turn} assistant ({} tok)",
            e["usage"]["output_tokens"].as_u64().unwrap_or(0)
        ),
        "note" => format!(
            "t{turn} note: {}",
            short(e["text"].as_str().unwrap_or(""), 200)
        ),
        "message" => format!(
            "t{turn} message from {}: {}",
            e["from"].as_str().unwrap_or("?"),
            short(e["body"].as_str().unwrap_or(""), 160)
        ),
        "verify" => format!("t{turn} verify ok={} ({}s)", e["ok"], e["seconds"]),
        "done_refused" => format!(
            "t{turn} done refused: {}",
            short(e["reason"].as_str().unwrap_or(""), 160)
        ),
        "finish" => format!("finish: {}", e["outcome"]),
        other => format!("t{turn} {other}"),
    }
}

fn draw_diffs(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);
    let items: Vec<ListItem> = app
        .diffs
        .iter()
        .map(|d| ListItem::new(d.branch.clone()))
        .collect();
    let mut state = ListState::default();
    state.select(if app.diffs.is_empty() {
        None
    } else {
        Some(app.diff_selected)
    });
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" pending reflect branches "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, cols[0], &mut state);
    let text = app
        .diffs
        .get(app.diff_selected)
        .map(|d| d.diff.clone())
        .unwrap_or_else(|| "no pending diff".into());
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" diff ")),
        cols[1],
    );
}

pub async fn run(socket: &Path) -> anyhow::Result<()> {
    let mut app = App {
        socket: socket.to_path_buf(),
        sessions: Vec::new(),
        order: Vec::new(),
        depth: Vec::new(),
        selected: 0,
        detail: None,
        diffs: Vec::new(),
        diff_selected: 0,
        view: View::Sessions,
        input: String::new(),
        status: String::new(),
        typing: false,
    };
    app.refresh().await;
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let result = event_loop(&mut terminal, &mut app).await;
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    let mut last_refresh = std::time::Instant::now();
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if app.typing {
                    match k.code {
                        KeyCode::Esc => {
                            app.typing = false;
                            app.input.clear();
                        }
                        KeyCode::Enter => {
                            if let Some(id) = app.selected_id() {
                                let body = std::mem::take(&mut app.input);
                                app.act(
                                    Request::Send {
                                        id: id.to_string(),
                                        body,
                                        from_name: Some("cockpit".into()),
                                    },
                                    "send",
                                )
                                .await;
                            }
                            app.typing = false;
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Char(c) => app.input.push(c),
                        _ => {}
                    }
                    continue;
                }
                match (k.code, k.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    (KeyCode::Up, _) => {
                        if app.view == View::Sessions {
                            app.selected = app.selected.saturating_sub(1);
                        } else {
                            app.diff_selected = app.diff_selected.saturating_sub(1);
                        }
                        app.refresh().await;
                    }
                    (KeyCode::Down, _) => {
                        if app.view == View::Sessions {
                            app.selected =
                                (app.selected + 1).min(app.order.len().saturating_sub(1));
                        } else {
                            app.diff_selected =
                                (app.diff_selected + 1).min(app.diffs.len().saturating_sub(1));
                        }
                        app.refresh().await;
                    }
                    (KeyCode::Char('i'), _) | (KeyCode::Enter, _) if app.view == View::Sessions => {
                        app.typing = true
                    }
                    (KeyCode::Char('d'), _) => {
                        app.view = if app.view == View::Sessions {
                            View::Diffs
                        } else {
                            View::Sessions
                        };
                        app.refresh().await;
                    }
                    (KeyCode::Char(c), _) if app.view == View::Sessions => {
                        if let Some(id) = app.selected_id() {
                            let id = id.to_string();
                            match c {
                                'p' => app.act(Request::Pause { id }, "pause").await,
                                'r' => app.act(Request::Resume { id }, "resume").await,
                                'k' => app.act(Request::Kill { id }, "kill").await,
                                'o' => app.act(Request::Offload { id }, "offload").await,
                                'R' => app.act(Request::Reflect { id }, "reflect").await,
                                _ => {}
                            }
                            app.refresh().await;
                        }
                    }
                    (KeyCode::Char(c), _) => {
                        if let Some(d) = app.diffs.get(app.diff_selected) {
                            let branch = d.branch.clone();
                            match c {
                                'a' => app.act(Request::Accept { branch }, "accept").await,
                                'x' => app.act(Request::Reject { branch }, "reject").await,
                                _ => {}
                            }
                            app.refresh().await;
                        }
                    }
                    _ => {}
                }
            }
        }
        if last_refresh.elapsed() > Duration::from_millis(700) {
            app.refresh().await;
            last_refresh = std::time::Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrapped_rows_counts_visual_lines() {
        assert_eq!(wrapped_rows(0, 10), 1, "an empty line still occupies a row");
        assert_eq!(wrapped_rows(10, 10), 1);
        assert_eq!(wrapped_rows(11, 10), 2);
        assert_eq!(wrapped_rows(30, 10), 3);
        assert_eq!(
            wrapped_rows(5, 0),
            5,
            "a zero width must not divide by zero"
        );
    }

    #[test]
    fn fit_tail_keeps_the_newest_and_never_exceeds_the_pane() {
        // Each line is 20 chars wide, so it wraps over 2 rows in a 10-wide pane.
        let lines: Vec<Line> = (0..10).map(|i| Line::from(format!("{i:0>20}"))).collect();
        let kept = fit_tail(lines, 10, 6);
        let rows: u16 = kept.iter().map(|l| wrapped_rows(line_chars(l), 10)).sum();
        assert!(rows <= 6, "{rows} rows must fit in 6");
        assert_eq!(kept.len(), 3);
        assert!(
            plain(kept.last().unwrap()).ends_with('9'),
            "the newest event must remain the last visible row"
        );
    }

    #[test]
    fn fit_tail_keeps_one_line_even_when_it_cannot_fit() {
        let long = Line::from("x".repeat(500));
        let kept = fit_tail(vec![long], 10, 3);
        assert_eq!(kept.len(), 1, "never render an empty pane");
    }

    #[test]
    fn truncate_line_cuts_at_the_pane_width() {
        let l = Line::from(vec![Span::raw("abcde"), Span::raw("fghij")]);
        assert_eq!(plain(&truncate_line(l.clone(), 20)), "abcdefghij");
        let cut = truncate_line(l, 7);
        assert_eq!(plain(&cut).chars().count(), 7);
        assert!(plain(&cut).ends_with('…'));
    }

    #[test]
    fn lua_highlighting_classifies_the_pieces() {
        let spans = lua_spans("local x = fs.read('a.txt') -- note");
        let styled: Vec<(String, Option<Color>)> = spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        let find = |t: &str| styled.iter().find(|(c, _)| c == t).map(|(_, f)| *f);
        assert_eq!(find("local"), Some(Some(Color::Magenta)), "keyword");
        assert_eq!(find("fs"), Some(Some(Color::Cyan)), "runtime binding");
        assert_eq!(find("'a.txt'"), Some(Some(Color::Green)), "string");
        assert_eq!(find("-- note"), Some(Some(Color::DarkGray)), "comment");
        assert_eq!(find("x"), Some(Some(Color::Gray)), "identifier");
        // Reassembling the spans must reproduce the input exactly.
        let round: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(round, "local x = fs.read('a.txt') -- note");
    }

    #[test]
    fn lua_highlighting_survives_odd_input() {
        for code in [
            "",
            "'unterminated",
            "-- only a comment",
            "x = \"a\\\"b\"",
            "1.5e3 + 0x1f",
        ] {
            let round: String = lua_spans(code).iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(round, code, "tokenizer must be lossless for {code:?}");
        }
    }

    #[test]
    fn event_lines_are_single_line_and_labelled() {
        let exec = event_line(&json!({
            "kind": "exec", "turn": 3,
            "code": "fs.write('a',\n'b')", "output": "true"
        }));
        let text = plain(&exec);
        assert!(text.starts_with("t3 exec "), "{text}");
        assert!(text.contains(" ⏎ "), "newlines are flattened: {text}");
        assert!(!text.contains('\n'), "an event must stay one logical line");

        let failed = event_line(&json!({"kind": "verify", "turn": 1, "ok": false, "seconds": 4}));
        assert!(plain(&failed).contains("FAILED"));
        assert_eq!(failed.spans[1].style.fg, Some(Color::Red));

        let ok = event_line(&json!({"kind": "verify", "turn": 1, "ok": true, "seconds": 4}));
        assert_eq!(ok.spans[1].style.fg, Some(Color::Green));

        let err = event_line(&json!({
            "kind": "exec", "turn": 2, "code": "x", "output": "", "error": "boom"
        }));
        assert!(plain(&err).contains("boom"));
    }

    #[test]
    fn long_event_payloads_are_bounded() {
        let e = event_line(&json!({
            "kind": "note", "turn": 1, "text": "z".repeat(5000)
        }));
        assert!(line_chars(&e) < 300, "a single event cannot flood the pane");
    }
}
