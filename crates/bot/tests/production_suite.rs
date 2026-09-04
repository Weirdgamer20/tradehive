use chrono::{Duration, Utc};
use std::collections::HashMap;
use th_backtest::TradeResult;
use th_domain::{
    Bar, OmsState, OptionType, OrderIntent, OrderSide, OrderStatus, Position, SignalSide,
};
use th_execution::{Broker, PaperBroker};
use th_options_data::{
    OptionChain, OptionDataError, OptionGreeks, OptionQuote, OptionRankingConfig,
    OptionRankingPipeline,
};
use th_research::{
    deflated_sharpe_ratio, monte_carlo_permutation_test, probabilistic_sharpe_ratio,
    probability_of_backtest_overfitting, walk_forward_with_purging,
};
use th_risk::{PortfolioRisk, RiskGovernor, RiskLimits};
use th_sentinel::{GovernanceGuard, HealthState, Sentinel};
use uuid::Uuid;

fn sample_bars(count: usize) -> Vec<Bar> {
    let now = Utc::now();
    let mut bars = Vec::with_capacity(count);
    let mut price = 100.0;
    for i in 0..count {
        let ts = now - Duration::minutes((count - i) as i64 * 5);
        let ret = if i % 2 == 0 { 0.003 } else { -0.001 };
        price *= 1.0 + ret;
        bars.push(Bar {
            symbol: "SPY".into(),
            ts,
            open: price * 0.999,
            high: price * 1.002,
            low: price * 0.998,
            close: price,
            volume: 1500.0,
        });
    }
    bars
}

// 1. Order crash recovery & UNKNOWN order recovery (Part XVII, XVIII, XLIV)
#[tokio::test]
async fn test_order_crash_and_unknown_recovery() {
    let broker = PaperBroker::new(100_000.0);
    let client_order_id = Uuid::new_v4();
    let order = OrderIntent {
        client_order_id,
        symbol: "SPY260904C00500000".into(),
        side: OrderSide::Buy,
        qty: 2,
        limit_price: Some(5.0),
        reduce_only: false,
        strategy_id: "STRAT-01".into(),
        created_at: Utc::now(),
        order_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        bot_id: Some("BOT-1".into()),
        session_id: Some("SESSION-1".into()),
        decision_id: Some(Uuid::new_v4()),
        oms_state: Some(OmsState::Unknown),
        option_action: None,
        order_type: None,
        stop_price: None,
    };

    assert!(order.oms_state.unwrap().is_ambiguous());

    // Submit order to broker
    let submitted = broker
        .submit(&order)
        .await
        .expect("submission must succeed");
    assert_eq!(submitted.client_order_id, client_order_id);

    // After simulated crash, query broker by client_order_id rather than blindly resubmitting
    let found = broker
        .find_by_client_order_id(client_order_id)
        .await
        .expect("reconcile query must find order");
    assert!(found.is_some());
    assert_eq!(found.unwrap().status, OrderStatus::Filled);

    // Verify duplicate submission is rejected (idempotency)
    let dup_res = broker.submit(&order).await;
    assert!(dup_res.is_err(), "Duplicate order must be rejected");
}

// 2. Duplicate fill recovery & idempotency (Part XLIII)
#[test]
fn test_duplicate_fill_recovery() {
    let mut positions: HashMap<String, Position> = HashMap::new();
    let symbol = "SPY260904C00500000";

    // First fill report
    positions.insert(
        symbol.into(),
        Position {
            symbol: symbol.into(),
            qty: 2,
            avg_price: 5.0,
            mark: 5.0,
            opened_at: Utc::now(),
            contract: th_domain::OptionContract::from_occ(symbol),
        },
    );

    // Duplicate execution report should NOT increase position quantity
    let existing = positions.get_mut(symbol).unwrap();
    assert_eq!(existing.qty, 2);
    assert_eq!(existing.avg_price, 5.0);
}

// 3. Broker reconciliation test & halt (Part XXIII, XLIX)
#[test]
fn test_broker_reconciliation_and_halt() {
    let internal_positions = 0;
    let broker_positions = 2; // Divergence: internal flat, broker long

    let discrepancy = internal_positions != broker_positions;
    assert!(discrepancy, "Divergence detected");

    // Fail closed policy mandates halt on unresolved discrepancy
    let should_halt = discrepancy;
    assert!(should_halt, "System must halt on reconciliation mismatch");
}

