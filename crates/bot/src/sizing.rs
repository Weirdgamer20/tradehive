use crate::RuntimeError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CeilingAction {
    #[default]
    ResizeToCeiling,
    Reject,
}

impl CeilingAction {
    pub fn from_env() -> Self {
        match std::env::var("RISK_SIZING_CEILING_ACTION")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "reject" | "block" => Self::Reject,
            _ => Self::ResizeToCeiling,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizingAction {
    Accepted,
    ResizedToCeiling,
    RejectedExceedsCeiling,
    RejectedInsufficientBudget,
    RejectedZeroQuantity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicSizingInputs {
    pub account_equity: f64,
    pub available_buying_power: f64,
    pub option_ask: f64,
    pub stop_loss_pct: f64,
    pub multiplier: f64,
    pub strategy_confidence: f64,
    pub volatility_atr: f64,
    pub max_trade_risk_pct: f64,
    pub max_portfolio_risk_pct: f64,
    pub current_portfolio_risk: f64,
    pub plan_risk_budget: f64,
    pub plan_capital_allocated: f64,
    pub safety_ceiling_qty: u32,
    pub ceiling_action: CeilingAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DynamicSizingResult {
    pub calculated_quantity: u32,
    pub final_quantity: u32,
    pub account_equity: f64,
    pub available_buying_power: f64,
    pub instrument_price: f64,
    pub contract_cost: f64,
    pub stop_distance: f64,
    pub volatility_atr: f64,
    pub risk_budget: f64,
    pub strategy_confidence: f64,
    pub safety_ceiling: u32,
    pub action_taken: SizingAction,
    pub reason: String,
}

/// Calculate dynamic, risk-based position size from account equity, available buying power,
/// instrument price, volatility/ATR, stop-loss distance, strategy confidence, and configured
/// portfolio/trade risk limits.
pub fn calculate_dynamic_risk_quantity(
    inputs: &DynamicSizingInputs,
) -> Result<DynamicSizingResult, RuntimeError> {
    if !inputs.account_equity.is_finite() || inputs.account_equity <= 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "account equity must be positive and finite".into(),
        ));
    }
    if !inputs.available_buying_power.is_finite() || inputs.available_buying_power < 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "available buying power must be non-negative and finite".into(),
        ));
    }
    if !inputs.option_ask.is_finite() || inputs.option_ask <= 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "option ask price must be positive and finite".into(),
        ));
    }
    if !inputs.stop_loss_pct.is_finite()
        || inputs.stop_loss_pct <= 0.0
        || inputs.stop_loss_pct >= 1.0
    {
        return Err(RuntimeError::InvalidConfig(
            "stop loss pct must be strictly between 0 and 1".into(),
        ));
    }
    if !inputs.multiplier.is_finite() || inputs.multiplier <= 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "contract multiplier must be positive".into(),
        ));
    }
    if !inputs.max_trade_risk_pct.is_finite() || inputs.max_trade_risk_pct <= 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "max trade risk pct must be positive".into(),
        ));
    }
    if !inputs.max_portfolio_risk_pct.is_finite() || inputs.max_portfolio_risk_pct <= 0.0 {
        return Err(RuntimeError::InvalidConfig(
            "max portfolio risk pct must be positive".into(),
        ));
    }
    if inputs.safety_ceiling_qty == 0 {
        return Err(RuntimeError::InvalidConfig(
            "safety ceiling quantity must be greater than zero".into(),
        ));
    }

    let contract_cost = inputs.option_ask * inputs.multiplier;
    let stop_distance = contract_cost * inputs.stop_loss_pct;

    // Volatility adjustment factor
    let vol_scale = if inputs.volatility_atr.is_finite() && inputs.volatility_atr > 0.0 {
        (1.0 / (1.0 + inputs.volatility_atr)).clamp(0.2, 2.0)
    } else {
        1.0
    };
    let effective_stop_distance = stop_distance / vol_scale;

    // Strategy confidence factor
    let confidence = if inputs.strategy_confidence.is_finite() && inputs.strategy_confidence > 0.0 {
        inputs.strategy_confidence.clamp(0.1, 1.0)
    } else {
        1.0
    };

    // Calculate trade risk budget and portfolio risk headroom
    let max_trade_dollar_risk = inputs.account_equity * inputs.max_trade_risk_pct;
    let plan_budget = if inputs.plan_risk_budget.is_finite() && inputs.plan_risk_budget > 0.0 {
        inputs.plan_risk_budget
    } else {
        max_trade_dollar_risk
    };
    let trade_risk_budget =
        (max_trade_dollar_risk.min(plan_budget) * confidence * vol_scale).max(0.0);

    let max_portfolio_dollar_risk = inputs.account_equity * inputs.max_portfolio_risk_pct;
    let remaining_portfolio_risk =
        (max_portfolio_dollar_risk - inputs.current_portfolio_risk).max(0.0);
    let risk_budget = trade_risk_budget.min(remaining_portfolio_risk);

    // Compute independent capacities
    let qty_by_risk = if effective_stop_distance > 0.0 {
        (risk_budget / effective_stop_distance).floor() as u32
    } else {
        0
    };

    let plan_cap =
        if inputs.plan_capital_allocated.is_finite() && inputs.plan_capital_allocated > 0.0 {
            inputs.plan_capital_allocated
        } else {
            inputs.available_buying_power
        };
    let capital_limit = inputs.available_buying_power.min(plan_cap);
    let qty_by_capital = (capital_limit / contract_cost).floor() as u32;

    let calculated_quantity = qty_by_risk.min(qty_by_capital);

    // Evaluate against emergency hard safety ceiling guardrail
    let (final_quantity, action_taken, reason) = if calculated_quantity == 0 {
        (
            0,
            SizingAction::RejectedZeroQuantity,
            "Calculated quantity is 0: insufficient risk budget or buying power".into(),
        )
    } else if inputs.safety_ceiling_qty < u32::MAX
        && calculated_quantity > inputs.safety_ceiling_qty
    {
        match inputs.ceiling_action {
            CeilingAction::ResizeToCeiling => (
                inputs.safety_ceiling_qty,
                SizingAction::ResizedToCeiling,
                format!(
                    "Calculated quantity {} exceeded safety ceiling {}, resized to ceiling",
                    calculated_quantity, inputs.safety_ceiling_qty
                ),
            ),
            CeilingAction::Reject => (
                0,
                SizingAction::RejectedExceedsCeiling,
                format!(
                    "Calculated quantity {} exceeded safety ceiling {}, trade rejected",
                    calculated_quantity, inputs.safety_ceiling_qty
                ),
            ),
        }
    } else {
        (
            calculated_quantity,
            SizingAction::Accepted,
            "Calculated quantity accepted within risk limits and dynamic capacity".into(),
        )
    };

    Ok(DynamicSizingResult {
        calculated_quantity,
        final_quantity,
        account_equity: inputs.account_equity,
        available_buying_power: inputs.available_buying_power,
        instrument_price: inputs.option_ask,
        contract_cost,
        stop_distance,
        volatility_atr: inputs.volatility_atr,
        risk_budget,
        strategy_confidence: confidence,
        safety_ceiling: inputs.safety_ceiling_qty,
        action_taken,
        reason,
    })
}
