//! `longed`: the daemon. Owns the tree, listens on a unix socket (and optionally HTTP),
//! runs crons, schedules reflection. Clients come and go; sessions stay.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{BudgetCfg, CronAction, Harness, ModelRef};
use crate::cron::Schedule;
use crate::llm::LlmRegistry;
use crate::session::tree::{spawn_tree_with, FinishEvent};
use crate::session::{Message, Outcome, SessionId, SpawnSpec, TreeHandle};
use crate::store::Store;

/// The wire protocol: one JSON object per line, one request per connection.
pub mod proto {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    pub enum Request {
        Ping,
        Spawn {
            name: String,
            task: String,
            workspace: PathBuf,
            #[serde(default)]
            model: Option<ModelRef>,
            #[serde(default)]
            budget: Option<BudgetCfg>,
        },
        List,
        Get {
            id: String,
            #[serde(default = "default_tail")]
            tail: usize,
        },
        Send {
            id: String,
            body: String,
            #[serde(default)]
            from_name: Option<String>,
        },
        Pause {
            id: String,
        },
        Resume {
            id: String,
        },
        Kill {
            id: String,
        },
        Offload {
            id: String,
        },
        Reflect {
            id: String,
        },
        Diffs,
        Accept {
            branch: String,
        },
        Reject {
            branch: String,
        },
        Log {
            #[serde(default = "default_tail")]
            n: usize,
        },
        Shutdown,
    }

    fn default_tail() -> usize {
        20
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    pub enum Response {
        Ok { data: Value },
        Error { message: String },
    }

    impl Response {
        pub fn err(e: impl std::fmt::Display) -> Self {
            Self::Error {
                message: e.to_string(),
            }
        }
    }
}

use proto::{Request, Response};

/// Everything a request handler needs. Shared by the socket server and HTTP.
#[derive(Clone)]
pub struct Api {
    pub store: Arc<Store>,
    pub harness: Arc<Harness>,
    pub llm: Arc<LlmRegistry>,
    pub tree: TreeHandle,
    pub shutdown: CancellationToken,
}

impl Api {
    pub async fn call(&self, req: Request) -> Response {
        match self.dispatch(req).await {
            Ok(v) => Response::Ok { data: v },
            Err(e) => Response::err(e),
        }
    }

    fn sid(s: &str) -> anyhow::Result<SessionId> {
        Ok(SessionId::parse(s)?)
    }

