use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub at: DateTime<Utc>,
    pub process: bool,
    pub market_data: bool,
    pub broker: bool,
    pub database: bool,
    pub gate3a_open: bool,
}
impl Health {
    pub fn ready(&self) -> bool {
        self.process && self.market_data && self.broker && self.database && self.gate3a_open
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub at: DateTime<Utc>,
}
#[derive(Debug, Default)]
pub struct Operations {
    pub metrics: Vec<Metric>,
}
impl Operations {
    pub fn metric(&mut self, name: &str, value: f64) {
        self.metrics.push(Metric {
            name: name.into(),
            value,
            at: Utc::now(),
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ControlCommand {
    Pause,
    Resume,
    Flatten,
    Stop,
    Health,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResult {
    pub command: ControlCommand,
    pub accepted: bool,
    pub reason: String,
    pub at: DateTime<Utc>,
}
pub fn authorize(cmd: ControlCommand, health: &Health) -> ControlResult {
    let ok = match cmd {
        ControlCommand::Resume => health.ready(),
        ControlCommand::Pause
        | ControlCommand::Flatten
        | ControlCommand::Stop
        | ControlCommand::Health => true,
    };
    ControlResult {
        command: cmd,
        accepted: ok,
        reason: if ok {
            "authorized".into()
        } else {
            "health gate closed".into()
        },
        at: Utc::now(),
    }
}
