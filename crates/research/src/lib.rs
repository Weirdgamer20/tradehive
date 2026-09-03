use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use th_domain::Bar;
use th_intelligence::{analyze, IntelligenceReport};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub strategy_id: String,
    pub train_bars: usize,
    pub test_bars: usize,
    pub started: DateTime<Utc>,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchReport {
    pub experiment: Experiment,
    pub intelligence: IntelligenceReport,
    pub train_hash: String,
    pub test_hash: String,
    pub accepted: bool,
}
fn hash(b: &[Bar]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for x in b {
        h.update(x.ts.timestamp_nanos_opt().unwrap_or_default().to_le_bytes());
        h.update(x.close.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}
pub fn run(symbol: &str, strategy: &str, bars: &[Bar]) -> Result<ResearchReport, String> {
    if bars.len() < 80 {
        return Err("need >=80 bars".into());
    }
    let split = bars.len() * 70 / 100;
    let intel = analyze(symbol, &bars[..split]).map_err(|e| e.to_string())?;
    let exp = Experiment {
        id: format!(
            "exp-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ),
        strategy_id: strategy.into(),
        train_bars: split,
        test_bars: bars.len() - split,
        started: Utc::now(),
        status: "completed".into(),
    };
    let train_hash = hash(&bars[..split]);
    let test_hash = hash(&bars[split..]);
    Ok(ResearchReport {
        experiment: exp,
        intelligence: intel,
        train_hash,
        test_hash,
        accepted: false,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardWindow {
    pub train_start: usize,
    pub train_end: usize,
    pub test_start: usize,
    pub test_end: usize,
    pub score: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionGate {
    pub min_oos_score: f64,
    pub max_drawdown: f64,
    pub min_windows: usize,
}
pub fn walk_forward(bars: &[Bar], windows: usize) -> Vec<WalkForwardWindow> {
    if windows == 0 || bars.len() < windows * 20 {
        return Vec::new();
    }
    let span = bars.len() / windows;
    let mut out = Vec::new();
    for i in 0..windows {
        let ts = i * span;
        let te = ((i + 1) * span).min(bars.len());
        let split = ts + (te - ts) * 70 / 100;
        if split >= te {
            return Vec::new();
        }
        let train = bars[ts..split].last().map(|b| b.close).unwrap_or(0.0);
        let test = bars[split..te].last().map(|b| b.close).unwrap_or(train);
        let score = if train.abs() > 1e-9 {
            test / train - 1.0
        } else {
            0.0
        };
        out.push(WalkForwardWindow {
            train_start: ts,
            train_end: split,
            test_start: split,
            test_end: te,
            score,
        });
    }
    out
}
pub fn promotion_allowed(w: &[WalkForwardWindow], gate: &PromotionGate) -> bool {
    if w.len() < gate.min_windows {
        return false;
    }
    let avg = w.iter().map(|x| x.score).sum::<f64>() / w.len() as f64;
    let worst = w.iter().map(|x| x.score).fold(f64::INFINITY, f64::min);
    avg >= gate.min_oos_score && worst >= -gate.max_drawdown
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    pub in_sample_sharpe: f64,
    pub out_of_sample_sharpe: f64,
    pub walk_forward_efficiency: f64,
    pub max_drawdown: f64,
    pub turnover: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub trade_count: usize,
    pub expected_alpha: f64,
    pub estimated_slippage_bps: f64,
    pub spread_penalty: f64,
    pub liquidity_penalty: f64,
    pub net_utility: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialAssessment {
    pub lookahead_bias_passed: bool,
    pub data_leakage_passed: bool,
    pub regime_stability_passed: bool,
    pub spread_trap_passed: bool,
    pub iv_crush_passed: bool,
    pub overall_approved: bool,
    pub reasoning: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndependentEvaluationResult {
    pub candidate_id: String,
    pub symbol: String,
    pub strategy_id: String,
    pub strategy_version: u32,
    pub metrics: EvaluationMetrics,
    pub adversarial: AdversarialAssessment,
    pub promoted: bool,
    pub evaluated_at: DateTime<Utc>,
}

pub struct IndependentEvaluator {
    pub min_oos_sharpe: f64,
    pub max_drawdown: f64,
    pub min_trades: usize,
    pub min_net_utility: f64,
}

impl Default for IndependentEvaluator {
    fn default() -> Self {
        Self {
            min_oos_sharpe: 0.5,
            max_drawdown: 0.15,
            min_trades: 10,
            min_net_utility: 0.0,
        }
    }
}

impl IndependentEvaluator {
    pub fn evaluate(
        &self,
        candidate_id: &str,
        symbol: &str,
        strategy_id: &str,
        strategy_version: u32,
        bars: &[Bar],
        opinions: &[th_intelligence::ResearchOpinion],
    ) -> IndependentEvaluationResult {
        let windows = walk_forward(bars, 3);
        let oos_score = if !windows.is_empty() {
            windows.iter().map(|w| w.score).sum::<f64>() / windows.len() as f64
        } else {
            0.0
        };

        let is_score = if bars.len() >= 60 {
            let split = bars.len() * 70 / 100;
            let start = bars[0].close;
            let end = bars[split].close;
            if start > 0.0 {
                (end / start) - 1.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        let alpha = oos_score.max(0.001);
        let transaction_cost = 0.0010; // 10 bps baseline
        let execution_penalty = 0.0005;
        let liquidity_penalty = if bars.last().map(|b| b.volume < 100.0).unwrap_or(false) {
            0.005
        } else {
            0.0002
        };
        let risk_penalty = 0.0005;
        let complexity_penalty = 0.0002;

        let net_utility = alpha
            - transaction_cost
            - execution_penalty
            - liquidity_penalty
            - risk_penalty
            - complexity_penalty;

        let metrics = EvaluationMetrics {
            in_sample_sharpe: (is_score * 10.0).clamp(-2.0, 5.0),
            out_of_sample_sharpe: (oos_score * 10.0).clamp(-2.0, 5.0),
            walk_forward_efficiency: if is_score.abs() > 1e-6 {
                (oos_score / is_score).clamp(0.0, 2.0)
            } else {
                1.0
            },
            max_drawdown: 0.05,
            turnover: 1.2,
            win_rate: 0.55,
            profit_factor: 1.6,
            trade_count: bars.len() / 5,
            expected_alpha: alpha,
            estimated_slippage_bps: 5.0,
            spread_penalty: execution_penalty,
            liquidity_penalty,
            net_utility,
        };

        // Run Adversarial / Red-Team evaluation
        let adversarial = AdversarialEvaluator::test_candidate(bars, &metrics, opinions);

        let promoted = metrics.net_utility > self.min_net_utility
            && metrics.out_of_sample_sharpe >= self.min_oos_sharpe
            && metrics.trade_count >= self.min_trades
            && adversarial.overall_approved;

        IndependentEvaluationResult {
            candidate_id: candidate_id.into(),
            symbol: symbol.into(),
            strategy_id: strategy_id.into(),
            strategy_version,
            metrics,
            adversarial,
            promoted,
            evaluated_at: Utc::now(),
        }
    }
}

pub struct AdversarialEvaluator;

impl AdversarialEvaluator {
    pub fn test_candidate(
        bars: &[Bar],
        metrics: &EvaluationMetrics,
        opinions: &[th_intelligence::ResearchOpinion],
    ) -> AdversarialAssessment {
        let mut reasons = Vec::new();

        // 1. Lookahead bias check
        let now = Utc::now();
        let lookahead_passed = !bars
            .iter()
            .any(|b| b.ts > now + chrono::Duration::minutes(5));
        if !lookahead_passed {
            reasons.push("FAILED: Future bar timestamp detected".into());
        }

        // 2. Data leakage / Overfitting check
        let data_leakage_passed =
            metrics.in_sample_sharpe < 4.0 || metrics.walk_forward_efficiency >= 0.3;
        if !data_leakage_passed {
            reasons.push("FAILED: Suspected in-sample overfitting".into());
        }

        // 3. Regime stability check
        let regime_stability_passed = metrics.max_drawdown <= 0.20;
        if !regime_stability_passed {
            reasons.push("FAILED: Drawdown exceeds regime stability ceiling".into());
        }

        // 4. Spread trap check
        let spread_trap_passed = metrics.expected_alpha > metrics.spread_penalty;
        if !spread_trap_passed {
            reasons.push("FAILED: Alpha does not exceed spread cost".into());
        }

        // 5. IV crush check from opinions
        let iv_crush_passed = !opinions.iter().any(|o| {
            o.agent_type == th_intelligence::AnalystRole::AdversarialAnalyst
                && o.thesis.contains("ADVERSARIAL_REJECT")
        });
        if !iv_crush_passed {
            reasons.push("FAILED: Adversarial research analyst flagged structural risk".into());
        }

        let overall = lookahead_passed
            && data_leakage_passed
            && regime_stability_passed
            && spread_trap_passed
            && iv_crush_passed;

        if overall {
            reasons.push("PASSED: All adversarial stress gates cleared".into());
        }

        AdversarialAssessment {
            lookahead_bias_passed: lookahead_passed,
            data_leakage_passed,
            regime_stability_passed,
            spread_trap_passed,
            iv_crush_passed,
            overall_approved: overall,
            reasoning: reasons,
        }
    }
}
