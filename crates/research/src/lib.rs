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