    async fn dispatch(&self, req: Request) -> anyhow::Result<Value> {
        Ok(match req {
            Request::Ping => {
                json!({"pong": true, "pid": std::process::id(), "store": self.store.root()})
            }
            Request::Spawn {
                name,
                task,
                workspace,
                model,
                budget,
            } => {
                let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
                std::fs::create_dir_all(&workspace)?;
                let id = self
                    .tree
                    .spawn(SpawnSpec {
                        name,
                        task,
                        workspace,
                        parent: None,
                        model,
                        budget,
                    })
                    .await?;
                json!({"id": id})
            }
            Request::List => serde_json::to_value(self.tree.list().await?)?,
            Request::Get { id, tail } => match self.tree.get(Self::sid(&id)?, tail).await? {
                Some(d) => serde_json::to_value(d)?,
                None => anyhow::bail!("no such session {id}"),
            },
            Request::Send {
                id,
                body,
                from_name,
            } => {
                let msg = Message::text(
                    None,
                    from_name.unwrap_or_else(|| "human".into()),
                    Self::sid(&id)?,
                    body,
                );
                self.tree.send(msg).await?;
                json!({"sent": true})
            }
            Request::Pause { id } => {
                self.tree.pause(Self::sid(&id)?).await?;
                json!({"paused": true})
            }
            Request::Resume { id } => {
                self.tree.resume(Self::sid(&id)?).await?;
                json!({"resumed": true})
            }
            Request::Kill { id } => {
                self.tree.kill(Self::sid(&id)?).await?;
                json!({"killed": true})
            }
            Request::Offload { id } => {
                self.tree.offload(Self::sid(&id)?).await?;
                json!({"offloaded": true})
            }
            Request::Reflect { id } => {
                let report =
                    crate::reflect::run(&self.store, &self.llm, &self.harness, &Self::sid(&id)?)
                        .await?;
                serde_json::to_value(report)?
            }
            Request::Diffs => serde_json::to_value(crate::reflect::pending(&self.store)?)?,
            Request::Accept { branch } => {
                crate::reflect::accept(&self.store, &branch)?;
                json!({"accepted": branch})
            }
            Request::Reject { branch } => {
                crate::reflect::reject(&self.store, &branch)?;
                json!({"rejected": branch})
            }
            Request::Log { n } => json!(self.store.git_log(n)?),
            Request::Shutdown => {
                self.shutdown.cancel();
                json!({"shutting_down": true})
            }
        })
    }
}

/// Where the socket lives for a store. Unix socket paths are capped near 104 bytes,
/// so unless the config names one, use a short deterministic path under the temp dir
/// keyed by the store's canonical path (a store nested deep in a temp tree would
/// otherwise overflow `SUN_LEN`).
pub fn socket_path(store_root: &Path, harness: &Harness) -> PathBuf {
    if let Some(p) = &harness.daemon.socket {
        return p.clone();
    }
    let canon = std::fs::canonicalize(store_root).unwrap_or_else(|_| store_root.to_path_buf());
    let short = store_root.join("longed.sock");
    if short.as_os_str().len() < 100 {
        return short;
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in canon.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    std::env::temp_dir().join(format!("longe-{hash:016x}.sock"))
}

pub fn log_path(store_root: &Path) -> PathBuf {
    store_root.join("longed.log")
}

/// Run the daemon until shutdown. Blocks the current runtime.
pub async fn serve(store_root: PathBuf) -> anyhow::Result<()> {
    let store = Arc::new(Store::open(&store_root)?);
    let harness = Arc::new(store.harness()?);
    let llm = Arc::new(LlmRegistry::from_harness(&harness)?);
    let (finish_tx, mut finish_rx) = mpsc::channel::<FinishEvent>(64);
    let (tree, tree_task) =
        spawn_tree_with(store.clone(), llm.clone(), harness.clone(), Some(finish_tx));
    let shutdown = CancellationToken::new();
    let api = Api {
        store: store.clone(),
        harness: harness.clone(),
        llm: llm.clone(),
        tree: tree.clone(),
        shutdown: shutdown.clone(),
    };
    let tracker = TaskTracker::new();

    // Socket.
    let sock = socket_path(store.root(), &harness);
    if sock.exists() {
        if UnixStream::connect(&sock).await.is_ok() {
            anyhow::bail!("another daemon is already listening on {}", sock.display());
        }
        std::fs::remove_file(&sock)?;
    }
    let listener = UnixListener::bind(&sock)?;
    tracing::info!(socket = %sock.display(), pid = std::process::id(), "longed listening");
    {
        let api = api.clone();
        let shutdown = shutdown.clone();
        let tracker2 = tracker.clone();
        tracker.spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let api = api.clone();
                            tracker2.spawn(async move {
                                if let Err(e) = handle_conn(stream, api).await {
                                    tracing::debug!("connection ended: {e}");
                                }
                            });
                        }
                        Err(e) => tracing::warn!("accept failed: {e}"),
                    }
                }
            }
        });
    }

    // HTTP.
    let http_addr = harness
        .daemon
        .http
        .clone()
        .unwrap_or_else(|| "127.0.0.1:7878".into());
    if !http_addr.is_empty() {
        match tokio::net::TcpListener::bind(&http_addr).await {
            Ok(l) => {
                tracing::info!(addr = %http_addr, "http listening");
                let api = api.clone();
                let shutdown = shutdown.clone();
                tracker.spawn(async move {
                    let app = crate::cockpit::http::router(api);
                    let _ = axum::serve(l, app)
                        .with_graceful_shutdown(async move { shutdown.cancelled().await })
                        .await;
                });
            }
            Err(e) => tracing::warn!(addr = %http_addr, "http bind failed: {e}"),
        }
    }

    // Reflection on finish.
    {
        let api = api.clone();
        let shutdown = shutdown.clone();
        tracker.spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    ev = finish_rx.recv() => match ev {
                        Some(FinishEvent { id, outcome: Outcome::Done { .. } }) if api.harness.reflect.enabled => {
                            tracing::info!(session = %id, "reflect job");
                            match crate::reflect::run(&api.store, &api.llm, &api.harness, &id).await {
                                Ok(r) => tracing::info!(session = %id, branch = %r.branch, accepted = ?r.accepted, "reflect done"),
                                Err(e) => tracing::warn!(session = %id, "reflect failed: {e}"),
                            }
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
            }
        });
    }

    // Crons and sweeps: every 30 s, fire schedules that match the current minute once.
    {
        let api = api.clone();
        let shutdown = shutdown.clone();
        tracker.spawn(async move {
            let mut last_minute: Option<i64> = None;
            let schedules: Vec<(String, Schedule, CronAction)> = api
                .harness
                .cron
                .iter()
                .filter_map(|c| match Schedule::parse(&c.schedule) {
                    Ok(s) => Some((c.name.clone(), s, c.action.clone())),
                    Err(e) => {
                        tracing::warn!(cron = %c.name, "bad schedule: {e}");
                        None
                    }
                })
                .collect();
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tick.tick() => {
                        let now = chrono::Local::now();
                        let minute = now.timestamp().div_euclid(60);
                        if last_minute == Some(minute) { continue; }
                        last_minute = Some(minute);
                        let _ = api.tree.sweep().await;
                        for (name, s, action) in &schedules {
                            if s.matches(&now) {
                                tracing::info!(cron = %name, "fire");
                                run_cron(&api, action).await;
                            }
                        }
                    }
                }
            }
        });
    }

    shutdown.cancelled().await;
    tracing::info!("shutting down");
    tracker.close();
    let _ = tree.shutdown().await;
    let _ = tree_task.await;
    tracker.wait().await;
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

