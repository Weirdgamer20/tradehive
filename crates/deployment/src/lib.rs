use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use th_domain::{OptionQuote, OptionType};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BotStatus {
    Draft,
    Paper,
    Active,
    Paused,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSpec {
    pub id: String,
    pub strategy_id: String,
    pub version: String,
    pub status: BotStatus,
    pub created_at: DateTime<Utc>,
    pub capital_limit: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotCreationPlan {
    pub plan_id: String,
    pub bot_id: String,
    pub strategy_id: String,
    pub strategy_version: u32,
    pub config_version: String,
    pub underlying: String,
    pub option_symbol: String,
    pub option_type: OptionType,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub capital_allocated: f64,
    pub risk_budget: f64,
    pub min_expiry_minutes: u32,
    pub max_expiry_minutes: u32,
    pub created_at: DateTime<Utc>,
    pub fingerprint: String,
    // Legacy persisted fields are retained only for database compatibility. Hive never
    // supplies quantity, entry price, stop loss, or take profit to a bot.
    pub quantity: u32,
    pub entry_limit: f64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    #[serde(default = "default_generation_id")]
    pub generation_id: String,
    #[serde(default = "default_risk_pct")]
    pub risk_pct: f64,
    #[serde(default)]
    pub max_capital_exposure: f64,
    #[serde(default)]
    pub rl_state: Option<String>,
    #[serde(default)]
    pub rl_action: Option<String>,
    #[serde(default = "default_rl_confidence")]
    pub rl_confidence: f64,
}

fn default_generation_id() -> String {
    "GEN-DEFAULT".into()
}

fn default_risk_pct() -> f64 {
    0.02
}

fn default_rl_confidence() -> f64 {
    1.0
}

#[derive(Debug, Error)]
pub enum DeploymentError {
    #[error("bot not found")]
    NotFound,
    #[error("invalid status transition from {0:?}")]
    Invalid(BotStatus),
    #[error("capital limit must be positive and finite")]
    InvalidCapital,
    #[error("option quote is not tradeable")]
    InvalidQuote,
    #[error("capital is insufficient for one contract")]
    InsufficientCapital,
    #[error("option expiry must satisfy minimum 180 minute requirement")]
    InvalidExpiryHorizon,
    #[error("strategy id/config version cannot be empty")]
    InvalidIdentity,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BotFleet {
    pub bots: Vec<BotSpec>,
}

impl BotFleet {
    pub fn create(
        &mut self,
        id: &str,
        strategy: &str,
        version: &str,
        capital_limit: f64,
    ) -> Result<BotSpec, DeploymentError> {
        if !capital_limit.is_finite() || capital_limit <= 0.0 {
            return Err(DeploymentError::InvalidCapital);
        }
        if strategy.trim().is_empty() || version.trim().is_empty() {
            return Err(DeploymentError::InvalidIdentity);
        }
        if self
            .bots
            .iter()
            .any(|b| b.id == id && b.status != BotStatus::Retired)
        {
            return Err(DeploymentError::Invalid(BotStatus::Draft));
        }
        let b = BotSpec {
            id: id.into(),
            strategy_id: strategy.into(),
            version: version.into(),
            status: BotStatus::Draft,
            created_at: Utc::now(),
            capital_limit,
        };
        self.bots.push(b.clone());
        Ok(b)
    }
    fn set(&mut self, id: &str, to: BotStatus) -> Result<(), DeploymentError> {
        let b = self
            .bots
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or(DeploymentError::NotFound)?;
        let valid = matches!(
            (&b.status, &to),
            (BotStatus::Draft, BotStatus::Paper)
                | (BotStatus::Paper, BotStatus::Active)
                | (BotStatus::Active, BotStatus::Paused)
                | (BotStatus::Paused, BotStatus::Active)
                | (BotStatus::Paper, BotStatus::Retired)
                | (BotStatus::Paused, BotStatus::Retired)
                | (BotStatus::Active, BotStatus::Retired)
        );
        if !valid {
            return Err(DeploymentError::Invalid(b.status.clone()));
        }
        b.status = to;
        Ok(())
    }
    pub fn promote_paper(&mut self, id: &str) -> Result<(), DeploymentError> {
        self.set(id, BotStatus::Paper)
    }
    pub fn activate(&mut self, id: &str) -> Result<(), DeploymentError> {
        self.set(id, BotStatus::Active)
    }
    pub fn pause(&mut self, id: &str) -> Result<(), DeploymentError> {
        self.set(id, BotStatus::Paused)
    }
    pub fn retire(&mut self, id: &str) -> Result<(), DeploymentError> {
        self.set(id, BotStatus::Retired)
    }
    pub fn active(&self) -> Vec<&BotSpec> {
        self.bots
            .iter()
            .filter(|b| b.status == BotStatus::Active)
            .collect()
    }
    pub fn get(&self, id: &str) -> Option<&BotSpec> {
        self.bots.iter().find(|b| b.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct BotManufacturingRequest<'a> {
    pub strategy_id: &'a str,
    pub strategy_version: u32,
    pub config_version: &'a str,
    pub underlying: &'a str,
    pub quote: &'a OptionQuote,
    pub capital_budget: f64,
    pub risk_budget: f64,
    pub now: DateTime<Utc>,
    pub generation_id: Option<&'a str>,
    pub risk_pct: Option<f64>,
    pub rl_state: Option<&'a str>,
    pub rl_action: Option<&'a str>,
    pub rl_confidence: Option<f64>,
}

/// Hive-side bot manufacturing. This converts a validated research decision and a
/// concrete option quote into a fully specified, deterministic execution assignment.
pub fn manufacture_bot_plan(
    req: &BotManufacturingRequest<'_>,
) -> Result<BotCreationPlan, DeploymentError> {
    if req.strategy_id.trim().is_empty()
        || req.config_version.trim().is_empty()
        || req.underlying.trim().is_empty()
    {
        return Err(DeploymentError::InvalidIdentity);
    }
    if !req.capital_budget.is_finite()
        || req.capital_budget <= 0.0
        || !req.risk_budget.is_finite()
        || req.risk_budget <= 0.0
    {
        return Err(DeploymentError::InvalidCapital);
    }
    if !req.quote.is_tradeable(req.now, 30) || req.quote.underlying != req.underlying {
        return Err(DeploymentError::InvalidQuote);
    }
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
    if !expiry_policy.is_valid_expiry(req.now, req.quote.expiry) {
        return Err(DeploymentError::InvalidExpiryHorizon);
    }
    let seed = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        req.strategy_id,
        req.strategy_version,
        req.config_version,
        req.underlying,
        req.quote.symbol,
        req.quote.expiry.to_rfc3339(),
        req.capital_budget,
        req.risk_budget
    );
    let mut h = Sha256::new();
    h.update(seed.as_bytes());
    let fingerprint = format!("{:x}", h.finalize());
    let bot_id = format!("BOT-{}", &fingerprint[..16]);
    let generation_id = req.generation_id.unwrap_or("GEN-DEFAULT").to_string();
    let risk_pct = req.risk_pct.unwrap_or(if req.capital_budget > 0.0 {
        req.risk_budget / req.capital_budget
    } else {
        0.02
    });
    let max_capital_exposure = req.capital_budget;
    let rl_state = req.rl_state.map(|s| s.to_string());
    let rl_action = req.rl_action.map(|s| s.to_string());
    let rl_confidence = req.rl_confidence.unwrap_or(1.0);

    Ok(BotCreationPlan {
        plan_id: format!("PLAN-{}", &fingerprint[..16]),
        bot_id,
        strategy_id: req.strategy_id.into(),
        strategy_version: req.strategy_version,
        config_version: req.config_version.into(),
        underlying: req.underlying.into(),
        option_symbol: req.quote.symbol.clone(),
        option_type: req.quote.option_type,
        strike: req.quote.strike,
        expiry: req.quote.expiry,
        capital_allocated: req.capital_budget,
        risk_budget: req.risk_budget,
        min_expiry_minutes: expiry_policy.min_expiry_minutes,
        max_expiry_minutes: expiry_policy.max_expiry_minutes.unwrap_or(u32::MAX),
        created_at: req.now,
        fingerprint,
        quantity: 0,
        entry_limit: 0.0,
        stop_loss_pct: 0.0,
        take_profit_pct: 0.0,
        generation_id,
        risk_pct,
        max_capital_exposure,
        rl_state,
        rl_action,
        rl_confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quote() -> OptionQuote {
        OptionQuote {
            symbol: "SPY260828C00400000".into(),
            underlying: "SPY".into(),
            option_type: OptionType::Call,
            strike: 400.0,
            expiry: Utc::now() + chrono::Duration::minutes(240),
            bid: 1.9,
            ask: 2.0,
            last: 2.0,
            iv: 0.2,
            greeks: None,
            open_interest: 100,
            volume: 100,
            quote_ts: Utc::now(),
        }
    }
    #[test]
    fn hive_manufactures_complete_plan() {
        let q = quote();
        let p = manufacture_bot_plan(&BotManufacturingRequest {
            strategy_id: "momentum",
            strategy_version: 2,
            config_version: "research-1",
            underlying: "SPY",
            quote: &q,
            capital_budget: 1000.0,
            risk_budget: 100.0,
            now: Utc::now(),
            generation_id: Some("GEN-TEST"),
            risk_pct: Some(0.10),
            rl_state: Some("test_state"),
            rl_action: Some("BuyCall"),
            rl_confidence: Some(0.9),
        })
        .unwrap();
        assert_eq!(p.strategy_id, "momentum");
        assert_eq!(p.underlying, "SPY");
        assert_eq!(p.option_symbol, "SPY260828C00400000");
        assert_eq!(p.quantity, 0);
        assert_eq!(p.generation_id, "GEN-TEST");
        assert_eq!(p.risk_pct, 0.10);
        assert_eq!(p.rl_confidence, 0.9);
        assert!(p.capital_allocated > 0.0);
        assert!(!p.fingerprint.is_empty());
    }
    #[test]
    fn invalid_expiry_rejected() {
        let mut q = quote();
        q.expiry = Utc::now() + chrono::Duration::minutes(179);
        assert!(matches!(
            manufacture_bot_plan(&BotManufacturingRequest {
                strategy_id: "m",
                strategy_version: 1,
                config_version: "v",
                underlying: "SPY",
                quote: &q,
                capital_budget: 100.0,
                risk_budget: 10.0,
                now: Utc::now(),
                generation_id: None,
                risk_pct: None,
                rl_state: None,
                rl_action: None,
                rl_confidence: None,
            }),
            Err(DeploymentError::InvalidExpiryHorizon)
        ));
    }
    #[test]
    fn manufacturing_never_assigns_trade_quantity() {
        let now = Utc::now();
        let mut q = quote();
        q.expiry = now + chrono::Duration::minutes(240);
        q.quote_ts = now;
        let p = manufacture_bot_plan(&BotManufacturingRequest {
            strategy_id: "m",
            strategy_version: 1,
            config_version: "v",
            underlying: "SPY",
            quote: &q,
            capital_budget: 1000.0,
            risk_budget: 100.0,
            now,
            generation_id: None,
            risk_pct: None,
            rl_state: None,
            rl_action: None,
            rl_confidence: None,
        })
        .unwrap();
        assert_eq!(p.quantity, 0);
        assert_eq!(p.entry_limit, 0.0);
        assert_eq!(p.stop_loss_pct, 0.0);
        assert_eq!(p.take_profit_pct, 0.0);
        assert_eq!(p.min_expiry_minutes, 180);
        assert!(!p.bot_id.is_empty());
    }
}
