//! The session tree actor: the single owner of every session that is not running,
//! of the message bus, and of the loop tasks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use super::state::{info_from_disk, Session};
use super::{
    Ctl, Message, Outcome, SessionDetail, SessionId, SessionInfo, SessionState, SpawnSpec, TreeCmd,
    TreeError, TreeHandle,
};
use crate::agent_loop::{self, LoopDeps};
use crate::config::Harness;
use crate::llm::LlmRegistry;
use crate::sandbox::Sandbox;
use crate::session::bus::Bus;
use crate::store::Store;

/// Raised by the tree when a session finishes; the daemon schedules reflection.
#[derive(Debug, Clone)]
pub struct FinishEvent {
    pub id: SessionId,
    pub outcome: Outcome,
}

enum Slot {
    Running {
        ctl: mpsc::Sender<Ctl>,
        info: watch::Receiver<SessionInfo>,
    },
    Idle {
        session: Box<Session>,
        since: Instant,
    },
    Paused {
        session: Box<Session>,
    },
    Offloaded {
        info: Box<SessionInfo>,
    },
}

struct Tree {
    deps: Arc<LoopDeps>,
    slots: HashMap<SessionId, Slot>,
    bus: Bus,
    tasks: JoinSet<(Box<Session>, Outcome)>,
    handle: TreeHandle,
    finished: Option<mpsc::Sender<FinishEvent>>,
    offload_after: Duration,
}

/// Start the tree actor. Sessions already on disk are registered as offloaded.
#[cfg(test)]
pub fn spawn_tree(store: Arc<Store>, llm: Arc<LlmRegistry>, harness: Arc<Harness>) -> TreeHandle {
    spawn_tree_with(store, llm, harness, None).0
}

pub fn spawn_tree_with(
    store: Arc<Store>,
    llm: Arc<LlmRegistry>,
    harness: Arc<Harness>,
    finished: Option<mpsc::Sender<FinishEvent>>,
) -> (TreeHandle, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(64);
    let handle = TreeHandle::new(tx);
    let sandbox: Arc<dyn Sandbox> = Arc::from(crate::sandbox::select(
        harness.sandbox.backend,
        harness.sandbox.mode,
    ));
    tracing::info!(backend = sandbox.name(), mode = ?harness.sandbox.mode, "sandbox");
    let offload_after = Duration::from_secs(harness.daemon.offload_after_minutes * 60);
    let deps = Arc::new(LoopDeps {
        store,
        llm,
        harness,
        sandbox,
        tree: handle.clone(),
    });
    let mut tree = Tree {
        deps,
        slots: HashMap::new(),
        bus: Bus::default(),
        tasks: JoinSet::new(),
        handle: handle.clone(),
        finished,
        offload_after,
    };
    for id in Session::list_on_disk(&tree.deps.store) {
        if let Some(info) = info_from_disk(&tree.deps.store, &id) {
            tree.slots.insert(
                id,
                Slot::Offloaded {
                    info: Box::new(info),
                },
            );
        }
    }
    let task = tokio::spawn(async move { tree.run(rx).await });
    (handle, task)
}