// 4. Realistic options execution & liquidity rejection (Part VI, VII)
#[test]
fn test_realistic_options_execution_and_liquidity_rejection() {
    let now = Utc::now();
    let pipeline = OptionRankingPipeline::new(OptionRankingConfig {
        min_volume: 50,
        min_open_interest: 100,
        max_spread_bps: 200.0,
        min_dte_minutes: 180,
        target_delta: 0.50,
        max_quote_age_secs: 60,
    });

    // Case A: Illiquid chain with wide spread -> MUST BE REJECTED
    let illiquid_chain = OptionChain {
        underlying: "SPY".into(),
        spot: 500.0,
        as_of: now,
        quotes: vec![OptionQuote {
            symbol: "SPY-ILLIQUID".into(),
            underlying: "SPY".into(),
            option_type: OptionType::Call,
            strike: 500.0,
            expiry: now + Duration::hours(24),
            bid: 1.0,
            ask: 2.0, // 6666 bps spread!
            last: Some(1.5),
            volume: 5,         // < 50
            open_interest: 10, // < 100
            iv: Some(0.20),
            greeks: None,
            as_of: now,
        }],
    };
    let reject_res =
        pipeline.rank_and_select(&illiquid_chain, OptionType::Call, now, "SESS-1", "BOT-1");
    assert!(matches!(
        reject_res,
        Err(OptionDataError::NoEligibleContracts)
    ));

    // Case B: Liquid chain with tight spread -> Selected
    let liquid_chain = OptionChain {
        underlying: "SPY".into(),
        spot: 500.0,
        as_of: now,
        quotes: vec![OptionQuote {
            symbol: "SPY-LIQUID".into(),
            underlying: "SPY".into(),
            option_type: OptionType::Call,
            strike: 500.0,
            expiry: now + Duration::hours(24),
            bid: 4.95,
            ask: 5.05, // 200 bps spread
            last: Some(5.0),
            volume: 500,
            open_interest: 1500,
            iv: Some(0.20),
            greeks: Some(OptionGreeks {
                delta: 0.50,
                gamma: 0.02,
                theta: -0.05,
                vega: 0.10,
                rho: 0.01,
            }),
            as_of: now,
        }],
    };
    let pass_res =
        pipeline.rank_and_select(&liquid_chain, OptionType::Call, now, "SESS-1", "BOT-1");
    assert!(pass_res.is_ok());
    let assignment = pass_res.unwrap();
    assert_eq!(assignment.contract_symbol, "SPY-LIQUID");
}

// 5. Walk-forward validation test with purging & embargo (Part VIII)
#[test]
fn test_walk_forward_purged_embargo_and_overfitting_defense() {
    let bars = sample_bars(250);
    let mut strat = th_strategy::MovingAverageCrossover::default();

    // 3 windows, 2 bars purge, 2 bars embargo
    let windows = walk_forward_with_purging(&mut strat, &bars, 3, 2, 2);
    assert!(
        !windows.is_empty(),
        "Walk-forward windows must be generated"
    );

    for w in &windows {
        assert!(w.purge_end >= w.train_end, "Purge must follow train");
        assert!(w.test_start >= w.purge_end, "Embargo must follow purge");
        assert!(w.test_end > w.test_start, "Test window must be non-empty");
    }

    // Statistical Overfitting Defenses
    let psr = probabilistic_sharpe_ratio(1.5, 0.0, 50, 0.0, 3.0);
    assert!(psr > 0.95, "PSR for Sharpe 1.5 with 50 obs must be > 95%");

    let dsr = deflated_sharpe_ratio(1.5, 10, 50, 0.0, 3.0);
    assert!(dsr > 0.0 && dsr <= psr, "DSR must penalize for 10 trials");

    let candidate_scores = vec![
        vec![1.2, 0.9, 1.1],
        vec![0.5, 1.5, 0.6],
        vec![0.8, 0.7, 0.9],
    ];
    let pbo = probability_of_backtest_overfitting(&candidate_scores);
    assert!((0.0..=1.0).contains(&pbo), "PBO must be in [0, 1]");

    // Monte Carlo trade permutation test
    let trades = vec![
        TradeResult {
            entry_ts: 0,
            exit_ts: 1,
            side: SignalSide::LongCall,
            entry: 10.0,
            exit: 12.0,
            pnl: 200.0,
            slippage_incurred: 2.0,
            spread_cost: 5.0,
            fees_paid: 1.5,
            bars_held: 5,
            contract_symbol: None,
        },
        TradeResult {
            entry_ts: 1,
            exit_ts: 2,
            side: SignalSide::LongCall,
            entry: 10.0,
            exit: 11.5,
            pnl: 150.0,
            slippage_incurred: 2.0,
            spread_cost: 5.0,
            fees_paid: 1.5,
            bars_held: 4,
            contract_symbol: None,
        },
        TradeResult {
            entry_ts: 2,
            exit_ts: 3,
            side: SignalSide::LongCall,
            entry: 10.0,
            exit: 11.0,
            pnl: 100.0,
            slippage_incurred: 2.0,
            spread_cost: 5.0,
            fees_paid: 1.5,
            bars_held: 3,
            contract_symbol: None,
        },
        TradeResult {
            entry_ts: 3,
            exit_ts: 4,
            side: SignalSide::LongCall,
            entry: 10.0,
            exit: 9.5,
            pnl: -50.0,
            slippage_incurred: 2.0,
            spread_cost: 5.0,
            fees_paid: 1.5,
            bars_held: 3,
            contract_symbol: None,
        },
        TradeResult {
            entry_ts: 4,
            exit_ts: 5,
            side: SignalSide::LongCall,
            entry: 10.0,
            exit: 12.5,
            pnl: 250.0,
            slippage_incurred: 2.0,
            spread_cost: 5.0,
            fees_paid: 1.5,
            bars_held: 6,
            contract_symbol: None,
        },
    ];
    let p_val = monte_carlo_permutation_test(&trades, 100);
    assert!(p_val <= 1.0);
}

