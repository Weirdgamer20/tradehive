use chrono::{NaiveDate, Utc};
use std::time::Duration;
use th_bot::{
    HiveSupervisor, RuntimeConfig, SupervisorConfig, SupervisorState, TradingRuntime,
    WatchdogNotifier,
};
use th_domain::{Bar, HolidayCalendar, MarketSessionClock, OptionChain};
use th_execution::PaperBroker;
use th_market_data::{MarketDataError, MarketDataProvider, NewsEvent, SyntheticProvider};
use uuid::Uuid;

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

fn create_test_runtime() -> (TradingRuntime<PaperBroker, SyntheticProvider>, String) {
    let (runtime, _broker, db) = create_test_runtime_with_broker();
    (runtime, db)
}

/// Test provider that always fails universe discovery.
/// Used to force the `PreparingSession` → `Recovering` path without
/// requiring live Alpaca access.
#[derive(Clone)]
struct FailingProvider;

#[async_trait::async_trait]
impl MarketDataProvider for FailingProvider {
    async fn bars(
        &self,
        symbol: &str,
        _start: chrono::DateTime<Utc>,
        _end: chrono::DateTime<Utc>,
    ) -> Result<Vec<Bar>, MarketDataError> {
        // Return enough bars so that if most_actives somehow succeeded the
        // bars path would not itself be the failure point.
        SyntheticProvider
            .bars(symbol, _start, _end)
            .await
    }
    async fn option_chain(
        &self,
        _underlying: &str,
        _as_of: chrono::DateTime<Utc>,
    ) -> Result<OptionChain, MarketDataError> {
        Err(MarketDataError::Unavailable("failing_provider".into()))
    }
    async fn news(
        &self,
        _symbol: &str,
        _start: chrono::DateTime<Utc>,
        _end: chrono::DateTime<Utc>,
    ) -> Result<Vec<NewsEvent>, MarketDataError> {
        Ok(vec![])
    }
    async fn most_actives(&self, _limit: usize) -> Result<Vec<String>, MarketDataError> {
        // Simulate universe discovery failure so the supervisor enters Recovering.
        Err(MarketDataError::Unavailable(
            "test_universe_discovery_forced_failure".into(),
        ))
    }
}

