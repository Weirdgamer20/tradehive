use chrono::Utc;
use th_domain::{AuthorizationClass, GovernancePolicy, OptionType, Regime};
use th_hive::{PortfolioMetaController, SelfImprovementEngine};
use th_memory::{ExperienceStore, TradeRecord};
use th_research::{AdversarialEvaluator, EvaluationMetrics, IndependentEvaluator};
use th_sentinel::GovernanceGuard;
use th_strategy::StrategyGenome;
use uuid::Uuid;

#[test]
fn test_governance_authorization_and_guard() {
    let mut policy = GovernancePolicy::default();
    assert!(!policy.allow_live_execution);
    assert!(policy.validate_execution_mode(true).is_err());
    assert!(policy.validate_execution_mode(false).is_ok());

    let guard = GovernanceGuard::new(policy.clone());
    assert!(guard.verify_action(AuthorizationClass::Research).is_ok());
    assert!(guard
        .verify_action(AuthorizationClass::LiveExecution)
        .is_err());

    policy
        .active_authorizations
        .push(AuthorizationClass::LiveExecution);
    policy.allow_live_execution = true;
    policy.max_live_capital = 50000.0;
    let guard_live = GovernanceGuard::new(policy);
    assert!(guard_live
        .authorize_capital_allocation(25000.0, true)
        .is_ok());
    assert!(guard_live
        .authorize_capital_allocation(75000.0, true)
        .is_err());
}

#[test]
fn test_strategy_genome_lineage_and_recombination() {
    let root = StrategyGenome::new_root("STRAT-MOMENTUM", 1);
    assert_eq!(root.generation, 0);
    assert!(root.parent_id.is_none());

    let child = root.mutate(2, "PARAM_MUTATION", 0.1);
    assert_eq!(child.version, 2);
    assert_eq!(child.generation, 1);
    assert_eq!(child.parent_id, Some("STRAT-MOMENTUM".into()));

    let partner = StrategyGenome::new_root("STRAT-VOLATILITY", 1);
    let hybrid = StrategyGenome::recombine(&child, &partner, "STRAT-HYBRID", 1);
    assert_eq!(hybrid.generation, 2);
    assert!(hybrid.parent_id.unwrap().contains('+'));
}

#[test]
fn test_independent_and_adversarial_evaluator() {
    let evaluator = IndependentEvaluator::default();
    let bars = vec![];
    let opinions = vec![];
    let res = evaluator.evaluate("CAND-01", "SPY", "STRAT-01", 1, &bars, &opinions);
    assert!(
        !res.promoted,
        "Empty/insufficient candidate must not be promoted"
    );

    let metrics = EvaluationMetrics {
        in_sample_sharpe: 1.5,
        out_of_sample_sharpe: 1.2,
        walk_forward_efficiency: 0.8,
        max_drawdown: 0.05,
        turnover: 1.0,
        win_rate: 0.58,
        profit_factor: 1.8,
        trade_count: 25,
        expected_alpha: 0.02,
        estimated_slippage_bps: 4.0,
        spread_penalty: 0.001,
        liquidity_penalty: 0.0005,
        net_utility: 0.015,
    };

    let adv = AdversarialEvaluator::test_candidate(&[], &metrics, &[]);
    assert!(
        adv.overall_approved,
        "Valid metrics should clear adversarial evaluation"
    );
}

#[test]
fn test_portfolio_meta_controller_and_experience_store() {
    let controller = PortfolioMetaController::default();
    let plan = th_deployment::BotCreationPlan {
        plan_id: "PLAN-1".into(),
        bot_id: "BOT-1".into(),
        strategy_id: "STRAT-01".into(),
        strategy_version: 1,
        config_version: "v1".into(),
        underlying: "SPY".into(),
        option_symbol: "SPY260904C00500000".into(),
        option_type: OptionType::Call,
        strike: 500.0,
        expiry: Utc::now() + chrono::Duration::hours(24),
        capital_allocated: 10000.0,
        risk_budget: 200.0,
        min_expiry_minutes: 180,
        max_expiry_minutes: 1440,
        created_at: Utc::now(),
        fingerprint: "fp".into(),
        quantity: 0,
        entry_limit: 0.0,
        stop_loss_pct: 0.05,
        take_profit_pct: 0.10,
        generation_id: "GEN-01".into(),
        risk_pct: 0.02,
        max_capital_exposure: 10000.0,
        rl_state: None,
        rl_action: None,
        rl_confidence: 1.0,
        session_id: "SESSION-01".into(),
    };

    let signal = th_domain::Signal {
        id: Uuid::new_v4(),
        strategy_id: "STRAT-01".into(),
        symbol: "SPY".into(),
        side: th_domain::SignalSide::LongCall,
        strength: 0.8,
        reason: "Momentum".into(),
        generated_at: Utc::now(),
        config_version: "v1".into(),
    };

    let allocations =
        controller.calculate_target_allocations(&[signal], &[plan], 50000.0, Regime::TrendingBull);
    assert!(allocations.contains_key("BOT-1"));
    assert!(*allocations.get("BOT-1").unwrap() > 0.0);

    let mut exp = ExperienceStore::default();
    let trade = TradeRecord {
        trade_id: "T-01".into(),
        symbol: "SPY".into(),
        strategy_id: "STRAT-01".into(),
        session_id: "SESSION-01".into(),
        entry: Utc::now(),
        exit: Some(Utc::now()),
        pnl: 250.0,
        fees: 1.0,
        reason: "take_profit".into(),
        signal_price: Some(5.0),
        quote_spread_bps: Some(10.0),
        entry_fill_price: Some(5.0),
        exit_fill_price: Some(6.25),
        slippage_bps: Some(2.0),
        latency_ms: Some(15),
        regime_at_entry: Some("TrendingBull".into()),
    };
    exp.record_trade(trade.clone());
    let autopsy = exp.autopsy(&trade);
    assert_eq!(autopsy.outcome, "win");
    assert!(autopsy.execution_quality_score > 0.8);
    assert_eq!(exp.success_rate_for_regime("TrendingBull"), 1.0);
}

#[test]
fn test_bounded_self_improvement_engine() {
    let mut engine = SelfImprovementEngine::default();
    let mut exp = ExperienceStore::default();

    let loss_trade = TradeRecord {
        trade_id: "T-LOSS-01".into(),
        symbol: "SPY".into(),
        strategy_id: "STRAT-01".into(),
        session_id: "SESSION-01".into(),
        entry: Utc::now(),
        exit: Some(Utc::now()),
        pnl: -300.0,
        fees: 1.0,
        reason: "stop_loss".into(),
        signal_price: Some(5.0),
        quote_spread_bps: Some(40.0),
        entry_fill_price: Some(5.0),
        exit_fill_price: Some(3.5),
        slippage_bps: Some(35.0),
        latency_ms: Some(20),
        regime_at_entry: Some("Range".into()),
    };
    exp.record_trade(loss_trade.clone());
    let autopsy = exp.autopsy(&loss_trade);

    let hypotheses = engine.diagnose(&[autopsy]);
    assert_eq!(hypotheses.len(), 1);

    let genome = StrategyGenome::new_root("STRAT-01", 1);
    let evolved = engine.evolve_strategy(&genome, &hypotheses[0]);
    assert_eq!(evolved.version, 2);

    let promo = engine.promote_candidate(&evolved, "Passed validation sandbox");
    assert_eq!(promo.new_version, 2);
}
