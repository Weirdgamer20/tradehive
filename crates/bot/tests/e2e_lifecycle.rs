use chrono::Utc;
use th_bot::{OpenTrade, RuntimeConfig, TradingRuntime};
use th_domain::{Bar, Position};
use th_execution::{Broker, PaperBroker};
use th_market_data::{MarketDataError, MarketDataProvider, NewsEvent};
use uuid::Uuid;

#[derive(Clone, Default)]
struct TestLifecycleProvider;

#[async_trait::async_trait]
impl MarketDataProvider for TestLifecycleProvider {
    async fn most_actives(&self, _limit: usize) -> Result<Vec<String>, MarketDataError> {
        Ok(vec!["SPY".into(), "QQQ".into()])
    }
    async fn bars(
        &self,
        symbol: &str,
        start: chrono::DateTime<Utc>,
        _end: chrono::DateTime<Utc>,
    ) -> Result<Vec<Bar>, MarketDataError> {
        let mut bars = Vec::new();
        for i in 0..60 {
            bars.push(Bar {
                symbol: symbol.into(),
                ts: start + chrono::Duration::minutes(5 * i as i64),
                open: 500.0 + (i as f64 * 0.1),
                high: 501.0 + (i as f64 * 0.1),
                low: 499.0 + (i as f64 * 0.1),
                close: 500.5 + (i as f64 * 0.1),
                volume: 1000.0,
            });
        }
        Ok(bars)
    }
    async fn option_chain(
        &self,
        underlying: &str,
        as_of: chrono::DateTime<Utc>,
    ) -> Result<th_domain::OptionChain, MarketDataError> {
        let expiry = as_of + chrono::Duration::hours(24);
        Ok(th_domain::OptionChain {
            underlying: underlying.into(),
            as_of,
            quotes: vec![
                th_domain::OptionQuote {
                    symbol: format!("{}-500-0", underlying),
                    underlying: underlying.into(),
                    option_type: th_domain::OptionType::Call,
                    strike: 500.0,
                    expiry,
                    bid: 4.90,
                    ask: 5.10,
                    last: 5.00,
                    iv: 0.20,
                    greeks: Some(th_domain::Greeks {
                        delta: 0.50,
                        gamma: 0.02,
                        theta: -0.02,
                        vega: 0.10,
                        rho: 0.01,
                    }),
                    open_interest: 1000,
                    volume: 500,
                    quote_ts: as_of,
                },
                th_domain::OptionQuote {
                    symbol: format!("{}-500-1", underlying),
                    underlying: underlying.into(),
                    option_type: th_domain::OptionType::Put,
                    strike: 500.0,
                    expiry,
                    bid: 4.90,
                    ask: 5.10,
                    last: 5.00,
                    iv: 0.20,
                    greeks: Some(th_domain::Greeks {
                        delta: -0.50,
                        gamma: 0.02,
                        theta: -0.02,
                        vega: 0.10,
                        rho: 0.01,
                    }),
                    open_interest: 1000,
                    volume: 500,
                    quote_ts: as_of,
                },
            ],
        })
    }
    async fn news(
        &self,
        _symbol: &str,
        _start: chrono::DateTime<Utc>,
        _end: chrono::DateTime<Utc>,
    ) -> Result<Vec<NewsEvent>, MarketDataError> {
        Ok(vec![])
    }
}

fn set_test_env() {
    std::env::set_var("RISK_MAX_ORDER_NOTIONAL", "5000.0");
    std::env::set_var("RISK_MAX_TOTAL_NOTIONAL", "20000.0");
    std::env::set_var("RISK_MAX_DAILY_LOSS", "1000.0");
    std::env::set_var("RISK_MAX_POSITIONS", "10");
    std::env::set_var("RISK_MAX_SINGLE_POSITION_QTY", "10");
    std::env::set_var("RISK_MAX_SPREAD_BPS", "1000.0");
    std::env::set_var("RISK_MAX_SYMBOL_EXPOSURE", "10000.0");
    std::env::set_var("RISK_MAX_TRADE_RISK_PCT", "0.02");
    std::env::set_var("RISK_MAX_PORTFOLIO_RISK_PCT", "0.10");

    std::env::set_var("HIVE_TOTAL_CAPITAL", "100000.0");
    std::env::set_var("HIVE_MAX_BOTS", "5");
    std::env::set_var("HIVE_MAX_BOTS_PER_SYMBOL", "2");
    std::env::set_var("HIVE_MAX_SYMBOL_CAPITAL_PCT", "0.40");
    std::env::set_var("HIVE_RISK_FRACTION", "0.02");
}

