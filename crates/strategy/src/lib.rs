use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use th_domain::{Bar, MarketState, OptionChain, OptionType, Regime, Signal, SignalSide};
use thiserror::Error;
use uuid::Uuid;

pub mod multi_horizon_momentum;
pub use multi_horizon_momentum::{
    MultiHorizonMomentumConfig, MultiHorizonMomentumFeatures, MultiHorizonMomentumStrategy,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySpec {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub warmup: usize,
    pub max_hold_bars: u32,
    pub enabled: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyGenome {
    pub strategy_id: String,
    pub version: u32,
    pub parent_id: Option<String>,
    pub generation: u32,
    pub mutation_type: String,
    pub parameters: HashMap<String, f64>,
    pub feature_weights: HashMap<String, f64>,
    pub entry_threshold: f64,
    pub exit_threshold: f64,
    pub max_hold_bars: u32,
    pub regime_eligibility: Vec<Regime>,
}

impl StrategyGenome {
    pub fn new_root(strategy_id: &str, version: u32) -> Self {
        let mut params = HashMap::new();
        params.insert("fast_period".into(), 10.0);
        params.insert("slow_period".into(), 30.0);
        params.insert("volatility_scalar".into(), 1.0);

        let mut weights = HashMap::new();
        weights.insert("momentum".into(), 0.6);
        weights.insert("trend".into(), 0.4);

        Self {
            strategy_id: strategy_id.into(),
            version,
            parent_id: None,
            generation: 0,
            mutation_type: "ROOT".into(),
            parameters: params,
            feature_weights: weights,
            entry_threshold: 0.55,
            exit_threshold: 0.20,
            max_hold_bars: 36,
            regime_eligibility: vec![Regime::TrendingBull, Regime::TrendingBear, Regime::Range],
        }
    }

    pub fn mutate(&self, new_version: u32, mutation_type: &str, variance: f64) -> Self {
        let mut new_params = self.parameters.clone();
        for val in new_params.values_mut() {
            *val = (*val * (1.0 + variance * 0.1)).max(1.0);
        }

        let mut new_weights = self.feature_weights.clone();
        for val in new_weights.values_mut() {
            *val = (*val * (1.0 + variance * 0.05)).clamp(0.01, 1.0);
        }

        Self {
            strategy_id: format!("{}.{}", self.strategy_id, new_version),
            version: new_version,
            parent_id: Some(self.strategy_id.clone()),
            generation: self.generation + 1,
            mutation_type: mutation_type.into(),
            parameters: new_params,
            feature_weights: new_weights,
            entry_threshold: (self.entry_threshold * (1.0 + variance * 0.05)).clamp(0.3, 0.9),
            exit_threshold: (self.exit_threshold * (1.0 + variance * 0.05)).clamp(0.05, 0.5),
            max_hold_bars: self.max_hold_bars,
            regime_eligibility: self.regime_eligibility.clone(),
        }
    }

    pub fn recombine(parent_a: &Self, parent_b: &Self, new_id: &str, new_version: u32) -> Self {
        let mut params = HashMap::new();
        for (k, v) in &parent_a.parameters {
            let vb = parent_b.parameters.get(k).unwrap_or(v);
            params.insert(k.clone(), (v + vb) / 2.0);
        }

        let mut weights = HashMap::new();
        for (k, v) in &parent_a.feature_weights {
            let vb = parent_b.feature_weights.get(k).unwrap_or(v);
            weights.insert(k.clone(), (v + vb) / 2.0);
        }

        Self {
            strategy_id: new_id.into(),
            version: new_version,
            parent_id: Some(format!("{}+{}", parent_a.strategy_id, parent_b.strategy_id)),
            generation: parent_a.generation.max(parent_b.generation) + 1,
            mutation_type: "RECOMBINATION".into(),
            parameters: params,
            feature_weights: weights,
            entry_threshold: (parent_a.entry_threshold + parent_b.entry_threshold) / 2.0,
            exit_threshold: (parent_a.exit_threshold + parent_b.exit_threshold) / 2.0,
            max_hold_bars: (parent_a.max_hold_bars + parent_b.max_hold_bars) / 2,
            regime_eligibility: parent_a.regime_eligibility.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyAction {
    Long,
    Short,
    Flat,
}
pub trait Strategy: Send {
    fn spec(&self) -> &StrategySpec;
    fn update(&mut self, bar: &Bar, state: &MarketState) -> Option<Signal>;
    fn reset(&mut self) {}
    fn genome(&self) -> Option<StrategyGenome> {
        None
    }
}

fn sma(xs: &[f64], n: usize) -> Option<f64> {
    if xs.len() < n {
        None
    } else {
        Some(xs[xs.len() - n..].iter().sum::<f64>() / n as f64)
    }
}
fn ema(prev: Option<f64>, x: f64, n: usize) -> f64 {
    match prev {
        Some(p) => p + (2.0 / (n as f64 + 1.0)) * (x - p),
        None => x,
    }
}
fn stddev(xs: &[f64], n: usize) -> Option<f64> {
    let m = sma(xs, n)?;
    Some(
        (xs[xs.len() - n..]
            .iter()
            .map(|x| (x - m).powi(2))
            .sum::<f64>()
            / n as f64)
            .sqrt(),
    )
}
fn rsi(xs: &[f64], n: usize) -> Option<f64> {
    if xs.len() < n + 1 {
        return None;
    }
    let mut g = 0.0;
    let mut l = 0.0;
    for i in xs.len() - n..xs.len() {
        let d = xs[i] - xs[i - 1];
        if d >= 0.0 {
            g += d
        } else {
            l -= d
        }
    }
    if l == 0.0 {
        Some(100.0)
    } else {
        Some(100.0 - 100.0 / (1.0 + g / l))
    }
}
pub fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if bars.len() < n + 1 {
        return None;
    }
    let mut tr = Vec::new();
    for i in bars.len() - n..bars.len() {
        let p = &bars[i - 1];
        let b = &bars[i];
        tr.push(
            (b.high - b.low)
                .max((b.high - p.close).abs())
                .max((b.low - p.close).abs()),
        );
    }
    Some(tr.iter().sum::<f64>() / n as f64)
}
fn vwap(bars: &[Bar], n: usize) -> Option<f64> {
    if bars.len() < n {
        return None;
    }
    let mut pv = 0.0;
    let mut v = 0.0;
    for b in &bars[bars.len() - n..] {
        let tp = (b.high + b.low + b.close) / 3.0;
        pv += tp * b.volume;
        v += b.volume;
    }
    if v == 0.0 {
        None
    } else {
        Some(pv / v)
    }
}
fn macd(xs: &[f64]) -> Option<(f64, f64)> {
    if xs.len() < 26 {
        return None;
    }
    let mut e12 = None;
    let mut e26 = None;
    let mut hist = Vec::new();
    for x in xs {
        e12 = Some(ema(e12, *x, 12));
        e26 = Some(ema(e26, *x, 26));
        if let (Some(a), Some(b)) = (e12, e26) {
            hist.push(a - b)
        }
    }
    if hist.len() < 9 {
        return None;
    }
    let mut sig = None;
    for h in &hist {
        sig = Some(ema(sig, *h, 9));
    }
    match (hist.last().copied(), sig) {
        (Some(h), Some(s)) => Some((h, s)),
        _ => None,
    }
}
fn signal(
    spec: &StrategySpec,
    symbol: &str,
    side: SignalSide,
    strength: f64,
    reason: &str,
) -> Signal {
    Signal {
        id: Uuid::new_v4(),
        strategy_id: spec.id.clone(),
        symbol: symbol.into(),
        side,
        strength: strength.clamp(0.0, 1.0),
        reason: reason.into(),
        generated_at: Utc::now(),
        config_version: "production-v1".into(),
        session_id: None,
        bot_id: None,
        candidate_id: None,
        proposed_stop_loss_pct: None,
        proposed_take_profit_pct: None,
        proposed_max_hold_minutes: None,
    }
}

macro_rules! simple_strategy {
    ($name:ident,$id:expr,$desc:expr,$body:expr) => {
        pub struct $name {
            spec: StrategySpec,
            bars: Vec<Bar>,
        }
        impl $name {
            pub fn new() -> Self {
                Self {
                    spec: StrategySpec {
                        id: $id.into(),
                        name: stringify!($name).into(),
                        version: 1,
                        warmup: 20,
                        max_hold_bars: 24,
                        enabled: true,
                        description: $desc.into(),
                    },
                    bars: Vec::new(),
                }
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl Strategy for $name {
            fn spec(&self) -> &StrategySpec {
                &self.spec
            }
            fn update(&mut self, bar: &Bar, state: &MarketState) -> Option<Signal> {
                self.bars.push(bar.clone());
                if self.bars.len() > 300 {
                    self.bars.remove(0);
                }
                let f: fn(&[Bar], &StrategySpec, &MarketState) -> Option<Signal> = $body;
                f(&self.bars, &self.spec, state)
            }
            fn reset(&mut self) {
                self.bars.clear()
            }
        }
    };
}

simple_strategy!(
    Momentum,
    "momentum",
    "Short-horizon return momentum",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        if b.len() < 11 {
            return None;
        }
        let r = b.last()?.close / b[b.len() - 11].close - 1.0;
        if r > 0.005 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                (r * 50.0).min(1.0),
                "10-bar positive momentum",
            ))
        } else if r < -0.005 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                (-r * 50.0).min(1.0),
                "10-bar negative momentum",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    MovingAverageCrossover,
    "ma_crossover",
    "Fast/slow moving average crossover",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        if xs.len() < 30 {
            return None;
        }
        let f = sma(&xs, 10)?;
        let sl = sma(&xs, 30)?;
        let prev = &xs[..xs.len() - 1];
        let pf = sma(prev, 10)?;
        let ps = sma(prev, 30)?;
        if pf <= ps && f > sl {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.8,
                "bullish MA crossover",
            ))
        } else if pf >= ps && f < sl {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.8,
                "bearish MA crossover",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    RsiMeanReversion,
    "rsi_mean_reversion",
    "RSI oversold/overbought mean reversion",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        let r = rsi(&xs, 14)?;
        if r < 30.0 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                (30.0 - r) / 30.0,
                "RSI oversold",
            ))
        } else if r > 70.0 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                (r - 70.0) / 30.0,
                "RSI overbought",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    BollingerMeanReversion,
    "bollinger_mean_reversion",
    "Bollinger band mean reversion",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        let m = sma(&xs, 20)?;
        let sd = stddev(&xs, 20)?;
        let x = *xs.last()?;
        if x < m - 2.0 * sd {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.8,
                "below lower Bollinger band",
            ))
        } else if x > m + 2.0 * sd {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.8,
                "above upper Bollinger band",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    BollingerBreakout,
    "bollinger_breakout",
    "Bollinger expansion breakout",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        let m = sma(&xs, 20)?;
        let sd = stddev(&xs, 20)?;
        let x = *xs.last()?;
        if x > m + 2.0 * sd {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.75,
                "upper band breakout",
            ))
        } else if x < m - 2.0 * sd {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.75,
                "lower band breakout",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    AtrTrend,
    "atr_trend",
    "ATR-normalized trend following",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        let a = atr(b, 14)?;
        let r = (xs.last()? - xs[xs.len() - 15]) / a.max(1e-9);
        if r > 1.5 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                (0.8_f64).min(r / 3.0),
                "ATR trend up",
            ))
        } else if r < -1.5 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                (0.8_f64).min((-r) / 3.0),
                "ATR trend down",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    DonchianBreakout,
    "donchian_breakout",
    "Rolling high/low breakout",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        if b.len() < 21 {
            return None;
        }
        let hi = b[b.len() - 21..b.len() - 1]
            .iter()
            .map(|x| x.high)
            .fold(f64::MIN, f64::max);
        let lo = b[b.len() - 21..b.len() - 1]
            .iter()
            .map(|x| x.low)
            .fold(f64::MAX, f64::min);
        let x = b.last()?;
        if x.close > hi {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.85,
                "20-bar high breakout",
            ))
        } else if x.close < lo {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.85,
                "20-bar low breakout",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    MacdMomentum,
    "macd_momentum",
    "MACD trend momentum",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let xs: Vec<f64> = b.iter().map(|x| x.close).collect();
        let (m, sg) = macd(&xs)?;
        if m > sg && m > 0.0 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.7,
                "MACD bullish",
            ))
        } else if m < sg && m < 0.0 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.7,
                "MACD bearish",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    VwapReversion,
    "vwap_reversion",
    "VWAP distance reversion",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        let v = vwap(b, 20)?;
        let x = b.last()?.close;
        let d = x / v - 1.0;
        if d < -0.008 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.7,
                "price below VWAP",
            ))
        } else if d > 0.008 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.7,
                "price above VWAP",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    VolumePriceConfirmation,
    "volume_price_confirmation",
    "Price direction confirmed by volume",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        if b.len() < 21 {
            return None;
        }
        let avg = b[b.len() - 21..b.len() - 1]
            .iter()
            .map(|x| x.volume)
            .sum::<f64>()
            / 20.0;
        let x = b.last()?;
        let ratio = x.volume / avg.max(1e-9);
        let r = x.close / b[b.len() - 2].close - 1.0;
        if ratio > 1.5 && r > 0.002 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                (ratio / 3.0).min(1.0),
                "volume confirms up move",
            ))
        } else if ratio > 1.5 && r < -0.002 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                (ratio / 3.0).min(1.0),
                "volume confirms down move",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    RateOfChange,
    "rate_of_change",
    "Rate-of-change momentum",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        if b.len() < 16 {
            return None;
        }
        let r = b.last()?.close / b[b.len() - 16].close - 1.0;
        if r > 0.01 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.7,
                "ROC positive",
            ))
        } else if r < -0.01 {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.7,
                "ROC negative",
            ))
        } else {
            None
        }
    }
);
simple_strategy!(
    VolatilityBreakout,
    "volatility_breakout",
    "Range expansion breakout",
    |b: &[Bar], s: &StrategySpec, _: &MarketState| {
        if b.len() < 21 {
            return None;
        }
        let avg = b[b.len() - 21..b.len() - 1]
            .iter()
            .map(|x| x.range())
            .sum::<f64>()
            / 20.0;
        let x = b.last()?;
        let rr = x.range() / avg.max(1e-9);
        if rr > 1.8 && x.close > x.open {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongCall,
                0.8,
                "range expansion up",
            ))
        } else if rr > 1.8 && x.close < x.open {
            Some(signal(
                s,
                &b[0].symbol,
                SignalSide::LongPut,
                0.8,
                "range expansion down",
            ))
        } else {
            None
        }
    }
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyBlueprint {
    pub id: String,
    pub version: u32,
    pub parent_a: String,
    pub parent_b: String,
    pub confidence: f64,
    pub weight_a: f64,
    pub weight_b: f64,
    pub agreement_threshold: f64,
    pub rationale: String,
}

