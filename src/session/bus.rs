//! The message bus: parent <-> child, siblings, and the human. Queues are owned by
//! the tree and persisted per session so an offloaded session loses nothing.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::SessionId;
use crate::store::{write_atomic, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// Free text.
    Text,
    /// A child's end-of-task report.
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// `None` when sent by a human (CLI, cockpit, HTTP).
    pub from: Option<SessionId>,
    pub from_name: String,
    pub to: SessionId,
    pub kind: MessageKind,
    pub body: String,
    pub ts: String,
}

impl Message {
    pub fn text(
        from: Option<SessionId>,
        from_name: impl Into<String>,
        to: SessionId,
        body: impl Into<String>,
    ) -> Self {
        Self {
            from,
            from_name: from_name.into(),
            to,
            kind: MessageKind::Text,
            body: body.into(),
            ts: super::now_rfc3339(),
        }
    }

    pub fn report(
        from: SessionId,
        from_name: impl Into<String>,
        to: SessionId,
        body: impl Into<String>,
    ) -> Self {
        Self {
            from: Some(from),
            from_name: from_name.into(),
            to,
            kind: MessageKind::Report,
            body: body.into(),
            ts: super::now_rfc3339(),
        }
    }

    /// How a message reads once injected in a context.
    pub fn render(&self) -> String {
        let who = match &self.from {
            Some(id) => format!("{} ({id})", self.from_name),
            None => "human".to_string(),
        };
        match self.kind {
            MessageKind::Text => format!("[message from {who} at {}]\n{}", self.ts, self.body),
            MessageKind::Report => format!("[report from {who} at {}]\n{}", self.ts, self.body),
        }
    }
}

/// All inboxes. Bounded per session so a chatty child cannot fill memory.
#[derive(Debug, Default)]
pub struct Bus {
    queues: HashMap<SessionId, VecDeque<Message>>,
}

pub const MAX_QUEUE: usize = 256;

impl Bus {
    /// Enqueue; the oldest message is dropped past `MAX_QUEUE`.
    pub fn push(&mut self, msg: Message) {
        let q = self.queues.entry(msg.to.clone()).or_default();
        if q.len() >= MAX_QUEUE {
            q.pop_front();
        }
        q.push_back(msg);
    }

    pub fn drain(&mut self, id: &SessionId) -> Vec<Message> {
        self.queues
            .get_mut(id)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn pending(&self, id: &SessionId) -> usize {
        self.queues.get(id).map_or(0, VecDeque::len)
    }

    /// Persist one inbox (called on offload) and forget it in RAM.
    pub fn save(&mut self, id: &SessionId, dir: &Path) -> Result<(), StoreError> {
        let msgs: Vec<Message> = self.drain(id);
        let path = dir.join("inbox.json");
        write_atomic(&path, &serde_json::to_vec(&msgs)?)
    }

    /// Reload an inbox from disk (called on wake), appending to whatever arrived since.
    pub fn load(&mut self, id: &SessionId, dir: &Path) -> Result<usize, StoreError> {
        let path = dir.join("inbox.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(0);
        };
        let msgs: Vec<Message> = serde_json::from_slice(&bytes)?;
        let n = msgs.len();
        let fresh = self.drain(id);
        for m in msgs.into_iter().chain(fresh) {
            self.push(m);
        }
        let _ = std::fs::remove_file(&path);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_drain_order_and_bound() {
        let mut bus = Bus::default();
        let to = SessionId::parse("aaaa0000").unwrap();
        for i in 0..(MAX_QUEUE + 5) {
            bus.push(Message::text(None, "h", to.clone(), format!("m{i}")));
        }
        assert_eq!(bus.pending(&to), MAX_QUEUE);
        let all = bus.drain(&to);
        assert_eq!(all[0].body, "m5");
        assert_eq!(all.last().unwrap().body, format!("m{}", MAX_QUEUE + 4));
        assert_eq!(bus.pending(&to), 0);
    }

    #[test]
    fn save_and_load_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let mut bus = Bus::default();
        let to = SessionId::parse("bbbb0000").unwrap();
        bus.push(Message::text(None, "h", to.clone(), "first"));
        bus.save(&to, d.path()).unwrap();
        assert_eq!(bus.pending(&to), 0);
        bus.push(Message::text(None, "h", to.clone(), "second"));
        assert_eq!(bus.load(&to, d.path()).unwrap(), 1);
        let msgs = bus.drain(&to);
        assert_eq!(
            msgs.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(!d.path().join("inbox.json").exists());
    }

    #[test]
    fn render_mentions_sender() {
        let from = SessionId::parse("cccc0000").unwrap();
        let to = SessionId::parse("dddd0000").unwrap();
        let r = Message::report(from, "parser", to, "all tests pass").render();
        assert!(r.starts_with("[report from parser (cccc0000)"));
    }
}
