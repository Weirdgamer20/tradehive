use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub trade_id: String,
    pub symbol: String,
    pub strategy_id: String,
    pub entry: DateTime<Utc>,
    pub exit: Option<DateTime<Utc>>,
    pub pnl: f64,
    pub fees: f64,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeAutopsy {
    pub trade_id: String,
    pub outcome: String,
    pub pnl: f64,
    pub attribution: Vec<(String, f64)>,
    pub lessons: Vec<String>,
    pub fingerprint: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub id: String,
    pub kind: String,
    pub at: DateTime<Utc>,
    pub payload: String,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ExperienceStore {
    pub trades: Vec<TradeRecord>,
    pub autopsies: Vec<TradeAutopsy>,
    pub events: Vec<MemoryEvent>,
}
impl ExperienceStore {
    pub fn record_trade(&mut self, t: TradeRecord) {
        self.trades.push(t)
    }
    pub fn autopsy(&mut self, t: &TradeRecord) -> TradeAutopsy {
        let outcome = if t.pnl > 0.0 {
            "win"
        } else if t.pnl < 0.0 {
            "loss"
        } else {
            "flat"
        }
        .into();
        let lessons = vec![format!("{} outcome={}", t.strategy_id, outcome)];
        let mut h = Sha256::new();
        h.update(t.trade_id.as_bytes());
        h.update(t.pnl.to_le_bytes());
        let a = TradeAutopsy {
            trade_id: t.trade_id.clone(),
            outcome,
            pnl: t.pnl,
            attribution: vec![("pnl".into(), t.pnl)],
            lessons,
            fingerprint: format!("{:x}", h.finalize()),
        };
        self.autopsies.push(a.clone());
        a
    }
    pub fn retrieve(&self, symbol: &str, limit: usize) -> Vec<&TradeRecord> {
        self.trades
            .iter()
            .rev()
            .filter(|t| t.symbol == symbol)
            .take(limit)
            .collect()
    }
}