impl Tree {
    async fn run(mut self, mut rx: mpsc::Receiver<TreeCmd>) {
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(TreeCmd::Shutdown { reply }) => {
                        self.shutdown().await;
                        let _ = reply.send(());
                        return;
                    }
                    Some(cmd) => self.handle_cmd(cmd).await,
                    None => {
                        self.shutdown().await;
                        return;
                    }
                },
                Some(res) = self.tasks.join_next() => match res {
                    Ok((session, outcome)) => self.on_finished(session, outcome).await,
                    Err(e) => tracing::error!("session task panicked: {e}"),
                },
            }
        }
    }

    async fn handle_cmd(&mut self, cmd: TreeCmd) {
        match cmd {
            TreeCmd::Spawn { spec, reply } => {
                let _ = reply.send(self.spawn(spec));
            }
            TreeCmd::Send { msg, reply } => {
                let _ = reply.send(self.send(msg));
            }
            TreeCmd::Drain { id, reply } => {
                let _ = reply.send(self.bus.drain(&id));
            }
            TreeCmd::List { reply } => {
                let _ = reply.send(self.list());
            }
            TreeCmd::Get { id, tail, reply } => {
                let _ = reply.send(self.get(&id, tail));
            }
            TreeCmd::Pause { id, reply } => {
                let _ = reply.send(self.pause(&id).await);
            }
            TreeCmd::Resume { id, reply } => {
                let _ = reply.send(self.resume(&id));
            }
            TreeCmd::Kill { id, reply } => {
                let _ = reply.send(self.kill(&id).await);
            }
            TreeCmd::Offload { id, reply } => {
                let _ = reply.send(self.offload(&id));
            }
            TreeCmd::Sweep { reply } => {
                let _ = reply.send(self.sweep());
            }
            TreeCmd::Shutdown { .. } => {}
        }
    }

    fn start(&mut self, session: Box<Session>) {
        let id = session.id().clone();
        let (ctl_tx, ctl_rx) = mpsc::channel(8);
        let (info_tx, info_rx) = watch::channel(session.info(SessionState::Running));
        self.slots.insert(
            id,
            Slot::Running {
                ctl: ctl_tx,
                info: info_rx,
            },
        );
        let deps = self.deps.clone();
        self.tasks
            .spawn(async move { agent_loop::run(session, deps, ctl_rx, info_tx).await });
    }

    fn spawn(&mut self, spec: SpawnSpec) -> Result<SessionId, TreeError> {
        let h = &self.deps.harness;
        let mut session = Box::new(Session::new(&spec, h.model.model_ref(), h.budget));
        if let Some(parent) = &spec.parent {
            match self.slots.get_mut(parent) {
                Some(Slot::Idle { session: p, .. } | Slot::Paused { session: p }) => {
                    p.meta.children.push(session.id().clone());
                }
                Some(_) => {} // running parents record children through their own loop
                None => return Err(TreeError::NoSuchSession(parent.clone())),
            }
        }
        session
            .save(&self.deps.store)
            .map_err(|e| TreeError::Other(e.to_string()))?;
        let id = session.id().clone();
        tracing::info!(session = %id, name = %spec.name, "spawn");
        self.start(session);
        Ok(id)
    }

    fn send(&mut self, msg: Message) -> Result<(), TreeError> {
        let to = msg.to.clone();
        match self.slots.get(&to) {
            None => Err(TreeError::NoSuchSession(to)),
            Some(Slot::Running { .. } | Slot::Paused { .. }) => {
                self.bus.push(msg);
                Ok(())
            }
            Some(Slot::Idle { .. }) => {
                self.bus.push(msg);
                self.wake(&to)
            }
            Some(Slot::Offloaded { .. }) => {
                self.bus.push(msg);
                self.wake(&to)
            }
        }
    }

    /// Idle or offloaded -> running.
    fn wake(&mut self, id: &SessionId) -> Result<(), TreeError> {
        let slot = self
            .slots
            .remove(id)
            .ok_or_else(|| TreeError::NoSuchSession(id.clone()))?;
        let session = match slot {
            Slot::Idle { session, .. } => session,
            Slot::Offloaded { info } => match Session::load(&self.deps.store, id) {
                Ok(s) => {
                    let _ = self.bus.load(id, &self.deps.store.session_dir(id.as_str()));
                    Box::new(s)
                }
                Err(e) => {
                    self.slots.insert(id.clone(), Slot::Offloaded { info });
                    return Err(TreeError::Other(format!("cannot reload {id}: {e}")));
                }
            },
            other => {
                self.slots.insert(id.clone(), other);
                return Ok(());
            }
        };
        tracing::info!(session = %id, "wake");
        self.start(session);
        Ok(())
    }

    fn list(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .slots
            .values()
            .map(|s| match s {
                Slot::Running { info, .. } => info.borrow().clone(),
                Slot::Idle { session, .. } => session.info(SessionState::Idle),
                Slot::Paused { session } => session.info(SessionState::Paused),
                Slot::Offloaded { info } => (**info).clone(),
            })
            .collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        v
    }

    fn get(&self, id: &SessionId, tail: usize) -> Option<SessionDetail> {
        let slot = self.slots.get(id)?;
        let (info, task) = match slot {
            Slot::Running { info, .. } => {
                let info = info.borrow().clone();
                let task = Session::load(&self.deps.store, id)
                    .map(|s| s.meta.task)
                    .unwrap_or_default();
                (info, task)
            }
            Slot::Idle { session, .. } => {
                (session.info(SessionState::Idle), session.meta.task.clone())
            }
            Slot::Paused { session } => (
                session.info(SessionState::Paused),
                session.meta.task.clone(),
            ),
            Slot::Offloaded { info } => {
                let task = Session::load(&self.deps.store, id)
                    .map(|s| s.meta.task)
                    .unwrap_or_default();
                ((**info).clone(), task)
            }
        };
        let tail = self
            .deps
            .store
            .trajectory_tail(id.as_str(), tail)
            .unwrap_or_default();
        Some(SessionDetail {
            info,
            task,
            pending_messages: self.bus.pending(id),
            tail,
        })
    }

    async fn pause(&mut self, id: &SessionId) -> Result<(), TreeError> {
        match self.slots.get(id) {
            None => Err(TreeError::NoSuchSession(id.clone())),
            Some(Slot::Running { ctl, .. }) => {
                ctl.send(Ctl::Pause).await.map_err(|_| TreeError::Closed)?;
                Ok(())
            }
            Some(Slot::Idle { .. }) => {
                if let Some(Slot::Idle { session, .. }) = self.slots.remove(id) {
                    self.slots.insert(id.clone(), Slot::Paused { session });
                }
                Ok(())
            }
            Some(Slot::Paused { .. }) => Ok(()),
            Some(Slot::Offloaded { .. }) => Err(TreeError::WrongState(
                id.clone(),
                SessionState::Offloaded,
                "pause",
            )),
        }
    }

    fn resume(&mut self, id: &SessionId) -> Result<(), TreeError> {
        match self.slots.get(id) {
            None => Err(TreeError::NoSuchSession(id.clone())),
            Some(Slot::Paused { .. }) => {
                if let Some(Slot::Paused { session }) = self.slots.remove(id) {
                    self.start(session);
                }
                Ok(())
            }
            Some(Slot::Idle { .. } | Slot::Offloaded { .. }) => self.wake(id),
            Some(Slot::Running { .. }) => Ok(()),
        }
    }

    async fn kill(&mut self, id: &SessionId) -> Result<(), TreeError> {
        match self.slots.get(id) {
            None => Err(TreeError::NoSuchSession(id.clone())),
            Some(Slot::Running { ctl, .. }) => {
                ctl.send(Ctl::Kill).await.map_err(|_| TreeError::Closed)?;
                Ok(())
            }
            Some(_) => {
                // Not running: mark killed and park it.
                if let Some(slot) = self.slots.remove(id) {
                    let session = match slot {
                        Slot::Idle { session, .. } | Slot::Paused { session } => Some(session),
                        Slot::Offloaded { .. } => {
                            Session::load(&self.deps.store, id).ok().map(Box::new)
                        }
                        Slot::Running { .. } => None,
                    };
                    if let Some(mut s) = session {
                        s.meta.outcome = Some(Outcome::Killed);
                        let _ = s.save(&self.deps.store);
                        self.slots.insert(id.clone(), Slot::Paused { session: s });
                    }
                }
                Ok(())
            }
        }
    }

    fn offload(&mut self, id: &SessionId) -> Result<(), TreeError> {
        match self.slots.get(id) {
            None => Err(TreeError::NoSuchSession(id.clone())),
            Some(Slot::Running { .. }) => Err(TreeError::WrongState(
                id.clone(),
                SessionState::Running,
                "offload",
            )),
            Some(Slot::Offloaded { .. }) => Ok(()),
            Some(Slot::Idle { .. } | Slot::Paused { .. }) => {
                let Some(slot) = self.slots.remove(id) else {
                    return Ok(());
                };
                let mut session = match slot {
                    Slot::Idle { session, .. } | Slot::Paused { session } => session,
                    other => {
                        self.slots.insert(id.clone(), other);
                        return Ok(());
                    }
                };
                let store = &self.deps.store;
                session
                    .save(store)
                    .map_err(|e| TreeError::Other(e.to_string()))?;
                let _ = self.bus.save(id, &store.session_dir(id.as_str()));
                let info = session.info(SessionState::Offloaded);
                tracing::info!(session = %id, "offload");
                self.slots.insert(
                    id.clone(),
                    Slot::Offloaded {
                        info: Box::new(info),
                    },
                );
                Ok(())
            }
        }
    }

    fn sweep(&mut self) -> u32 {
        let stale: Vec<SessionId> = self
            .slots
            .iter()
            .filter_map(|(id, s)| match s {
                Slot::Idle { since, .. } if since.elapsed() >= self.offload_after => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();
        let mut n = 0;
        for id in stale {
            if self.offload(&id).is_ok() {
                n += 1;
            }
        }
        n
    }

    async fn on_finished(&mut self, mut session: Box<Session>, outcome: Outcome) {
        let id = session.id().clone();
        session.meta.outcome = Some(outcome.clone());
        if let Err(e) = session.save(&self.deps.store) {
            tracing::error!(session = %id, "save on finish failed: {e}");
        }
        tracing::info!(session = %id, %outcome, "finished");
        // Report to the parent, which wakes it if needed.
        if let Some(parent) = session.meta.parent.clone() {
            let body = match &outcome {
                Outcome::Done { summary } => format!("{} finished.\n{summary}", session.meta.name),
                other => format!(
                    "{} stopped: {other}\nlast note: {}",
                    session.meta.name,
                    session.meta.last_note.as_deref().unwrap_or("-")
                ),
            };
            if !matches!(outcome, Outcome::Paused) {
                let msg = Message::report(id.clone(), session.meta.name.clone(), parent, body);
                if let Err(e) = self.send(msg) {
                    tracing::warn!(session = %id, "cannot report to parent: {e}");
                }
            }
        }
        let slot = match outcome {
            Outcome::Paused | Outcome::Killed => Slot::Paused { session },
            _ => Slot::Idle {
                session,
                since: Instant::now(),
            },
        };
        self.slots.insert(id.clone(), slot);
        if let Some(tx) = &self.finished {
            let _ = tx.send(FinishEvent { id, outcome }).await;
        }
    }

    async fn shutdown(&mut self) {
        // Ask every running loop to pause, wait for them, persist everything.
        let running: Vec<SessionId> = self
            .slots
            .iter()
            .filter(|(_, s)| matches!(s, Slot::Running { .. }))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &running {
            if let Some(Slot::Running { ctl, .. }) = self.slots.get(id) {
                let _ = ctl.send(Ctl::Pause).await;
            }
        }
        while let Some(res) = self.tasks.join_next().await {
            if let Ok((session, outcome)) = res {
                self.on_finished(session, outcome).await;
            }
        }
        let ids: Vec<SessionId> = self.slots.keys().cloned().collect();
        for id in ids {
            let _ = self.offload(&id);
        }
        let _ = &self.handle;
    }
}
