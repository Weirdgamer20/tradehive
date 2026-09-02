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
        now.signed_duration_since(self.as_of).num_seconds() > max_age_secs
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
#[derive(Debug, Error)]
pub enum OptionDataError {
    #[error("invalid spot")]
    InvalidSpot,
    #[error("mixed underlying symbols")]
    MixedUnderlying,
    #[error("invalid option quote")]
    InvalidQuote,
}