async fn run_cron(api: &Api, action: &CronAction) {
    match action {
        CronAction::Reflect => {
            // The most recent finished session not yet reflected.
            let done = crate::reflect::reflected_ids(&api.store);
            let list = api.tree.list().await.unwrap_or_default();
            let candidate = list
                .iter()
                .rev()
                .find(|s| matches!(s.outcome, Some(Outcome::Done { .. })) && !done.contains(&s.id));
            if let Some(s) = candidate {
                if let Err(e) = crate::reflect::run(&api.store, &api.llm, &api.harness, &s.id).await
                {
                    tracing::warn!("cron reflect failed: {e}");
                }
            }
        }
        CronAction::Task { task, workspace } => {
            let _ = api
                .call(Request::Spawn {
                    name: "cron".into(),
                    task: task.clone(),
                    workspace: workspace.clone(),
                    model: None,
                    budget: None,
                })
                .await;
        }
        CronAction::Cleanup { max_age_days } => {
            let cutoff = std::time::SystemTime::now()
                - Duration::from_secs(u64::from(*max_age_days) * 86_400);
            if let Ok(rd) = std::fs::read_dir(api.store.trajectories_dir()) {
                for e in rd.flatten() {
                    if e.metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t < cutoff)
                    {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
}

async fn handle_conn(stream: UnixStream, api: Api) -> anyhow::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => api.call(req).await,
            Err(e) => Response::err(format!("bad request: {e}")),
        };
        let mut out = serde_json::to_vec(&resp)?;
        out.push(b'\n');
        w.write_all(&out).await?;
    }
    Ok(())
}
