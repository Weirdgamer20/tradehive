use chrono::{Duration, Utc};
use th_domain::Bar;
use th_hive::{discover_variables, evaluate_strategies};
fn bs() -> Vec<Bar> {
    let s = Utc::now();
    (0..100)
        .map(|i| {
            let p = 100.0 + i as f64 * 0.1;
            Bar {
                symbol: "SPY".into(),
                ts: s + Duration::minutes(i),
                open: p,
                high: p + 0.2,
                low: p - 0.1,
                close: p + 0.1,
                volume: 1000.0,
            }
        })
        .collect()
}
#[test]
fn research_pipeline_produces_candidates() {
    let b = bs();
    assert!(!discover_variables(&b).is_empty());
    assert!(!evaluate_strategies(&b).is_empty())
}

#[test]
fn q_learning_state_round_trips() {
    let mut q = th_hive::QLearning::default();
    let st = th_hive::StateKey {
        regime: "Range".into(),
        vol_bucket: 1,
        momentum_bucket: 2,
        volume_bucket: 3,
        ..Default::default()
    };
    q.q.insert((st.clone(), "momentum".into()), 0.42);
    let x = th_hive::QLearning::from_entries(&q.entries());
    assert_eq!(x.q.get(&(st, "momentum".into())).copied(), Some(0.42));
}

#[test]
fn confidence_allocation_is_monotonic_and_budget_bounded() {
    let x =
        th_hive::confidence_allocate(1000.0, &[("a".to_string(), 0.9), ("b".to_string(), 0.3)], 2);
    assert_eq!(x.len(), 2);
    assert!(x[0].1 > x[1].1);
    let total: f64 = x.iter().map(|(_, v)| *v).sum();
    assert!((total - 1000.0).abs() < 1e-9);
}

#[test]
fn seed_population_is_exactly_thirty() {
    assert_eq!(th_strategy::StrategyRegistry::new().seed_ids().len(), 30);
}

#[test]
fn q_learning_can_generate_a_new_strategy_blueprint() {
    use chrono::Utc;
    let now = Utc::now();
    let ids = th_strategy::StrategyRegistry::new().seed_ids();
    let evaluations = ids
        .iter()
        .map(|id| th_hive::StrategyEvaluation {
            strategy_id: id.clone(),
            train_pnl: 1.0,
            validation_pnl: 1.0,
            oos_pnl: 1.0,
            oos_sharpe: 1.0,
            profit_factor: 1.5,
            max_drawdown: 1.0,
            trades: 10,
            accepted: true,
            robustness: 1.0,
            p_value: 0.01,
            fdr_q: 0.01,
            confidence: 0.8,
        })
        .collect();
    let q_table = vec![
        th_hive::QEntry {
            state: th_hive::StateKey {
                regime: "Range".into(),
                vol_bucket: 1,
                momentum_bucket: 0,
                volume_bucket: 1,
                ..Default::default()
            },
            action: ids[0].clone(),
            value: 0.9,
        },
        th_hive::QEntry {
            state: th_hive::StateKey {
                regime: "Range".into(),
                vol_bucket: 1,
                momentum_bucket: 0,
                volume_bucket: 1,
                ..Default::default()
            },
            action: ids[1].clone(),
            value: 0.5,
        },
    ];
    let report = th_hive::AnalysisReport {
        started: now,
        finished: now,
        evaluations,
        promoted: Vec::new(),
        variables: Vec::new(),
        learning_updates: 2,
        config_version: "test".into(),
        q_table,
        dataset_hash: "hash".into(),
        generated_strategy: None,
        experiences: Vec::new(),
    };
    let generated = th_hive::synthesize_strategy(&report).expect("RL must synthesize a candidate");
    assert!(generated.blueprint.id.starts_with("STRAT-"));
    assert_eq!(generated.blueprint.parent_a, ids[0]);
    assert_eq!(generated.blueprint.parent_b, ids[1]);
    assert!((generated.blueprint.weight_a + generated.blueprint.weight_b - 1.0).abs() < 1e-9);
}

#[test]
fn test_manufacturing_stress_test_250_bots() {
    let now = Utc::now();
    let temp_db =
        std::env::temp_dir().join(format!("th-stress-test-{}.sqlite", uuid::Uuid::new_v4()));
    let store = th_storage::Store::open(temp_db.to_str().unwrap()).unwrap();

    let chain = th_market_data::synthetic_option_chain("SPY", 500.0, now);
    let report = th_hive::run_manufacturing_stress_test(250, "SPY", &chain, Some(&store), now)
        .expect("Stress test must succeed");

    assert!(
        report.manufacturing_test,
        "Must be flagged as manufacturing test"
    );
    assert_eq!(report.bots_created, 250);
    assert_eq!(report.bots_valid, 250);
    assert_eq!(report.bots_invalid, 0);
    assert_eq!(report.strategies_created, 250);
    assert_eq!(report.risk_configs_created, 250);
    assert_eq!(report.option_configs_created, 250);
    assert_eq!(report.rl_configs_created, 250);
    assert!(report.database_records_created >= 250);
    assert_eq!(
        report.execution_attempts, 0,
        "Execution attempts MUST remain strictly 0"
    );

    let bots = store.get_generation_bots(&report.generation_id).unwrap();
    assert_eq!(bots.len(), 250);
    assert_eq!(bots[0].execution_status, "ManufacturedValid");
    assert!(bots[0].risk_budget > 0.0);

    let _ = std::fs::remove_file(temp_db);
}
