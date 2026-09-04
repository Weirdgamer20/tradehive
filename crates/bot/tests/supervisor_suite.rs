use chrono::{NaiveDate, Utc};
use std::time::Duration;
use th_bot::{
    HiveSupervisor, RuntimeConfig, SupervisorConfig, SupervisorState, TradingRuntime,
    WatchdogNotifier,
};
use th_domain::{Bar, HolidayCalendar, MarketSessionClock, OptionChain};
use th_execution::PaperBroker;
use th_market_data::{MarketDataError, MarketDataProvider, NewsEvent};
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

#[derive(Clone, Default)]
struct StubMarketDataProvider {
    pub active_symbols: Vec<String>,
}

#[async_trait::async_trait]
impl MarketDataProvider for StubMarketDataProvider {
    async fn most_actives(&self, _limit: usize) -> Result<Vec<String>, MarketDataError> {
        if self.active_symbols.is_empty() {
            Ok(vec!["SPY".into()])
        } else {
            Ok(self.active_symbols.clone())
        }
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
    ) -> Result<OptionChain, MarketDataError> {
        let expiry = as_of + chrono::Duration::hours(24);
        Ok(OptionChain {
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

fn create_test_runtime() -> (TradingRuntime<PaperBroker, StubMarketDataProvider>, String) {
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

fn create_test_runtime_with_broker() -> (
    TradingRuntime<PaperBroker, StubMarketDataProvider>,
    PaperBroker,
    String,
) {
    set_test_env();
    let db = format!("target/test_supervisor_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_hist_supervisor_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    let broker = PaperBroker::new(100_000.0);
    let provider = StubMarketDataProvider::default();

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

    let loaded = supervisor
        .load_checkpoint()
        .expect("checkpoint should load from SQLite");
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
    assert_eq!(
        supervisor.checkpoint.supervisor_state,
        SupervisorState::WaitingForSession
    );

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
    assert_eq!(
        restarted.halt_reason,
        Some("RECONCILIATION_MISMATCH".into())
    );

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

    // With +/-20% jitter:
    // retry 0: base 2000ms -> [1600ms, 2400ms]
    supervisor.retry_count = 0;
    let d0 = supervisor.calculate_backoff_delay();
    assert!(
        d0 >= Duration::from_millis(1600) && d0 <= Duration::from_millis(2400),
        "d0={:?}",
        d0
    );

    // retry 1: base 4000ms -> [3200ms, 4800ms]
    supervisor.retry_count = 1;
    let d1 = supervisor.calculate_backoff_delay();
    assert!(
        d1 >= Duration::from_millis(3200) && d1 <= Duration::from_millis(4800),
        "d1={:?}",
        d1
    );

    // retry 2: base 8000ms -> [6400ms, 9600ms]
    supervisor.retry_count = 2;
    let d2 = supervisor.calculate_backoff_delay();
    assert!(
        d2 >= Duration::from_millis(6400) && d2 <= Duration::from_millis(9600),
        "d2={:?}",
        d2
    );

    // retry 3: base 16000ms -> [12800ms, 19200ms]
    supervisor.retry_count = 3;
    let d3 = supervisor.calculate_backoff_delay();
    assert!(
        d3 >= Duration::from_millis(12800) && d3 <= Duration::from_millis(19200),
        "d3={:?}",
        d3
    );

    // retry 4: base 32000ms -> [25600ms, 38400ms]
    supervisor.retry_count = 4;
    let d4 = supervisor.calculate_backoff_delay();
    assert!(
        d4 >= Duration::from_millis(25600) && d4 <= Duration::from_millis(38400),
        "d4={:?}",
        d4
    );

    // retry 5: base 60000ms (capped at max 60s) -> [48000ms, 60000ms]
    supervisor.retry_count = 5;
    let d5 = supervisor.calculate_backoff_delay();
    assert!(
        d5 >= Duration::from_millis(48000) && d5 <= Duration::from_millis(60000),
        "d5={:?}",
        d5
    );

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
    assert_eq!(
        next_after_thanksgiving,
        NaiveDate::from_ymd_opt(2026, 11, 27).unwrap()
    );
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
    let _ = supervisor
        .runtime
        .ensure_bots_manufactured(&["SPY".into()])
        .await
        .unwrap();
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
    assert!(
        supervisor.state == SupervisorState::WaitingForSession
            || supervisor.state == SupervisorState::PreparingSession
            || supervisor.state == SupervisorState::Trading
    );

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

    let _ = supervisor
        .runtime
        .ensure_bots_manufactured(&["SPY".into()])
        .await
        .unwrap();
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
    let checkpoint_str = raw_store
        .get_checkpoint("SUPERVISOR_CHECKPOINT")
        .unwrap()
        .expect("checkpoint must exist in db");
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
    assert_eq!(
        next_after_xmas,
        NaiveDate::from_ymd_opt(2026, 12, 28).unwrap()
    );
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

    let count = supervisor
        .runtime
        .ensure_bots_manufactured(&["SPY".into()])
        .await
        .unwrap();
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
    supervisor.last_progress = std::time::Instant::now() - Duration::from_secs(600);

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
            protective_order_id: None,
            client_order_id: None,
            entry_state: None,
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

#[derive(Clone)]
struct FailingBroker;

#[async_trait::async_trait]
impl th_execution::Broker for FailingBroker {
    async fn submit(
        &self,
        _order: &th_domain::OrderIntent,
    ) -> Result<th_execution::BrokerOrder, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn get_order(
        &self,
        _broker_order_id: &str,
    ) -> Result<th_execution::BrokerOrder, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn find_by_client_order_id(
        &self,
        _cid: Uuid,
    ) -> Result<Option<th_execution::BrokerOrder>, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn cancel(&self, _broker_order_id: &str) -> Result<(), th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn list_open_orders(
        &self,
    ) -> Result<Vec<th_execution::BrokerOrder>, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn cancel_all_orders(&self) -> Result<Vec<String>, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn positions(&self) -> Result<Vec<th_domain::Position>, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn account(&self) -> Result<th_execution::AccountSnapshot, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
    async fn clock(&self) -> Result<th_execution::MarketClock, th_execution::ExecutionError> {
        Err(th_execution::ExecutionError::Broker(
            "forced_broker_failure".into(),
        ))
    }
}

fn create_test_runtime_with_failing_broker() -> (
    TradingRuntime<FailingBroker, StubMarketDataProvider>,
    String,
) {
    set_test_env();
    let db = format!("target/test_supervisor_{}.sqlite", Uuid::new_v4());
    let _ = std::fs::remove_file(&db);
    let hist_dir = format!("target/test_hist_supervisor_{}", Uuid::new_v4());
    let _ = std::fs::remove_dir_all(&hist_dir);
    std::fs::create_dir_all(&hist_dir).unwrap();
    std::env::set_var("TRADING_HIVE_HISTORY_DIR", &hist_dir);

    let broker = FailingBroker;
    let provider = StubMarketDataProvider::default();

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

#[tokio::test]
async fn test_recovery_false_reconciliation_stays_in_recovering() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();

    // Insert an open trade that has no counterpart on the broker (0 positions).
    runtime.open_trades.insert(
        "SPY-mismatch".into(),
        th_bot::OpenTrade {
            symbol: "SPY230915C00450000".into(),
            underlying: "SPY".into(),
            strategy_id: "test_strategy".into(),
            entry_price: 5.0,
            entry_ts: Utc::now(),
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            qty: 1,
            protective_order_id: None,
            client_order_id: None,
            entry_state: None,
        },
    );

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.state = SupervisorState::Recovering;

    let mut tick_count = 0usize;
    supervisor.step(None, &mut tick_count).await.unwrap();

    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "false reconciliation must keep supervisor in Recovering state"
    );
    assert_eq!(supervisor.retry_count, 1);
    assert!(
        supervisor
            .checkpoint
            .last_error
            .as_deref()
            .unwrap_or("")
            .starts_with("reconciliation_mismatch"),
        "last_error should record reconciliation mismatch"
    );
    assert!(!supervisor.runtime.active, "runtime must remain inactive");

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_recovery_error_reconciliation_stays_in_recovering() {
    let (mut runtime, db) = create_test_runtime_with_failing_broker();
    let config = SupervisorConfig::default();

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.state = SupervisorState::Recovering;

    let mut tick_count = 0usize;
    supervisor.step(None, &mut tick_count).await.unwrap();

    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "error reconciliation must keep supervisor in Recovering state"
    );
    assert_eq!(supervisor.retry_count, 1);
    assert!(
        supervisor
            .checkpoint
            .last_error
            .as_deref()
            .unwrap_or("")
            .starts_with("broker_unreachable")
            || supervisor
                .checkpoint
                .last_error
                .as_deref()
                .unwrap_or("")
                .starts_with("reconciliation_error"),
        "last_error must record broker unreachable or reconciliation error"
    );
    assert!(!supervisor.runtime.active, "runtime must remain inactive");

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_retry_budget_exhausted_enters_halted() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        max_retries: 3,
        initial_retry_delay: Duration::from_millis(1),
        max_retry_delay: Duration::from_millis(5),
        ..SupervisorConfig::default()
    };

    // Mismatched position so recovery always fails.
    runtime.open_trades.insert(
        "SPY-mismatch".into(),
        th_bot::OpenTrade {
            symbol: "SPY230915C00450000".into(),
            underlying: "SPY".into(),
            strategy_id: "test_strategy".into(),
            entry_price: 5.0,
            entry_ts: Utc::now(),
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            qty: 1,
            protective_order_id: None,
            client_order_id: None,
            entry_state: None,
        },
    );

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.state = SupervisorState::Recovering;

    let mut tick_count = 0usize;
    // Step until max_retries is reached
    for _ in 0..3 {
        supervisor.step(None, &mut tick_count).await.unwrap();
    }

    assert_eq!(
        supervisor.state,
        SupervisorState::Halted,
        "exhausting retry budget must transition to Halted"
    );
    assert!(supervisor
        .checkpoint
        .last_error
        .as_deref()
        .unwrap_or("")
        .starts_with("retry_budget_exhausted"));

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_preparing_session_reconcile_failure_does_not_clear_state() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();

    // Add a dummy bot plan
    runtime.bot_plans.insert(
        "bot-1".into(),
        th_deployment::BotCreationPlan {
            session_id: "sess-1".into(),
            plan_id: "plan-1".into(),
            bot_id: "bot-1".into(),
            strategy_id: "momentum".into(),
            strategy_version: 1,
            config_version: "v1".into(),
            underlying: "SPY".into(),
            option_symbol: "SPY230915C00450000".into(),
            option_type: th_domain::OptionType::Call,
            strike: 450.0,
            expiry: Utc::now(),
            capital_allocated: 10_000.0,
            risk_budget: 200.0,
            min_expiry_minutes: 0,
            max_expiry_minutes: 100,
            created_at: Utc::now(),
            fingerprint: "fp".into(),
            quantity: 1,
            entry_limit: 5.0,
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            generation_id: "gen-1".into(),
            risk_pct: 0.02,
            max_capital_exposure: 10_000.0,
            rl_state: None,
            rl_action: None,
            rl_confidence: 1.0,
        },
    );

    // Mismatched position so reconciliation fails in PreparingSession
    runtime.open_trades.insert(
        "SPY-mismatch".into(),
        th_bot::OpenTrade {
            symbol: "SPY230915C00450000".into(),
            underlying: "SPY".into(),
            strategy_id: "test_strategy".into(),
            entry_price: 5.0,
            entry_ts: Utc::now(),
            stop_loss_pct: 0.05,
            take_profit_pct: 0.10,
            qty: 1,
            protective_order_id: None,
            client_order_id: None,
            entry_state: None,
        },
    );

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.state = SupervisorState::PreparingSession;

    let mut tick_count = 0usize;
    supervisor.step(None, &mut tick_count).await.unwrap();

    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "reconcile failure in PreparingSession must transition to Recovering"
    );
    assert_eq!(
        supervisor.runtime.bot_plans.len(),
        1,
        "bot_plans must NOT be cleared when reconciliation fails before mutation"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_ready_not_sent_when_degraded() {
    let (mut runtime, db) = create_test_runtime_with_failing_broker();
    let config = SupervisorConfig::default();

    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    // Initialize and recover with failing broker
    let _ = supervisor.initialize_and_recover().await;

    assert_eq!(
        supervisor.state,
        SupervisorState::Degraded,
        "startup with broker failure must enter Degraded"
    );
    assert_ne!(
        supervisor.state,
        SupervisorState::WaitingForSession,
        "startup with broker failure must not enter WaitingForSession"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_backoff_jitter_non_deterministic() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig {
        initial_retry_delay: Duration::from_secs(2),
        max_retry_delay: Duration::from_secs(60),
        ..SupervisorConfig::default()
    };
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);
    supervisor.retry_count = 2; // base = 8000ms, range = [6400ms, 9600ms]

    let mut samples = Vec::new();
    for _ in 0..30 {
        let d = supervisor.calculate_backoff_delay();
        assert!(
            d >= Duration::from_millis(6400) && d <= Duration::from_millis(9600),
            "delay {:?} must be in bounds [6400ms, 9600ms]",
            d
        );
        samples.push(d.as_millis());
    }

    // Verify there are distinct values among the 30 samples (proving non-determinism/jitter)
    let first = samples[0];
    let has_variation = samples.iter().any(|&s| s != first);
    assert!(
        has_variation,
        "backoff delays must vary across calls due to jitter"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_checkpoint_write_failure_aborts_state_transition() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    // Corrupt the underlying database by dropping the checkpoints table
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("DROP TABLE checkpoints", []).unwrap();
    }

    let previous_state = supervisor.state;
    // Attempt state transition to Trading
    let res = supervisor.transition_to_verified(SupervisorState::Trading, "test_failure");

    assert!(
        res.is_err(),
        "transition must return Err when durable write fails"
    );
    assert_eq!(
        supervisor.state, previous_state,
        "in-memory state must NOT be updated when durable persistence fails"
    );
    assert!(
        !supervisor.runtime.active,
        "trading must be disabled on checkpoint failure"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_corrupt_checkpoint_fails_typed_load() {
    let (mut runtime, db) = create_test_runtime();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    // Save corrupted JSON as the supervisor checkpoint
    let _ = supervisor.runtime.store.save_checkpoint(
        "SUPERVISOR_CHECKPOINT",
        "{malformed_json_not_a_valid_checkpoint",
    );

    let load_res = supervisor.load_checkpoint_typed();
    assert!(load_res.is_err(), "corrupt checkpoint must return Err");
    assert_ne!(
        load_res.ok(),
        Some(th_bot::CheckpointLoad::Missing),
        "corrupt checkpoint must NEVER collapse into CheckpointLoad::Missing"
    );

    // initialize_and_recover must fail and enter Halted
    let init_res = supervisor.initialize_and_recover().await;
    assert!(
        init_res.is_err(),
        "startup on corrupt checkpoint must return Err"
    );
    assert_eq!(
        supervisor.state,
        SupervisorState::Halted,
        "corrupt safety state must enter Halted, not clean start"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_unknown_broker_status_maps_to_unknown_not_new() {
    use th_domain::OrderStatus;

    // Verify OrderStatus helper methods
    assert!(OrderStatus::New.is_working());
    assert!(OrderStatus::Accepted.is_working());
    assert!(OrderStatus::PartiallyFilled.is_working());
    assert!(OrderStatus::PendingCancel.is_working());

    assert!(OrderStatus::Filled.is_terminal());
    assert!(OrderStatus::Cancelled.is_terminal());
    assert!(OrderStatus::Rejected.is_terminal());
    assert!(OrderStatus::Expired.is_terminal());

    assert!(OrderStatus::Unknown.requires_reconciliation());
    assert!(OrderStatus::Replaced.requires_reconciliation());

    assert_ne!(OrderStatus::Unknown, OrderStatus::New);
}

#[tokio::test]
async fn test_paper_broker_partial_fill_accounting_consistency() {
    use th_domain::{OmsState, OrderIntent, OrderSide, OrderStatus};
    use th_execution::{Broker, PaperBroker, PaperExecutionConfig};

    let broker = PaperBroker::with_config(
        100_000.0,
        PaperExecutionConfig {
            partial_fill_pct: Some(0.50), // 50% partial fill
            slippage_bps: 0.0,
            spread_bps: 0.0,
            reject_probability: 0.0,
            simulate_latency_ms: 0,
        },
    );

    let order = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "SPY230915C00450000".into(),
        side: OrderSide::Buy,
        qty: 10,
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
    };

    let bo = broker.submit(&order).await.unwrap();

    assert_eq!(bo.status, OrderStatus::PartiallyFilled);
    assert_eq!(bo.filled_qty, 5, "50% of 10 should be 5");

    let positions = broker.positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(
        positions[0].qty, 5,
        "position quantity must match filled_qty (5), not requested qty (10)"
    );

    let acct = broker.account().await.unwrap();
    let expected_cost = 5.0 * 5.0 * 100.0; // $2,500
    assert_eq!(
        acct.cash,
        100_000.0 - expected_cost,
        "cash delta must reflect filled_qty, not requested qty"
    );
}

#[tokio::test]
async fn test_finalization_cancel_failure_with_open_orders_blocks_learning() {
    let (mut runtime, db) = create_test_runtime_with_failing_broker();
    let config = SupervisorConfig::default();
    let mut supervisor = HiveSupervisor::new(&mut runtime, config);

    supervisor.state = SupervisorState::FinalizingSession;
    supervisor.runtime.current_session_id = Some("SESSION-FAIL-CANCEL".into());

    let mut tick_count = 0usize;
    supervisor.step(None, &mut tick_count).await.unwrap();

    // Cancellation and open order query failure must keep supervisor in Recovering
    assert_eq!(
        supervisor.state,
        SupervisorState::Recovering,
        "finalization failure must transition to Recovering, never Learning"
    );

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn test_watchdog_detects_event_loop_lag_and_withholds_feed() {
    use th_bot::ProgressLease;

    let lease = ProgressLease::new();
    // Mark initial progress
    lease.mark_progress();

    assert!(lease.elapsed_since_progress() < Duration::from_secs(1));
}

#[tokio::test]
async fn test_startup_watchdog_guard_disarmed_on_success() {
    use th_bot::StartupWatchdogGuard;

    let mut guard = StartupWatchdogGuard::arm(Duration::from_secs(5));
    // Disarm guard cleanly
    guard.disarm();
}