fn create_test_runtime_failing() -> (TradingRuntime<PaperBroker, FailingProvider>, String) {
    set_test_env();
    let db = format!("target/test_supervisor_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_hist_supervisor_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    let broker = PaperBroker::new(100_000.0);
    let provider = FailingProvider;

    let runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.clone(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        broker,
        provider,
    )
    .unwrap();

    (runtime, db)
}

fn create_test_runtime_with_broker() -> (TradingRuntime<PaperBroker, SyntheticProvider>, PaperBroker, String) {
    set_test_env();
    let db = format!("target/test_supervisor_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_hist_supervisor_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    let broker = PaperBroker::new(100_000.0);
    let provider = SyntheticProvider;

    let runtime = TradingRuntime::new(
        RuntimeConfig {
            database_path: db.clone(),
            candidate_universe: vec!["SPY".into()],
            ..RuntimeConfig::testing()
        },
        broker.clone(),
        provider,
    )
    .unwrap();

    (runtime, broker, db)
}

#[tokio::test]
async fn test_supervisor_initialization_and_clean_start() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    assert_eq!(supervisor.state, SupervisorState::Starting);
    assert_eq!(supervisor.retry_count, 0);
    assert!(!supervisor.is_stopped());

    supervisor.initialize_and_recover().await.unwrap();

    assert_eq!(supervisor.state, SupervisorState::Starting);
    assert_eq!(supervisor.checkpoint.restart_count, 1);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_checkpoint_roundtrip_persistence() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();

    supervisor.checkpoint.session_id = "SESSION-20260904".into();
    supervisor.checkpoint.session_date = "20260904".into();
    supervisor.checkpoint.supervisor_state = SupervisorState::Trading;
    supervisor.checkpoint.retry_count = 3;
    supervisor.checkpoint.restart_count = 2;
    supervisor.checkpoint.active_bots = 5;
    supervisor.checkpoint.active_universe = vec!["SPY".into(), "AAPL".into()];

    supervisor.persist_checkpoint();

    let loaded = supervisor.load_checkpoint().expect("checkpoint should load from SQLite");
    assert_eq!(loaded.session_id, "SESSION-20260904");
    assert_eq!(loaded.supervisor_state, SupervisorState::Trading);
    assert_eq!(loaded.retry_count, 3);
    assert_eq!(loaded.restart_count, 2);
    assert_eq!(loaded.active_universe, vec!["SPY", "AAPL"]);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_state_transitions() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.transition_to(SupervisorState::WaitingForSession, "test_market_closed");
    assert_eq!(supervisor.state, SupervisorState::WaitingForSession);
    assert_eq!(supervisor.checkpoint.supervisor_state, SupervisorState::WaitingForSession);

    supervisor.transition_to(SupervisorState::PreparingSession, "test_pre_open");
    assert_eq!(supervisor.state, SupervisorState::PreparingSession);

    supervisor.transition_to(SupervisorState::Trading, "test_open");
    assert_eq!(supervisor.state, SupervisorState::Trading);

    supervisor.transition_to(SupervisorState::FinalizingSession, "test_close");
    assert_eq!(supervisor.state, SupervisorState::FinalizingSession);

    supervisor.transition_to(SupervisorState::Learning, "test_post_market");
    assert_eq!(supervisor.state, SupervisorState::Learning);

    supervisor.transition_to(SupervisorState::ShuttingDown, "test_shutdown");
    assert_eq!(supervisor.state, SupervisorState::ShuttingDown);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_hard_readiness_invariants_no_bots() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.runtime.bot_plans.clear();
    supervisor.state = SupervisorState::Trading;

    let mut tick_count = 0;
    supervisor.step(Some(1), &mut tick_count).await.unwrap();

    // Must never allow trading when bot_plans is empty - automatically shifts to PreparingSession
    assert_eq!(supervisor.state, SupervisorState::PreparingSession);
    assert!(!supervisor.runtime.active);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_halt_state_preservation() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    supervisor.halt_reason = Some("RECONCILIATION_MISMATCH".into());
    supervisor.transition_to(SupervisorState::Halted, "safety_halt_triggered");

    assert_eq!(supervisor.state, SupervisorState::Halted);

    // Drop and re-instantiate supervisor to simulate process restart
    let mut restarted = HiveSupervisor::new(supervisor.runtime, SupervisorConfig::default());
    restarted.initialize_and_recover().await.unwrap();

    // Supervisor must remain in Halted state and not blindly restart trading
    assert_eq!(restarted.state, SupervisorState::Halted);
    assert_eq!(restarted.halt_reason, Some("RECONCILIATION_MISMATCH".into()));

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_exponential_backoff_calculation() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        initial_retry_delay: Duration::from_secs(2),
        max_retry_delay: Duration::from_secs(60),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.retry_count = 0;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(2));

    supervisor.retry_count = 1;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(4));

    supervisor.retry_count = 2;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(8));

    supervisor.retry_count = 3;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(16));

    supervisor.retry_count = 4;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(32));

    supervisor.retry_count = 5;
    assert_eq!(supervisor.calculate_backoff_delay(), Duration::from_secs(60)); // capped at max_delay

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_calendar_next_trading_date_and_holidays() {
    let clock = MarketSessionClock::default();

    // Friday before Labor Day -> Tuesday (skipping weekend and Labor Day Monday)
    let friday = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
    let next_day = clock.next_trading_date(friday);
    assert_eq!(next_day, NaiveDate::from_ymd_opt(2026, 9, 8).unwrap());

    // Verify holidays are detected
    let christmas = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
    assert!(HolidayCalendar::is_market_holiday(christmas).is_some());

    let thanksgiving = NaiveDate::from_ymd_opt(2026, 11, 26).unwrap();
    assert!(HolidayCalendar::is_market_holiday(thanksgiving).is_some());

    // Next trading date skips weekend and holiday
    let day_before_thanksgiving = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
    let next_after_thanksgiving = clock.next_trading_date(day_before_thanksgiving);
    assert_eq!(next_after_thanksgiving, NaiveDate::from_ymd_opt(2026, 11, 27).unwrap());
}

