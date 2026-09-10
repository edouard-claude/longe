//! End-to-end tests: a real tree actor, real Lua, real store, and a scripted fake
//! OpenAI-compatible server. Each session name has its own script of replies.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::config::{BudgetCfg, Harness, ProviderCfg, ProviderKind, SandboxBackend};
use crate::llm::LlmRegistry;
use crate::session::tree::{spawn_tree_with, FinishEvent};
use crate::session::{Message, Outcome, SessionId, SessionState, SpawnSpec, TreeHandle};
use crate::store::Store;

#[derive(Default)]
struct Fake {
    scripts: Mutex<HashMap<String, VecDeque<String>>>,
    /// (model, system, messages) per request, in order.
    requests: Mutex<Vec<(String, String, Vec<Value>)>>,
    prompt_tokens: Mutex<u64>,
}

fn session_name(system: &str) -> String {
    system
        .split("| name: ")
        .nth(1)
        .and_then(|s| s.split(" |").next())
        .unwrap_or("?")
        .to_string()
}

async fn handler(
    State(fake): State<Arc<Fake>>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let model = body["model"].as_str().unwrap_or("").to_string();
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    let system = msgs
        .iter()
        .find(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();
    let history: Vec<Value> = msgs
        .iter()
        .filter(|m| m["role"] != "system")
        .cloned()
        .collect();
    fake.requests.lock().push((model, system.clone(), history));
    let reply = if system.starts_with("You compress") {
        "## Progress\nSUMMARY OF OLDER TURNS\n## Next steps\ncontinue".to_string()
    } else if system.starts_with("You improve the harness") {
        json!({"analysis": "fine", "changes": [{"path": "skills/e2e.md", "content": "# e2e skill\nwrite tests first"}]}).to_string()
    } else {
        let name = session_name(&system);
        let mut scripts = fake.scripts.lock();
        scripts
            .get_mut(&name)
            .and_then(|q| q.pop_front())
            .unwrap_or_else(|| "```lua\nprint('tick')\n```".to_string())
    };
    let pt = *fake.prompt_tokens.lock();
    let chunks = [
        json!({"choices":[{"delta":{"content": reply},"finish_reason":"stop"}]}).to_string(),
        json!({"choices":[],"usage":{"prompt_tokens": pt, "completion_tokens": 10}}).to_string(),
    ];
    let mut sse = String::new();
    for c in chunks {
        sse.push_str(&format!("data: {c}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], sse)
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    fake: Arc<Fake>,
    tree: TreeHandle,
    finished: mpsc::Receiver<FinishEvent>,
    ws: std::path::PathBuf,
    harness: Arc<Harness>,
    llm: Arc<LlmRegistry>,
}

async fn world(tweak: impl FnOnce(&mut Harness)) -> World {
    let fake = Arc::new(Fake::default());
    *fake.prompt_tokens.lock() = 50;
    let app = Router::new()
        .route("/chat/completions", post(handler))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("store")).unwrap());
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let mut h = Harness::default();
    for name in ["o", "p"] {
        h.providers.insert(
            name.into(),
            ProviderCfg {
                kind: ProviderKind::Openai,
                base_url: url.clone(),
                api_key_env: None,
                max_retries: 1,
                timeout_seconds: 10,
                extra: toml::Table::new(),
            },
        );
    }
    h.model.provider = "o".into();
    h.model.name = "m".into();
    h.model.context_window = 100_000;
    h.budget = BudgetCfg {
        min_seconds: 0,
        min_turns: 1,
        max_turns: 30,
        max_tokens: 1_000_000,
    };
    h.sandbox.backend = SandboxBackend::None;
    h.sandbox.timeout_seconds = 10;
    h.verify.command = None;
    h.reflect.enabled = false;
    h.daemon.offload_after_minutes = 1000;
    tweak(&mut h);
    let harness = Arc::new(h);
    let llm = Arc::new(LlmRegistry::from_harness(&harness).unwrap());
    let (tx, rx) = mpsc::channel(32);
    let (tree, _task) = spawn_tree_with(store.clone(), llm.clone(), harness.clone(), Some(tx));
    World {
        _dir: dir,
        store,
        fake,
        tree,
        finished: rx,
        ws,
        harness,
        llm,
    }
}

impl World {
    fn script(&self, name: &str, replies: &[&str]) {
        self.fake.scripts.lock().insert(
            name.to_string(),
            replies.iter().map(|s| s.to_string()).collect(),
        );
    }

    async fn spawn(&self, name: &str, task: &str) -> SessionId {
        self.tree
            .spawn(SpawnSpec {
                name: name.into(),
                task: task.into(),
                workspace: self.ws.clone(),
                parent: None,
                model: None,
                budget: None,
            })
            .await
            .unwrap()
    }

    async fn wait_finish(&mut self, id: &SessionId) -> Outcome {
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(30), self.finished.recv())
                .await
                .expect("finish event")
                .unwrap();
            if &ev.id == id {
                return ev.outcome;
            }
        }
    }

    fn events(&self, id: &SessionId) -> Vec<Value> {
        self.store.trajectory_all(id.as_str()).unwrap()
    }

    fn exec_outputs(&self, id: &SessionId) -> Vec<String> {
        self.events(id)
            .iter()
            .filter(|e| e["kind"] == "exec")
            .map(|e| e["output"].as_str().unwrap_or("").to_string())
            .collect()
    }

    async fn state(&self, id: &SessionId) -> SessionState {
        self.tree
            .get(id.clone(), 0)
            .await
            .unwrap()
            .unwrap()
            .info
            .state
    }
}

