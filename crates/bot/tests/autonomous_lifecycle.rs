use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use th_bot::{RuntimeConfig, TradingRuntime};
use th_domain::{Bar, MarketSessionClock, MarketSessionConfig, OptionChain, SessionPhase};
use th_execution::PaperBroker;
use th_market_data::{MarketDataError, MarketDataProvider, NewsEvent, SyntheticProvider};

struct EmptyBarsProvider;
#[async_trait]
impl MarketDataProvider for EmptyBarsProvider {
    async fn bars(
        &self,
        _symbol: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> Result<Vec<Bar>, MarketDataError> {
        Ok(vec![])
    }
    async fn option_chain(
        &self,
        symbol: &str,
        as_of: DateTime<Utc>,
    ) -> Result<OptionChain, MarketDataError> {
        Ok(th_market_data::synthetic_option_chain(symbol, 500.0, as_of))
    }
    async fn news(
        &self,
        _symbol: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> Result<Vec<NewsEvent>, MarketDataError> {
        Ok(vec![])
    }
    async fn most_actives(&self, _limit: usize) -> Result<Vec<String>, MarketDataError> {
        Ok(vec![])
    }
}

#[test]
fn test_clock_governs_phases_deterministically() {
    let clock = MarketSessionClock::new(MarketSessionConfig::default());
    let ny = chrono_tz::America::New_York;

    // Wednesday Sep 2, 2026 09:00 ET -> PreMarket
    let pre = ny
        .with_ymd_and_hms(2026, 9, 2, 9, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(pre), SessionPhase::PreMarket);

    // Wednesday Sep 2, 2026 10:00 ET -> MarketOpen
    let open = ny
        .with_ymd_and_hms(2026, 9, 2, 10, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(open), SessionPhase::MarketOpen);

    // Wednesday Sep 2, 2026 15:56 ET -> MarketClosing
    let closing = ny
        .with_ymd_and_hms(2026, 9, 2, 15, 56, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(closing), SessionPhase::MarketClosing);

    // Wednesday Sep 2, 2026 16:15 ET -> PostMarket
    let post = ny
        .with_ymd_and_hms(2026, 9, 2, 16, 15, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(post), SessionPhase::PostMarket);

    // Wednesday Sep 2, 2026 17:00 ET -> Learning
    let learn = ny
        .with_ymd_and_hms(2026, 9, 2, 17, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(learn), SessionPhase::Learning);

    // Wednesday Sep 2, 2026 20:00 ET -> WaitingForNextSession
    let overnight = ny
        .with_ymd_and_hms(2026, 9, 2, 20, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.phase_at(overnight),
        SessionPhase::WaitingForNextSession
    );

    // Saturday Sep 5, 2026 10:00 ET -> WaitingForNextSession
    let weekend = ny
        .with_ymd_and_hms(2026, 9, 5, 10, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(weekend), SessionPhase::WaitingForNextSession);

    // New Year's Day 2026 -> WaitingForNextSession
    let holiday = ny
        .with_ymd_and_hms(2026, 1, 1, 10, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.phase_at(holiday), SessionPhase::WaitingForNextSession);
}

#[tokio::test]
async fn test_run_requires_no_symbols_and_selects_universe() {
    let db = "target/test_run_requires_no_symbols.sqlite";
    let _ = std::fs::remove_file(db);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into(), "QQQ".into()],
            max_universe_size: 2,
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    // Passing empty symbols slice triggers autonomous universe selection
    let stats = runtime.run_session(&[], Some(1)).await.unwrap();
    assert_eq!(stats.rejected_orders, 0);
    assert!(
        !runtime.active_universe.is_empty(),
        "Hive should have autonomously selected its universe"
    );
}

#[tokio::test]
async fn test_post_market_finalizes_dataset_and_retires_bots() {
    let db = "target/test_post_market.sqlite";
    let _ = std::fs::remove_file(db);
    let hist_dir = "target/test_post_market_hist";
    let _ = std::fs::remove_dir_all(hist_dir);
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", hist_dir);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    let symbols = vec!["SPY".into()];
    let manufactured = runtime.ensure_bots_manufactured(&symbols).await.unwrap();
    assert!(manufactured > 0);
    assert!(!runtime.bot_plans.is_empty());

    let now = Utc::now();
    runtime
        .execute_post_market("SESSION-TEST-001", &symbols, now)
        .await
        .unwrap();

    // Bots must be retired and plans cleared
    assert!(
        runtime.bot_plans.is_empty(),
        "Session bot plans should be cleared after post-market"
    );
    assert!(runtime.open_trades.is_empty());

    // Dataset must be recorded
    let session_file = std::path::Path::new(hist_dir).join("session_history.json");
    assert!(
        session_file.exists(),
        "session_history.json must be persisted"
    );
    let content = std::fs::read_to_string(session_file).unwrap();
    assert!(content.contains("SESSION-TEST-001"));
    assert!(content.contains("FINALIZED"));
}

#[tokio::test]
async fn test_closed_loop_rl_knowledge_transfer() {
    let db = "target/test_closed_loop.sqlite";
    let _ = std::fs::remove_file(db);
    let hist_dir = "target/test_closed_loop_hist";
    let _ = std::fs::remove_dir_all(hist_dir);
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", hist_dir);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    let symbols = vec!["SPY".into()];
    let now = Utc::now();

    // Populate historical bars for SPY
    let bars = SyntheticProvider
        .bars("SPY", now - Duration::days(5), now)
        .await
        .unwrap();
    runtime.bars.insert("SPY".into(), bars);

    // Record trade autopsy in store so RL has trade records to learn from
    runtime
        .store
        .event(
            "TRADE_AUTOPSY",
            &serde_json::json!({
                "trade": th_memory::TradeRecord {
                    trade_id: "T-001".into(),
                    symbol: "SPY".into(),
                    strategy_id: "momentum_spread".into(),
                    session_id: "SESSION-DAY-1".into(),
                    entry: now - Duration::minutes(30),
                    exit: Some(now - Duration::minutes(5)),
                    pnl: 10.0,
                    fees: 1.0,
                    reason: "TAKE_PROFIT".into(),
                }
            }),
        )
        .unwrap();

    // Execute learning
    runtime
        .execute_learning("SESSION-DAY-1", &symbols, now)
        .await
        .unwrap();

    // Check that RL session history was recorded
    let latest_rl = runtime.json_history.latest_rl_session().unwrap();
    assert!(
        latest_rl.is_some(),
        "RL session must be recorded when trades exist"
    );

    // Start Day 2 in Pre-Market: verify prior learned state is loaded
    let day2_symbols = runtime
        .execute_pre_market("SESSION-DAY-2", &symbols, now + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(day2_symbols, symbols);
    assert!(
        !runtime.bot_plans.is_empty(),
        "Day 2 manufactured bots from learned state"
    );
}

#[tokio::test]
async fn test_fail_closed_on_missing_market_data() {
    let db = "target/test_fail_closed.sqlite";
    let _ = std::fs::remove_file(db);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["UNKNOWN_SYM".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        EmptyBarsProvider,
    )
    .unwrap();

    // When provider returns no bars, select_trading_universe MUST fail closed
    let res = runtime.select_trading_universe(Utc::now()).await;
    assert!(res.is_err(), "Must fail closed when no bars are available");

    // ensure_bots_manufactured MUST ALSO fail closed
    let res_bots = runtime
        .ensure_bots_manufactured(&["UNKNOWN_SYM".into()])
        .await;
    assert!(
        res_bots.is_err(),
        "ensure_bots_manufactured must fail closed without synthesizing fake bars"
    );
}

#[tokio::test]
async fn test_market_closing_disables_trading_and_new_entries() {
    let db = "target/test_closing.sqlite";
    let _ = std::fs::remove_file(db);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    runtime.active = true;
    runtime
        .execute_market_closing("SESSION-TEST-CLOSE")
        .await
        .unwrap();
    assert!(!runtime.active, "Market closing must disable trading");
}

#[tokio::test]
async fn test_insufficient_data_rl_does_not_fabricate() {
    let db = "target/test_insufficient_data.sqlite";
    let _ = std::fs::remove_file(db);
    let hist_dir = "target/test_insufficient_data_hist";
    let _ = std::fs::remove_dir_all(hist_dir);
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", hist_dir);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    let symbols = vec!["SPY".into()];
    let now = Utc::now();

    // No trades in store -> execute_learning should gracefully record INSUFFICIENT_DATA without panicking
    let res = runtime
        .execute_learning("SESSION-NO-TRADES", &symbols, now)
        .await;
    assert!(
        res.is_ok(),
        "RL must succeed gracefully when insufficient data"
    );

    // No new RL history should be written
    let latest_rl = runtime.json_history.latest_rl_session().unwrap();
    assert!(
        latest_rl.is_none(),
        "No RL history should be persisted for empty trade session"
    );
}

#[tokio::test]
async fn test_runtime_survives_market_closed_without_terminating() {
    let db = "target/test_closed_survival.sqlite";
    let _ = std::fs::remove_file(db);

    let mut runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.into(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(100000.0),
        SyntheticProvider,
    )
    .unwrap();

    // Explicitly set phase to WaitingForNextSession to verify closed market behavior deterministically
    runtime.phase_override = Some(SessionPhase::WaitingForNextSession);

    // Run for 3 ticks outside market hours: must survive and complete ticks cleanly without error
    let stats = runtime.run_session(&[], Some(3)).await.unwrap();
    assert_eq!(stats.rejected_orders, 0);
    assert!(
        !runtime.active,
        "Runtime must remain inactive when market is closed"
    );
}
