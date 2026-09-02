use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use th_domain::{
    Bar, MarketSessionClock, MarketSessionState, OptionExpiryPolicy, Signal, SignalSide,
};

use crate::{signal, Strategy, StrategySpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiHorizonMomentumConfig {
    pub short_lookback: usize,
    pub medium_lookback: usize,
    pub long_lookback: usize,
    pub short_weight: f64,
    pub medium_weight: f64,
    pub long_weight: f64,
    pub min_agreement_ratio: f64,
    pub min_signal_strength: f64,
    pub min_expiry_minutes: u32,
}

impl Default for MultiHorizonMomentumConfig {
    fn default() -> Self {
        Self {
            short_lookback: 5,
            medium_lookback: 20,
            long_lookback: 60,
            short_weight: 0.25,
            medium_weight: 0.40,
            long_weight: 0.35,
            min_agreement_ratio: 2.0 / 3.0,
            min_signal_strength: 0.05,
            min_expiry_minutes: 180,
        }
    }
}

impl MultiHorizonMomentumConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Some(v) = std::env::var("MOMENTUM_SHORT_LOOKBACK")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.short_lookback = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_MEDIUM_LOOKBACK")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.medium_lookback = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_LONG_LOOKBACK")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.long_lookback = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_SHORT_WEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.short_weight = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_MEDIUM_WEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.medium_weight = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_LONG_WEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.long_weight = v;
        }
        if let Some(v) = std::env::var("MOMENTUM_MIN_AGREEMENT")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.min_agreement_ratio = v;
        }
        if let Some(v) = std::env::var("MIN_EXPIRY_MINUTES")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            cfg.min_expiry_minutes = v;
        }

        cfg
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiHorizonMomentumFeatures {
    pub short_momentum: f64,
    pub medium_momentum: f64,
    pub long_momentum: f64,
    pub composite_score: f64,
    pub consensus: bool,
    pub confidence: f64,
    pub session_state: MarketSessionState,
    pub expiry_valid: bool,
    pub timestamp: DateTime<Utc>,
}

pub struct MultiHorizonMomentumStrategy {
    spec: StrategySpec,
    config: MultiHorizonMomentumConfig,
    clock: MarketSessionClock,
    expiry_policy: OptionExpiryPolicy,
    bars: Vec<Bar>,
}

impl MultiHorizonMomentumStrategy {
    pub fn new() -> Self {
        Self::with_config(MultiHorizonMomentumConfig::default())
    }

    pub fn from_env() -> Self {
        Self::with_config(MultiHorizonMomentumConfig::from_env())
    }

    pub fn with_config(config: MultiHorizonMomentumConfig) -> Self {
        let warmup = config.long_lookback + 2;
        let clock = MarketSessionClock::from_env();
        let expiry_policy = OptionExpiryPolicy::from_env();

        Self {
            spec: StrategySpec {
                id: "multi_horizon_momentum".into(),
                name: "Multi-Horizon Momentum".into(),
                version: 1,
                warmup,
                max_hold_bars: 24,
                enabled: true,
                description: "Multi-horizon momentum strategy normalized across short, medium, and long lookbacks with consensus filtering and official US session gating".into(),
            },
            config,
            clock,
            expiry_policy,
            bars: Vec::new(),
        }
    }

    pub fn config(&self) -> &MultiHorizonMomentumConfig {
        &self.config
    }

    pub fn clock(&self) -> &MarketSessionClock {
        &self.clock
    }

    pub fn expiry_policy(&self) -> &OptionExpiryPolicy {
        &self.expiry_policy
    }

    /// Calculate normalized momentum for a specific lookback horizon k.
    /// Normalized via volatility-adjusted returns mapped to (-1.0, 1.0) using tanh.
    pub fn calculate_horizon_momentum(closes: &[f64], lookback: usize) -> Option<f64> {
        if closes.len() <= lookback || lookback == 0 {
            return None;
        }

        let current = *closes.last()?;
        let past = closes[closes.len() - 1 - lookback];
        if past <= 0.0 {
            return None;
        }

        let ret = (current - past) / past;

        // Compute rolling standard deviation of single-bar returns over lookback
        let start_idx = closes.len() - 1 - lookback;
        let mut bar_returns = Vec::with_capacity(lookback);
        for i in start_idx + 1..closes.len() {
            let prev = closes[i - 1];
            if prev > 0.0 {
                bar_returns.push((closes[i] - prev) / prev);
            }
        }

        if bar_returns.is_empty() {
            return None;
        }

        let mean = bar_returns.iter().sum::<f64>() / bar_returns.len() as f64;
        let variance =
            bar_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / bar_returns.len() as f64;
        let vol = variance.sqrt().max(1e-6);

        // Volatility-adjusted return over the horizon normalized by sqrt(k)
        let z = ret / (vol * (lookback as f64).sqrt());
        Some(z.tanh())
    }