pub struct SynthesizedStrategy {
    spec: StrategySpec,
    parent_a: Box<dyn Strategy>,
    parent_b: Box<dyn Strategy>,
    weight_a: f64,
    weight_b: f64,
    agreement_threshold: f64,
}
impl SynthesizedStrategy {
    pub fn new(
        blueprint: &StrategyBlueprint,
        registry: &StrategyRegistry,
    ) -> Result<Self, StrategyError> {
        let mut a = registry.create(&blueprint.parent_a)?;
        let mut b = registry.create(&blueprint.parent_b)?;
        let warmup = a.spec().warmup.max(b.spec().warmup);
        if !blueprint.confidence.is_finite() || !(0.0..=1.0).contains(&blueprint.confidence) {
            return Err(StrategyError::Unknown(
                "invalid synthesized confidence".into(),
            ));
        }
        if !blueprint.weight_a.is_finite()
            || !blueprint.weight_b.is_finite()
            || blueprint.weight_a < 0.0
            || blueprint.weight_b < 0.0
            || (blueprint.weight_a + blueprint.weight_b) <= 0.0
        {
            return Err(StrategyError::Unknown("invalid synthesized weights".into()));
        }
        if !blueprint.agreement_threshold.is_finite()
            || !(0.0..=1.0).contains(&blueprint.agreement_threshold)
        {
            return Err(StrategyError::Unknown(
                "invalid synthesized threshold".into(),
            ));
        }
        let spec = StrategySpec {
            id: blueprint.id.clone(),
            name: format!("RL Evolved {} + {}", blueprint.parent_a, blueprint.parent_b),
            version: blueprint.version,
            warmup,
            max_hold_bars: 24,
            enabled: true,
            description: blueprint.rationale.clone(),
        };
        // force independent stateful processors to remain aligned with the same market stream
        a.reset();
        b.reset();
        Ok(Self {
            spec,
            parent_a: a,
            parent_b: b,
            weight_a: blueprint.weight_a,
            weight_b: blueprint.weight_b,
            agreement_threshold: blueprint.agreement_threshold,
        })
    }
}
impl Strategy for SynthesizedStrategy {
    fn spec(&self) -> &StrategySpec {
        &self.spec
    }
    fn update(&mut self, bar: &Bar, state: &MarketState) -> Option<Signal> {
        let a = self.parent_a.update(bar, state);
        let b = self.parent_b.update(bar, state);
        match (a, b) {
            (Some(x), Some(y)) if x.side == y.side && x.side != SignalSide::Flat => {
                let total = self.weight_a + self.weight_b;
                let strength = (x.strength * self.weight_a + y.strength * self.weight_b) / total;
                if strength < self.agreement_threshold {
                    return None;
                }
                Some(signal(
                    &self.spec,
                    &bar.symbol,
                    x.side,
                    strength,
                    "RL evolved weighted agreement",
                ))
            }
            _ => None,
        }
    }
    fn reset(&mut self) {
        self.parent_a.reset();
        self.parent_b.reset();
    }
}

