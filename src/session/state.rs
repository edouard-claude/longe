//! One session's persistent state: metadata, L1 history, and (lazily) its Lua VM.
//! Everything is written under `sessions/<id>/` every turn.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{now_rfc3339, Outcome, SessionId, SessionInfo, SessionState, SpawnSpec};
use crate::budget::BudgetCounters;
use crate::config::{BudgetCfg, ModelRef};
use crate::llm::Role;
use crate::repl::Repl;
use crate::store::{write_atomic, Store, StoreError};
use crate::verify::VerifyOutcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: String,
    pub ts: String,
}

/// `meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    pub parent: Option<SessionId>,
    pub name: String,
    pub task: String,
    pub workspace: PathBuf,
    pub model: ModelRef,
    pub budget: BudgetCfg,
    #[serde(default)]
    pub counters: BudgetCounters,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub last_note: Option<String>,
    #[serde(default)]
    pub last_verify: Option<VerifyOutcome>,
    #[serde(default)]
    pub compactions: u32,
    #[serde(default)]
    pub spawned: u32,
    #[serde(default)]
    pub children: Vec<SessionId>,
    pub created: String,
    pub updated: String,
}

/// A session in RAM.
#[derive(Debug)]
pub struct Session {
    pub meta: SessionMeta,
    pub history: Vec<Turn>,
    /// Built on the first run and kept while idle; dropped on offload.
    pub repl: Option<Repl>,
}

impl Session {
    pub fn new(spec: &SpawnSpec, default_model: ModelRef, default_budget: BudgetCfg) -> Self {
        let now = now_rfc3339();
        let id = SessionId::fresh();
        Self {
            meta: SessionMeta {
                id,
                parent: spec.parent.clone(),
                name: spec.name.clone(),
                task: spec.task.clone(),
                workspace: spec.workspace.clone(),
                model: spec.model.clone().unwrap_or(default_model),
                budget: spec.budget.unwrap_or(default_budget),
                counters: BudgetCounters::default(),
                outcome: None,
                last_note: None,
                last_verify: None,
                compactions: 0,
                spawned: 0,
                children: Vec::new(),
                created: now.clone(),
                updated: now,
            },
            history: Vec::new(),
            repl: None,
        }
    }

    pub fn id(&self) -> &SessionId {
        &self.meta.id
    }

    pub fn dir(&self, store: &Store) -> PathBuf {
        store.session_dir(self.meta.id.as_str())
    }

    pub fn state_lua_path(dir: &Path) -> PathBuf {
        dir.join("state.lua")
    }

    pub fn push_turn(&mut self, role: Role, content: impl Into<String>) {
        self.history.push(Turn {
            role,
            content: content.into(),
            ts: now_rfc3339(),
        });
    }

    /// Persist meta, history and the Lua state.
    pub fn save(&mut self, store: &Store) -> Result<(), StoreError> {
        self.meta.updated = now_rfc3339();
        let dir = self.dir(store);
        std::fs::create_dir_all(&dir).map_err(|source| StoreError::Io {
            path: dir.clone(),
            source,
        })?;
        write_atomic(
            &dir.join("meta.json"),
            &serde_json::to_vec_pretty(&self.meta)?,
        )?;
        write_atomic(
            &dir.join("history.json"),
            &serde_json::to_vec(&self.history)?,
        )?;
        if let Some(repl) = &self.repl {
            let src = repl.dump_state().map_err(|e| StoreError::Io {
                path: Self::state_lua_path(&dir),
                source: std::io::Error::other(e.to_string()),
            })?;
            write_atomic(&Self::state_lua_path(&dir), src.as_bytes())?;
        }
        Ok(())
    }

    /// Load from disk. The Lua VM is rebuilt lazily by the loop from `state.lua`.
    pub fn load(store: &Store, id: &SessionId) -> Result<Self, StoreError> {
        let dir = store.session_dir(id.as_str());
        let meta_path = dir.join("meta.json");
        let meta_bytes = std::fs::read(&meta_path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => StoreError::NotFound(format!("session {id}")),
            _ => StoreError::Io {
                path: meta_path.clone(),
                source,
            },
        })?;
        let meta: SessionMeta = serde_json::from_slice(&meta_bytes)?;
        let hist_path = dir.join("history.json");
        let history = match std::fs::read(&hist_path) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(StoreError::Io {
                    path: hist_path,
                    source,
                })
            }
        };
        Ok(Self {
            meta,
            history,
            repl: None,
        })
    }

    /// Saved Lua state, if any.
    pub fn saved_state(store: &Store, id: &SessionId) -> Option<String> {
        std::fs::read_to_string(Self::state_lua_path(&store.session_dir(id.as_str()))).ok()
    }

    /// Ids of every session persisted on disk.
    pub fn list_on_disk(store: &Store) -> Vec<SessionId> {
        let Ok(rd) = std::fs::read_dir(store.sessions_dir()) else {
            return Vec::new();
        };
        let mut ids: Vec<SessionId> = rd
            .flatten()
            .filter(|e| e.path().join("meta.json").exists())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|s| SessionId::parse(s).ok())
            })
            .collect();
        ids.sort();
        ids
    }

    pub fn info(&self, state: SessionState) -> SessionInfo {
        SessionInfo {
            id: self.meta.id.clone(),
            parent: self.meta.parent.clone(),
            name: self.meta.name.clone(),
            state,
            outcome: self.meta.outcome.clone(),
            model: self.meta.model.clone(),
            counters: self.meta.counters,
            budget: self.meta.budget,
            last_note: self.meta.last_note.clone(),
            last_verify_ok: self.meta.last_verify.as_ref().map(|v| v.ok),
            children: u32::try_from(self.meta.children.len()).unwrap_or(u32::MAX),
            created: self.meta.created.clone(),
            updated: self.meta.updated.clone(),
            workspace: self.meta.workspace.clone(),
        }
    }
}

/// Info for a session that is on disk only.
pub fn info_from_disk(store: &Store, id: &SessionId) -> Option<SessionInfo> {
    Session::load(store, id)
        .ok()
        .map(|s| s.info(SessionState::Offloaded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            name: "root".into(),
            task: "build it".into(),
            workspace: PathBuf::from("/tmp/ws"),
            parent: None,
            model: None,
            budget: None,
        }
    }

    #[test]
    fn save_load_round_trip_without_repl() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path().join("store")).unwrap();
        let model = ModelRef {
            provider: "p".into(),
            name: "m".into(),
        };
        let mut s = Session::new(&spec(), model.clone(), BudgetCfg::default());
        s.push_turn(Role::User, "task");
        s.push_turn(Role::Assistant, "```lua\nx=1\n```");
        s.meta.counters.turns = 3;
        s.meta.last_note = Some("note".into());
        s.save(&store).unwrap();
        let back = Session::load(&store, s.id()).unwrap();
        assert_eq!(back.history.len(), 2);
        assert_eq!(back.meta.counters.turns, 3);
        assert_eq!(back.meta.model, model);
        assert_eq!(back.meta.last_note.as_deref(), Some("note"));
        assert_eq!(Session::list_on_disk(&store), vec![s.id().clone()]);
        assert!(Session::saved_state(&store, s.id()).is_none());
        let info = back.info(SessionState::Idle);
        assert_eq!(info.name, "root");
        assert_eq!(info.state, SessionState::Idle);
    }

    #[test]
    fn loading_unknown_session_is_not_found() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path().join("store")).unwrap();
        let err = Session::load(&store, &SessionId::parse("deadbeef").unwrap()).unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }
}