fn lua(code: &str) -> String {
    format!("```lua\n{code}\n```")
}

#[tokio::test(flavor = "multi_thread")]
async fn state_and_store_persist_across_turns_and_restart() {
    let mut w = world(|_| {}).await;
    w.script(
        "root",
        &[
            &lua("x = 7; mem.set('k', 'v'); skill.set('s', 'proc'); prompt.set('custom prompt')"),
            &lua("print(x)"),
            &lua("done('first run')"),
        ],
    );
    let id = w.spawn("root", "persist things").await;
    assert!(matches!(w.wait_finish(&id).await, Outcome::Done { .. }));
    assert_eq!(w.exec_outputs(&id)[1], "7");
    // The custom prompt reached the system prompt of the next turn.
    assert!(
        w.fake.requests.lock()[1].1.starts_with("custom prompt"),
        "{}",
        &w.fake.requests.lock()[1].1[..60]
    );
    assert!(w.store.session_dir(id.as_str()).join("state.lua").exists());

    // Offload, then a fresh tree on the same store (daemon restart).
    w.tree.offload(id.clone()).await.unwrap();
    assert_eq!(w.state(&id).await, SessionState::Offloaded);
    let (tx, rx) = mpsc::channel(32);
    let (tree2, _t) = spawn_tree_with(w.store.clone(), w.llm.clone(), w.harness.clone(), Some(tx));
    w.finished = rx;
    let listed = tree2.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, SessionState::Offloaded);
    w.script(
        "root",
        &[
            &lua("print(x, mem.get('k'), skill.get('s'))"),
            &lua("done('second run')"),
        ],
    );
    tree2
        .send(Message::text(None, "human", id.clone(), "are you there?"))
        .await
        .unwrap();
    w.tree = tree2;
    assert!(matches!(w.wait_finish(&id).await, Outcome::Done { .. }));
    let outs = w.exec_outputs(&id);
    assert!(outs.iter().any(|o| o == "7\tv\tproc"), "{outs:?}");
    let ev = w.events(&id);
    assert!(ev
        .iter()
        .any(|e| e["kind"] == "message" && e["body"] == "are you there?"));
    assert!(ev.iter().any(|e| e["kind"] == "resume"));
}

