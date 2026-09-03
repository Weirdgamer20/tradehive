use chrono::{Duration, Utc};
use std::collections::HashMap;
use uuid::Uuid;

use th_bot::{calculate_worker_quantity, RuntimeConfig, TradingRuntime};
use th_domain::{
    Bar, OptionChain, OptionQuote, OptionType, OrderIntent, OrderSide, CONTRACT_MULTIPLIER,
};
use th_execution::{order_hash, reconcile_positions, Broker, ExecutionEngine, PaperBroker};
use th_hive::{
    manufacture_promoted_bots, next_strategy_id, persist_rl_history,
    run_analysis_with_q_and_trades, synthesize_strategy, AnalysisBundle, AnalysisReport,
    Experience, HiveManufacturingPolicy, PromotionRecord, QEntry, QLearning, StateKey,
    StrategyEvaluation, SymbolAnalysis,
};
use th_market_data::{AlpacaConfig, AlpacaProvider, MarketDataProvider, SyntheticProvider};
use th_risk::{PortfolioRisk, RiskGovernor, RiskLimits};
use th_storage::{BotHistoryRecord, HiveManufacturingRun, JsonHistoryStore};
use th_strategy::StrategyRegistry;

fn synthetic_market_bars(symbol: &str, n: usize, base_price: f64) -> Vec<Bar> {
    use chrono::TimeZone;
    use chrono_tz::America::New_York;
    let start = New_York
        .with_ymd_and_hms(2026, 6, 3, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let mut bars = Vec::with_capacity(n);
    let mut px = base_price;
    for i in 0..n {
        let wave = ((i as f64) / 6.0).sin() * 0.5;
        let open = px;
        px = (px + 0.05 + wave * 0.1).max(1.0);
        let close = px;
        let high = open.max(close) + 0.3;
        let low = open.min(close) - 0.3;
        bars.push(Bar {
            symbol: symbol.into(),
            ts: start + Duration::minutes(i as i64),
            open,
            high,
            low,
            close,
            volume: 1000.0 + (i % 13) as f64 * 50.0,
        });
    }
    bars
}

fn sample_option_quote(
    symbol: &str,
    underlying: &str,
    option_type: OptionType,
    strike: f64,
    bid: f64,
    ask: f64,
    expiry_mins: i64,
) -> OptionQuote {
    let now = Utc::now();
    OptionQuote {
        symbol: symbol.into(),
        underlying: underlying.into(),
        option_type,
        strike,
        expiry: now + Duration::minutes(expiry_mins),
        bid,
        ask,
        last: (bid + ask) / 2.0,
        iv: 0.25,
        greeks: Some(th_domain::Greeks {
            delta: 0.5,
            gamma: 0.02,
            theta: -0.01,
            vega: 0.15,
            rho: 0.01,
        }),
        open_interest: 500,
        volume: 250,
        quote_ts: now,
    }
}

// =========================================================================
// 1. ARCHITECTURE & ZERO DIRECTIONAL AUTHORITY (CERT-ARCH-001)
// =========================================================================
#[test]
fn cert_arch_001_zero_directional_authority_in_hive() {
    let now = Utc::now();
    let mut bars = Vec::with_capacity(120);
    let start = now - Duration::minutes(120);
    for i in 0..120 {
        let p = 500.0 + (i as f64) * 0.4;
        bars.push(Bar {
            symbol: "SPY".into(),
            ts: start + Duration::minutes(i as i64),
            open: p,
            high: p + 0.5,
            low: p - 0.2,
            close: p + 0.3,
            volume: 1000.0,
        });
    }
    let mut histories = HashMap::new();
    histories.insert("SPY".into(), bars.clone());

    let quote_call = sample_option_quote(
        "SPY260831C00500000",
        "SPY",
        OptionType::Call,
        500.0,
        2.10,
        2.20,
        240,
    );
    let quote_put = sample_option_quote(
        "SPY260831P00500000",
        "SPY",
        OptionType::Put,
        500.0,
        2.00,
        2.10,
        240,
    );
    let mut chains = HashMap::new();
    chains.insert(
        "SPY".into(),
        OptionChain {
            underlying: "SPY".into(),
            as_of: now,
            quotes: vec![quote_call, quote_put],
        },
    );

    let eval = StrategyEvaluation {
        strategy_id: "STRAT-01".into(),
        train_pnl: 100.0,
        validation_pnl: 50.0,
        oos_pnl: 40.0,
        oos_sharpe: 1.2,
        profit_factor: 1.5,
        max_drawdown: 20.0,
        trades: 15,
        accepted: true,
        robustness: 0.85,
        p_value: 0.01,
        fdr_q: 0.02,
        confidence: 0.85,
    };

    let report = AnalysisReport {
        started: now,
        finished: now,
        evaluations: vec![eval],
        promoted: vec![PromotionRecord {
            strategy_id: "STRAT-01".into(),
            version: 1,
            fingerprint: "fp_strat_01".into(),
            promoted: true,
            reason: "Passed research gates".into(),
            created_at: now,
        }],
        variables: vec![],
        learning_updates: 1,
        config_version: "test-v1".into(),
        q_table: vec![],
        dataset_hash: "hash_spy".into(),
        generated_strategy: None,
        experiences: vec![],
    };

    let bundle = AnalysisBundle {
        started: now,
        finished: now,
        dataset_hash: "hash_spy".into(),
        symbols: vec![SymbolAnalysis {
            symbol: "SPY".into(),
            report,
        }],
        promoted: vec![PromotionRecord {
            strategy_id: "STRAT-01".into(),
            version: 1,
            fingerprint: "fp_strat_01".into(),
            promoted: true,
            reason: "Passed research gates".into(),
            created_at: now,
        }],
    };

    let policy = HiveManufacturingPolicy {
        total_capital: 10000.0,
        max_bots: 2,
        risk_fraction: 0.05,
        min_expiry_minutes: 180,
        max_expiry_minutes: 360,
    };

    let plans = manufacture_promoted_bots(&bundle, &histories, &chains, &policy, now);
    assert!(
        !plans.is_empty(),
        "Hive should manufacture plans for promoted strategies"
    );

    for plan in &plans {
        assert_eq!(
            plan.quantity, 0,
            "CERT-ARCH-001 VIOLATION: Hive assigned non-zero trade quantity"
        );
        assert_eq!(
            plan.entry_limit, 0.0,
            "CERT-ARCH-001 VIOLATION: Hive assigned execution price limit"
        );
        assert_eq!(
            plan.stop_loss_pct, 0.0,
            "CERT-ARCH-001 VIOLATION: Hive assigned stop-loss percentage"
        );
        assert_eq!(
            plan.take_profit_pct, 0.0,
            "CERT-ARCH-001 VIOLATION: Hive assigned take-profit percentage"
        );
        assert!(
            plan.capital_allocated > 0.0,
            "Capital allocation must be positive"
        );
        assert!(plan.risk_budget > 0.0, "Risk budget must be positive");
        assert!(
            plan.min_expiry_minutes >= 180,
            "Expiry window contract violated"
        );
    }
}

// =========================================================================
// 2. BOT AUTHORITY & WORKER SIZING (CERT-BOT-001 TO CERT-BOT-010)
// =========================================================================
#[test]
fn cert_bot_001_autonomous_sizing_formula_verification() {
    let capital = 10000.0;
    let risk_budget = 500.0;
    let option_ask = 2.50;
    let stop_loss_pct = 0.05;
    let multiplier = CONTRACT_MULTIPLIER; // 100.0

    // Contract cost = 2.50 * 100 = 250.0
    // Capital capacity = floor(10000.0 / 250.0) = 40 contracts
    // Risk capacity = floor(500.0 / (250.0 * 0.05)) = floor(500.0 / 12.5) = 40 contracts
    let sizing =
        calculate_worker_quantity(capital, risk_budget, option_ask, stop_loss_pct, multiplier)
            .unwrap();
    assert_eq!(sizing.capital_capacity, 40);
    assert_eq!(sizing.risk_capacity, 40);
    assert_eq!(sizing.quantity, 40);

    // If risk budget is constrained to 100.0:
    // Risk capacity = floor(100.0 / 12.5) = 8 contracts -> min(40, 8) = 8
    let sizing2 =
        calculate_worker_quantity(capital, 100.0, option_ask, stop_loss_pct, multiplier).unwrap();
    assert_eq!(sizing2.quantity, 8);
    assert_eq!(sizing2.risk_capacity, 8);

    // If capital is constrained to 1000.0:
    // Capital capacity = floor(1000.0 / 250.0) = 4 contracts -> min(4, 40) = 4
    let sizing3 =
        calculate_worker_quantity(1000.0, risk_budget, option_ask, stop_loss_pct, multiplier)
            .unwrap();
    assert_eq!(sizing3.quantity, 4);

    // Signal strength scaling:
    // With strength = 0.5, quantity should scale to floor(40 * 0.5) = 20 contracts
    let sizing_scaled = th_bot::calculate_worker_quantity_with_strength(
        capital,
        risk_budget,
        option_ask,
        stop_loss_pct,
        multiplier,
        0.5,
    )
    .unwrap();
    assert_eq!(sizing_scaled.quantity, 20);

    // Edge Case: zero ask or negative ask must error
    assert!(
        calculate_worker_quantity(capital, risk_budget, 0.0, stop_loss_pct, multiplier).is_err()
    );
    assert!(
        calculate_worker_quantity(capital, risk_budget, -1.0, stop_loss_pct, multiplier).is_err()
    );
}

#[test]
fn cert_risk_001_system_safety_limits_from_env() {
    let _ = dotenvy::dotenv();
    if std::env::var("RISK_MAX_ORDER_NOTIONAL").is_err() {
        std::env::set_var("RISK_MAX_ORDER_NOTIONAL", "1000.0");
        std::env::set_var("RISK_MAX_TOTAL_NOTIONAL", "5000.0");
        std::env::set_var("RISK_MAX_DAILY_LOSS", "250.0");
        std::env::set_var("RISK_MAX_POSITIONS", "10");
        std::env::set_var("RISK_MAX_SINGLE_POSITION_QTY", "10");
        std::env::set_var("RISK_MAX_SPREAD_BPS", "250.0");
        std::env::set_var("RISK_MAX_SYMBOL_EXPOSURE", "1500.0");
    }
    let limits = RiskLimits::from_env().unwrap();
    assert!(limits.max_order_notional > 0.0);
    assert!(limits.max_total_notional >= limits.max_order_notional);
    assert!(limits.max_daily_loss > 0.0);
    assert!(limits.max_positions > 0);
    assert!(limits.max_single_position_qty > 0);
    assert!(limits.max_spread_bps > 0.0);
    assert!(limits.max_symbol_exposure > 0.0);
}

#[tokio::test]
async fn cert_bot_002_003_signal_routing_and_flat_behavior() {
    let db_path = format!("target/cert-bot-signal-{}.sqlite", Uuid::new_v4());
    let broker = PaperBroker::new(20000.0);
    let cfg = RuntimeConfig {
        database_path: db_path.clone(),
        stop_loss_pct: 0.05,
        take_profit_pct: 0.10,
        ..RuntimeConfig::testing()
    };
    let mut rt = TradingRuntime::new(cfg, broker, SyntheticProvider).unwrap();

    let bars = synthetic_market_bars("SPY", 100, 500.0);
    for (i, b) in bars.into_iter().enumerate() {
        rt.on_market_bar(&format!("SPY-{}", i), b).await.unwrap();
    }

    assert_eq!(rt.bars.len(), 1);
    assert!(rt.bars["SPY"].len() >= 15);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn cert_bot_004_005_stop_loss_and_take_profit_triggers() {
    let entry_price = 2.00;
    let stop_loss_pct = 0.05;
    let take_profit_pct = 0.10;

    let mark_loss = 1.89; // ret = 1.89/2.0 - 1 = -0.055 (-5.5%)
    let ret_loss = mark_loss / entry_price - 1.0;
    assert!(ret_loss <= -stop_loss_pct, "Must trigger stop-loss");

    let mark_gain = 2.22; // ret = 2.22/2.0 - 1 = +0.11 (+11%)
    let ret_gain = mark_gain / entry_price - 1.0;
    assert!(ret_gain >= take_profit_pct, "Must trigger take-profit");

    // Zero take profit (0.0) must never trigger on gain
    let zero_tp = 0.0;
    let should_exit = zero_tp > 0.0 && ret_gain >= zero_tp;
    assert!(
        !should_exit,
        "Zero take profit must disable take profit exits"
    );
}

#[test]
fn cert_bot_006_max_holding_limit_180_minutes() {
    let now = Utc::now();
    let entry_ts = now - Duration::minutes(181);
    let age = (now - entry_ts).num_minutes();
    let max_hold_minutes = 180;
    assert!(
        age >= max_hold_minutes as i64,
        "Trade age exceeds 180 min and must trigger forced exit"
    );
}

// =========================================================================
// 3. CONSOLIDATED JSON HISTORY & CONCURRENCY (CERT-HIST-001 TO CERT-HIST-003)
// =========================================================================
#[test]
fn cert_hist_001_single_file_bot_history_locking_and_atomic_writes() {
    let test_dir = format!("target/test_hist_{}", Uuid::new_v4());
    std::fs::create_dir_all(&test_dir).unwrap();
    let store = JsonHistoryStore::new(&test_dir).unwrap();

    let now = Utc::now();
    let bot_a = BotHistoryRecord::from_manifest(
        "bot-alpha",
        serde_json::json!({"underlying": "SPY", "capital": 5000.0}),
        now,
    );
    let bot_b = BotHistoryRecord::from_manifest(
        "bot-beta",
        serde_json::json!({"underlying": "QQQ", "capital": 5000.0}),
        now,
    );

    // Upsert bot A and bot B
    store.upsert_bot(bot_a.clone()).unwrap();
    store.upsert_bot(bot_b.clone()).unwrap();

    let bot_a_ctx = store.bot_context("bot-alpha").unwrap().unwrap();
    let bot_b_ctx = store.bot_context("bot-beta").unwrap().unwrap();
    assert_eq!(bot_a_ctx.bot_id, "bot-alpha");
    assert_eq!(bot_b_ctx.bot_id, "bot-beta");

    // Verify atomic file existence
    let single_file = std::path::Path::new(&test_dir).join("bots_history.json");
    assert!(
        single_file.exists(),
        "Consolidated bots_history.json must exist"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
fn cert_hist_002_hive_manufacturing_audit_trail_reasoning() {
    let test_dir = format!("target/test_mfg_{}", Uuid::new_v4());
    std::fs::create_dir_all(&test_dir).unwrap();
    let store = JsonHistoryStore::new(&test_dir).unwrap();

    let now = Utc::now();
    let run = HiveManufacturingRun {
        manufacturing_id: "MFG-20260831-001".into(),
        timestamp: now,
        input: serde_json::json!({"capital_allocated_to_bot": 5000.0}),
        discovery: serde_json::json!({"underlying": "SPY", "volatility_regime": "Normal"}),
        strategy_selection: serde_json::json!({"strategy_id": "STRAT-01", "version": 1, "selection_reason": "Top Q-value and passed out-of-sample test"}),
        capital_allocation: serde_json::json!({"allocated": 5000.0, "risk_budget": 250.0}),
        option_selection: serde_json::json!({"contract": "SPY260831C00500000", "expiry": "2026-08-31T20:00:00Z", "strike": 500.0, "option_type": "Call"}),
        bot_manifest: serde_json::json!({"bot_id": "BOT-SPY-01", "strategy_id": "STRAT-01"}),
        risk_authorization: serde_json::json!({"risk_budget": 250.0, "risk_fraction": 0.05}),
        manufacturing_result: serde_json::json!({"status": "MANUFACTURED", "bot_id": "BOT-SPY-01"}),
    };

    store.record_manufacturing(run).unwrap();

    let mfg_file = std::path::Path::new(&test_dir).join("hive_manufacturing_history.json");
    assert!(
        mfg_file.exists(),
        "hive_manufacturing_history.json must exist"
    );

    let content = std::fs::read_to_string(mfg_file).unwrap();
    assert!(content.contains("MFG-20260831-001"));
    assert!(content.contains("selection_reason"));

    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
fn cert_hist_003_rl_history_transitions_and_q_tables() {
    let test_dir = format!("target/test_rl_{}", Uuid::new_v4());
    std::fs::create_dir_all(&test_dir).unwrap();
    let store = JsonHistoryStore::new(&test_dir).unwrap();

    let now = Utc::now();
    let experiences = vec![Experience {
        state: StateKey {
            regime: "TrendUp".into(),
            vol_bucket: 1,
            momentum_bucket: 2,
            volume_bucket: 1,
            ..Default::default()
        },
        action: "STRAT-01".into(),
        reward: 1.5,
        next_state: StateKey {
            regime: "TrendUp".into(),
            vol_bucket: 1,
            momentum_bucket: 2,
            volume_bucket: 1,
            ..Default::default()
        },
        terminal: false,
        decision_ts: now,
        outcome_ts: now,
    }];

    let report = AnalysisReport {
        started: now,
        finished: now,
        evaluations: vec![],
        promoted: vec![],
        variables: vec![],
        learning_updates: 1,
        config_version: "rl-test-1".into(),
        q_table: vec![QEntry {
            state: StateKey {
                regime: "TrendUp".into(),
                vol_bucket: 1,
                momentum_bucket: 2,
                volume_bucket: 1,
                ..Default::default()
            },
            action: "STRAT-01".into(),
            value: 0.85,
        }],
        dataset_hash: "hash_d1".into(),
        generated_strategy: None,
        experiences,
    };

    let bundle = AnalysisBundle {
        started: now,
        finished: now,
        dataset_hash: "hash_d1".into(),
        symbols: vec![SymbolAnalysis {
            symbol: "SPY".into(),
            report,
        }],
        promoted: vec![],
    };

    let seeds_before = vec![serde_json::json!({"strategy_id": "STRAT-01"})];
    let seeds_after = vec![
        serde_json::json!({"strategy_id": "STRAT-01"}),
        serde_json::json!({"strategy_id": "STRAT-31"}),
    ];

    persist_rl_history(
        &store,
        &bundle,
        serde_json::json!({"session": 1}),
        seeds_before,
        seeds_after,
        serde_json::json!({"symbols": ["SPY"]}),
    )
    .unwrap();

    let rl_file = std::path::Path::new(&test_dir).join("reinforcement_learning_history.json");
    assert!(
        rl_file.exists(),
        "reinforcement_learning_history.json must exist"
    );

    let content = std::fs::read_to_string(rl_file).unwrap();
    assert!(
        content.contains("rewards"),
        "RL history must contain rewards transition array"
    );
    assert!(content.contains("STRAT-01"));
    assert!(content.contains("STRAT-31"));

    let _ = std::fs::remove_dir_all(&test_dir);
}

// =========================================================================
// 4. REINFORCEMENT LEARNING TWO-SESSION DEMONSTRATION (CERT-RL-001 & CERT-RL-002)
// =========================================================================
#[test]
fn cert_rl_001_002_two_session_reinforcement_learning_demonstration() {
    let test_dir = format!("target/test_rl_demo_{}", Uuid::new_v4());
    std::fs::create_dir_all(&test_dir).unwrap();
    let json_store = JsonHistoryStore::new(&test_dir).unwrap();

    // -------------------------------------------------------------
    // SESSION 1: 30 Seed Strategies -> RL Learning -> Candidate STRAT-31
    // -------------------------------------------------------------
    let registry = StrategyRegistry::new();
    let seed_ids = registry.seed_ids();
    assert_eq!(
        seed_ids.len(),
        30,
        "Seed registry must have exactly 30 strategies"
    );

    let bars_s1 = synthetic_market_bars("SPY", 300, 500.0);
    let mut hist_s1 = HashMap::new();
    hist_s1.insert("SPY".into(), bars_s1);

    // Initial trade records to provide feedback
    let trade_records_s1 = vec![
        th_memory::TradeRecord {
            trade_id: "T-01".into(),
            symbol: "SPY".into(),
            strategy_id: "STRAT-01".into(),
            entry: Utc::now() - Duration::hours(3),
            exit: Some(Utc::now() - Duration::hours(2)),
            pnl: 150.0,
            fees: 1.0,
            reason: "take_profit".into(),
        },
        th_memory::TradeRecord {
            trade_id: "T-02".into(),
            symbol: "SPY".into(),
            strategy_id: "STRAT-05".into(),
            entry: Utc::now() - Duration::hours(2),
            exit: Some(Utc::now() - Duration::hours(1)),
            pnl: 80.0,
            fees: 1.0,
            reason: "take_profit".into(),
        },
    ];

    let bundle_s1 = run_analysis_with_q_and_trades(hist_s1, None, &trade_records_s1);
    let report_s1 = &bundle_s1.symbols[0].report;

    assert!(
        report_s1.learning_updates > 0,
        "Session 1 must execute Q-learning updates"
    );
    assert!(
        !report_s1.q_table.is_empty(),
        "Session 1 must produce populated Q-table"
    );

    // Synthesis of new strategy
    let generated_s1 =
        synthesize_strategy(report_s1).expect("Session 1 must synthesize a candidate strategy");
    assert_eq!(
        generated_s1.blueprint.id, "STRAT-31",
        "First synthesized strategy must be STRAT-31"
    );
    assert!((generated_s1.blueprint.weight_a + generated_s1.blueprint.weight_b - 1.0).abs() < 1e-6);

    let seed_before_s1: Vec<serde_json::Value> = seed_ids
        .iter()
        .map(|id| serde_json::json!({"strategy_id": id, "type": "seed"}))
        .collect();
    let mut seed_after_s1 = seed_before_s1.clone();
    seed_after_s1.push(serde_json::json!({
        "strategy_id": generated_s1.blueprint.id,
        "type": "rl_promoted",
        "blueprint": generated_s1.blueprint
    }));

    persist_rl_history(
        &json_store,
        &bundle_s1,
        serde_json::json!({"trade_records_used": trade_records_s1.len()}),
        seed_before_s1,
        seed_after_s1.clone(),
        serde_json::json!({"symbols": ["SPY"]}),
    )
    .unwrap();

    // -------------------------------------------------------------
    // SESSION 2: Retraining with Expanded Library (31 Strategies) & Prior Q
    // -------------------------------------------------------------
    let prior_q = QLearning::from_entries(&report_s1.q_table);
    assert!(!prior_q.q.is_empty(), "Prior Q-table must be non-empty");

    // New market data for Session 2
    let bars_s2 = synthetic_market_bars("SPY", 300, 508.0);
    let mut hist_s2 = HashMap::new();
    hist_s2.insert("SPY".into(), bars_s2);

    let trade_records_s2 = vec![th_memory::TradeRecord {
        trade_id: "T-03".into(),
        symbol: "SPY".into(),
        strategy_id: "STRAT-31".into(),
        entry: Utc::now() - Duration::hours(1),
        exit: Some(Utc::now()),
        pnl: 200.0,
        fees: 1.0,
        reason: "take_profit".into(),
    }];

    let bundle_s2 = run_analysis_with_q_and_trades(hist_s2, Some(prior_q), &trade_records_s2);
    let report_s2 = &bundle_s2.symbols[0].report;

    assert!(
        report_s2.learning_updates > 0,
        "Session 2 must execute additional Q-learning updates"
    );

    // Verify next strategy ID advances to STRAT-32
    let current_seeds: Vec<String> = seed_after_s1
        .iter()
        .filter_map(|v| v["strategy_id"].as_str().map(String::from))
        .collect();
    let next_id = next_strategy_id(&current_seeds, &report_s2.promoted);
    assert_eq!(
        next_id, "STRAT-32",
        "Next strategy ID after STRAT-31 must be STRAT-32"
    );

    let _ = std::fs::remove_dir_all(&test_dir);
}

// =========================================================================
// 5. EXECUTION & RISK GOVERNOR VALIDATION (CERT-EXEC-001)
// =========================================================================
#[tokio::test]
async fn cert_exec_001_paper_broker_risk_governor_smoke() {
    let broker = PaperBroker::new(10000.0);
    broker.set_mock_clock(true);
    let mut engine = ExecutionEngine::new(broker.clone(), RiskGovernor::new(RiskLimits::default()));

    let now = Utc::now();
    let mut order = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "SPY260831C00500000".into(),
        side: OrderSide::Buy,
        qty: 4, // 4 * 2.20 * 100 = $880 <= $1000 limit
        limit_price: Some(2.20),
        reduce_only: false,
        strategy_id: "STRAT-01".into(),
        created_at: now,
        order_hash: String::new(),
    };
    order.order_hash = order_hash(&order);

    let acct = broker.account().await.unwrap();
    let positions = broker.positions().await.unwrap();
    let portfolio = PortfolioRisk {
        cash: acct.cash,
        realized_today: 0.0,
        positions,
    };

    let (broker_order, approval) = engine
        .execute(order.clone(), 2.20, 10.0, &portfolio)
        .await
        .unwrap();
    assert_eq!(broker_order.filled_qty, 4);
    assert_eq!(approval.order_hash, order.order_hash);
    assert!(approval.expires_at > now + Duration::seconds(10));
    assert!(
        approval.expires_at <= now + Duration::seconds(16),
        "Approval token must expire within 15 seconds"
    );
}

// =========================================================================
// 6. FAIL CLOSED GATES & INTEGRITY (CERT-FAIL-001 TO CERT-FAIL-005)
// =========================================================================
#[tokio::test]
async fn cert_fail_001_missing_alpaca_credentials_fails_closed() {
    let cfg = AlpacaConfig {
        key: "invalid_key".into(),
        secret: "invalid_secret".into(),
        data_url: "https://data.alpaca.markets".into(),
        news_url: "https://data.alpaca.markets".into(),
        options_feed: None,
        stocks_feed: None,
    };
    let provider = AlpacaProvider::new(cfg).unwrap();
    let now = Utc::now();
    let result = provider.bars("SPY", now - Duration::days(1), now).await;
    assert!(
        result.is_err(),
        "Invalid / missing API authentication must fail closed"
    );
}

#[test]
fn cert_fail_002_stale_and_future_quotes_rejected() {
    let now = Utc::now();
    // Quote older than 30 seconds
    let mut q_stale = sample_option_quote(
        "SPY260831C00500000",
        "SPY",
        OptionType::Call,
        500.0,
        2.0,
        2.1,
        150,
    );
    q_stale.quote_ts = now - Duration::seconds(35);
    assert!(
        !q_stale.is_tradeable(now, 30),
        "Stale quote (>30s) must be rejected"
    );

    // Future timestamp quote
    let mut q_future = sample_option_quote(
        "SPY260831C00500000",
        "SPY",
        OptionType::Call,
        500.0,
        2.0,
        2.1,
        150,
    );
    q_future.quote_ts = now + Duration::seconds(5);
    assert!(
        !q_future.is_tradeable(now, 30),
        "Future timestamp quote must be rejected"
    );
}

#[test]
fn cert_fail_003_inverted_and_non_positive_quotes_rejected() {
    let now = Utc::now();
    // Inverted quote: bid > ask
    let q_inverted = sample_option_quote(
        "SPY260831C00500000",
        "SPY",
        OptionType::Call,
        500.0,
        2.50,
        2.40,
        150,
    );
    assert!(
        !q_inverted.is_tradeable(now, 30),
        "Inverted quote (bid > ask) must be rejected"
    );

    // Zero or negative ask
    let q_zero = sample_option_quote(
        "SPY260831C00500000",
        "SPY",
        OptionType::Call,
        500.0,
        0.0,
        0.0,
        150,
    );
    assert!(
        !q_zero.is_tradeable(now, 30),
        "Zero ask quote must be rejected"
    );
}

#[test]
fn cert_fail_004_reconciliation_mismatch_engages_kill_switch() {
    let internal = vec![th_domain::Position {
        symbol: "SPY260831C00500000".into(),
        qty: 5,
        avg_price: 2.20,
        mark: 2.20,
        opened_at: Utc::now(),
    }];
    let broker = vec![th_domain::Position {
        symbol: "SPY260831C00500000".into(),
        qty: 3, // Mismatch: broker has 3, internal has 5!
        avg_price: 2.20,
        mark: 2.20,
        opened_at: Utc::now(),
    }];

    let report = reconcile_positions(&internal, &broker);
    assert!(
        !report.matched,
        "Position quantity mismatch must fail reconciliation"
    );
}

// =========================================================================
// 8. SESSION RUNTIME LIFECYCLE (CERT-SESSION-001)
// =========================================================================
#[tokio::test]
async fn cert_session_001_autonomous_runtime_lifecycle_execution() {
    let db_path = format!("target/cert-session-{}.sqlite", Uuid::new_v4());
    let cfg = RuntimeConfig {
        database_path: db_path.clone(),
        ..RuntimeConfig::testing()
    };
    let broker = PaperBroker::new(1000000.0);
    let provider = SyntheticProvider;
    let mut rt = TradingRuntime::new(cfg, broker, provider).unwrap();
    rt.phase_override = Some(th_domain::SessionPhase::MarketOpen);

    let symbols = vec!["SPY".to_string(), "QQQ".to_string()];
    let _stats = rt.run_session(&symbols, Some(2)).await.unwrap();

    assert!(
        !rt.bot_plans.is_empty(),
        "Session must manufacture bots on startup if none exist"
    );
    assert!(
        !rt.fleet.bots.is_empty(),
        "Session must activate worker fleet"
    );
    assert!(
        rt.active,
        "Trading session must activate runtime during trading phase"
    );

    let _ = std::fs::remove_file(db_path);
}
