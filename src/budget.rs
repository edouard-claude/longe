//! The grind: `done()` is refused before the minimum, the session is force-finished
//! at the maximum.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::BudgetCfg;

/// Why `done()` was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneRefusal {
    MinSeconds { elapsed: u64, min: u64 },
    MinTurns { turns: u32, min: u32 },
}

impl std::fmt::Display for DoneRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MinSeconds { elapsed, min } => write!(
                f,
                "done() refused: minimum wall-clock budget not spent ({elapsed}s of {min}s). Keep working: harden, test more, improve."
            ),
            Self::MinTurns { turns, min } => write!(
                f,
                "done() refused: minimum turn budget not spent ({turns} of {min} turns). Keep working."
            ),
        }
    }
}

/// Why the session is force-finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exhausted {
    MaxTurns,
    MaxTokens,
}

impl std::fmt::Display for Exhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxTurns => f.write_str("max turns reached"),
            Self::MaxTokens => f.write_str("max tokens reached"),
        }
    }
}

/// Persisted counters (the part that survives a daemon restart).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetCounters {
    pub turns: u32,
    pub tokens: u64,
    /// Share of `tokens` the provider reported as reasoning (informational).
    #[serde(default)]
    pub reasoning_tokens: u64,
    pub done_refused: u32,
    /// Seconds spent in earlier runs of this session (before a pause/offload).
    pub elapsed_before: u64,
}

/// Live budget of one session.
#[derive(Debug, Clone)]
pub struct Budget {
    cfg: BudgetCfg,
    started: Instant,
    counters: BudgetCounters,
}

impl Budget {
    #[cfg(test)]
    pub fn new(cfg: BudgetCfg) -> Self {
        Self {
            cfg,
            started: Instant::now(),
            counters: BudgetCounters::default(),
        }
    }

    /// Resume from persisted counters; the clock restarts now.
    pub fn resume(cfg: BudgetCfg, counters: BudgetCounters) -> Self {
        Self {
            cfg,
            started: Instant::now(),
            counters,
        }
    }

    #[cfg(test)]
    fn with_started(mut self, started: Instant) -> Self {
        self.started = started;
        self
    }

    /// Counters to persist: elapsed time is folded in.
    pub fn freeze(&self) -> BudgetCounters {
        BudgetCounters {
            elapsed_before: self.elapsed().as_secs(),
            ..self.counters
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed() + Duration::from_secs(self.counters.elapsed_before)
    }

    #[cfg(test)]
    pub fn done_refused(&self) -> u32 {
        self.counters.done_refused
    }

    /// Tokens spent outside the main turn (llm.query leaves).
    pub fn tick_tokens(&mut self, tokens: u64) {
        self.counters.tokens = self.counters.tokens.saturating_add(tokens);
    }

    /// One LLM round trip happened.
    pub fn tick(&mut self, tokens: u64) {
        self.counters.turns = self.counters.turns.saturating_add(1);
        self.counters.tokens = self.counters.tokens.saturating_add(tokens);
    }

    /// Reasoning share reported by the provider for the round trips already ticked.
    pub fn tick_reasoning(&mut self, tokens: u64) {
        self.counters.reasoning_tokens = self.counters.reasoning_tokens.saturating_add(tokens);
    }

    /// Is `done()` allowed now? A refusal is counted.
    pub fn check_done(&mut self) -> Result<(), DoneRefusal> {
        let elapsed = self.elapsed().as_secs();
        if elapsed < self.cfg.min_seconds {
            self.counters.done_refused += 1;
            return Err(DoneRefusal::MinSeconds {
                elapsed,
                min: self.cfg.min_seconds,
            });
        }
        if self.counters.turns < self.cfg.min_turns {
            self.counters.done_refused += 1;
            return Err(DoneRefusal::MinTurns {
                turns: self.counters.turns,
                min: self.cfg.min_turns,
            });
        }
        Ok(())
    }

    pub fn exhausted(&self) -> Option<Exhausted> {
        if self.counters.turns >= self.cfg.max_turns {
            Some(Exhausted::MaxTurns)
        } else if self.counters.tokens >= self.cfg.max_tokens {
            Some(Exhausted::MaxTokens)
        } else {
            None
        }
    }

    /// One line for the system prompt.
    pub fn status_line(&self) -> String {
        let e = self.elapsed().as_secs();
        format!(
            "budget: {e}s elapsed (min {min_s}s) | turn {t}/{max_t} (min {min_t}) | tokens {tok}/{max_tok} | done() refused {r} times",
            min_s = self.cfg.min_seconds,
            t = self.counters.turns,
            max_t = self.cfg.max_turns,
            min_t = self.cfg.min_turns,
            tok = self.counters.tokens,
            max_tok = self.cfg.max_tokens,
            r = self.counters.done_refused,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BudgetCfg {
        BudgetCfg {
            min_seconds: 60,
            min_turns: 3,
            max_turns: 5,
            max_tokens: 1000,
        }
    }

    #[test]
    fn done_refused_before_min_seconds() {
        let mut b = Budget::new(cfg());
        for _ in 0..3 {
            b.tick(10);
        }
        let err = b.check_done().unwrap_err();
        assert!(matches!(err, DoneRefusal::MinSeconds { min: 60, .. }));
        assert_eq!(b.done_refused(), 1);
    }

    #[test]
    fn done_refused_before_min_turns() {
        let mut b = Budget::new(cfg()).with_started(Instant::now() - Duration::from_secs(120));
        b.tick(10);
        let err = b.check_done().unwrap_err();
        assert_eq!(err, DoneRefusal::MinTurns { turns: 1, min: 3 });
    }

    #[test]
    fn done_allowed_after_minimums() {
        let mut b = Budget::new(cfg()).with_started(Instant::now() - Duration::from_secs(120));
        for _ in 0..3 {
            b.tick(10);
        }
        assert!(b.check_done().is_ok());
        assert_eq!(b.done_refused(), 0);
    }

    #[test]
    fn exhaustion_by_turns_then_tokens() {
        let mut b = Budget::new(cfg());
        assert_eq!(b.exhausted(), None);
        for _ in 0..5 {
            b.tick(1);
        }
        assert_eq!(b.exhausted(), Some(Exhausted::MaxTurns));
        let mut b = Budget::new(cfg());
        b.tick(2000);
        assert_eq!(b.exhausted(), Some(Exhausted::MaxTokens));
    }

    #[test]
    fn resume_keeps_elapsed_and_counters() {
        let b = Budget::new(cfg()).with_started(Instant::now() - Duration::from_secs(50));
        let frozen = b.freeze();
        assert!(frozen.elapsed_before >= 50);
        let r = Budget::resume(cfg(), frozen);
        assert!(r.elapsed().as_secs() >= 50);
    }
}