#[tokio::test]
async fn test_complete_e2e_paper_lifecycle() {
    set_test_env();
    let db = format!("target/test_e2e_lifecycle_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_e2e_hist_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    let broker = PaperBroker::new(100000.0);
    let provider = TestLifecycleProvider;

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.clone(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        broker.clone(),
        provider,
    )
    .unwrap();

    // 1. Session Setup & Dynamic Manufacturing
    let session_id = "SESSION-E2E-001";
    runtime.current_session_id = Some(session_id.into());
    let symbols = vec!["SPY".into()];
    let manufactured = runtime.ensure_bots_manufactured(&symbols).await.unwrap();
    assert!(manufactured > 0, "Bots should be successfully manufactured");
    assert!(!runtime.bot_plans.is_empty());

    // Verify session binding on manufactured plans
    for plan in runtime.bot_plans.values() {
        assert_eq!(plan.session_id, session_id);
    }

    // 2. Fast Trading Loop Candle Ingestion & Strategy Signal
    let now = Utc::now();
    let candle = Bar {
        symbol: "SPY".into(),
        ts: now,
        open: 500.0,
        high: 505.0,
        low: 499.0,
        close: 504.0,
        volume: 2500.0,
    };

    // Feed bar to drive runtime
    runtime
        .on_market_bar_at("EVENT-01", candle, now)
        .await
        .unwrap();

    // 3. Trade Entry & Position Lifecycle
    let option_symbol = "SPY-500-0";
    runtime.open_trades.insert(
        option_symbol.into(),
        OpenTrade {
            symbol: option_symbol.into(),
            underlying: "SPY".into(),
            strategy_id: "STRAT-01".into(),
            entry_price: 5.0,
            entry_ts: now,
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            qty: 2,
            protective_order_id: None,
            client_order_id: None,
            entry_state: None,
        },
    );

    assert_eq!(runtime.open_trades.len(), 1);
    broker.seed_position(Position {
        symbol: option_symbol.into(),
        qty: 2,
        avg_price: 5.0,
        mark: 5.5,
        opened_at: now,
        contract: th_domain::OptionContract::from_occ(option_symbol),
    });

    // 4. Market Closing & Mandatory EOD Flatten
    runtime.execute_market_closing(session_id).await.unwrap();

    // Verify internal trades and broker positions are completely flat
    assert!(
        runtime.open_trades.is_empty(),
        "Internal positions must be flat after closing"
    );
    let broker_positions = broker.positions().await.unwrap();
    assert!(
        broker_positions.is_empty(),
        "Broker positions must be flat after closing"
    );

    // 5. Post-Market & Experience Finalization
    runtime
        .execute_post_market(session_id, &symbols, now)
        .await
        .unwrap();

    // Verify plans retired
    assert!(
        runtime.bot_plans.is_empty(),
        "Plans must be retired post market"
    );

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&hist_dir);
}

#[tokio::test]
async fn test_multi_session_transition_lifecycle() {
    set_test_env();
    let db = format!("target/test_multi_session_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_multi_session_hist_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    std::env::set_var("HIVE_TOTAL_CAPITAL", "100000.0");
    std::env::set_var("HIVE_MAX_BOTS", "5");
    std::env::set_var("HIVE_MAX_BOTS_PER_SYMBOL", "2");
    std::env::set_var("HIVE_MAX_SYMBOL_CAPITAL_PCT", "0.40");
    std::env::set_var("HIVE_RISK_FRACTION", "0.02");

    let broker = PaperBroker::new(100000.0);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.clone(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        broker.clone(),
        TestLifecycleProvider,
    )
    .unwrap();

    // === SESSION 1 ===
    let session_1 = "SESSION-DAY-1";
    runtime.current_session_id = Some(session_1.into());
    let symbols_day1 = vec!["SPY".into()];
    runtime
        .ensure_bots_manufactured(&symbols_day1)
        .await
        .unwrap();
    assert!(!runtime.bot_plans.is_empty());

    let now = Utc::now();
    runtime.execute_market_closing(session_1).await.unwrap();
    runtime
        .execute_post_market(session_1, &symbols_day1, now)
        .await
        .unwrap();
    assert!(runtime.bot_plans.is_empty());

    // === SESSION 2 (Simulate restart on new day) ===
    let mut runtime_day2 = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.clone(),
            candidate_universe: vec!["QQQ".into()],
            ..RuntimeConfig::testing()
        },
        broker.clone(),
        TestLifecycleProvider,
    )
    .unwrap();

    // Must start with 0 active bot plans despite historical plans in SQLite
    assert!(
        runtime_day2.bot_plans.is_empty(),
        "New session must not hydrate stale active fleet"
    );

    let session_2 = "SESSION-DAY-2";
    runtime_day2.current_session_id = Some(session_2.into());
    let symbols_day2 = vec!["QQQ".into()];
    runtime_day2
        .ensure_bots_manufactured(&symbols_day2)
        .await
        .unwrap();

    // Verify all manufactured bots belong exclusively to session 2 and QQQ
    for plan in runtime_day2.bot_plans.values() {
        assert_eq!(plan.session_id, session_2);
        assert_eq!(plan.underlying, "QQQ");
    }

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&hist_dir);
}