#[tokio::test]
async fn test_supervisor_graceful_shutdown() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    supervisor.step_shutdown().await;

    assert!(supervisor.is_stopped());
    assert!(!supervisor.runtime.active);

    let loaded = supervisor.load_checkpoint().expect("checkpoint exists");
    assert_eq!(loaded.supervisor_state, SupervisorState::ShuttingDown);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_watchdog_notifier() {
    let mut notifier = WatchdogNotifier::default();
    // Default without NOTIFY_SOCKET is disabled and safe
    assert!(!notifier.is_enabled());
    notifier.notify_ready();
    notifier.notify_watchdog();
    notifier.notify_stopping();
}

#[tokio::test]
async fn test_supervisor_heartbeat_emission() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        heartbeat_interval: Duration::from_millis(1),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.state = SupervisorState::Trading;
    supervisor.runtime.active = true;
    supervisor.emit_heartbeat(Utc::now());

    supervisor.state = SupervisorState::WaitingForSession;
    supervisor.emit_heartbeat(Utc::now());

    supervisor.state = SupervisorState::Recovering;
    supervisor.emit_heartbeat(Utc::now());

    supervisor.state = SupervisorState::Halted;
    supervisor.emit_heartbeat(Utc::now());

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_session_bot_retirement() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    let session_id = "SESSION-TEST-RETIRE";
    supervisor.runtime.current_session_id = Some(session_id.into());
    let _ = supervisor.runtime.ensure_bots_manufactured(&["SPY".into()]).await.unwrap();
    assert!(!supervisor.runtime.bot_plans.is_empty());

    supervisor.state = SupervisorState::Learning;
    let mut tick_count = 0;
    supervisor.step(Some(1), &mut tick_count).await.unwrap();

    // After Learning phase, session bots must be retired and plans cleared
    assert!(supervisor.runtime.bot_plans.is_empty());
    assert_eq!(supervisor.state, SupervisorState::WaitingForSession);
    assert!(supervisor.checkpoint.target_next_session.is_some());

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_learning_zero_trades_clean_continuation() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    supervisor.runtime.current_session_id = Some("SESSION-EMPTY-TRADES".into());
    supervisor.runtime.open_trades.clear();

    supervisor.state = SupervisorState::Learning;
    let mut tick_count = 0;
    // Step must not error even with zero trades
    let res = supervisor.step(Some(1), &mut tick_count).await;
    assert!(res.is_ok());
    assert_eq!(supervisor.state, SupervisorState::WaitingForSession);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_transient_broker_failure_and_recovery() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        initial_retry_delay: Duration::from_millis(10),
        max_retry_delay: Duration::from_millis(50),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    supervisor.retry_count = 1;
    supervisor.state = SupervisorState::Recovering;

    let mut tick_count = 0;
    supervisor.step(Some(1), &mut tick_count).await.unwrap();

    // Since PaperBroker is responsive, recovery succeeds and retry_count resets
    assert_eq!(supervisor.retry_count, 0);
    assert!(supervisor.state == SupervisorState::WaitingForSession || supervisor.state == SupervisorState::PreparingSession || supervisor.state == SupervisorState::Trading);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_kill_switch_safety_halt() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    // Mark last_market_event as stale (2 hours ago) so data_healthy is false.
    // Do NOT set runtime.active = true; the stale-data kill-switch check fires
    // before the runtime-activation path, ensuring Halted is the outcome.
    supervisor.runtime.stats.last_market_event = Some(Utc::now() - chrono::Duration::hours(2));
    supervisor.runtime.cfg.max_quote_age_secs = 10;

    let _ = supervisor.runtime.ensure_bots_manufactured(&["SPY".into()]).await.unwrap();
    supervisor.state = SupervisorState::Trading;

    let mut tick_count = 0;
    supervisor.step(Some(1), &mut tick_count).await.unwrap();

    // Unhealthy quote age triggers kill switch -> Halted state
    assert_eq!(supervisor.state, SupervisorState::Halted);
    assert!(!supervisor.runtime.active);
    assert!(supervisor.halt_reason.is_some());

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_checkpoint_sqlite_durability_across_reopen() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    {
        let mut supervisor = HiveSupervisor::new(&mut runtime, config.clone());
        supervisor.initialize_and_recover().await.unwrap();
        supervisor.checkpoint.session_id = "SESSION-DURABILITY-99".into();
        supervisor.checkpoint.active_universe = vec!["QQQ".into(), "SPY".into()];
        supervisor.checkpoint.supervisor_state = SupervisorState::WaitingForSession;
        supervisor.persist_checkpoint();
    }

    // Reopen directly from raw SQLite Store
    let raw_store = th_storage::Store::open(&db).unwrap();
    let checkpoint_str = raw_store.get_checkpoint("SUPERVISOR_CHECKPOINT").unwrap().expect("checkpoint must exist in db");
    let cp: th_bot::SupervisorCheckpoint = serde_json::from_str(&checkpoint_str).unwrap();

    assert_eq!(cp.session_id, "SESSION-DURABILITY-99");
    assert_eq!(cp.active_universe, vec!["QQQ", "SPY"]);
    assert_eq!(cp.supervisor_state, SupervisorState::WaitingForSession);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_weekend_and_holiday_waiting() {
    let clock = MarketSessionClock::default();

    // Saturday Dec 26, 2026 -> next trading day is Monday Dec 28, 2026
    let saturday = NaiveDate::from_ymd_opt(2026, 12, 26).unwrap();
    assert!(!clock.is_trading_day(saturday));
    let next_trading = clock.next_trading_date(saturday);
    assert_eq!(next_trading, NaiveDate::from_ymd_opt(2026, 12, 28).unwrap());

    // Christmas Friday Dec 25, 2026 is a market holiday
    let christmas = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
    assert!(!clock.is_trading_day(christmas));
    let next_after_xmas = clock.next_trading_date(christmas);
    assert_eq!(next_after_xmas, NaiveDate::from_ymd_opt(2026, 12, 28).unwrap());
}