    /// Extract deterministic features at the given timestamp with no look-ahead bias.
    pub fn extract_features(
        &self,
        bars: &[Bar],
        decision_ts: DateTime<Utc>,
        contract_expiry: Option<DateTime<Utc>>,
    ) -> Option<MultiHorizonMomentumFeatures> {
        let closes: Vec<f64> = bars
            .iter()
            .take_while(|b| b.ts <= decision_ts)
            .map(|b| b.close)
            .collect();

        if closes.len() <= self.config.long_lookback {
            return None;
        }

        let short_m = Self::calculate_horizon_momentum(&closes, self.config.short_lookback)?;
        let med_m = Self::calculate_horizon_momentum(&closes, self.config.medium_lookback)?;
        let long_m = Self::calculate_horizon_momentum(&closes, self.config.long_lookback)?;

        let total_weight =
            self.config.short_weight + self.config.medium_weight + self.config.long_weight;
        let composite = if total_weight > 0.0 {
            (short_m * self.config.short_weight
                + med_m * self.config.medium_weight
                + long_m * self.config.long_weight)
                / total_weight
        } else {
            0.0
        };

        // Consensus check: require at least majority (e.g. 2 of 3) to share the same sign
        let horizons = [short_m, med_m, long_m];
        let pos_count = horizons.iter().filter(|&&m| m > 0.0).count();
        let neg_count = horizons.iter().filter(|&&m| m < 0.0).count();
        let required_agree = (3.0 * self.config.min_agreement_ratio).ceil() as usize;

        let consensus = (pos_count >= required_agree && composite > 0.0)
            || (neg_count >= required_agree && composite < 0.0);

        let confidence = composite.abs().clamp(0.0, 1.0);
        let session_state = self.clock.session_state_at(decision_ts);

        let expiry_valid = match contract_expiry {
            Some(exp) => self.expiry_policy.is_valid_expiry(decision_ts, exp),
            None => true,
        };

        Some(MultiHorizonMomentumFeatures {
            short_momentum: short_m,
            medium_momentum: med_m,
            long_momentum: long_m,
            composite_score: composite,
            consensus,
            confidence,
            session_state,
            expiry_valid,
            timestamp: decision_ts,
        })
    }
}

impl Default for MultiHorizonMomentumStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for MultiHorizonMomentumStrategy {
    fn spec(&self) -> &StrategySpec {
        &self.spec
    }

    fn update(&mut self, bar: &Bar, _state: &th_domain::MarketState) -> Option<Signal> {
        self.bars.push(bar.clone());
        if self.bars.len() > 300 {
            self.bars.remove(0);
        }

        let symbol = &bar.symbol;
        let ts = bar.ts;

        // Constraint 1: Trading-time constraint.
        // Outside the regular market session: force NEUTRAL ("MARKET_CLOSED")
        if !self.clock.is_open(ts) {
            return Some(signal(
                &self.spec,
                symbol,
                SignalSide::Flat,
                0.0,
                "MARKET_CLOSED",
            ));
        }

        if self.bars.len() < self.spec.warmup {
            return None;
        }

        let features = self.extract_features(&self.bars, ts, None)?;

        if !features.consensus {
            return Some(signal(
                &self.spec,
                symbol,
                SignalSide::Flat,
                features.confidence,
                "multi-horizon momentum: consensus not reached",
            ));
        }

        if features.composite_score > self.config.min_signal_strength {
            Some(signal(
                &self.spec,
                symbol,
                SignalSide::LongCall,
                features.confidence,
                &format!(
                    "multi-horizon bullish consensus (S={:.2}, M={:.2}, L={:.2}, composite={:.2})",
                    features.short_momentum,
                    features.medium_momentum,
                    features.long_momentum,
                    features.composite_score
                ),
            ))
        } else if features.composite_score < -self.config.min_signal_strength {
            Some(signal(
                &self.spec,
                symbol,
                SignalSide::LongPut,
                features.confidence,
                &format!(
                    "multi-horizon bearish consensus (S={:.2}, M={:.2}, L={:.2}, composite={:.2})",
                    features.short_momentum,
                    features.medium_momentum,
                    features.long_momentum,
                    features.composite_score
                ),
            ))
        } else {
            Some(signal(
                &self.spec,
                symbol,
                SignalSide::Flat,
                features.confidence,
                "multi-horizon momentum: composite strength below threshold",
            ))
        }
    }

    fn reset(&mut self) {
        self.bars.clear();
    }
}