pub struct StrategyRegistry {
    factories: HashMap<String, fn() -> Box<dyn Strategy>>,
}
impl Default for StrategyRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl StrategyRegistry {
    pub fn new() -> Self {
        let mut f: HashMap<String, fn() -> Box<dyn Strategy>> = HashMap::new();
        f.insert("momentum".into(), || Box::new(Momentum::new()));
        f.insert("ma_crossover".into(), || {
            Box::new(MovingAverageCrossover::new())
        });
        f.insert("rsi_mean_reversion".into(), || {
            Box::new(RsiMeanReversion::new())
        });
        f.insert("bollinger_mean_reversion".into(), || {
            Box::new(BollingerMeanReversion::new())
        });
        f.insert("bollinger_breakout".into(), || {
            Box::new(BollingerBreakout::new())
        });
        f.insert("atr_trend".into(), || Box::new(AtrTrend::new()));
        f.insert("donchian_breakout".into(), || {
            Box::new(DonchianBreakout::new())
        });
        f.insert("macd_momentum".into(), || Box::new(MacdMomentum::new()));
        f.insert("vwap_reversion".into(), || Box::new(VwapReversion::new()));
        f.insert("volume_price_confirmation".into(), || {
            Box::new(VolumePriceConfirmation::new())
        });
        f.insert("rate_of_change".into(), || Box::new(RateOfChange::new()));
        f.insert("volatility_breakout".into(), || {
            Box::new(VolatilityBreakout::new())
        });
        f.insert("multi_horizon_momentum".into(), || {
            Box::new(MultiHorizonMomentumStrategy::new())
        });
        Self { factories: f }
    }
    pub fn ids(&self) -> Vec<String> {
        let mut x = self.factories.keys().cloned().collect::<Vec<_>>();
        x.sort();
        x
    }
    pub fn create(&self, id: &str) -> Result<Box<dyn Strategy>, StrategyError> {
        if id == "multi_horizon_momentum" {
            return Ok(Box::new(MultiHorizonMomentumStrategy::new()));
        }
        if let Some(x) = create_extended(id) {
            return Ok(x);
        }
        self.factories
            .get(id)
            .map(|f| f())
            .ok_or_else(|| StrategyError::Unknown(id.into()))
    }
    pub fn seed_ids(&self) -> Vec<String> {
        // Exactly 30 immutable research seeds. Pair-trading and the microstructure proxy
        // require data that the single-symbol market-data contract does not guarantee.
        let mut ids = self.ids();
        ids.extend(
            extended_strategy_ids()
                .into_iter()
                .filter(|&id| id != "pairs_trading" && id != "microstructure_order_flow_scalper")
                .map(str::to_string),
        );
        ids.retain(|id| id != "multi_horizon_momentum");
        ids.sort();
        ids.dedup();
        ids.into_iter().take(30).collect()
    }
    pub fn all(&self) -> Vec<Box<dyn Strategy>> {
        self.seed_ids()
            .into_iter()
            .filter_map(|id| self.create(&id).ok())
            .collect()
    }
    pub fn merge_promoted_seed_ids(&self, promoted: &[String]) -> Vec<String> {
        let mut ids = self.seed_ids();
        for id in promoted {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }
    pub fn create_synthesized(
        &self,
        b: &StrategyBlueprint,
    ) -> Result<Box<dyn Strategy>, StrategyError> {
        Ok(Box::new(SynthesizedStrategy::new(b, self)?))
    }
}

pub fn classify_regime(bars: &[Bar]) -> MarketState {
    let now = bars.last().map(|b| b.ts).unwrap_or_else(chrono::Utc::now);
    let symbol = bars.last().map(|b| b.symbol.clone()).unwrap_or_default();
    if bars.len() < 21 {
        return MarketState {
            symbol,
            regime: Regime::Unknown,
            volatility: 0.0,
            momentum: 0.0,
            volume_ratio: 1.0,
            as_of: now,
        };
    }
    let xs: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mom = xs.last().copied().unwrap_or(0.0) / xs[xs.len() - 21] - 1.0;
    let vol = stddev(&xs, 20).unwrap_or(0.0) / sma(&xs, 20).unwrap_or(1.0);
    let avgv = bars[bars.len() - 21..bars.len() - 1]
        .iter()
        .map(|b| b.volume)
        .sum::<f64>()
        / 20.0;
    let vr = bars.last().map(|b| b.volume).unwrap_or(0.0) / avgv.max(1e-9);
    let regime = if vol > 0.03 {
        Regime::HighVol
    } else if mom > 0.01 {
        Regime::TrendingBull
    } else if mom < -0.01 {
        Regime::TrendingBear
    } else {
        Regime::Range
    };
    MarketState {
        symbol,
        regime,
        volatility: vol,
        momentum: mom,
        volume_ratio: vr,
        as_of: now,
    }
}

pub fn choose_option(
    chain: &OptionChain,
    side: SignalSide,
    now: chrono::DateTime<chrono::Utc>,
    spot: f64,
) -> Option<th_domain::OptionQuote> {
    let wanted = match side {
        SignalSide::LongCall => OptionType::Call,
        SignalSide::LongPut => OptionType::Put,
        SignalSide::Flat => return None,
    };
    chain
        .quotes
        .iter()
        .filter(|q| {
            q.option_type == wanted
                && q.bid > 0.0
                && q.ask >= q.bid
                && q.dte(now) > 0.5
                && q.dte(now) < 30.0
                && q.open_interest >= 50
        })
        .filter_map(|q| {
            let d = q.greeks.as_ref().map(|g| g.delta.abs()).unwrap_or(0.0);
            if d < 0.30 || d > 0.70 {
                return None;
            }
            let spread = q.spread() / q.mid().max(0.01);
            let dist = (q.strike - spot).abs() / spot.max(0.01);
            Some((q.clone(), spread * 10.0 + dist + d * 0.1))
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|x| x.0)
}

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("unknown strategy: {0}")]
    Unknown(String),
}

