use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use th_domain::{OrderIntent, Position, CONTRACT_MULTIPLIER};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskLimits {
    pub max_order_notional: f64,
    pub max_total_notional: f64,
    pub max_daily_loss: f64,
    pub max_positions: u32,
    pub max_single_position_qty: u32,
    pub max_spread_bps: f64,
    pub max_symbol_exposure: f64,
    pub max_trade_risk_pct: f64,
    pub max_portfolio_risk_pct: f64,
}
impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_notional: 1000.0,
            max_total_notional: 5000.0,
            max_daily_loss: 250.0,
            max_positions: 10,
            max_single_position_qty: 10,
            max_spread_bps: 250.0,
            max_symbol_exposure: 1500.0,
            max_trade_risk_pct: 0.01,
            max_portfolio_risk_pct: 0.05,
        }
    }
}
impl RiskLimits {
    pub fn from_env() -> Result<Self, RiskError> {
        let required = |name: &str| {
            std::env::var(name).map_err(|_| RiskError::Invalid(format!("{name} missing")))
        };
        let limits = Self {
            max_order_notional: required("RISK_MAX_ORDER_NOTIONAL")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_ORDER_NOTIONAL invalid".into()))?,
            max_total_notional: required("RISK_MAX_TOTAL_NOTIONAL")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_TOTAL_NOTIONAL invalid".into()))?,
            max_daily_loss: required("RISK_MAX_DAILY_LOSS")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_DAILY_LOSS invalid".into()))?,
            max_positions: required("RISK_MAX_POSITIONS")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_POSITIONS invalid".into()))?,
            max_single_position_qty: std::env::var("RISK_POSITION_SAFETY_CEILING")
                .or_else(|_| required("RISK_MAX_SINGLE_POSITION_QTY"))?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_SINGLE_POSITION_QTY invalid".into()))?,
            max_spread_bps: required("RISK_MAX_SPREAD_BPS")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_SPREAD_BPS invalid".into()))?,
            max_symbol_exposure: required("RISK_MAX_SYMBOL_EXPOSURE")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_SYMBOL_EXPOSURE invalid".into()))?,
            max_trade_risk_pct: required("RISK_MAX_TRADE_RISK_PCT")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_TRADE_RISK_PCT invalid".into()))?,
            max_portfolio_risk_pct: required("RISK_MAX_PORTFOLIO_RISK_PCT")?
                .parse()
                .map_err(|_| RiskError::Invalid("RISK_MAX_PORTFOLIO_RISK_PCT invalid".into()))?,
        };
        limits.validate()?;
        Ok(limits)
    }
    pub fn validate(&self) -> Result<(), RiskError> {
        if !self.max_order_notional.is_finite() || self.max_order_notional <= 0.0 {
            return Err(RiskError::Invalid(
                "max_order_notional must be positive".into(),
            ));
        }
        if !self.max_total_notional.is_finite() || self.max_total_notional <= 0.0 {
            return Err(RiskError::Invalid(
                "max_total_notional must be positive".into(),
            ));
        }
        if !self.max_daily_loss.is_finite() || self.max_daily_loss <= 0.0 {
            return Err(RiskError::Invalid("max_daily_loss must be positive".into()));
        }
        if self.max_positions == 0 {
            return Err(RiskError::Invalid("max_positions must be > 0".into()));
        }
        if self.max_single_position_qty == 0 {
            return Err(RiskError::Invalid(
                "max_single_position_qty must be > 0".into(),
            ));
        }
        if !self.max_spread_bps.is_finite() || self.max_spread_bps <= 0.0 {
            return Err(RiskError::Invalid("max_spread_bps must be positive".into()));
        }
        if !self.max_symbol_exposure.is_finite() || self.max_symbol_exposure <= 0.0 {
            return Err(RiskError::Invalid(
                "max_symbol_exposure must be positive".into(),
            ));
        }
        if !self.max_trade_risk_pct.is_finite()
            || self.max_trade_risk_pct <= 0.0
            || self.max_trade_risk_pct > 1.0
        {
            return Err(RiskError::Invalid(
                "max_trade_risk_pct must be between 0.0 and 1.0".into(),
            ));
        }
        if !self.max_portfolio_risk_pct.is_finite()
            || self.max_portfolio_risk_pct <= 0.0
            || self.max_portfolio_risk_pct > 1.0
        {
            return Err(RiskError::Invalid(
                "max_portfolio_risk_pct must be between 0.0 and 1.0".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioRisk {
    pub cash: f64,
    pub realized_today: f64,
    pub positions: Vec<Position>,
}
impl PortfolioRisk {
    pub fn total_notional(&self) -> f64 {
        self.positions
            .iter()
            .map(|p| p.mark.abs() * p.qty.unsigned_abs() as f64 * CONTRACT_MULTIPLIER)
            .sum()
    }
    pub fn symbol_notional(&self, symbol: &str) -> f64 {
        self.positions
            .iter()
            .filter(|p| p.symbol == symbol)
            .map(|p| p.mark.abs() * p.qty.unsigned_abs() as f64 * CONTRACT_MULTIPLIER)
            .sum()
    }
    pub fn open_positions(&self) -> u32 {
        self.positions.iter().filter(|p| p.qty != 0).count() as u32
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskApproval {
    pub token: Uuid,
    pub approved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub client_order_id: Uuid,
    pub order_hash: String,
    pub reason: String,
}
#[derive(Debug, Error)]
pub enum RiskError {
    #[error("kill switch active")]
    KillSwitch,
    #[error("risk limit: {0}")]
    Limit(String),
    #[error("invalid risk state: {0}")]
    Invalid(String),
    #[error("expired or unknown token")]
    TokenInvalid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskAllocation {
    pub strategy_id: String,
    pub risk_pct: f64,
    pub capital_allocated: f64,
    pub risk_budget: f64,
}
#[derive(Debug)]
pub struct RiskGovernor {
    limits: RiskLimits,
    kill_switch: bool,
    tokens: HashMap<Uuid, RiskApproval>,
    strategy_risks: HashMap<String, StrategyRiskAllocation>,
}
impl RiskGovernor {
    pub fn new(limits: RiskLimits) -> Self {
        Self {
            limits,
            kill_switch: false,
            tokens: HashMap::new(),
            strategy_risks: HashMap::new(),
        }
    }
    pub fn register_strategy_risk(&mut self, alloc: StrategyRiskAllocation) {
        self.strategy_risks.insert(alloc.strategy_id.clone(), alloc);
    }
    pub fn strategy_risks(&self) -> &HashMap<String, StrategyRiskAllocation> {
        &self.strategy_risks
    }
    pub fn engage_kill_switch(&mut self) {
        self.kill_switch = true;
        self.tokens.clear();
    }
    pub fn clear_kill_switch(&mut self) {
        self.kill_switch = false;
    }
    pub fn is_killed(&self) -> bool {
        self.kill_switch
    }
    pub fn limits(&self) -> &RiskLimits {
        &self.limits
    }
    pub fn authorize(
        &mut self,
        order: &OrderIntent,
        price: f64,
        spread_bps: f64,
        portfolio: &PortfolioRisk,
    ) -> Result<RiskApproval, RiskError> {
        order
            .validate()
            .map_err(|e| RiskError::Invalid(e.to_string()))?;
        if self.kill_switch {
            return Err(RiskError::KillSwitch);
        }
        if order.order_hash.len() != 64 || !order.order_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(RiskError::Invalid("order hash missing or malformed".into()));
        }
        if order.qty == 0 || order.qty > self.limits.max_single_position_qty {
            return Err(RiskError::Limit("MAX_SINGLE_POSITION_QTY".into()));
        }
        if !price.is_finite() || price <= 0.0 {
            return Err(RiskError::Invalid("invalid reference price".into()));
        }
        if spread_bps < 0.0 || !spread_bps.is_finite() {
            return Err(RiskError::Invalid("invalid spread".into()));
        }
        self.limits.validate()?;
        if !portfolio.cash.is_finite() || portfolio.cash < 0.0 {
            return Err(RiskError::Invalid("invalid portfolio cash".into()));
        }
        if !portfolio.realized_today.is_finite() {
            return Err(RiskError::Invalid("invalid realized P&L".into()));
        }
        if order.reduce_only && !matches!(order.side, th_domain::OrderSide::Sell) {
            return Err(RiskError::Invalid(
                "reduce-only orders must be sells".into(),
            ));
        }
        let n = order.qty as f64 * price * CONTRACT_MULTIPLIER;
        if !n.is_finite() || n <= 0.0 {
            return Err(RiskError::Invalid("invalid order notional".into()));
        }
        if !order.reduce_only && n > portfolio.cash {
            return Err(RiskError::Limit("INSUFFICIENT_BUYING_POWER".into()));
        }
        if let Some(strat_risk) = self.strategy_risks.get(&order.strategy_id) {
            if !strat_risk.risk_pct.is_finite()
                || strat_risk.risk_pct <= 0.0
                || strat_risk.risk_pct > 1.0
            {
                return Err(RiskError::Invalid(
                    "invalid strategy risk percentage".into(),
                ));
            }
            if !strat_risk.capital_allocated.is_finite() || strat_risk.capital_allocated <= 0.0 {
                return Err(RiskError::Invalid(
                    "invalid strategy capital allocation".into(),
                ));
            }
            if !strat_risk.risk_budget.is_finite() || strat_risk.risk_budget <= 0.0 {
                return Err(RiskError::Invalid("invalid strategy risk budget".into()));
            }
            if !order.reduce_only && n > strat_risk.capital_allocated {
                return Err(RiskError::Limit("STRATEGY_CAPITAL".into()));
            }
        }
        if n > self.limits.max_order_notional {
            return Err(RiskError::Limit("MAX_ORDER_NOTIONAL".into()));
        }
        if !order.reduce_only && portfolio.total_notional() + n > self.limits.max_total_notional {
            return Err(RiskError::Limit("MAX_TOTAL_NOTIONAL".into()));
        }
        if !order.reduce_only
            && portfolio.symbol_notional(&order.symbol) + n > self.limits.max_symbol_exposure
        {
            return Err(RiskError::Limit("MAX_SYMBOL_EXPOSURE".into()));
        }
        if !order.reduce_only && portfolio.open_positions() >= self.limits.max_positions {
            return Err(RiskError::Limit("MAX_POSITIONS".into()));
        }
        if portfolio.realized_today <= -self.limits.max_daily_loss {
            return Err(RiskError::Limit("DAILY_LOSS".into()));
        }
        if spread_bps > self.limits.max_spread_bps {
            return Err(RiskError::Limit("SPREAD".into()));
        }
        let now = Utc::now();
        let a = RiskApproval {
            token: Uuid::new_v4(),
            approved_at: now,
            expires_at: now + Duration::seconds(15),
            client_order_id: order.client_order_id,
            order_hash: order.order_hash.clone(),
            reason: "APPROVED".into(),
        };
        self.tokens.insert(a.token, a.clone());
        Ok(a)
    }
    pub fn validate_token(
        &mut self,
        a: &RiskApproval,
        client: Uuid,
        order_hash: &str,
    ) -> Result<(), RiskError> {
        let Some(stored) = self.tokens.get(&a.token) else {
            return Err(RiskError::TokenInvalid);
        };
        if stored.client_order_id != client
            || stored.order_hash != order_hash
            || Utc::now() > stored.expires_at
        {
            return Err(RiskError::TokenInvalid);
        }
        self.tokens.remove(&a.token);
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapitalAllocation {
    pub bot_id: String,
    pub amount: f64,
    pub reserved: f64,
}
#[derive(Debug, Default)]
pub struct CapitalAuthority {
    total: f64,
    allocations: HashMap<String, CapitalAllocation>,
}
impl CapitalAuthority {
    pub fn new(total: f64) -> Self {
        Self {
            total,
            allocations: HashMap::new(),
        }
    }
    pub fn total(&self) -> f64 {
        self.total
    }
    pub fn reserved(&self) -> f64 {
        self.allocations.values().map(|x| x.reserved).sum()
    }
    pub fn available(&self) -> f64 {
        (self.total - self.reserved()).max(0.0)
    }
    pub fn reserve(&mut self, bot_id: &str, amount: f64) -> Result<(), RiskError> {
        if amount <= 0.0 || !amount.is_finite() {
            return Err(RiskError::Invalid("invalid allocation".into()));
        }
        if self.available() < amount {
            return Err(RiskError::Limit("CAPITAL".into()));
        }
        if self.allocations.contains_key(bot_id) {
            return Err(RiskError::Limit("BOT_ALREADY_ALLOCATED".into()));
        }
        self.allocations.insert(
            bot_id.into(),
            CapitalAllocation {
                bot_id: bot_id.into(),
                amount,
                reserved: amount,
            },
        );
        Ok(())
    }
    pub fn release(&mut self, bot_id: &str) -> Option<CapitalAllocation> {
        self.allocations.remove(bot_id)
    }
    pub fn allocation(&self, bot_id: &str) -> Option<&CapitalAllocation> {
        self.allocations.get(bot_id)
    }
}
