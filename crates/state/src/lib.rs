use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gate3A {
    Open,
    Closed,
    Blocked,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub revision: u64,
    pub at: DateTime<Utc>,
    pub gate: Gate3A,
    pub trading_enabled: bool,
    pub reason: String,
}
#[derive(Debug, Error)]
pub enum StateError {
    #[error("stale revision")]
    Stale,
    #[error("invalid transition")]
    Invalid,
}
#[derive(Debug)]
pub struct StateAuthority {
    snapshot: StateSnapshot,
}
impl Default for StateAuthority {
    fn default() -> Self {
        Self::new()
    }
}
impl StateAuthority {
    pub fn new() -> Self {
        Self {
            snapshot: StateSnapshot {
                revision: 0,
                at: Utc::now(),
                gate: Gate3A::Closed,
                trading_enabled: false,
                reason: "startup".into(),
            },
        }
    }
    pub fn snapshot(&self) -> StateSnapshot {
        self.snapshot.clone()
    }
    pub fn transition(
        &mut self,
        expected: u64,
        gate: Gate3A,
        enabled: bool,
        reason: String,
    ) -> Result<StateSnapshot, StateError> {
        if expected != self.snapshot.revision {
            return Err(StateError::Stale);
        }
        if enabled && gate != Gate3A::Open {
            return Err(StateError::Invalid);
        }
        self.snapshot.revision += 1;
        self.snapshot.at = Utc::now();
        self.snapshot.gate = gate;
        self.snapshot.trading_enabled = enabled;
        self.snapshot.reason = reason;
        Ok(self.snapshot.clone())
    }
}