// V9 strategy family: each strategy has an independent signal function and parameters.
#[derive(Clone, Copy)]
enum Kind {
    EmaTrend,
    TripleEma,
    GapContinuation,
    GapMeanReversion,
    Pairs,
    RelativeStrength,
    RiskAdjustedMomentum,
    MultiTimeframe,
    TrendStrength,
    TrendMeanReversion,
    VolCompression,
    VolRegime,
    VolAdjustedBreakout,
    ZScore,
    Microstructure,
    MovingAverageTrend,
    Ensemble,
    MomentumRotation,
    VwapMomentum,
    VwapScalper,
}
struct ExtendedStrategy {
    spec: StrategySpec,
    bars: Vec<Bar>,
    kind: Kind,
}
impl ExtendedStrategy {
    fn make(id: &str, name: &str, desc: &str, kind: Kind, warmup: usize) -> Self {
        Self {
            spec: StrategySpec {
                id: id.into(),
                name: name.into(),
                version: 1,
                warmup,
                max_hold_bars: 24,
                enabled: true,
                description: desc.into(),
            },
            bars: Vec::new(),
            kind,
        }
    }
}
impl Strategy for ExtendedStrategy {
    fn spec(&self) -> &StrategySpec {
        &self.spec
    }
    fn update(&mut self, bar: &Bar, _state: &MarketState) -> Option<Signal> {
        self.bars.push(bar.clone());
        if self.bars.len() > 300 {
            self.bars.remove(0);
        }
        let b = &self.bars;
        if b.len() < self.spec.warmup {
            return None;
        }
        let x = b.last()?;
        let xs: Vec<f64> = b.iter().map(|z| z.close).collect();
        let sym = &x.symbol;
        let mk = |side, strength, reason| signal(&self.spec, sym, side, strength, reason);
        match self.kind {
            Kind::EmaTrend => {
                let e8 = xs.iter().fold(None, |p, &x| Some(ema(p, x, 8)))?;
                let e21 = xs.iter().fold(None, |p, &x| Some(ema(p, x, 21)))?;
                if e8 > e21 {
                    Some(mk(SignalSide::LongCall, 0.68, "EMA8 above EMA21"))
                } else if e8 < e21 {
                    Some(mk(SignalSide::LongPut, 0.68, "EMA8 below EMA21"))
                } else {
                    None
                }
            }
            Kind::TripleEma => {
                let e8 = xs.iter().fold(None, |p, &x| Some(ema(p, x, 8)))?;
                let e21 = xs.iter().fold(None, |p, &x| Some(ema(p, x, 21)))?;
                let e55 = xs.iter().fold(None, |p, &x| Some(ema(p, x, 55)))?;
                if e8 > e21 && e21 > e55 {
                    Some(mk(
                        SignalSide::LongCall,
                        0.78,
                        "triple EMA bullish alignment",
                    ))
                } else if e8 < e21 && e21 < e55 {
                    Some(mk(
                        SignalSide::LongPut,
                        0.78,
                        "triple EMA bearish alignment",
                    ))
                } else {
                    None
                }
            }
            Kind::GapContinuation => {
                if b.len() < 2 {
                    return None;
                }
                let gap = b[b.len() - 2].close;
                let g = x.open / gap - 1.0;
                if g.abs() < 0.01 {
                    return None;
                }
                if g > 0.01 && x.close > x.open {
                    Some(mk(SignalSide::LongCall, 0.75, "gap continuation"))
                } else if g < -0.01 && x.close < x.open {
                    Some(mk(SignalSide::LongPut, 0.75, "gap continuation"))
                } else {
                    None
                }
            }
            Kind::GapMeanReversion => {
                let gap = b[b.len() - 2].close;
                let g = x.open / gap - 1.0;
                if g > 0.015 && x.close < x.open {
                    Some(mk(SignalSide::LongPut, 0.72, "gap fade"))
                } else if g < -0.015 && x.close > x.open {
                    Some(mk(SignalSide::LongCall, 0.72, "gap fade"))
                } else {
                    None
                }
            }
            Kind::Pairs => {
                let m = sma(&xs, 20)?;
                let z = (x.close - m) / stddev(&xs, 20)?.max(1e-9);
                if z > 2.0 {
                    Some(mk(SignalSide::LongPut, 0.7, "pair spread zscore high"))
                } else if z < -2.0 {
                    Some(mk(SignalSide::LongCall, 0.7, "pair spread zscore low"))
                } else {
                    None
                }
            }
            Kind::RelativeStrength => {
                let r = x.close / xs[xs.len() - 21] - 1.0;
                if r > 0.015 {
                    Some(mk(SignalSide::LongCall, 0.7, "relative strength positive"))
                } else if r < -0.015 {
                    Some(mk(SignalSide::LongPut, 0.7, "relative strength negative"))
                } else {
                    None
                }
            }
            Kind::RiskAdjustedMomentum => {
                let r = x.close / xs[xs.len() - 21] - 1.0;
                let sd = stddev(&xs, 20)?.max(1e-6);
                let score = r / sd;
                if score > 1.0 {
                    Some(mk(
                        SignalSide::LongCall,
                        (score / 3.0).min(1.0),
                        "risk adjusted momentum",
                    ))
                } else if score < -1.0 {
                    Some(mk(
                        SignalSide::LongPut,
                        (-score / 3.0).min(1.0),
                        "risk adjusted momentum",
                    ))
                } else {
                    None
                }
            }
            Kind::MultiTimeframe => {
                let f = sma(&xs, 10)?;
                let m = sma(&xs, 30)?;
                let l = sma(&xs, 60)?;
                if f > m && m > l {
                    Some(mk(SignalSide::LongCall, 0.8, "multi timeframe alignment"))
                } else if f < m && m < l {
                    Some(mk(SignalSide::LongPut, 0.8, "multi timeframe alignment"))
                } else {
                    None
                }
            }
            Kind::TrendStrength => {
                let m = sma(&xs, 20)?;
                let sd = stddev(&xs, 20)?.max(1e-9);
                let t = (x.close - m) / sd;
                if t > 1.5 {
                    Some(mk(SignalSide::LongCall, 0.75, "trend strength"))
                } else if t < -1.5 {
                    Some(mk(SignalSide::LongPut, 0.75, "trend strength"))
                } else {
                    None
                }
            }
            Kind::TrendMeanReversion => {
                let m = sma(&xs, 30)?;
                let d = x.close / m - 1.0;
                if d.abs() > 0.025 && d < 0.0 {
                    Some(mk(SignalSide::LongCall, 0.7, "trend mean reversion"))
                } else if d > 0.025 {
                    Some(mk(SignalSide::LongPut, 0.7, "trend mean reversion"))
                } else {
                    None
                }
            }
            Kind::VolCompression => {
                let sd = stddev(&xs, 20)? / sma(&xs, 20)?.max(1e-9);
                let prev = stddev(&xs[..xs.len() - 1], 20).unwrap_or(sd);
                if sd < prev * 0.7 {
                    if x.close > x.open {
                        Some(mk(
                            SignalSide::LongCall,
                            0.65,
                            "volatility compression release up",
                        ))
                    } else {
                        Some(mk(
                            SignalSide::LongPut,
                            0.65,
                            "volatility compression release down",
                        ))
                    }
                } else {
                    None
                }
            }
            Kind::VolRegime => {
                let v = stddev(&xs, 20)? / sma(&xs, 20)?.max(1e-9);
                if v > 0.04 {
                    if x.close > x.open {
                        Some(mk(SignalSide::LongCall, 0.7, "high volatility regime"))
                    } else {
                        Some(mk(SignalSide::LongPut, 0.7, "high volatility regime"))
                    }
                } else {
                    None
                }
            }
            Kind::VolAdjustedBreakout => {
                let sd = stddev(&xs, 20)?.max(1e-9);
                let z = (x.close - sma(&xs, 20)?) / sd;
                if z > 2.0 {
                    Some(mk(SignalSide::LongCall, 0.8, "vol adjusted breakout"))
                } else if z < -2.0 {
                    Some(mk(SignalSide::LongPut, 0.8, "vol adjusted breakout"))
                } else {
                    None
                }
            }
            Kind::ZScore => {
                let z = (x.close - sma(&xs, 20)?) / stddev(&xs, 20)?.max(1e-9);
                if z > 2.2 {
                    Some(mk(SignalSide::LongPut, 0.76, "zscore mean reversion"))
                } else if z < -2.2 {
                    Some(mk(SignalSide::LongCall, 0.76, "zscore mean reversion"))
                } else {
                    None
                }
            }
            Kind::Microstructure => {
                let range = x.range().max(1e-9);
                let body = (x.close - x.open).abs() / range;
                if x.volume > 0.0 && body > 0.7 {
                    if x.close > x.open {
                        Some(mk(
                            SignalSide::LongCall,
                            0.74,
                            "directional order-flow proxy",
                        ))
                    } else {
                        Some(mk(
                            SignalSide::LongPut,
                            0.74,
                            "directional order-flow proxy",
                        ))
                    }
                } else {
                    None
                }
            }
            Kind::MovingAverageTrend => {
                let m = sma(&xs, 50)?;
                if x.close > m * 1.01 {
                    Some(mk(SignalSide::LongCall, 0.7, "MA trend"))
                } else if x.close < m * 0.99 {
                    Some(mk(SignalSide::LongPut, 0.7, "MA trend"))
                } else {
                    None
                }
            }
            Kind::Ensemble => {
                let a = sma(&xs, 10)?;
                let z = (x.close - sma(&xs, 20)?) / stddev(&xs, 20)?.max(1e-9);
                if a > sma(&xs, 30)? && z > 0.5 {
                    Some(mk(SignalSide::LongCall, 0.82, "ensemble agreement"))
                } else if a < sma(&xs, 30)? && z < -0.5 {
                    Some(mk(SignalSide::LongPut, 0.82, "ensemble agreement"))
                } else {
                    None
                }
            }
            Kind::MomentumRotation => {
                let r5 = x.close / xs[xs.len() - 6] - 1.0;
                let r20 = x.close / xs[xs.len() - 21] - 1.0;
                if r5 > 0.0 && r20 > 0.01 {
                    Some(mk(SignalSide::LongCall, 0.72, "momentum rotation"))
                } else if r5 < 0.0 && r20 < -0.01 {
                    Some(mk(SignalSide::LongPut, 0.72, "momentum rotation"))
                } else {
                    None
                }
            }
            Kind::VwapMomentum => {
                let v = vwap(b, 20)?;
                let d = x.close / v - 1.0;
                if d > 0.01 {
                    Some(mk(SignalSide::LongCall, 0.73, "VWAP momentum"))
                } else if d < -0.01 {
                    Some(mk(SignalSide::LongPut, 0.73, "VWAP momentum"))
                } else {
                    None
                }
            }
            Kind::VwapScalper => {
                let v = vwap(b, 8)?;
                let d = x.close / v - 1.0;
                if d < -0.004 {
                    Some(mk(SignalSide::LongCall, 0.65, "VWAP scalper reversion"))
                } else if d > 0.004 {
                    Some(mk(SignalSide::LongPut, 0.65, "VWAP scalper reversion"))
                } else {
                    None
                }
            }
        }
    }
    fn reset(&mut self) {
        self.bars.clear()
    }
}

