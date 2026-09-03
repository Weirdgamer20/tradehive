use chrono::Utc;
use std::collections::HashMap;
use th_domain::{Bar, OptionChain, OptionQuote, OptionType};
use th_hive::{
    manufacture_promoted_bots, portfolio_confidence_allocate, AllocationCandidate, AnalysisBundle,
    AnalysisReport, HiveManufacturingPolicy, PromotionRecord, SymbolAnalysis,
};

#[test]
fn test_portfolio_allocation_preserves_distinct_symbol_strategy_pairs() {
    let policy = HiveManufacturingPolicy {
        total_capital: 100_000.0,
        max_bots: 10,
        max_bots_per_symbol: 2,
        max_symbol_capital_pct: 0.50,
        risk_fraction: 0.05,
        min_expiry_minutes: 180,
        max_expiry_minutes: u32::MAX,
    };

    let candidates = vec![
        AllocationCandidate {
            symbol: "SPY".into(),
            strategy_id: "STRAT-31".into(),
            score: 0.85,
        },
        AllocationCandidate {
            symbol: "QQQ".into(),
            strategy_id: "STRAT-31".into(),
            score: 0.80,
        },
        AllocationCandidate {
            symbol: "IWM".into(),
            strategy_id: "STRAT-12".into(),
            score: 0.75,
        },
    ];

    let allocations = portfolio_confidence_allocate(policy.total_capital, &candidates, &policy);
    assert_eq!(allocations.len(), 3);

    // Verify all 3 distinct pairs exist
    let pairs: Vec<(String, String)> = allocations
        .iter()
        .map(|(c, _)| (c.symbol.clone(), c.strategy_id.clone()))
        .collect();

    assert_eq!(
        pairs,
        vec![
            ("SPY".to_string(), "STRAT-31".to_string()),
            ("QQQ".to_string(), "STRAT-31".to_string()),
            ("IWM".to_string(), "STRAT-12".to_string()),
        ]
    );

    // Highest score gets largest allocation
    assert!(allocations[0].1 > allocations[1].1);
    assert!(allocations[1].1 > allocations[2].1);
}

#[test]
fn test_ranked_spillover_when_symbol_reaches_bot_cap() {
    let policy = HiveManufacturingPolicy {
        total_capital: 100_000.0,
        max_bots: 4,
        max_bots_per_symbol: 2, // Max 2 bots for any symbol
        max_symbol_capital_pct: 0.50,
        risk_fraction: 0.05,
        min_expiry_minutes: 180,
        max_expiry_minutes: u32::MAX,
    };

    let candidates = vec![
        AllocationCandidate {
            symbol: "SPY".into(),
            strategy_id: "STRAT-01".into(),
            score: 0.99,
        },
        AllocationCandidate {
            symbol: "SPY".into(),
            strategy_id: "STRAT-02".into(),
            score: 0.98,
        },
        AllocationCandidate {
            symbol: "SPY".into(),
            strategy_id: "STRAT-03".into(),
            score: 0.97, // Should be skipped: SPY bot cap reached
        },
        AllocationCandidate {
            symbol: "QQQ".into(),
            strategy_id: "STRAT-04".into(),
            score: 0.90, // Should be selected
        },
        AllocationCandidate {
            symbol: "IWM".into(),
            strategy_id: "STRAT-05".into(),
            score: 0.85, // Should be selected
        },
    ];

    let allocations = portfolio_confidence_allocate(policy.total_capital, &candidates, &policy);
    assert_eq!(allocations.len(), 4);

    let symbols: Vec<String> = allocations.iter().map(|(c, _)| c.symbol.clone()).collect();
    assert_eq!(
        symbols,
        vec![
            "SPY".to_string(),
            "SPY".to_string(),
            "QQQ".to_string(),
            "IWM".to_string()
        ]
    );
}