#[tokio::test]
async fn test_supervisor_pre_market_window_calculation() {
    let clock = MarketSessionClock::default();
    let now = Utc::now();
    let next_open = clock.next_market_open(now);
    let prep_window = clock.pre_market_window_start(now);

    assert_eq!(next_open - prep_window, chrono::Duration::minutes(60));
}

#[tokio::test]
async fn test_supervisor_no_bot_manufacturing_recovery_transition() {
    // Use FailingProvider so that universe discovery always fails.
    // This reliably triggers the PreparingSession → Recovering transition
    // without any dependence on live APIs or environment configuration.
    let (mut runtime, db) = create_test_runtime_failing();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    // Skip initialize_and_recover (it calls broker which PaperBroker handles fine).
    // Directly force PreparingSession state to isolate the manufacturing path.
    supervisor.state = SupervisorState::PreparingSession;

    let mut tick_count = 0;
    supervisor.step(Some(1), &mut tick_count).await.unwrap();

    // Universe discovery fails → supervisor must transition to Recovering,
    // preserving supervisor lifetime (INVARIANT 10 + 11).
    assert_eq!(supervisor.state, SupervisorState::Recovering);
    assert_eq!(supervisor.retry_count, 1);
    assert!(!supervisor.runtime.active);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_position_reconciliation_on_startup() {
    let (mut runtime, broker, db) = create_test_runtime_with_broker();
    let config = SupervisorConfig::default();

    // Seed broker with existing position before supervisor starts.
    // The runtime has no local trade record for this position → mismatch.
    let now = Utc::now();
    let option_symbol = "SPY-500-0";
    broker.seed_position(th_domain::Position {
        symbol: option_symbol.into(),
        qty: 1,
        avg_price: 5.0,
        mark: 5.2,
        opened_at: now,
        contract: th_domain::OptionContract::from_occ(option_symbol),
    });

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.initialize_and_recover().await.unwrap();

    // A broker/local position mismatch triggers fail-closed recovery.
    // The supervisor must enter Recovering, NOT blindly continue to Starting.
    assert_eq!(supervisor.checkpoint.restart_count, 1);
    assert_eq!(supervisor.state, SupervisorState::Recovering);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_run_supervised_bounded_execution() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        heartbeat_interval: Duration::from_millis(10),
        tick_interval: Duration::from_millis(10),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    // Run supervised with max_ticks: 3. It must cleanly finish without looping infinitely
    let res = supervisor.run_supervised(Some(3)).await;
    assert!(res.is_ok());

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_supervisor_automatic_universe_and_session_binding() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.initialize_and_recover().await.unwrap();
    let session_id = "SESSION-UNIVERSE-BIND-01";
    supervisor.runtime.current_session_id = Some(session_id.into());

    let count = supervisor.runtime.ensure_bots_manufactured(&["SPY".into()]).await.unwrap();
    assert!(count > 0);

    for plan in supervisor.runtime.bot_plans.values() {
        assert_eq!(plan.session_id, session_id);
    }

    let _ = std::fs::remove_file(&db);
}


// ---------------------------------------------------------------------------
// New tests: stall detection and finalization fail-closed behaviour
// ---------------------------------------------------------------------------

/// Verify that stall detection in `emit_heartbeat` withholds the watchdog and
/// transitions the supervisor from Trading to Recovering.
///
/// Uses a 1ms `operation_timeout` so 4x the timeout is trivially exceeded.
/// `last_progress` is backdated 600s into the past to guarantee the stall
/// fires on the very first heartbeat.
#[tokio::test]
async fn test_supervisor_stall_detection_withholds_watchdog_and_recovers() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        operation_timeout: Duration::from_millis(1),
        heartbeat_interval: Duration::from_millis(0),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.state = SupervisorState::Trading;
    supervisor.runtime.active = true;

    // Backdate last_progress far beyond 4 x 1 ms.
    supervisor.last_progress = std::time::Instant::now()
        - Duration::from_secs(600);

    let retry_before = supervisor.retry_count;
    supervisor.emit_heartbeat(Utc::now());

    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "stall detection must transition Trading -> Recovering"
    );
    assert!(
        supervisor.retry_count > retry_before,
        "stall detection must increment retry_count"
    );
    assert_eq!(
        supervisor.checkpoint.last_error.as_deref(),
        Some("runtime_stalled_in_trading"),
        "checkpoint must record the stall reason"
    );

    let _ = std::fs::remove_file(&db);
}