pub fn extended_strategy_ids() -> Vec<&'static str> {
    vec![
        "ema_trend",
        "triple_ema_trend",
        "gap_continuation",
        "gap_mean_reversion",
        "pairs_trading",
        "relative_strength_ranking",
        "risk_adjusted_momentum",
        "multi_timeframe_momentum",
        "trend_strength_momentum",
        "trend_mean_reversion_regime",
        "volatility_compression_expansion",
        "volatility_regime",
        "volatility_adjusted_breakout",
        "zscore_mean_reversion",
        "microstructure_order_flow_scalper",
        "moving_average_trend",
        "ensemble_strategy",
        "momentum_rotation",
        "vwap_momentum",
        "vwap_mean_reversion_scalper",
    ]
}
pub fn create_extended(id: &str) -> Option<Box<dyn Strategy>> {
    let x = match id {
        "ema_trend" => ExtendedStrategy::make(id, "EMA Trend", "EMA trend", Kind::EmaTrend, 22),
        "triple_ema_trend" => {
            ExtendedStrategy::make(id, "Triple EMA", "Triple EMA", Kind::TripleEma, 56)
        }
        "gap_continuation" => ExtendedStrategy::make(
            id,
            "Gap Continuation",
            "Gap continuation",
            Kind::GapContinuation,
            5,
        ),
        "gap_mean_reversion" => ExtendedStrategy::make(
            id,
            "Gap Mean Reversion",
            "Gap fade",
            Kind::GapMeanReversion,
            5,
        ),
        "pairs_trading" => {
            ExtendedStrategy::make(id, "Pairs Trading", "Spread proxy", Kind::Pairs, 21)
        }
        "relative_strength_ranking" => ExtendedStrategy::make(
            id,
            "Relative Strength",
            "Relative strength",
            Kind::RelativeStrength,
            21,
        ),
        "risk_adjusted_momentum" => ExtendedStrategy::make(
            id,
            "Risk Adjusted Momentum",
            "Risk adjusted momentum",
            Kind::RiskAdjustedMomentum,
            21,
        ),
        "multi_timeframe_momentum" => ExtendedStrategy::make(
            id,
            "Multi Timeframe Momentum",
            "Multi timeframe",
            Kind::MultiTimeframe,
            61,
        ),
        "trend_strength_momentum" => ExtendedStrategy::make(
            id,
            "Trend Strength",
            "Trend strength",
            Kind::TrendStrength,
            21,
        ),
        "trend_mean_reversion_regime" => ExtendedStrategy::make(
            id,
            "Trend Mean Reversion",
            "Trend mean reversion",
            Kind::TrendMeanReversion,
            31,
        ),
        "volatility_compression_expansion" => ExtendedStrategy::make(
            id,
            "Vol Compression Expansion",
            "Vol compression",
            Kind::VolCompression,
            21,
        ),
        "volatility_regime" => ExtendedStrategy::make(
            id,
            "Volatility Regime",
            "Volatility regime",
            Kind::VolRegime,
            21,
        ),
        "volatility_adjusted_breakout" => ExtendedStrategy::make(
            id,
            "Vol Adjusted Breakout",
            "Vol adjusted breakout",
            Kind::VolAdjustedBreakout,
            21,
        ),
        "zscore_mean_reversion" => ExtendedStrategy::make(
            id,
            "ZScore Mean Reversion",
            "Z-score mean reversion",
            Kind::ZScore,
            21,
        ),
        "microstructure_order_flow_scalper" => ExtendedStrategy::make(
            id,
            "Microstructure Scalper",
            "Order-flow proxy",
            Kind::Microstructure,
            21,
        ),
        "moving_average_trend" => ExtendedStrategy::make(
            id,
            "Moving Average Trend",
            "MA trend",
            Kind::MovingAverageTrend,
            51,
        ),
        "ensemble_strategy" => {
            ExtendedStrategy::make(id, "Ensemble", "Ensemble agreement", Kind::Ensemble, 31)
        }
        "momentum_rotation" => ExtendedStrategy::make(
            id,
            "Momentum Rotation",
            "Momentum rotation",
            Kind::MomentumRotation,
            21,
        ),
        "vwap_momentum" => {
            ExtendedStrategy::make(id, "VWAP Momentum", "VWAP momentum", Kind::VwapMomentum, 21)
        }
        "vwap_mean_reversion_scalper" => {
            ExtendedStrategy::make(id, "VWAP Scalper", "VWAP scalping", Kind::VwapScalper, 21)
        }
        _ => return None,
    };
    Some(Box::new(x))
}
