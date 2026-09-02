use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionExpiryPolicy {
    pub min_expiry_minutes: u32,
    pub max_expiry_minutes: Option<u32>,
}

impl Default for OptionExpiryPolicy {
    fn default() -> Self {
        Self {
            min_expiry_minutes: 180,
            max_expiry_minutes: None,
        }
    }
}

impl OptionExpiryPolicy {
    pub fn new(min_expiry_minutes: u32, max_expiry_minutes: Option<u32>) -> Self {
        Self {
            min_expiry_minutes,
            max_expiry_minutes,
        }
    }

    pub fn from_env() -> Self {
        let min = std::env::var("MIN_EXPIRY_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(180);

        let max = std::env::var("MAX_EXPIRY_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok());

        Self {
            min_expiry_minutes: min,
            max_expiry_minutes: max,
        }
    }

    /// Calculate time-to-expiration in minutes using timezone-aware UTC timestamps.
    pub fn time_to_expiration_minutes(decision_ts: DateTime<Utc>, expiry: DateTime<Utc>) -> i64 {
        (expiry - decision_ts).num_minutes()
    }

    /// Returns true if and only if time-to-expiration satisfies the minimum 180-minute policy
    /// (and optional upper limit if configured).
    pub fn is_valid_expiry(&self, decision_ts: DateTime<Utc>, expiry: DateTime<Utc>) -> bool {
        let minutes = Self::time_to_expiration_minutes(decision_ts, expiry);
        if minutes < self.min_expiry_minutes as i64 {
            return false;
        }
        if let Some(max_m) = self.max_expiry_minutes {
            if minutes > max_m as i64 {
                return false;
            }
        }
        true
    }
}
