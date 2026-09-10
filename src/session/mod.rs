//! Sessions: the unit of work. A tree of them lives in the daemon.

pub mod bus;
pub mod state;
pub mod tree;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::budget::{BudgetCounters, Exhausted};
use crate::config::{BudgetCfg, ModelRef};
pub use bus::Message;

/// Eight hex characters. Cheap to clone, impossible to confuse with a path or a name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Box<str>);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid session id `{0}`: expected 4 to 16 hex characters")]
pub struct BadSessionId(String);

impl SessionId {
    pub fn fresh() -> Self {
        let u = uuid::Uuid::new_v4().simple().to_string();
        Self(u[..8].into())
    }

    pub fn parse(s: &str) -> Result<Self, BadSessionId> {
        let ok = (4..=16).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit());
        if ok {
            Ok(Self(s.to_ascii_lowercase().into()))
        } else {
            Err(BadSessionId(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle of a session as seen from the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// The loop task is alive.
    Running,
    /// In RAM, wakeable by a message.
    Idle,
    /// Serialized on disk, reloaded on the first message.
    Offloaded,
    /// In RAM, not wakeable until `resume`.
    Paused,
}

/// How a run of the loop ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Outcome {
    Done { summary: String },
    Exhausted { reason: Exhausted },
    Killed,
    Paused,
    Error { message: String },
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done { summary } => write!(f, "done: {summary}"),
            Self::Exhausted { reason } => write!(f, "exhausted: {reason}"),
            Self::Killed => f.write_str("killed"),
            Self::Paused => f.write_str("paused"),
            Self::Error { message } => write!(f, "error: {message}"),
        }
    }
}

/// What it takes to create a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub name: String,
    pub task: String,
    pub workspace: PathBuf,
    pub parent: Option<SessionId>,
    pub model: Option<ModelRef>,
    pub budget: Option<BudgetCfg>,
}

/// Summary line for listings and the cockpit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub parent: Option<SessionId>,
    pub name: String,
    pub state: SessionState,
    pub outcome: Option<Outcome>,
    pub model: ModelRef,
    pub counters: BudgetCounters,
    pub budget: BudgetCfg,
    pub last_note: Option<String>,
    pub last_verify_ok: Option<bool>,
    pub children: u32,
    pub created: String,
    pub updated: String,
    pub workspace: PathBuf,
}

/// Full detail: info plus the task and the tail of the trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub info: SessionInfo,
    pub task: String,
    pub pending_messages: usize,
    pub tail: Vec<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("no such session {0}")]
    NoSuchSession(SessionId),
    #[error("session {0} is {1:?}; cannot {2}")]
    WrongState(SessionId, SessionState, &'static str),
    #[error("tree is shut down")]
    Closed,
    #[error("{0}")]
    Other(String),
}

impl<T> From<mpsc::error::SendError<T>> for TreeError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        Self::Closed
    }
}

impl From<oneshot::error::RecvError> for TreeError {
    fn from(_: oneshot::error::RecvError) -> Self {
        Self::Closed
    }
}

/// Commands accepted by the tree actor.
#[derive(Debug)]
pub enum TreeCmd {
    Spawn {
        spec: SpawnSpec,
        reply: oneshot::Sender<Result<SessionId, TreeError>>,
    },
    Send {
        msg: Message,
        reply: oneshot::Sender<Result<(), TreeError>>,
    },
    Drain {
        id: SessionId,
        reply: oneshot::Sender<Vec<Message>>,
    },
    List {
        reply: oneshot::Sender<Vec<SessionInfo>>,
    },
    Get {
        id: SessionId,
        tail: usize,
        reply: oneshot::Sender<Option<SessionDetail>>,
    },
    Pause {
        id: SessionId,
        reply: oneshot::Sender<Result<(), TreeError>>,
    },
    Resume {
        id: SessionId,
        reply: oneshot::Sender<Result<(), TreeError>>,
    },
    Kill {
        id: SessionId,
        reply: oneshot::Sender<Result<(), TreeError>>,
    },
    Offload {
        id: SessionId,
        reply: oneshot::Sender<Result<(), TreeError>>,
    },
    /// Offload every idle session older than the configured delay.
    Sweep {
        reply: oneshot::Sender<u32>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// Cheap, cloneable handle to the tree actor.
#[derive(Debug, Clone)]
pub struct TreeHandle {
    tx: mpsc::Sender<TreeCmd>,
}

impl TreeHandle {
    pub fn new(tx: mpsc::Sender<TreeCmd>) -> Self {
        Self { tx }
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> TreeCmd,
    ) -> Result<T, TreeError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(make(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn spawn(&self, spec: SpawnSpec) -> Result<SessionId, TreeError> {
        self.ask(|reply| TreeCmd::Spawn { spec, reply }).await?
    }

    pub async fn send(&self, msg: Message) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Send { msg, reply }).await?
    }

    pub async fn drain(&self, id: SessionId) -> Result<Vec<Message>, TreeError> {
        self.ask(|reply| TreeCmd::Drain { id, reply }).await
    }

    pub async fn list(&self) -> Result<Vec<SessionInfo>, TreeError> {
        self.ask(|reply| TreeCmd::List { reply }).await
    }

    pub async fn get(
        &self,
        id: SessionId,
        tail: usize,
    ) -> Result<Option<SessionDetail>, TreeError> {
        self.ask(|reply| TreeCmd::Get { id, tail, reply }).await
    }

    pub async fn pause(&self, id: SessionId) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Pause { id, reply }).await?
    }

    pub async fn resume(&self, id: SessionId) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Resume { id, reply }).await?
    }

    pub async fn kill(&self, id: SessionId) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Kill { id, reply }).await?
    }

    pub async fn offload(&self, id: SessionId) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Offload { id, reply }).await?
    }

    pub async fn sweep(&self) -> Result<u32, TreeError> {
        self.ask(|reply| TreeCmd::Sweep { reply }).await
    }

    pub async fn shutdown(&self) -> Result<(), TreeError> {
        self.ask(|reply| TreeCmd::Shutdown { reply }).await
    }
}

/// Control messages from the tree to a running loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctl {
    Pause,
    Kill,
}

pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = SessionId::fresh();
        assert_eq!(id.as_str().len(), 8);
        assert_eq!(SessionId::parse("ABCD1234").unwrap().as_str(), "abcd1234");
        assert!(SessionId::parse("xyz").is_err());
        assert!(SessionId::parse("../x").is_err());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
    }

    #[test]
    fn outcome_serializes_tagged() {
        let o = Outcome::Done {
            summary: "ok".into(),
        };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(v["kind"], "done");
        assert_eq!(v["summary"], "ok");
    }
}
