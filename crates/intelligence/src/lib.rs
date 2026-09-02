use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use th_domain::Bar;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub symbol: String,
    pub as_of: DateTime<Utc>,
    pub trend: f64,
    pub volatility: f64,
    pub momentum: f64,
    pub liquidity: f64,
    pub regime: String,
    pub confidence: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableObservation {
    pub name: String,
    pub value: f64,
    pub normalized: f64,
    pub source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEdge {
    pub cause: String,
    pub effect: String,
    pub strength: f64,
    pub evidence: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sensitivity {
    pub variable: String,
    pub elasticity: f64,
    pub stability: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceReport {
    pub state: MarketState,
    pub variables: Vec<VariableObservation>,
    pub causal_edges: Vec<CausalEdge>,
    pub sensitivities: Vec<Sensitivity>,
    pub dataset_hash: String,
}
#[derive(Debug, thiserror::Error)]
pub enum IntelligenceError {
    #[error("insufficient bars")]
    InsufficientData,
    #[error("invalid bar: {0}")]
    InvalidBar(String),
}
pub fn analyze(symbol: &str, bars: &[Bar]) -> Result<IntelligenceReport, IntelligenceError> {
    if bars.len() < 30 {
        return Err(IntelligenceError::InsufficientData);
    };
    for b in bars {
        b.validate()
            .map_err(|e| IntelligenceError::InvalidBar(e.to_string()))?;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let n = closes.len();
    let mean = closes.iter().sum::<f64>() / n as f64;
    let ret = (closes[n - 1] / closes[0]) - 1.0;
    let m = closes
        .iter()
        .zip(closes.iter().skip(1))
        .map(|(a, b)| (b / a) - 1.0)
        .collect::<Vec<_>>();
    let rv = (m.iter().map(|x| x * x).sum::<f64>() / m.len() as f64).sqrt();
    let slope = (closes[n - 1] - closes[0]) / (mean.max(1e-12));
    let regime = if slope > 0.03 {
        "trend_up"
    } else if slope < -0.03 {
        "trend_down"
    } else if rv > 0.02 {
        "volatile_range"
    } else {
        "range"
    }
    .to_string();
    let state = MarketState {
        symbol: symbol.into(),
        as_of: bars[n - 1].ts,
        trend: slope,
        volatility: rv,
        momentum: ret,
        liquidity: bars[n - 10..].iter().map(|b| b.volume).sum::<f64>() / 10.0,
        regime,
        confidence: (1.0 - (rv / 0.1)).clamp(0.0, 1.0),
    };
    let vars = vec![
        VariableObservation {
            name: "momentum".into(),
            value: ret,
            normalized: ret.clamp(-1.0, 1.0),
            source: "close_return".into(),
        },
        VariableObservation {
            name: "realized_volatility".into(),
            value: rv,
            normalized: (rv / 0.1).clamp(0.0, 1.0),
            source: "returns".into(),
        },
    ];
    let edges = vec![CausalEdge {
        cause: "volatility".into(),
        effect: "risk_capacity".into(),
        strength: -rv.abs(),
        evidence: 1.0,
    }];
    let sensitivities = vars
        .iter()
        .map(|v| Sensitivity {
            variable: v.name.clone(),
            elasticity: v.normalized,
            stability: state.confidence,
        })
        .collect();
    let mut h = Sha256::new();
    for b in bars {
        h.update(b.symbol.as_bytes());
        h.update(b.ts.timestamp_nanos_opt().unwrap_or_default().to_le_bytes());
        h.update(b.close.to_le_bytes());
    }
    Ok(IntelligenceReport {
        state,
        variables: vars,
        causal_edges: edges,
        sensitivities,
        dataset_hash: format!("{:x}", h.finalize()),
    })
}
