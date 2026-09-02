use th_bot::{calculate_dynamic_risk_quantity, CeilingAction, DynamicSizingInputs, SizingAction};
use th_domain::CONTRACT_MULTIPLIER;

fn default_test_inputs() -> DynamicSizingInputs {
    DynamicSizingInputs {
        account_equity: 10_000.0,
        available_buying_power: 5_000.0,
        option_ask: 2.0,                 // contract premium = $200
        stop_loss_pct: 0.05,             // stop distance = $10 per contract
        multiplier: CONTRACT_MULTIPLIER, // 100.0
        strategy_confidence: 1.0,
        volatility_atr: 0.0,          // baseline
        max_trade_risk_pct: 0.02,     // 2% of $10k = $200 risk budget
        max_portfolio_risk_pct: 0.10, // $1,000 total portfolio risk
        current_portfolio_risk: 0.0,
        plan_risk_budget: 200.0,
        plan_capital_allocated: 5_000.0,
        safety_ceiling_qty: 10,
        ceiling_action: CeilingAction::ResizeToCeiling,
    }
}

#[test]
fn dynamic_sizing_scales_with_account_equity() {
    let mut inputs = default_test_inputs();
    inputs.safety_ceiling_qty = 100; // expand ceiling to observe raw calculation

    // $10,000 equity -> $200 risk budget / $10 risk per contract = 20 contracts
    let res_10k = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_10k.account_equity, 10_000.0);
    assert_eq!(res_10k.stop_distance, 10.0);
    assert_eq!(res_10k.calculated_quantity, 20);
    assert_eq!(res_10k.final_quantity, 20);
    assert_eq!(res_10k.action_taken, SizingAction::Accepted);

    // $25,000 equity -> $500 risk budget / $10 risk per contract = 50 contracts
    inputs.account_equity = 25_000.0;
    inputs.available_buying_power = 20_000.0;
    inputs.plan_risk_budget = 1_000.0;
    inputs.plan_capital_allocated = 20_000.0;
    let res_25k = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_25k.calculated_quantity, 50);
    assert_eq!(res_25k.final_quantity, 50);
}

#[test]
fn dynamic_sizing_respects_buying_power_constraint() {
    let mut inputs = default_test_inputs();
    inputs.safety_ceiling_qty = 100;
    // Risk budget allows 20 contracts, but buying power is only $650 (at $200 per contract = 3 contracts)
    inputs.available_buying_power = 650.0;

    let res = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res.calculated_quantity, 3);
    assert_eq!(res.final_quantity, 3);
    assert_eq!(res.action_taken, SizingAction::Accepted);
}

#[test]
fn dynamic_sizing_adjusts_for_volatility_atr() {
    let mut inputs = default_test_inputs();
    inputs.safety_ceiling_qty = 100;

    // Normal volatility (atr = 0.0) -> 20 contracts
    let res_low_vol = calculate_dynamic_risk_quantity(&inputs).unwrap();

    // High volatility (e.g. atr = 0.50) -> scales down risk budget and contracts
    inputs.volatility_atr = 0.50;
    let res_high_vol = calculate_dynamic_risk_quantity(&inputs).unwrap();

    assert!(
        res_high_vol.calculated_quantity < res_low_vol.calculated_quantity,
        "High volatility must scale down position size to dampen risk exposure"
    );
}

#[test]
fn dynamic_sizing_scales_with_stop_loss_distance() {
    let mut inputs = default_test_inputs();
    inputs.safety_ceiling_qty = 100;

    // 5% stop -> $10 risk per contract -> 20 contracts
    let res_5pct = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_5pct.stop_distance, 10.0);
    assert_eq!(res_5pct.calculated_quantity, 20);

    // 10% stop -> $20 risk per contract -> 10 contracts
    inputs.stop_loss_pct = 0.10;
    let res_10pct = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_10pct.stop_distance, 20.0);
    assert_eq!(res_10pct.calculated_quantity, 10);
}

#[test]
fn dynamic_sizing_scales_with_strategy_confidence() {
    let mut inputs = default_test_inputs();
    inputs.safety_ceiling_qty = 100;

    // Confidence 1.0 -> 20 contracts
    let res_full = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_full.calculated_quantity, 20);

    // Confidence 0.5 -> 10 contracts
    inputs.strategy_confidence = 0.5;
    let res_half = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res_half.calculated_quantity, 10);
}

#[test]
fn dynamic_sizing_resizes_when_exceeding_safety_ceiling() {
    let mut inputs = default_test_inputs();
    // Risk budget allows 20 contracts, but safety ceiling is strictly 5
    inputs.safety_ceiling_qty = 5;
    inputs.ceiling_action = CeilingAction::ResizeToCeiling;

    let res = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res.calculated_quantity, 20);
    assert_eq!(res.final_quantity, 5);
    assert_eq!(res.action_taken, SizingAction::ResizedToCeiling);
    assert!(res
        .reason
        .contains("exceeded safety ceiling 5, resized to ceiling"));
}

#[test]
fn dynamic_sizing_rejects_when_configured_to_reject_on_ceiling_breach() {
    let mut inputs = default_test_inputs();
    // Risk budget allows 20 contracts, safety ceiling is 5, policy is Reject
    inputs.safety_ceiling_qty = 5;
    inputs.ceiling_action = CeilingAction::Reject;

    let res = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res.calculated_quantity, 20);
    assert_eq!(res.final_quantity, 0);
    assert_eq!(res.action_taken, SizingAction::RejectedExceedsCeiling);
    assert!(res
        .reason
        .contains("exceeded safety ceiling 5, trade rejected"));
}

#[test]
fn dynamic_sizing_rejects_zero_quantity_when_budget_insufficient() {
    let mut inputs = default_test_inputs();
    // Buying power only $50, but contract costs $200
    inputs.available_buying_power = 50.0;

    let res = calculate_dynamic_risk_quantity(&inputs).unwrap();
    assert_eq!(res.calculated_quantity, 0);
    assert_eq!(res.final_quantity, 0);
    assert_eq!(res.action_taken, SizingAction::RejectedZeroQuantity);
    assert!(res
        .reason
        .contains("insufficient risk budget or buying power"));
}

#[test]
fn dynamic_sizing_validates_safety_parameters() {
    let mut inputs = default_test_inputs();

    // Zero ceiling must be rejected
    inputs.safety_ceiling_qty = 0;
    assert!(calculate_dynamic_risk_quantity(&inputs).is_err());

    // Negative or zero equity must be rejected
    inputs.safety_ceiling_qty = 10;
    inputs.account_equity = 0.0;
    assert!(calculate_dynamic_risk_quantity(&inputs).is_err());

    // Invalid stop loss (> 1.0 or <= 0.0) must be rejected
    inputs.account_equity = 10_000.0;
    inputs.stop_loss_pct = 1.5;
    assert!(calculate_dynamic_risk_quantity(&inputs).is_err());
}
