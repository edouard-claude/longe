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
                    " {} t{} {}k{}",
                    s.name,
                    s.counters.turns,
                    s.counters.tokens.div_euclid(1000),
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
    let (detail_text, tail_text) = match &app.detail {
        Some(d) => {
            let i = &d.info;
            let head = format!(
                "{} ({}) parent={} model={}\nstate={:?} outcome={}\nbudget: turn {}/{} (min {}) tokens {}/{} elapsed≈{}s (min {}s) refused {}\nverify: {}  pending msgs: {}\nnote: {}\ntask: {}",
                i.name,
                i.id,
                i.parent.as_ref().map_or("-".into(), ToString::to_string),
                i.model,
                i.state,
                i.outcome.as_ref().map_or("-".into(), ToString::to_string),
                i.counters.turns,
                i.budget.max_turns,
                i.budget.min_turns,
                i.counters.tokens,
                i.budget.max_tokens,
                i.counters.elapsed_before,
                i.budget.min_seconds,
                i.counters.done_refused,
                i.last_verify_ok.map_or_else(|| String::from("never"), |ok| String::from(if ok { "OK" } else { "FAILED" })),
                d.pending_messages,
                i.last_note.as_deref().unwrap_or("-"),
                d.task.lines().next().unwrap_or(""),
            );
            let tail = d
                .tail
                .iter()
                .map(render_event)
                .collect::<Vec<_>>()
                .join("\n");
            (head, tail)
        }
        None => ("no session selected".into(), String::new()),
    };
    f.render_widget(
        Paragraph::new(detail_text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        right[0],
    );
    let lines = tail_text.lines().count() as u16;
    let h = right[1].height.saturating_sub(2);
    let scroll = lines.saturating_sub(h);
    f.render_widget(
        Paragraph::new(tail_text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" trajectory tail "),
            ),
        right[1],
    );
}

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
