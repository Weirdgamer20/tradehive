use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub name: String,
    pub state: HealthState,
    pub checked_at: DateTime<Utc>,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelSnapshot {
    pub at: DateTime<Utc>,
    pub trading_enabled: bool,
    pub gate_open: bool,
    pub components: Vec<ComponentHealth>,
}
impl SentinelSnapshot {
    pub fn healthy(&self) -> bool {
        self.trading_enabled
            && self.gate_open
            && self
                .components
                .iter()
                .all(|x| x.state == HealthState::Healthy)
    }
}
#[derive(Debug, Error)]
pub enum SentinelError {
    #[error("trading cannot be enabled while sentinel is unhealthy")]
    Unhealthy,
}
#[derive(Debug, Default)]
pub struct Sentinel {
    components: Vec<ComponentHealth>,
}
impl Sentinel {
    pub fn set(&mut self, name: &str, state: HealthState, detail: &str) {
        if let Some(x) = self.components.iter_mut().find(|x| x.name == name) {
            x.state = state;
            x.checked_at = Utc::now();
            x.detail = detail.into()
        } else {
            self.components.push(ComponentHealth {
                name: name.into(),
                state,
                checked_at: Utc::now(),
                detail: detail.into(),
            });
        }
    }
    pub fn snapshot(&self, trading_enabled: bool, gate_open: bool) -> SentinelSnapshot {
        SentinelSnapshot {
            at: Utc::now(),
            trading_enabled,
            gate_open,
            components: self.components.clone(),
        }
    }
    pub fn authorize_trading(&self) -> Result<(), SentinelError> {
        const REQUIRED: &[&str] = &["market_data", "broker", "database", "risk", "state"];
        let ready = !self.components.is_empty()
            && REQUIRED.iter().all(|name| {
                self.components
                    .iter()
                    .any(|x| x.name == *name && x.state == HealthState::Healthy)
            })
            && self
                .components
                .iter()
                .all(|x| x.state == HealthState::Healthy);
        if ready {
            Ok(())
        } else {
            Err(SentinelError::Unhealthy)
        }
    }
}