#[tokio::test(flavor = "multi_thread")]
async fn done_is_refused_before_budget_and_while_verify_fails() {
    let mut w = world(|h| {
        h.budget.min_turns = 3;
        h.verify.command = Some("test -f ok.txt".into());
    })
    .await;
    w.script(
        "root",
        &[
            &lua("done('too early')"),
            &lua("print(1)"),
            &lua("done('still no ok.txt')"),
            &lua("fs.write('ok.txt', '')"),
            &lua("done('now')"),
        ],
    );
    let id = w.spawn("root", "finish properly").await;
    let outcome = w.wait_finish(&id).await;
    assert_eq!(
        outcome,
        Outcome::Done {
            summary: "now".into()
        }
    );
    let ev = w.events(&id);
    let refusals: Vec<String> = ev
        .iter()
        .filter(|e| e["kind"] == "done_refused")
        .map(|e| e["reason"].as_str().unwrap().to_string())
        .collect();
    assert!(
        refusals.iter().any(|r| r.contains("minimum turn")),
        "{refusals:?}"
    );
    assert!(
        refusals.iter().any(|r| r.contains("verifier failed")),
        "{refusals:?}"
    );
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    let info = w.tree.get(id.clone(), 0).await.unwrap().unwrap().info;
    assert_eq!(
        info.counters.done_refused, 1,
        "only the budget refusal increments the counter"
    );
    assert_eq!(info.last_verify_ok, Some(true));
    // The refusal was fed back to the model.
    let reqs = w.fake.requests.lock();
    let second_user = reqs[1].2.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(second_user.contains("done() refused"), "{second_user}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sub_agent_spawn_send_recv_report_and_wake() {
    let w = world(|_| {}).await;
    w.script(
        "root",
        &[
            &lua("child = agent.spawn('worker', 'do a small job', {workspace = '.'}); agent.send(child, 'ping'); agent.send(agent.id(), 'note to self'); print(#agent.recv())"),
            &lua("print(#agent.list()); done('root idle, waiting for the report')"),
            &lua("done('ack')"),
        ],
    );
    w.script(
        "worker",
        &[
            &lua("m = agent.recv(); print(#m, m[1] and m[1].body)"),
            &lua("note('half'); done('worker finished')"),
        ],
    );
    let root = w.spawn("root", "delegate").await;

    // Poll observable state instead of finish events: whether the worker's report
    // reaches root during root's own turn or wakes it afterwards is a genuine race,
    // so nothing here may depend on the order or the count of events.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (report, worker) = loop {
        let sessions = w.tree.list().await.unwrap();
        let worker = sessions.iter().find(|s| s.name == "worker");
        let report = w
            .events(&root)
            .into_iter()
            .find(|e| e["kind"] == "message" && e["from"] == "worker");
        match (worker, report) {
            (Some(wk), Some(r)) if wk.state != SessionState::Running => break (r, wk.clone()),
            _ => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "worker never finished and reported to its parent"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    };

    assert_eq!(worker.parent.as_ref(), Some(&root));
    assert_eq!(worker.last_note.as_deref(), Some("half"));
    assert!(report["body"].as_str().unwrap().contains("worker finished"));

    let outs = w.exec_outputs(&root);
    assert_eq!(
        outs[0], "1",
        "the self-sent message must be readable through agent.recv()"
    );
    assert_eq!(outs[1], "2", "agent.list() sees both sessions");

    // `print` uses tostring: a delivered ping renders as "1\tping", an inbox still
    // empty as "0\tnil" (the ping is then read on a later turn). Both are legitimate.
    let wouts = w.exec_outputs(&worker.id);
    assert!(
        wouts[0] == "1\tping" || wouts[0] == "0\tnil",
        "unexpected first worker output: {wouts:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn model_switch_keeps_lua_state() {
    let mut w = world(|_| {}).await;
    w.script(
        "root",
        &[
            &lua("x = 41"),
            &lua("print(model.switch('p', 'm2'))"),
            &lua("print(x + 1); done('ok')"),
        ],
    );
    let id = w.spawn("root", "switch").await;
    assert!(matches!(w.wait_finish(&id).await, Outcome::Done { .. }));
    assert_eq!(w.exec_outputs(&id)[2], "42");
    let models: Vec<String> = { w.fake.requests.lock().iter().map(|r| r.0.clone()).collect() };
    assert_eq!(models, vec!["m", "m", "m2"]);
    let info = w.tree.get(id.clone(), 0).await.unwrap().unwrap().info;
    assert_eq!(info.model.to_string(), "p/m2");
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_keeps_the_task_and_recent_turns() {
    let mut w = world(|h| {
        h.model.context_window = 100;
        h.compact.keep_last = 2;
    })
    .await;
    *w.fake.prompt_tokens.lock() = 90; // 90 > 0.7 * 100 -> compaction after every turn
    w.script(
        "root",
        &[
            &lua("a = 1"),
            &lua("b = 2"),
            &lua("c = 3"),
            &lua("print(a + b + c); done('ok')"),
        ],
    );
    let id = w
        .spawn("root", "THE-UNIQUE-TASK-MARKER build a widget")
        .await;
    assert!(matches!(w.wait_finish(&id).await, Outcome::Done { .. }));
    let ev = w.events(&id);
    let compactions = ev.iter().filter(|e| e["kind"] == "compact").count();
    assert!(compactions >= 2, "{compactions}");
    assert_eq!(w.exec_outputs(&id).last().unwrap(), "6");
    let reqs = w.fake.requests.lock();
    let last = reqs
        .iter()
        .rev()
        .find(|r| !r.1.starts_with("You compress"))
        .unwrap();
    assert!(
        last.1.contains("THE-UNIQUE-TASK-MARKER"),
        "task must survive compaction in the system prompt"
    );
    assert!(last.2[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("[compacted context]"));
    let session = crate::session::state::Session::load(&w.store, &id).unwrap();
    assert!(session.meta.compactions >= 2);
    assert!(session.history.len() <= 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_kill() {
    let mut w = world(|h| h.budget.max_turns = 1000).await;
    // Default script: infinite ticks.
    let id = w.spawn("root", "loop forever").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    w.tree.pause(id.clone()).await.unwrap();
    assert_eq!(w.wait_finish(&id).await, Outcome::Paused);
    assert_eq!(w.state(&id).await, SessionState::Paused);
    let turns_at_pause = w
        .tree
        .get(id.clone(), 0)
        .await
        .unwrap()
        .unwrap()
        .info
        .counters
        .turns;
    // A message to a paused session queues but does not wake it.
    w.tree
        .send(Message::text(None, "human", id.clone(), "queued"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(w.state(&id).await, SessionState::Paused);
    assert_eq!(
        w.tree
            .get(id.clone(), 0)
            .await
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    w.tree.resume(id.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(w.state(&id).await, SessionState::Running);
    w.tree.kill(id.clone()).await.unwrap();
    assert_eq!(w.wait_finish(&id).await, Outcome::Killed);
    let info = w.tree.get(id.clone(), 0).await.unwrap().unwrap().info;
    assert!(info.counters.turns > turns_at_pause);
    assert_eq!(info.state, SessionState::Paused);
    assert!(w
        .events(&id)
        .iter()
        .any(|e| e["kind"] == "message" && e["body"] == "queued"));
}

#[tokio::test(flavor = "multi_thread")]
async fn budget_exhaustion_finishes_cleanly() {
    let mut w = world(|h| h.budget.max_turns = 3).await;
    let id = w.spawn("root", "never done").await;
    assert_eq!(
        w.wait_finish(&id).await,
        Outcome::Exhausted {
            reason: crate::budget::Exhausted::MaxTurns
        }
    );
    let info = w.tree.get(id.clone(), 0).await.unwrap().unwrap().info;
    assert_eq!(info.counters.turns, 3);
    assert!(w.events(&id).iter().any(|e| e["kind"] == "exhausted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reflect_produces_a_diff_and_git_shows_it() {
    let mut w = world(|h| {
        h.reflect.enabled = true;
        h.reflect.fitness = Some("echo 1".into());
    })
    .await;
    w.script("root", &[&lua("note('learned something'); done('ok')")]);
    let id = w.spawn("root", "quick").await;
    assert!(matches!(w.wait_finish(&id).await, Outcome::Done { .. }));
    let report = crate::reflect::run(&w.store, &w.llm, &w.harness, &id)
        .await
        .unwrap();
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.accepted, Some(true));
    assert!(w.store.skill_get("e2e").is_ok());
    let log = w.store.git_log(10).unwrap();
    assert!(
        log.iter().any(|l| l.contains("reflect: 1 change(s)")),
        "{log:?}"
    );
    assert!(log.iter().any(|l| l.contains("accept reflect/")), "{log:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn http_api_lists_and_messages_sessions() {
    let mut w = world(|_| {}).await;
    let api = crate::daemon::Api {
        store: w.store.clone(),
        harness: w.harness.clone(),
        llm: w.llm.clone(),
        tree: w.tree.clone(),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, crate::cockpit::http::router(api))
            .await
            .unwrap()
    });
    let http = reqwest::Client::new();
    w.script("root", &[&lua("print(#agent.recv())"), &lua("done('ok')")]);
    let v: Value = http
        .post(format!("{base}/sessions"))
        .json(&json!({"task": "via http", "workspace": w.ws, "name": "root"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    let r = http
        .post(format!("{base}/sessions/{id}/message"))
        .json(&json!({"body": "hello"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let sid = SessionId::parse(&id).unwrap();
    assert!(matches!(w.wait_finish(&sid).await, Outcome::Done { .. }));
    let list: Vec<Value> = http
        .get(format!("{base}/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "root");
    let detail: Value = http
        .get(format!("{base}/sessions/{id}?tail=50"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["task"], "via http");
    let bad = http
        .get(format!("{base}/sessions/zzzz"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let diffs: Value = http
        .get(format!("{base}/store/diffs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(diffs.as_array().unwrap().is_empty());
}