/// Verify FinalizingSession is fail-closed: when execute_market_closing errors
/// (real FailingProvider + seeded OpenTrade), supervisor moves to Recovering
/// without ever reaching Learning.
#[tokio::test]
async fn test_supervisor_finalization_failure_transitions_to_recovering() {
    let (mut runtime, db) = create_test_runtime_failing();
    let config = SupervisorConfig::default();

    // Seed an open trade so execute_market_closing calls option_chain, which
    // FailingProvider always rejects.
    runtime.open_trades.insert(
        "SPY-test-key".into(),
        th_bot::OpenTrade {
            symbol: "SPY230915C00450000".into(),
            underlying: "SPY".into(),
            strategy_id: "test_strategy".into(),
            entry_price: 5.0,
            entry_ts: Utc::now(),
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            qty: 1,
        },
    );

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.state = SupervisorState::FinalizingSession;
    supervisor.runtime.current_session_id = Some("SESSION-FAIL-TEST".into());

    let mut tick_count = 0usize;
    supervisor.step(None, &mut tick_count).await.unwrap();

    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "market_closing failure must transition to Recovering, not Learning"
    );
    assert!(
        supervisor.checkpoint.last_error.is_some(),
        "checkpoint must record the closing error"
    );
    assert!(
        supervisor.retry_count > 0,
        "retry_count must be incremented on finalization failure"
    );

    let _ = std::fs::remove_file(&db);
}