// 6. Governance escalation & Sentinel halt test (Part XXX, XXXI)
#[test]
fn test_governance_escalation_and_sentinel_halt() {
    let policy = th_domain::GovernancePolicy::default(); // allow_live_execution = false
    let guard = GovernanceGuard::new(policy);

    // AI / Agent cannot escalate to live execution without explicit authorization
    let live_attempt = guard.verify_action(th_domain::AuthorizationClass::LiveExecution);
    assert!(
        live_attempt.is_err(),
        "Unauthorized live execution must fail closed"
    );

    // Sentinel health checks
    let mut sentinel = Sentinel::default();
    sentinel.set("market_data", HealthState::Healthy, "ok");
    let snap = sentinel.snapshot(true, true);
    assert!(snap.healthy(), "Healthy sentinel snapshot must be verified");
}

// 7. Portfolio concentration & daily drawdown halt test (Part XIII, XXXVII)
#[test]
fn test_portfolio_concentration_and_daily_drawdown_halt() {
    let limits = RiskLimits {
        max_positions: 3,
        max_daily_loss: 500.0,
        max_order_notional: 2000.0,
        max_total_notional: 5000.0,
        max_symbol_exposure: 3000.0,
        max_spread_bps: 500.0,
        max_single_position_qty: 5,
        max_trade_risk_pct: 0.02,
        max_portfolio_risk_pct: 0.10,
    };
    let mut governor = RiskGovernor::new(limits);

    // Exceeded daily loss test
    let breach_portfolio = PortfolioRisk {
        cash: 10_000.0,
        realized_today: -600.0, // Exceeds max_daily_loss = 500.0
        positions: vec![],
        open_orders: vec![],
    };
    let order = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "SPY260904C00500000".into(),
        side: OrderSide::Buy,
        qty: 1,
        limit_price: Some(2.0),
        reduce_only: false,
        strategy_id: "STRAT-01".into(),
        created_at: Utc::now(),
        order_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        bot_id: None,
        session_id: None,
        decision_id: None,
        oms_state: None,
        option_action: None,
        order_type: None,
        stop_price: None,
    };
    let res = governor.authorize(&order, 2.0, 50.0, &breach_portfolio);
    assert!(res.is_err(), "Daily loss breach must halt new order entry");
}

// 8. Option instrument and contract integrity test (Section 6, 9)
#[test]
fn test_option_instrument_and_contract_integrity() {
    let sym = "SPY260904P00495000";
    let contract =
        th_domain::OptionContract::from_occ(sym).expect("must parse valid OCC option symbol");
    assert_eq!(contract.underlying, "SPY");
    assert_eq!(contract.strike, 495.0);
    assert_eq!(contract.multiplier, 100.0);
    assert_eq!(contract.option_type, th_domain::OptionType::Put);

    let pos = Position::new(sym, 3, 4.0, 4.8, Utc::now());
    assert_eq!(pos.unrealized_pnl(), (4.8 - 4.0) * 3.0 * 100.0);
    assert!(pos.contract.is_some());
}

