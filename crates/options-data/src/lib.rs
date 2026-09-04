use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use th_domain::OptionType;
use thiserror::Error;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionQuote {
    pub symbol: String,
    pub underlying: String,
    pub option_type: OptionType,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub bid: f64,
    pub ask: f64,
    pub last: Option<f64>,
    pub volume: u64,
    pub open_interest: u64,
    pub iv: Option<f64>,
    #[serde(default)]
    pub greeks: Option<OptionGreeks>,
    pub as_of: DateTime<Utc>,
}
impl OptionQuote {
    pub fn mid(&self) -> Result<f64, OptionDataError> {
        if !self.bid.is_finite() || !self.ask.is_finite() || self.bid < 0.0 || self.ask < self.bid {
            return Err(OptionDataError::InvalidQuote);
        }
        Ok((self.bid + self.ask) / 2.0)
    }
    pub fn is_stale(&self, now: DateTime<Utc>, max_age_secs: i64) -> bool {
        let diff = now.signed_duration_since(self.as_of).num_seconds();
        diff < 0 || diff > max_age_secs
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionChain {
    pub underlying: String,
    pub spot: f64,
    pub as_of: DateTime<Utc>,
    pub quotes: Vec<OptionQuote>,
}
impl OptionChain {
    pub fn validate(&self) -> Result<(), OptionDataError> {
        if !self.spot.is_finite() || self.spot <= 0.0 {
            return Err(OptionDataError::InvalidSpot);
        };
        for q in &self.quotes {
            if q.underlying != self.underlying {
                return Err(OptionDataError::MixedUnderlying);
            };
            q.mid()?;
        }
        Ok(())
    }
    pub fn tradable(&self, now: DateTime<Utc>, max_age_secs: i64) -> Vec<&OptionQuote> {
        self.quotes
            .iter()
            .filter(|q| {
                !q.is_stale(now, max_age_secs) && q.bid >= 0.0 && q.ask >= q.bid && q.ask > 0.0
            })
            .collect()
    }
}

impl From<&th_domain::OptionChain> for OptionChain {
    fn from(c: &th_domain::OptionChain) -> Self {
        // Real spot price must be provided via underlying_spot; no synthetic strike or 100.0 fallback
        let spot = c.underlying_spot.unwrap_or(0.0);
        Self {
            underlying: c.underlying.clone(),
            spot,
            as_of: c.as_of,
            quotes: c.quotes.iter().map(OptionQuote::from).collect(),
        }
    }
}

impl From<&th_domain::OptionQuote> for OptionQuote {
    fn from(q: &th_domain::OptionQuote) -> Self {
        Self {
            symbol: q.symbol.clone(),
            underlying: q.underlying.clone(),
            option_type: q.option_type,
            strike: q.strike,
            expiry: q.expiry,
            bid: q.bid,
            ask: q.ask,
            last: Some(q.last),
            volume: q.volume,
            open_interest: q.open_interest,
            iv: Some(q.iv),
            greeks: q.greeks.as_ref().map(|g| OptionGreeks {
                delta: g.delta,
                gamma: g.gamma,
                theta: g.theta,
                vega: g.vega,
                rho: g.rho,
            }),
            as_of: q.quote_ts,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionGreeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedOptionCandidate {
    pub quote: OptionQuote,
    pub greeks: Option<OptionGreeks>,
    pub spread_bps: f64,
    pub dte_minutes: i64,
    pub composite_score: f64,
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionContractAssignment {
    pub session_id: String,
    pub bot_id: String,
    pub underlying: String,
    pub contract_symbol: String,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub option_type: OptionType,
    pub assigned_at: DateTime<Utc>,
    pub assignment_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionRankingConfig {
    pub min_volume: u64,
    pub min_open_interest: u64,
    pub max_spread_bps: f64,
    pub min_dte_minutes: i64,
    pub target_delta: f64,
    pub max_quote_age_secs: i64,
}

impl Default for OptionRankingConfig {
    fn default() -> Self {
        Self {
            min_volume: 5,
            min_open_interest: 10,
            max_spread_bps: 400.0,
            min_dte_minutes: 180,
            target_delta: 0.50,
            max_quote_age_secs: 60,
        }
    }
}

#[derive(Default)]
pub struct OptionRankingPipeline {
    config: OptionRankingConfig,
}

impl OptionRankingPipeline {
    pub fn new(config: OptionRankingConfig) -> Self {
        Self { config }
    }

    /// Executes the deterministic 19-step ranking pipeline and selects exactly one contract.
    pub fn rank_and_select(
        &self,
        chain: &OptionChain,
        target_type: OptionType,
        now: DateTime<Utc>,
        session_id: &str,
        bot_id: &str,
    ) -> Result<OptionContractAssignment, OptionDataError> {
        chain.validate()?;

        let mut ranked = Vec::new();

        for q in &chain.quotes {
            // Step 1 & 2: Match target type & Untradeable / Stale filter
            if q.option_type != target_type {
                continue;
            }
            if q.is_stale(now, self.config.max_quote_age_secs) {
                continue;
            }
            let mid = match q.mid() {
                Ok(m) if m > 0.0 => m,
                _ => continue,
            };

            // Step 3: Filter valid expiry (min DTE requirement)
            let dte_mins = (q.expiry - now).num_minutes();
            if dte_mins < self.config.min_dte_minutes {
                continue;
            }

            // Step 4 & 5: Filter spread
            let spread = q.ask - q.bid;
            let spread_bps = (spread / mid) * 10_000.0;
            if spread_bps > self.config.max_spread_bps {
                continue;
            }

            // Step 6 & 7: Filter volume and open interest
            if q.volume < self.config.min_volume || q.open_interest < self.config.min_open_interest
            {
                continue;
            }

            // Step 8-12: Greeks & IV validation - STRICT REAL DATA ONLY
            let Some(iv) = q.iv else {
                continue;
            };
            if !iv.is_finite() || iv <= 0.0 {
                continue;
            }

            let Some(greeks) = &q.greeks else {
                continue;
            };
            if !greeks.delta.is_finite() {
                continue;
            }
            let delta = greeks.delta.abs();

            // Step 13-16: Scored factors
            let delta_fit = 1.0 - (delta - self.config.target_delta).abs() * 2.0;
            let spread_score = 1.0 - (spread_bps / self.config.max_spread_bps);
            let liquidity_score =
                ((q.volume as f64).ln_1p() + (q.open_interest as f64).ln_1p()) / 15.0;

            // Step 17: Score candidate
            let composite_score =
                (delta_fit * 0.40) + (spread_score * 0.35) + (liquidity_score * 0.25);

            ranked.push(RankedOptionCandidate {
                quote: q.clone(),
                greeks: Some(greeks.clone()),
                spread_bps,
                dte_minutes: dte_mins,
                composite_score,
                rank: 0,
            });
        }

        if ranked.is_empty() {
            return Err(OptionDataError::NoEligibleContracts);
        }

        // Deterministic sort: highest composite score first, breaking ties by tighter spread
        ranked.sort_by(|a, b| {
            b.composite_score
                .partial_cmp(&a.composite_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.spread_bps
                        .partial_cmp(&b.spread_bps)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        // Step 18: Select exactly one contract (rank 0)
        let chosen = &ranked[0];

        // Step 19: Persist immutable contract assignment
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(session_id.as_bytes());
        hasher.update(bot_id.as_bytes());
        hasher.update(chosen.quote.symbol.as_bytes());
        hasher.update(chosen.quote.strike.to_le_bytes());
        let assignment_hash = format!("{:x}", hasher.finalize());

        Ok(OptionContractAssignment {
            session_id: session_id.into(),
            bot_id: bot_id.into(),
            underlying: chain.underlying.clone(),
            contract_symbol: chosen.quote.symbol.clone(),
            strike: chosen.quote.strike,
            expiry: chosen.quote.expiry,
            option_type: target_type,
            assigned_at: now,
            assignment_hash,
        })
    }
}

#[derive(Debug, Error)]
pub enum OptionDataError {
    #[error("invalid spot")]
    InvalidSpot,
    #[error("mixed underlying symbols")]
    MixedUnderlying,
    #[error("invalid option quote")]
    InvalidQuote,
    #[error("no eligible option contracts in chain")]
    NoEligibleContracts,
}