#[test]
fn test_capital_concentration_cap_prevents_monopolization() {
    let policy = HiveManufacturingPolicy {
        total_capital: 100_000.0,
        max_bots: 5,
        max_bots_per_symbol: 5,
        max_symbol_capital_pct: 0.30, // Max 30% ($30,000) for any single symbol
        risk_fraction: 0.05,
        min_expiry_minutes: 180,
        max_expiry_minutes: u32::MAX,
    };

    let candidates = vec![
        AllocationCandidate {
            symbol: "SPY".into(),
            strategy_id: "STRAT-01".into(),
            score: 0.99, // Uncapped would get ~50%
        },
        AllocationCandidate {
            symbol: "QQQ".into(),
            strategy_id: "STRAT-02".into(),
            score: 0.50,
        },
        AllocationCandidate {
            symbol: "IWM".into(),
            strategy_id: "STRAT-03".into(),
            score: 0.50,
        },
    ];

    let allocations = portfolio_confidence_allocate(policy.total_capital, &candidates, &policy);
    let spy_allocation = allocations
        .iter()
        .find(|(c, _)| c.symbol == "SPY")
        .map(|(_, cap)| *cap)
        .unwrap_or(0.0);

    // SPY must not exceed $30,000 (30%)
    assert!(
        spy_allocation <= 30_000.01,
        "SPY allocated {spy_allocation} which exceeded max symbol capital cap of 30,000"
    );
}

#[test]
fn test_manufacture_promoted_bots_with_multi_symbol_universe() {
    let now = Utc::now();
    let symbols = vec!["SPY", "QQQ", "IWM"];

    let mut histories = HashMap::new();
    let mut chains = HashMap::new();
    let mut symbol_analyses = Vec::new();
    let mut promoted = Vec::new();

    for &sym in &symbols {
        let mut bars = Vec::new();
        for i in 0..80 {
            let p = 100.0 + i as f64 * 0.2;
            bars.push(Bar {
                symbol: sym.into(),
                ts: now - chrono::Duration::minutes((80 - i) as i64),
                open: p,
                high: p + 0.5,
                low: p - 0.5,
                close: p + 0.2,
                volume: 1000.0,
            });
        }
        histories.insert(sym.into(), bars);

        let quote = OptionQuote {
            symbol: format!("{sym}-100-C"),
            underlying: sym.into(),
            option_type: OptionType::Call,
            strike: 100.0,
            expiry: now + chrono::Duration::days(10),
            bid: 1.0,
            ask: 1.1,
            last: 1.05,
            iv: 0.2,
            greeks: None,
            open_interest: 100,
            volume: 100,
            quote_ts: now,
        };
        chains.insert(
            sym.into(),
            OptionChain {
                underlying: sym.into(),
                as_of: now,
                quotes: vec![quote],
            },
        );

        let strat_id = format!("STRAT-{sym}");
        let rec = PromotionRecord {
            symbol: sym.into(),
            strategy_id: strat_id.clone(),
            version: 1,
            fingerprint: format!("fp-{sym}"),
            promoted: true,
            reason: "PASSED".into(),
            created_at: now,
        };
        promoted.push(rec.clone());

        symbol_analyses.push(SymbolAnalysis {
            symbol: sym.into(),
            report: AnalysisReport {
                started: now,
                finished: now,
                evaluations: vec![th_hive::StrategyEvaluation {
                    strategy_id: strat_id,
                    train_pnl: 10.0,
                    validation_pnl: 5.0,
                    oos_pnl: 5.0,
                    oos_sharpe: 1.5,
                    profit_factor: 1.8,
                    max_drawdown: 0.05,
                    trades: 30,
                    accepted: true,
                    robustness: 1.0,
                    p_value: 0.01,
                    fdr_q: 0.01,
                    confidence: 0.8,
                }],
                promoted: vec![rec],
                variables: vec![],
                learning_updates: 0,
                config_version: "v1".into(),
                q_table: vec![],
                dataset_hash: "hash".into(),
                generated_strategy: None,
                experiences: vec![],
            },
        });
    }

    let bundle = AnalysisBundle {
        started: now,
        finished: now,
        dataset_hash: "hash".into(),
        symbols: symbol_analyses,
        promoted,
    };

    let policy = HiveManufacturingPolicy {
        total_capital: 100_000.0,
        max_bots: 10,
        max_bots_per_symbol: 2,
        max_symbol_capital_pct: 0.40,
        risk_fraction: 0.05,
        min_expiry_minutes: 180,
        max_expiry_minutes: u32::MAX,
    };

    let plans = manufacture_promoted_bots(&bundle, &histories, &chains, &policy, now);
    assert_eq!(plans.len(), 3);

    let plan_symbols: Vec<String> = plans.iter().map(|p| p.underlying.clone()).collect();
    assert!(plan_symbols.contains(&"SPY".to_string()));
    assert!(plan_symbols.contains(&"QQQ".to_string()));
    assert!(plan_symbols.contains(&"IWM".to_string()));
}
