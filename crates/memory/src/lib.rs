use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub trade_id: String,
    pub symbol: String,
    pub strategy_id: String,
    pub session_id: String,
    pub entry: DateTime<Utc>,
    pub exit: Option<DateTime<Utc>>,
    pub pnl: f64,
    pub fees: f64,
    pub reason: String,
    // TCA (Transaction Cost Analysis) Fields
    #[serde(default)]
    pub signal_price: Option<f64>,
    #[serde(default)]
    pub quote_spread_bps: Option<f64>,
    #[serde(default)]
    pub entry_fill_price: Option<f64>,
    #[serde(default)]
    pub exit_fill_price: Option<f64>,
    #[serde(default)]
    pub slippage_bps: Option<f64>,
    #[serde(default)]
    pub latency_ms: Option<i64>,
    #[serde(default)]
    pub regime_at_entry: Option<String>,
    #[serde(default)]
    pub exit_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeAutopsy {
    pub trade_id: String,
    pub outcome: String,
    pub pnl: f64,
    pub attribution: Vec<(String, f64)>,
    pub lessons: Vec<String>,
    pub signal_quality_score: f64,
    pub execution_quality_score: f64,
    pub option_selection_score: f64,
    pub regime_compatibility: f64,
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
    pub success_memory: Vec<TradeRecord>,
    pub failure_memory: Vec<TradeRecord>,
    pub regime_memory: Vec<(String, f64)>, // regime -> average pnl
    pub execution_memory: Vec<(String, f64)>, // symbol -> average slippage_bps
}

impl ExperienceStore {
    pub fn record_trade(&mut self, t: TradeRecord) {
        if t.pnl > 0.0 {
            self.success_memory.push(t.clone());
        } else if t.pnl < 0.0 {
            self.failure_memory.push(t.clone());
        }
        if let Some(regime) = &t.regime_at_entry {
            self.regime_memory.push((regime.clone(), t.pnl));
        }
        if let Some(slippage) = t.slippage_bps {
            self.execution_memory.push((t.symbol.clone(), slippage));
        }
        self.trades.push(t);
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

        let slippage = t.slippage_bps.unwrap_or(0.0);
        let spread = t.quote_spread_bps.unwrap_or(0.0);
        let notional = t
            .entry_fill_price
            .unwrap_or_else(|| t.signal_price.unwrap_or(1.0))
            * 100.0;
        let exec_friction = (slippage / 10_000.0) * notional + t.fees;
        let exec_attribution = -exec_friction;
        let alpha_attribution = t.pnl + exec_friction;

        let exec_score =
            (1.0 - (slippage / 50.0).clamp(0.0, 1.0) - (spread / 100.0).clamp(0.0, 0.5))
                .clamp(0.0, 1.0);
        let signal_score = ((t.pnl / notional.max(10.0)).clamp(-1.0, 1.0) + 1.0) / 2.0;
        let option_selection_score = (1.0 - (spread / 200.0)).clamp(0.0, 1.0);

        let regime_score = if let Some(ref reg) = t.regime_at_entry {
            let same_regime: Vec<&TradeRecord> = self
                .trades
                .iter()
                .filter(|tr| tr.regime_at_entry.as_ref() == Some(reg))
                .collect();
            if same_regime.is_empty() {
                0.5
            } else {
                let wins = same_regime.iter().filter(|tr| tr.pnl > 0.0).count();
                (wins as f64 / same_regime.len() as f64).clamp(0.0, 1.0)
            }
        } else if t.pnl > 0.0 {
            0.75
        } else {
            0.25
        };

        let lessons = vec![
            format!("{} outcome={} pnl={:.2}", t.strategy_id, outcome, t.pnl),
            format!(
                "Execution quality={:.2} slippage_bps={:.1} spread_bps={:.1}",
                exec_score, slippage, spread
            ),
        ];

        let mut h = Sha256::new();
        h.update(t.trade_id.as_bytes());
        h.update(t.pnl.to_le_bytes());

        let a = TradeAutopsy {
            trade_id: t.trade_id.clone(),
            outcome,
            pnl: t.pnl,
            attribution: vec![
                ("alpha".into(), alpha_attribution),
                ("execution".into(), exec_attribution),
            ],
            lessons,
            signal_quality_score: signal_score,
            execution_quality_score: exec_score,
            option_selection_score,
            regime_compatibility: regime_score,
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

    pub fn success_rate_for_regime(&self, regime: &str) -> f64 {
        let matching: Vec<_> = self
            .regime_memory
            .iter()
            .filter(|(r, _)| r == regime)
            .collect();
        if matching.is_empty() {
            return 0.5;
        }
        let wins = matching.iter().filter(|(_, pnl)| *pnl > 0.0).count();
        wins as f64 / matching.len() as f64
    }
}