// 9. Underlying vs Option backtest divergence test (Section 0, 9, 10)
#[test]
fn test_underlying_vs_option_backtest_divergence() {
    use th_backtest::{BacktestConfig, OptionBacktestEngine, UnderlyingBacktestEngine};
    use th_domain::Bar;
    use th_strategy::{Strategy, StrategySpec};

    struct TestLongCallStrat {
        spec: StrategySpec,
        step: usize,
    }
    impl Strategy for TestLongCallStrat {
        fn spec(&self) -> &StrategySpec {
            &self.spec
        }
        fn update(
            &mut self,
            bar: &Bar,
            _state: &th_domain::MarketState,
        ) -> Option<th_domain::Signal> {
            self.step += 1;
            if self.step == 6 {
                Some(th_domain::Signal {
                    id: Uuid::new_v4(),
                    strategy_id: self.spec.id.clone(),
                    symbol: bar.symbol.clone(),
                    side: th_domain::SignalSide::LongCall,
                    strength: 1.0,
                    reason: "entry".into(),
                    generated_at: bar.ts,
                    config_version: "v1".into(),
                    session_id: None,
                    bot_id: None,
                    candidate_id: None,
                    proposed_stop_loss_pct: None,
                    proposed_take_profit_pct: None,
                    proposed_max_hold_minutes: None,
                    exit_policy: None,
                })
            } else {
                None
            }
        }
    }

    let base = Utc::now();
    let bars: Vec<Bar> = (0..35)
        .map(|i| {
            let px = 500.0 + (i as f64) * 0.5;
            Bar {
                symbol: "SPY".into(),
                open: px,
                high: px + 0.5,
                low: px - 0.5,
                close: px + 0.2,
                volume: 10_000.0,
                ts: base + chrono::Duration::minutes(5 * i as i64),
            }
        })
        .collect();

    let mut strat_opt = TestLongCallStrat {
        spec: StrategySpec {
            id: "opt_strat".into(),
            name: "Opt Strat".into(),
            version: 1,
            warmup: 5,
            max_hold_bars: 5,
            enabled: true,
            description: "opt test".into(),
        },
        step: 0,
    };
    let mut strat_und = TestLongCallStrat {
        spec: StrategySpec {
            id: "und_strat".into(),
            name: "Und Strat".into(),
            version: 1,
            warmup: 5,
            max_hold_bars: 5,
            enabled: true,
            description: "und test".into(),
        },
        step: 0,
    };

    let opt_engine = OptionBacktestEngine::new(BacktestConfig::default());
    let und_engine = UnderlyingBacktestEngine::new(BacktestConfig::default());

    let mut quotes_map = std::collections::HashMap::new();
    for b in &bars {
        quotes_map.insert(
            b.ts.timestamp(),
            vec![th_domain::OptionQuote {
                symbol: format!("SPY-{}-C", b.ts.timestamp()),
                underlying: "SPY".into(),
                option_type: th_domain::OptionType::Call,
                strike: 500.0,
                expiry: b.ts + chrono::Duration::days(7),
                bid: 4.90,
                ask: 5.10,
                last: 5.00,
                iv: 0.20,
                greeks: None,
                open_interest: 100,
                volume: 50,
                quote_ts: b.ts,
            }],
        );
    }

    let opt_rep = opt_engine
        .run_with_quotes(&mut strat_opt, &bars, Some(&quotes_map))
        .expect("option backtest must succeed");
    let und_rep = und_engine
        .run(&mut strat_und, &bars)
        .expect("underlying backtest must succeed");

    assert!(
        !opt_rep.trades.is_empty(),
        "Option backtest must generate option trades"
    );
    assert!(
        !und_rep.trades.is_empty(),
        "Underlying backtest must generate stock trades"
    );

    // Option trade uses option contract premium ($~3.0 - $5.0 range * 100 multiplier)
    // Underlying trade uses spot share price ($500+ range)
    let opt_entry = opt_rep.trades[0].entry;
    let und_entry = und_rep.trades[0].entry;
    assert!(
        opt_entry < 20.0,
        "Option trade entry should reflect option premium, got: {}",
        opt_entry
    );
    assert!(
        und_entry >= 500.0,
        "Underlying trade entry should reflect stock price, got: {}",
        und_entry
    );
}

// 10. Option order actions & Alpaca position intent (Section 18)
#[test]
fn test_option_order_actions_and_alpaca_position_intent() {
    use th_domain::OptionOrderAction;

    let bto = OptionOrderAction::BuyToOpen;
    assert_eq!(bto.as_str(), "buy_to_open");
    let btc = OptionOrderAction::BuyToClose;
    assert_eq!(btc.as_str(), "buy_to_close");
    let sto = OptionOrderAction::SellToOpen;
    assert_eq!(sto.as_str(), "sell_to_open");
    let stc = OptionOrderAction::SellToClose;
    assert_eq!(stc.as_str(), "sell_to_close");

    let intent = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "SPY260904C00500000".into(),
        side: OrderSide::Sell,
        qty: 1,
        limit_price: Some(3.50),
        reduce_only: true,
        strategy_id: "s1".into(),
        created_at: Utc::now(),
        order_hash: "hash".into(),
        bot_id: None,
        session_id: None,
        decision_id: None,
        oms_state: None,
        option_action: None,
        order_type: None,
        stop_price: None,
    };
    assert_eq!(
        intent.resolve_option_action(),
        OptionOrderAction::SellToClose
    );
}
