use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use th_domain::SessionPhase;
use crate::{RuntimeError, RuntimeHealth, TradingRuntime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupervisorState {
    Starting,
    Recovering,
    WaitingForSession,
    PreparingSession,
    Trading,
    FinalizingSession,
    Learning,
    Degraded,
    Halted,
    ShuttingDown,
}

impl std::fmt::Display for SupervisorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "STARTING"),
            Self::Recovering => write!(f, "RECOVERING"),
            Self::WaitingForSession => write!(f, "WAITING_FOR_SESSION"),
            Self::PreparingSession => write!(f, "PREPARING_SESSION"),
            Self::Trading => write!(f, "TRADING"),
            Self::FinalizingSession => write!(f, "FINALIZING_SESSION"),
            Self::Learning => write!(f, "LEARNING"),
            Self::Degraded => write!(f, "DEGRADED"),
            Self::Halted => write!(f, "HALTED"),
            Self::ShuttingDown => write!(f, "SHUTTING_DOWN"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorCheckpoint {
    pub supervisor_state: SupervisorState,
    pub session_id: String,
    pub session_date: String,
    pub session_phase: SessionPhase,
    pub target_next_session: Option<DateTime<Utc>>,
    pub last_transition_time: DateTime<Utc>,
    pub last_successful_progress: DateTime<Utc>,
    pub last_broker_check: DateTime<Utc>,
    pub last_market_data_check: DateTime<Utc>,
    pub last_market_event: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub retry_count: u32,
    pub restart_count: u32,
    pub heartbeat_timestamp: DateTime<Utc>,
    pub active_bots: usize,
    pub active_universe: Vec<String>,
    pub config_version: String,
}

impl Default for SupervisorCheckpoint {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            supervisor_state: SupervisorState::Starting,
            session_id: String::new(),
            session_date: String::new(),
            session_phase: SessionPhase::WaitingForNextSession,
            target_next_session: None,
            last_transition_time: now,
            last_successful_progress: now,
            last_broker_check: now,
            last_market_data_check: now,
            last_market_event: None,
            last_error: None,
            retry_count: 0,
            restart_count: 0,
            heartbeat_timestamp: now,
            active_bots: 0,
            active_universe: Vec::new(),
            config_version: "production-v1".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub heartbeat_interval: Duration,
    pub tick_interval: Duration,
    pub operation_timeout: Duration,
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub max_retries: u32,
    pub pre_market_prep_window_minutes: i64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(15),
            tick_interval: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(30),
            initial_retry_delay: Duration::from_secs(2),
            max_retry_delay: Duration::from_secs(300),
            max_retries: 10,
            pre_market_prep_window_minutes: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchdogNotifier {
    enabled: bool,
    interval: Duration,
    last_ping: Instant,
}

impl Default for WatchdogNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchdogNotifier {
    pub fn new() -> Self {
        let has_socket = std::env::var("NOTIFY_SOCKET").is_ok();
        let usec_val = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        let (enabled, interval) = match (has_socket, usec_val) {
            (true, Some(usec)) => {
                let dur = Duration::from_micros(usec / 2);
                (true, dur.max(Duration::from_secs(1)))
            }
            (true, None) => (true, Duration::from_secs(15)),
            _ => (false, Duration::from_secs(15)),
        };
        Self {
            enabled,
            interval,
            last_ping: Instant::now() - Duration::from_secs(60),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn notify_ready(&self) {
        self.send("READY=1");
    }

    pub fn notify_watchdog(&mut self) {
        if self.last_ping.elapsed() >= self.interval {
            self.send("WATCHDOG=1");
            self.last_ping = Instant::now();
        }
    }

    pub fn notify_stopping(&self) {
        self.send("STOPPING=1");
    }

    fn send(&self, _msg: &str) {
        if self.enabled {
            #[cfg(unix)]
            {
                if let Ok(path) = std::env::var("NOTIFY_SOCKET") {
                    if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
                        let _ = socket.send_to(_msg.as_bytes(), path);
                    }
                }
            }
        }
    }
}

pub struct HiveSupervisor<'a, B: th_execution::Broker, P: th_market_data::MarketDataProvider> {
    pub runtime: &'a mut TradingRuntime<B, P>,
    pub state: SupervisorState,
    pub config: SupervisorConfig,
    pub checkpoint: SupervisorCheckpoint,
    pub watchdog: WatchdogNotifier,
    last_heartbeat: Instant,
    pub last_progress: Instant,
    pub retry_count: u32,
    pub halt_reason: Option<String>,
    should_stop: bool,
}

impl<'a, B: th_execution::Broker, P: th_market_data::MarketDataProvider> HiveSupervisor<'a, B, P> {
    pub fn new(runtime: &'a mut TradingRuntime<B, P>, config: SupervisorConfig) -> Self {
        let watchdog = WatchdogNotifier::new();
        Self {
            runtime,
            state: SupervisorState::Starting,
            config,
            checkpoint: SupervisorCheckpoint::default(),
            watchdog,
            last_heartbeat: Instant::now() - Duration::from_secs(60),
            last_progress: Instant::now(),
            retry_count: 0,
            halt_reason: None,
            should_stop: false,
        }
    }

    pub fn stop(&mut self) {
        self.should_stop = true;
    }

    pub fn is_stopped(&self) -> bool {
        self.should_stop
    }

    pub fn transition_to(&mut self, next: SupervisorState, reason: &str) {
        if self.state != next {
            println!(
                "SUPERVISOR_STATE_CHANGED from={} to={} reason=\"{}\"",
                self.state, next, reason
            );
            self.state = next;
            self.checkpoint.supervisor_state = next;
            self.checkpoint.last_transition_time = Utc::now();
            // Reset progress clock on every transition so we don't immediately
            // stall-detect in the new state.
            self.last_progress = Instant::now();
            self.checkpoint.last_successful_progress = Utc::now();
            if next == SupervisorState::Halted && self.checkpoint.last_error.is_none() {
                self.checkpoint.last_error = self.halt_reason.clone();
            }
            self.persist_checkpoint();
        }
    }

    pub fn persist_checkpoint(&mut self) {
        self.checkpoint.heartbeat_timestamp = Utc::now();
        self.checkpoint.active_bots = self.runtime.bot_plans.len();
        if self.checkpoint.active_universe.is_empty() && !self.runtime.active_universe.is_empty() {
            self.checkpoint.active_universe = self.runtime.active_universe.clone();
        }
        if let Some(sid) = &self.runtime.current_session_id {
            self.checkpoint.session_id = sid.clone();
        }
        if self.halt_reason.is_some() && self.checkpoint.last_error.is_none() {
            self.checkpoint.last_error = self.halt_reason.clone();
        }
        if let Ok(payload) = serde_json::to_string(&self.checkpoint) {
            let _ = self.runtime.store.save_checkpoint("SUPERVISOR_CHECKPOINT", &payload);
        }
    }

    pub fn load_checkpoint(&mut self) -> Option<SupervisorCheckpoint> {
        if let Ok(Some(payload)) = self.runtime.store.get_checkpoint("SUPERVISOR_CHECKPOINT") {
            if let Ok(cp) = serde_json::from_str::<SupervisorCheckpoint>(&payload) {
                return Some(cp);
            }
        }
        None
    }

    pub async fn initialize_and_recover(&mut self) -> Result<(), RuntimeError> {
        println!("SUPERVISOR_STARTED version={}", self.runtime.cfg.config_version);
        if let Some(cp) = self.load_checkpoint() {
            let restart_count = cp.restart_count + 1;
            println!(
                "SUPERVISOR_RECOVERING restored_session={} previous_state={} restart_count={}",
                cp.session_id, cp.supervisor_state, restart_count
            );
            if cp.supervisor_state == SupervisorState::Halted {
                self.halt_reason = cp.last_error.clone().or_else(|| Some("PREVIOUS_HALTED_STATE".into()));
                self.transition_to(SupervisorState::Halted, "restored_halted_state_requires_operator_action");
                return Ok(());
            }
            self.checkpoint = cp;
            self.checkpoint.restart_count = restart_count;
        } else {
            self.checkpoint.restart_count = 1;
        }

        // Validate broker connection
        match self.runtime.execution.broker().await {
            Ok(acct) => {
                self.checkpoint.last_broker_check = Utc::now();
                println!(
                    "BROKER_CONNECTED equity={:.2} cash={:.2} buying_power={:.2}",
                    acct.equity, acct.cash, acct.buying_power
                );
            }
            Err(e) => {
                println!("BROKER_UNHEALTHY error={}", e);
                self.transition_to(SupervisorState::Degraded, "broker_unhealthy_at_init");
                return Ok(());
            }
        }

        // Reconcile existing positions
        let reconciled = self.runtime.reconcile().await.unwrap_or(false);
        if !reconciled {
            println!("RECONCILIATION_FAILED_AT_STARTUP engaging_recovery");
            self.transition_to(SupervisorState::Recovering, "reconciliation_failed_at_startup");
            return Ok(());
        }

        // Reconstruct active bot plans from store if still valid for current session
        if !self.checkpoint.session_id.is_empty() {
            if let Ok(plans) = self.runtime.store.load_bot_plans_for_session(&self.checkpoint.session_id) {
                if !plans.is_empty() {
                    for p in plans {
                        self.runtime.bot_plans.insert(p.bot_id.clone(), p);
                    }
                    println!("SESSION_BOTS_RESTORED count={}", self.runtime.bot_plans.len());
                }
            }
        }

        self.watchdog.notify_ready();
        self.transition_to(SupervisorState::Starting, "initialization_complete");
        Ok(())
    }

    pub fn emit_heartbeat(&mut self, now: DateTime<Utc>) {
        if self.last_heartbeat.elapsed() < self.config.heartbeat_interval {
            return;
        }
        self.last_heartbeat = Instant::now();

        // Stall detection: if we have been in Trading for more than 4× the
        // operation_timeout without any domain progress, the runtime is stuck.
        // Withhold the watchdog notification so systemd eventually restarts us.
        let stall_timeout = self.config.operation_timeout * 4;
        let is_stalled = self.state == SupervisorState::Trading
            && self.last_progress.elapsed() > stall_timeout;

        if is_stalled {
            println!(
                "RUNTIME_STALLED elapsed_secs={} — withholding watchdog",
                self.last_progress.elapsed().as_secs()
            );
            self.checkpoint.last_error = Some("runtime_stalled_in_trading".into());
            self.retry_count += 1;
            self.transition_to(SupervisorState::Recovering, "runtime_stalled");
            // Do NOT call notify_watchdog() — let systemd expire and restart us.
        } else {
            self.watchdog.notify_watchdog();
        }

        let session_id = self.runtime.current_session_id.as_deref().unwrap_or("NONE");
        let market_open = self.runtime.market_clock.is_open(now);
        let trading_enabled = self.runtime.active && self.state == SupervisorState::Trading && !self.runtime.bot_plans.is_empty();

        match self.state {
            SupervisorState::Trading => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=TRADING session_id={} market_open={} trading_enabled={} active_bots={} open_trades={} retry_count={} health=HEALTHY",
                    session_id, market_open, trading_enabled, self.runtime.bot_plans.len(), self.runtime.open_trades.len(), self.retry_count
                );
            }
            SupervisorState::WaitingForSession => {
                let next_open = self.checkpoint.target_next_session.map(|t| t.to_rfc3339()).unwrap_or_else(|| "UNKNOWN".into());
                println!(
                    "HIVE_HEARTBEAT supervisor_state=WAITING_FOR_SESSION session_id={} market_open={} trading_enabled=false active_bots=0 target_next_session={} health=HEALTHY",
                    session_id, market_open, next_open
                );
            }
            SupervisorState::Recovering => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=RECOVERING session_id={} trading_enabled=false health=DEGRADED retry_count={} last_error=\"{}\"",
                    session_id, self.retry_count, self.checkpoint.last_error.as_deref().unwrap_or("NONE")
                );
            }
            SupervisorState::Halted => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=HALTED trading_enabled=false health=HALTED halt_reason=\"{}\" operator_intervention_required=true",
                    self.halt_reason.as_deref().unwrap_or("SAFETY_HALT")
                );
            }
            SupervisorState::Degraded => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=DEGRADED session_id={} trading_enabled=false health=DEGRADED retry_count={}",
                    session_id, self.retry_count
                );
            }
            _ => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state={} session_id={} market_open={} trading_enabled={} active_bots={} health=HEALTHY",
                    self.state, session_id, market_open, trading_enabled, self.runtime.bot_plans.len()
                );
            }
        }

        self.persist_checkpoint();
    }

    pub fn calculate_backoff_delay(&self) -> Duration {
        let base_secs = self.config.initial_retry_delay.as_secs().max(1);
        let exp = 2u64.saturating_pow(self.retry_count.min(6));
        let delay_secs = (base_secs * exp).min(self.config.max_retry_delay.as_secs());
        Duration::from_secs(delay_secs)
    }

    pub async fn step(&mut self, max_ticks: Option<usize>, tick_count: &mut usize) -> Result<(), RuntimeError> {
        let now = Utc::now();
        self.emit_heartbeat(now);

        match self.state {
            SupervisorState::Starting => {
                let current_phase = self.runtime.market_session_phase(now);
                let current_date = now
                    .with_timezone(&self.runtime.market_clock.config.timezone)
                    .format("%Y%m%d")
                    .to_string();
                let session_id = format!("SESSION-{}", current_date);
                self.runtime.current_session_id = Some(session_id.clone());
                self.checkpoint.session_id = session_id.clone();
                self.checkpoint.session_date = current_date;

                match current_phase {
                    SessionPhase::PreMarket => {
                        self.transition_to(SupervisorState::PreparingSession, "pre_market_detected");
                    }
                    SessionPhase::MarketOpen | SessionPhase::Trading => {
                        if self.runtime.bot_plans.is_empty() {
                            self.transition_to(SupervisorState::PreparingSession, "market_open_needs_preparation");
                        } else {
                            self.transition_to(SupervisorState::Trading, "market_open_ready_to_trade");
                        }
                    }
                    SessionPhase::MarketClosing => {
                        self.transition_to(SupervisorState::FinalizingSession, "market_closing_detected");
                    }
                    SessionPhase::PostMarket => {
                        self.transition_to(SupervisorState::FinalizingSession, "post_market_detected");
                    }
                    SessionPhase::Learning => {
                        self.transition_to(SupervisorState::Learning, "learning_phase_detected");
                    }
                    SessionPhase::WaitingForNextSession | SessionPhase::Analysis => {
                        let next_open = self.runtime.market_clock.next_market_open(now);
                        self.checkpoint.target_next_session = Some(next_open);
                        println!("NEXT_SESSION_SCHEDULED target_next_session={}", next_open.to_rfc3339());
                        self.transition_to(SupervisorState::WaitingForSession, "outside_market_hours");
                    }
                }
            }

            SupervisorState::WaitingForSession => {
                self.runtime.active = false;
                let next_open = self.runtime.market_clock.next_market_open(now);
                self.checkpoint.target_next_session = Some(next_open);
                let prep_window_start = self.runtime.market_clock.pre_market_window_start(now);

                let current_phase = self.runtime.market_session_phase(now);
                if current_phase == SessionPhase::PreMarket || now >= prep_window_start {
                    println!("PRE_OPEN_PREPARATION_WINDOW_REACHED next_open={}", next_open.to_rfc3339());
                    self.transition_to(SupervisorState::PreparingSession, "preparation_window_reached");
                } else if current_phase.is_trading_active() {
                    self.transition_to(SupervisorState::PreparingSession, "trading_phase_started");
                } else {
                    tokio::time::sleep(self.config.tick_interval).await;
                }
            }

            SupervisorState::PreparingSession => {
                println!("SESSION_PREPARATION_STARTED");
                let current_date = now
                    .with_timezone(&self.runtime.market_clock.config.timezone)
                    .format("%Y%m%d")
                    .to_string();
                let session_id = format!("SESSION-{}", current_date);
                self.runtime.current_session_id = Some(session_id.clone());
                self.checkpoint.session_id = session_id.clone();
                self.checkpoint.session_date = current_date;

                // Reconcile and clean previous session state
                let _ = self.runtime.reconcile().await;
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();

                // Discover universe with timeout
                let start_time = Instant::now();
                let universe_res = tokio::time::timeout(
                    self.config.operation_timeout,
                    self.runtime.select_trading_universe(now),
                ).await;

                let symbols = match universe_res {
                    Ok(Ok(syms)) => syms,
                    Ok(Err(e)) => {
                        println!("UNIVERSE_DISCOVERY_FAILED error={}", e);
                        self.checkpoint.last_error = Some(e.to_string());
                        self.retry_count += 1;
                        self.transition_to(SupervisorState::Recovering, "universe_discovery_failed");
                        return Ok(());
                    }
                    Err(_) => {
                        let elapsed = start_time.elapsed().as_millis();
                        println!(
                            "HIVE_OPERATION_TIMEOUT operation=select_trading_universe elapsed_ms={} session_id={} trading_enabled=false",
                            elapsed, session_id
                        );
                        self.checkpoint.last_error = Some("universe_timeout".into());
                        self.retry_count += 1;
                        self.transition_to(SupervisorState::Recovering, "universe_discovery_timeout");
                        return Ok(());
                    }
                };

                // Manufacture bots with timeout
                let mfg_start = Instant::now();
                let mfg_res = tokio::time::timeout(
                    self.config.operation_timeout * 2,
                    self.runtime.ensure_bots_manufactured(&symbols),
                ).await;

                match mfg_res {
                    Ok(Ok(count)) if count > 0 => {
                        println!("SESSION_PREPARED session_id={} active_bots={}", session_id, count);
                        self.retry_count = 0;
                        self.last_progress = Instant::now();
                        self.checkpoint.last_successful_progress = Utc::now();
                        let phase = self.runtime.market_session_phase(Utc::now());
                        if phase.is_trading_active() {
                            self.transition_to(SupervisorState::Trading, "prepared_and_market_open");
                        } else {
                            println!("SESSION_READY_WAITING_FOR_OPEN session_id={}", session_id);
                            tokio::time::sleep(self.config.tick_interval).await;
                        }
                    }
                    Ok(Ok(_)) | Ok(Err(_)) => {
                        println!("BOT_MANUFACTURING_BLOCKED reason=NO_BOT_PLANS_MANUFACTURED");
                        println!("TRADING_HALTED");
                        self.checkpoint.last_error = Some("manufacturing_failed".into());
                        self.retry_count += 1;
                        self.transition_to(SupervisorState::Recovering, "bot_manufacturing_failed");
                    }
                    Err(_) => {
                        let elapsed = mfg_start.elapsed().as_millis();
                        println!(
                            "HIVE_OPERATION_TIMEOUT operation=ensure_bots_manufactured elapsed_ms={} session_id={} trading_enabled=false",
                            elapsed, session_id
                        );
                        self.checkpoint.last_error = Some("manufacturing_timeout".into());
                        self.retry_count += 1;
                        self.transition_to(SupervisorState::Recovering, "bot_manufacturing_timeout");
                    }
                }
            }

            SupervisorState::Trading => {
                // Hard Readiness Invariants check
                if self.runtime.bot_plans.is_empty() {
                    println!("TRADING_BLOCKED reason=ZERO_ACTIVE_BOTS");
                    self.runtime.active = false;
                    self.transition_to(SupervisorState::PreparingSession, "zero_active_bots_in_trading_state");
                    return Ok(());
                }

                // Priority: stale-data kill-switch check runs BEFORE market-phase exits.
                // A stale-data condition is a safety-critical signal that must halt trading
                // regardless of whether the market is still open or transitioning.
                if let Ok(h) = self.runtime.health_snapshot(now).await {
                    if !h.data_healthy && self.runtime.stats.last_market_event.is_some() {
                        self.runtime.execution.risk_mut().engage_kill_switch();
                        self.runtime.active = false;
                        self.halt_reason = Some("market_data_unhealthy_kill_switch".into());
                        self.transition_to(SupervisorState::Halted, "data_unhealthy_kill_switch_engaged");
                        return Ok(());
                    }
                }

                if !self.runtime.active {
                    let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                    if let Err(e) = self.runtime.activate_market_open(&session_id).await {
                        println!("MARKET_OPEN_ACTIVATION_FAILED error={}", e);
                        self.transition_to(SupervisorState::Recovering, "market_open_activation_failed");
                        return Ok(());
                    }
                }

                let current_phase = self.runtime.market_session_phase(now);
                if current_phase == SessionPhase::MarketClosing {
                    self.transition_to(SupervisorState::FinalizingSession, "phase_transition_to_closing");
                    return Ok(());
                } else if !current_phase.is_trading_active() {
                    self.transition_to(SupervisorState::FinalizingSession, "phase_transition_past_open");
                    return Ok(());
                }

                // Execute tick with bounded timeout
                let active_symbols = self.runtime.active_universe.clone();
                for symbol in &active_symbols {
                    let now_tick = Utc::now();
                    let end = now_tick
                        - chrono::Duration::seconds(now_tick.timestamp().rem_euclid(60))
                        - chrono::Duration::minutes(1);
                    let start = end - chrono::Duration::minutes(6);

                    match self.runtime.provider.bars(symbol, start, end).await {
                        Ok(bs) => {
                            for b in bs {
                                let event_id = format!("{}:{}", symbol, b.ts.timestamp_nanos_opt().unwrap_or(0));
                                if let Err(e) = self.runtime.on_market_bar_at(&event_id, b, end).await {
                                    println!("BAR_PROCESSING_ERROR symbol={} error={}", symbol, e);
                                }
                            }
                        }
                        Err(e) => {
                            self.runtime.health = RuntimeHealth::Degraded;
                            self.checkpoint.last_error = Some(format!("market_data_error: {e}"));
                        }
                    }
                }

                println!(
                    "BOT_EVALUATION phase={:?} active_bots={} open_trades={} trades_opened={} trades_closed={} realized_pnl={:.2}",
                    current_phase,
                    self.runtime.bot_plans.len(),
                    self.runtime.open_trades.len(),
                    self.runtime.stats.trades_opened,
                    self.runtime.stats.trades_closed,
                    self.runtime.stats.realized_pnl
                );

                // Record domain progress so the stall watchdog stays satisfied.
                self.last_progress = Instant::now();
                self.checkpoint.last_successful_progress = Utc::now();

                *tick_count += 1;
                if let Some(max) = max_ticks {
                    if *tick_count >= max {
                        println!("SESSION_STOPPING max_ticks reached ({})", max);
                        self.transition_to(SupervisorState::FinalizingSession, "max_ticks_reached");
                        return Ok(());
                    }
                }

                tokio::time::sleep(self.config.tick_interval).await;
            }

            SupervisorState::FinalizingSession => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                println!("SESSION_FINALIZATION_STARTED session_id={}", session_id);
                self.runtime.active = false;

                // Flatten all open positions — fail-closed on error.
                if let Err(e) = self.runtime.execute_market_closing(&session_id).await {
                    println!("MARKET_CLOSING_ERROR error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.transition_to(SupervisorState::Recovering, "market_closing_failed");
                    return Ok(());
                }

                // Post-market reconciliation — fail-closed on error.
                let active_symbols = self.runtime.active_universe.clone();
                if let Err(e) = self.runtime.execute_post_market(&session_id, &active_symbols, now).await {
                    println!("POST_MARKET_ERROR error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.transition_to(SupervisorState::Recovering, "post_market_failed");
                    return Ok(());
                }

                // Both steps succeeded — safe to declare the session complete.
                println!("SESSION_FINALIZED session_id={}", session_id);
                self.transition_to(SupervisorState::Learning, "session_finalized_ready_for_learning");
            }

            SupervisorState::Learning => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                let active_symbols = self.runtime.active_universe.clone();
                println!("LEARNING_STARTED session_id={}", session_id);

                // Execute learning safely (does not crash supervisor on error or 0 trades)
                if let Err(e) = self.runtime.execute_learning(&session_id, &active_symbols, now).await {
                    println!("LEARNING_FAILED_OR_SKIPPED reason={} - continuing_24_7", e);
                } else {
                    println!("LEARNING_COMPLETED session_id={}", session_id);
                }

                // Retire session bots and clear session plans
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();
                self.runtime.open_trades.clear();

                // Compute next valid market session
                let next_open = self.runtime.market_clock.next_market_open(now);
                self.checkpoint.target_next_session = Some(next_open);
                println!("NEXT_SESSION_SCHEDULED target_next_session={}", next_open.to_rfc3339());
                println!("WAITING_FOR_NEXT_SESSION session_id={}", session_id);

                self.transition_to(SupervisorState::WaitingForSession, "learning_concluded_next_session_scheduled");
            }

            SupervisorState::Recovering | SupervisorState::Degraded => {
                println!("SESSION_RECOVERY_STARTED attempt={}", self.retry_count);
                let delay = self.calculate_backoff_delay();
                println!("SUPERVISOR_RETRY_SCHEDULED delay_secs={}", delay.as_secs());
                tokio::time::sleep(delay).await;

                // Test broker connectivity
                match self.runtime.execution.broker().await {
                    Ok(_) => {
                        let _ = self.runtime.reconcile().await;
                        self.retry_count = 0;
                        println!("SESSION_RECOVERY_SUCCEEDED");
                        let phase = self.runtime.market_session_phase(Utc::now());
                        if phase.is_trading_active() {
                            self.transition_to(SupervisorState::PreparingSession, "recovered_during_market_open");
                        } else {
                            self.transition_to(SupervisorState::WaitingForSession, "recovered_outside_market_hours");
                        }
                    }
                    Err(e) => {
                        self.retry_count += 1;
                        self.checkpoint.last_error = Some(format!("recovery_failed: {e}"));
                        println!("SESSION_RECOVERY_RETRY_FAILED error={}", e);
                    }
                }
            }

            SupervisorState::Halted => {
                // Safety halt: keep supervisor alive, do NOT trade, report halt periodically
                tokio::time::sleep(self.config.heartbeat_interval).await;
            }

            SupervisorState::ShuttingDown => {
                println!("SUPERVISOR_SHUTDOWN");
                self.watchdog.notify_stopping();
                self.runtime.active = false;
                self.persist_checkpoint();
                self.should_stop = true;
            }
        }

        Ok(())
    }

    pub async fn step_shutdown(&mut self) {
        self.transition_to(SupervisorState::ShuttingDown, "graceful_shutdown");
        println!("SUPERVISOR_SHUTDOWN");
        self.watchdog.notify_stopping();
        self.runtime.active = false;
        let _ = self.runtime.reconcile().await;
        self.persist_checkpoint();
        self.should_stop = true;
    }

    pub async fn run_supervised(&mut self, max_ticks: Option<usize>) -> Result<(), RuntimeError> {
        self.initialize_and_recover().await?;

        // `tick_count` tracks Trading-state ticks (used by the step() guard).
        // `loop_count` tracks total supervisor loop iterations for bounded test execution:
        // when the market is closed, we may never reach Trading, so max_ticks also caps
        // the total number of supervisor steps to prevent infinite waiting in tests.
        let mut tick_count = 0usize;
        let mut loop_count = 0usize;
        while !self.should_stop {
            if let Err(e) = self.step(max_ticks, &mut tick_count).await {
                println!("SUPERVISOR_STEP_ERROR error={} state={}", e, self.state);
                self.checkpoint.last_error = Some(e.to_string());
                self.retry_count += 1;
                self.transition_to(SupervisorState::Recovering, "step_error");
            }

            loop_count += 1;

            if let Some(max) = max_ticks {
                // Exit if we have reached the Trading-tick limit from within step(),
                // OR if we have performed max total loop iterations (handles market-closed case).
                if self.state == SupervisorState::ShuttingDown
                    || tick_count >= max
                    || loop_count >= max
                {
                    break;
                }
            }
        }

        Ok(())
    }
}
