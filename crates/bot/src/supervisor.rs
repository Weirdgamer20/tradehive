use crate::{RuntimeError, RuntimeHealth, TradingRuntime};
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use th_domain::SessionPhase;

// ---------------------------------------------------------------------------
// Supervisor state machine
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Durable checkpoint
// ---------------------------------------------------------------------------

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
            config_version: "production-v2".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub heartbeat_interval: Duration,
    pub tick_interval: Duration,
    pub operation_timeout: Duration,
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
    /// Maximum consecutive recovery failures before entering Halted.
    pub max_retries: u32,
    pub pre_market_prep_window_minutes: i64,
    /// Stale-progress threshold before the watchdog is withheld.
    /// Defaults to 4× operation_timeout when None.
    pub stall_threshold: Option<Duration>,
    /// Bounded shutdown: maximum time to wait for clean teardown before force-stopping.
    pub shutdown_deadline: Duration,
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
            stall_threshold: None,
            shutdown_deadline: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// D9: WatchdogNotifier — uses sd-notify crate (no hand-rolled socket)
// ---------------------------------------------------------------------------

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
        // sd-notify provides watchdog_enabled() which reads WATCHDOG_USEC.
        // We also check NOTIFY_SOCKET to determine if systemd is listening.
        let has_socket = std::env::var("NOTIFY_SOCKET").is_ok();
        let interval = {
            #[cfg(unix)]
            {
                if let Some((enabled, usec)) = sd_notify::watchdog_enabled(false) {
                    if enabled {
                        // systemd contract: notify at half the configured interval.
                        let half = Duration::from_micros(usec / 2);
                        half.max(Duration::from_secs(1))
                    } else {
                        Duration::from_secs(15)
                    }
                } else {
                    Duration::from_secs(15)
                }
            }
            #[cfg(not(unix))]
            {
                Duration::from_secs(15)
            }
        };
        Self {
            enabled: has_socket,
            interval,
            last_ping: Instant::now() - Duration::from_secs(60),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// D6: READY=1 must only be sent after verified operational readiness.
    /// Callers are responsible for calling this only when truly ready.
    pub fn notify_ready(&self) {
        #[cfg(unix)]
        if self.enabled {
            let _ = sd_notify::notify(
                false,
                &[
                    sd_notify::NotifyState::Ready,
                    sd_notify::NotifyState::Status("TradingHive operational"),
                ],
            );
        }
    }

    pub fn notify_watchdog(&mut self) {
        if !self.enabled {
            return;
        }
        if self.last_ping.elapsed() >= self.interval {
            #[cfg(unix)]
            {
                if sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]).is_ok() {
                    println!("WATCHDOG_HEALTHY");
                } else {
                    println!("WATCHDOG_SEND_FAILED");
                }
            }
            self.last_ping = Instant::now();
        }
    }

    pub fn notify_stopping(&self) {
        #[cfg(unix)]
        if self.enabled {
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
        }
    }

    pub fn notify_status(&self, _msg: &str) {
        #[cfg(unix)]
        if self.enabled {
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Status(_msg)]);
        }
    }
}

// ---------------------------------------------------------------------------
// D10: Independent watchdog progress lease (shared atomic)
// ---------------------------------------------------------------------------

/// A monotonic progress clock shared between the supervisor (writer) and
/// the independent watchdog task (reader). Stores Unix timestamp seconds.
pub type ProgressLease = Arc<AtomicI64>;

/// Spawns an independent watchdog task that feeds systemd WATCHDOG=1 based
/// on whether the supervisor has made progress recently. This task runs
/// independently of the main supervisor loop, so a stalled event loop will
/// still be detected by measuring how late the task wakes.
///
/// Returns the `ProgressLease` that the supervisor must update on each
/// meaningful domain operation. The watchdog task itself is fire-and-forget
/// (systemd will restart the process if WATCHDOG=1 stops coming).
pub fn spawn_watchdog_task(watchdog: WatchdogNotifier, stall_threshold: Duration) -> ProgressLease {
    let lease = Arc::new(AtomicI64::new(Utc::now().timestamp()));
    let lease_clone = Arc::clone(&lease);

    // The independent watchdog fires at half the systemd interval.
    let poll_interval = watchdog.interval.min(Duration::from_secs(10));

    tokio::spawn(async move {
        let mut local_watchdog = watchdog;
        loop {
            tokio::time::sleep(poll_interval).await;
            let last_ts = lease_clone.load(Ordering::Relaxed);
            let elapsed_secs = Utc::now().timestamp().saturating_sub(last_ts);
            let stale = elapsed_secs as u64 > stall_threshold.as_secs();

            if stale {
                // Do NOT feed the watchdog — systemd will eventually restart us.
                println!(
                    "WATCHDOG_UNHEALTHY elapsed_secs={} stall_threshold_secs={}",
                    elapsed_secs,
                    stall_threshold.as_secs()
                );
            } else {
                local_watchdog.notify_watchdog();
            }
        }
    });

    lease
}

// ---------------------------------------------------------------------------
// HiveSupervisor
// ---------------------------------------------------------------------------

pub struct HiveSupervisor<'a, B: th_execution::Broker, P: th_market_data::MarketDataProvider> {
    pub runtime: &'a mut TradingRuntime<B, P>,
    pub state: SupervisorState,
    pub config: SupervisorConfig,
    pub checkpoint: SupervisorCheckpoint,
    pub watchdog: WatchdogNotifier,
    last_heartbeat: Instant,
    /// D10: progress lease updated on meaningful domain operations.
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

    // -----------------------------------------------------------------------
    // State transition
    // -----------------------------------------------------------------------

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
            // Best-effort persist; failures logged but not fatal at this layer.
            if let Err(e) = self.persist_checkpoint_critical() {
                println!("CHECKPOINT_PERSIST_FAILED state={} error={}", next, e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // D3: Checkpoint persistence — errors are now visible
    // -----------------------------------------------------------------------

    /// Critical checkpoint: returns an error if the durable write fails.
    /// Every safety-critical transition must call this and handle the error.
    pub fn persist_checkpoint_critical(&mut self) -> Result<(), RuntimeError> {
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
        let payload = serde_json::to_string(&self.checkpoint)
            .map_err(|e| RuntimeError::Storage(format!("checkpoint_serialize: {e}")))?;
        self.runtime
            .store
            .save_checkpoint("SUPERVISOR_CHECKPOINT", &payload)
            .map_err(|e| RuntimeError::Storage(format!("checkpoint_persist: {e}")))?;
        Ok(())
    }

    /// Best-effort checkpoint for periodic heartbeats. Failures are logged but
    /// do not interrupt operation — this is NOT appropriate for safety-critical transitions.
    pub fn persist_checkpoint(&mut self) {
        if let Err(e) = self.persist_checkpoint_critical() {
            println!("CHECKPOINT_PERSIST_FAILED error={}", e);
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

    // -----------------------------------------------------------------------
    // Startup & recovery — D5, D6
    // -----------------------------------------------------------------------

    pub async fn initialize_and_recover(&mut self) -> Result<(), RuntimeError> {
        println!(
            "SUPERVISOR_STARTED version={}",
            self.runtime.cfg.config_version
        );
        if let Some(cp) = self.load_checkpoint() {
            let restart_count = cp.restart_count + 1;
            println!(
                "SUPERVISOR_RECOVERING restored_session={} previous_state={} restart_count={}",
                cp.session_id, cp.supervisor_state, restart_count
            );
            if cp.supervisor_state == SupervisorState::Halted {
                self.halt_reason = cp
                    .last_error
                    .clone()
                    .or_else(|| Some("PREVIOUS_HALTED_STATE".into()));
                self.transition_to(
                    SupervisorState::Halted,
                    "restored_halted_state_requires_operator_action",
                );
                return Ok(());
            }
            self.checkpoint = cp;
            self.checkpoint.restart_count = restart_count;
        } else {
            self.checkpoint.restart_count = 1;
        }

        // Validate broker connectivity.
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
                self.checkpoint.last_error = Some(format!("broker_unhealthy: {e}"));
                // D6: do NOT send READY=1 — broker is not operational.
                self.transition_to(SupervisorState::Degraded, "broker_unhealthy_at_init");
                return Ok(());
            }
        }

        // D5: Reconcile — propagate the actual error type, don't swallow it.
        println!("RECONCILIATION_STARTED phase=startup");
        match self.runtime.reconcile().await {
            Ok(true) => {
                println!("RECONCILIATION_SUCCEEDED phase=startup");
            }
            Ok(false) => {
                println!("RECONCILIATION_FAILED phase=startup reason=position_mismatch");
                self.checkpoint.last_error = Some("reconciliation_mismatch_at_startup".into());
                // D6: do NOT send READY=1.
                self.transition_to(
                    SupervisorState::Recovering,
                    "reconciliation_failed_at_startup",
                );
                return Ok(());
            }
            Err(e) => {
                println!("RECONCILIATION_FAILED phase=startup error={}", e);
                self.checkpoint.last_error = Some(format!("reconciliation_error_at_startup: {e}"));
                // D6: do NOT send READY=1.
                self.transition_to(
                    SupervisorState::Recovering,
                    "reconciliation_error_at_startup",
                );
                return Ok(());
            }
        }

        // Reconstruct active bot plans from store if still valid for current session.
        if !self.checkpoint.session_id.is_empty() {
            if let Ok(plans) = self
                .runtime
                .store
                .load_bot_plans_for_session(&self.checkpoint.session_id)
            {
                if !plans.is_empty() {
                    for p in plans {
                        self.runtime.bot_plans.insert(p.bot_id.clone(), p);
                    }
                    println!(
                        "SESSION_BOTS_RESTORED count={}",
                        self.runtime.bot_plans.len()
                    );
                }
            }
        }

        // D6: Only send READY=1 after we have verified broker + reconciliation.
        self.watchdog.notify_ready();
        self.watchdog
            .notify_status("TradingHive: initialized and reconciled");
        self.last_progress = Instant::now();
        self.checkpoint.last_successful_progress = Utc::now();
        self.transition_to(SupervisorState::Starting, "initialization_complete");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Heartbeat (runs in main loop, not the independent watchdog task)
    // -----------------------------------------------------------------------

    pub fn emit_heartbeat(&mut self, now: DateTime<Utc>) {
        if self.last_heartbeat.elapsed() < self.config.heartbeat_interval {
            return;
        }
        self.last_heartbeat = Instant::now();

        // Stall detection: the in-loop watchdog still withholds the notify in
        // Trading if no domain progress for stall_threshold. The independent
        // watchdog task (spawned separately) is the primary watchdog feeder.
        let stall_threshold = self
            .config
            .stall_threshold
            .unwrap_or(self.config.operation_timeout * 4);
        let is_stalled = self.state == SupervisorState::Trading
            && self.last_progress.elapsed() > stall_threshold;

        if is_stalled {
            println!(
                "RUNTIME_STALLED elapsed_secs={} — withholding watchdog",
                self.last_progress.elapsed().as_secs()
            );
            self.checkpoint.last_error = Some("runtime_stalled_in_trading".into());
            self.retry_count += 1;
            self.transition_to(SupervisorState::Recovering, "runtime_stalled");
            // Do NOT feed the watchdog — let systemd expire and restart us.
        } else {
            self.watchdog.notify_watchdog();
        }

        let session_id = self.runtime.current_session_id.as_deref().unwrap_or("NONE");
        let market_open = self.runtime.market_clock.is_open(now);
        let trading_enabled = self.runtime.active
            && self.state == SupervisorState::Trading
            && !self.runtime.bot_plans.is_empty();

        match self.state {
            SupervisorState::Trading => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=TRADING session_id={} market_open={} trading_enabled={} active_bots={} open_trades={} retry_count={} health=HEALTHY",
                    session_id,
                    market_open,
                    trading_enabled,
                    self.runtime.bot_plans.len(),
                    self.runtime.open_trades.len(),
                    self.retry_count
                );
            }
            SupervisorState::WaitingForSession => {
                let next_open = self
                    .checkpoint
                    .target_next_session
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "UNKNOWN".into());
                println!(
                    "HIVE_HEARTBEAT supervisor_state=WAITING_FOR_SESSION session_id={} market_open={} trading_enabled=false active_bots=0 target_next_session={} health=HEALTHY",
                    session_id, market_open, next_open
                );
            }
            SupervisorState::Recovering | SupervisorState::Degraded => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state={} session_id={} trading_enabled=false health=DEGRADED retry_count={} last_error=\"{}\"",
                    self.state,
                    session_id,
                    self.retry_count,
                    self.checkpoint.last_error.as_deref().unwrap_or("NONE")
                );
            }
            SupervisorState::Halted => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state=HALTED trading_enabled=false health=HALTED halt_reason=\"{}\" operator_intervention_required=true",
                    self.halt_reason.as_deref().unwrap_or("SAFETY_HALT")
                );
            }
            _ => {
                println!(
                    "HIVE_HEARTBEAT supervisor_state={} session_id={} market_open={} trading_enabled={} active_bots={} health=HEALTHY",
                    self.state,
                    session_id,
                    market_open,
                    trading_enabled,
                    self.runtime.bot_plans.len()
                );
            }
        }

        self.persist_checkpoint();
    }

    // -----------------------------------------------------------------------
    // D7: Backoff with ±20% jitter
    // -----------------------------------------------------------------------

    pub fn calculate_backoff_delay(&self) -> Duration {
        let base_ms = self.config.initial_retry_delay.as_millis().max(1) as u64;
        let exp = 2u64.saturating_pow(self.retry_count.min(6));
        let max_ms = self.config.max_retry_delay.as_millis().max(1) as u64;
        let delay_ms = base_ms.saturating_mul(exp).min(max_ms);

        // ±20% jitter to prevent synchronized retry storms.
        let jitter_range = (delay_ms / 5).max(1);
        let jitter_offset = rand::thread_rng().gen_range(0..jitter_range * 2);
        let jittered_ms = delay_ms.saturating_sub(jitter_range) + jitter_offset;
        Duration::from_millis(jittered_ms.min(max_ms))
    }

    // -----------------------------------------------------------------------
    // D8: Enforce restart budget — enter Halted on meltdown
    // -----------------------------------------------------------------------

    fn check_retry_budget(&mut self) {
        if self.retry_count >= self.config.max_retries {
            println!(
                "RETRY_BUDGET_EXHAUSTED retry_count={} max_retries={}",
                self.retry_count, self.config.max_retries
            );
            self.halt_reason = Some(format!(
                "retry_budget_exhausted after {} attempts",
                self.retry_count
            ));
            self.checkpoint.last_error = self.halt_reason.clone();
            self.transition_to(SupervisorState::Halted, "retry_budget_exhausted");
        }
    }

    // -----------------------------------------------------------------------
    // Main step
    // -----------------------------------------------------------------------

    pub async fn step(
        &mut self,
        max_ticks: Option<usize>,
        tick_count: &mut usize,
    ) -> Result<(), RuntimeError> {
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
                        self.transition_to(
                            SupervisorState::PreparingSession,
                            "pre_market_detected",
                        );
                    }
                    SessionPhase::MarketOpen | SessionPhase::Trading => {
                        if self.runtime.bot_plans.is_empty() {
                            self.transition_to(
                                SupervisorState::PreparingSession,
                                "market_open_needs_preparation",
                            );
                        } else {
                            self.transition_to(
                                SupervisorState::Trading,
                                "market_open_ready_to_trade",
                            );
                        }
                    }
                    SessionPhase::MarketClosing => {
                        self.transition_to(
                            SupervisorState::FinalizingSession,
                            "market_closing_detected",
                        );
                    }
                    SessionPhase::PostMarket => {
                        self.transition_to(
                            SupervisorState::FinalizingSession,
                            "post_market_detected",
                        );
                    }
                    SessionPhase::Learning => {
                        self.transition_to(SupervisorState::Learning, "learning_phase_detected");
                    }
                    SessionPhase::WaitingForNextSession | SessionPhase::Analysis => {
                        let next_open = self.runtime.market_clock.next_market_open(now);
                        self.checkpoint.target_next_session = Some(next_open);
                        println!(
                            "NEXT_SESSION_SCHEDULED target_next_session={}",
                            next_open.to_rfc3339()
                        );
                        self.transition_to(
                            SupervisorState::WaitingForSession,
                            "outside_market_hours",
                        );
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
                    println!(
                        "PRE_OPEN_PREPARATION_WINDOW_REACHED next_open={}",
                        next_open.to_rfc3339()
                    );
                    self.transition_to(
                        SupervisorState::PreparingSession,
                        "preparation_window_reached",
                    );
                } else if current_phase.is_trading_active() {
                    self.transition_to(SupervisorState::PreparingSession, "trading_phase_started");
                } else {
                    tokio::time::sleep(self.config.tick_interval).await;
                }
            }

            // D2: Reconcile BEFORE mutating fleet/bot_plans. If reconciliation
            // fails, abort preparation without destroying local state.
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

                // Reconcile broker state BEFORE touching fleet/bot_plans.
                println!("RECONCILIATION_STARTED phase=preparation");
                self.runtime.active = false;
                match self.runtime.reconcile().await {
                    Ok(true) => {
                        println!("RECONCILIATION_SUCCEEDED phase=preparation");
                    }
                    Ok(false) => {
                        println!(
                            "RECONCILIATION_FAILED phase=preparation reason=position_mismatch"
                        );
                        self.checkpoint.last_error =
                            Some("reconciliation_mismatch_in_preparation".into());
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "preparation_reconciliation_mismatch",
                            );
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        println!("RECONCILIATION_FAILED phase=preparation error={}", e);
                        self.checkpoint.last_error =
                            Some(format!("reconciliation_error_in_preparation: {e}"));
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "preparation_reconciliation_error",
                            );
                        }
                        return Ok(());
                    }
                }

                // Only now: retire previous session state (reconciliation verified clean).
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();

                // Discover universe with timeout.
                let start_time = Instant::now();
                let universe_res = tokio::time::timeout(
                    self.config.operation_timeout,
                    self.runtime.select_trading_universe(now),
                )
                .await;

                let symbols = match universe_res {
                    Ok(Ok(syms)) => syms,
                    Ok(Err(e)) => {
                        println!("UNIVERSE_DISCOVERY_FAILED error={}", e);
                        self.checkpoint.last_error = Some(e.to_string());
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "universe_discovery_failed",
                            );
                        }
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
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "universe_discovery_timeout",
                            );
                        }
                        return Ok(());
                    }
                };

                // Manufacture bots with timeout.
                let mfg_start = Instant::now();
                let mfg_res = tokio::time::timeout(
                    self.config.operation_timeout * 2,
                    self.runtime.ensure_bots_manufactured(&symbols),
                )
                .await;

                match mfg_res {
                    Ok(Ok(count)) if count > 0 => {
                        println!(
                            "SESSION_PREPARED session_id={} active_bots={}",
                            session_id, count
                        );
                        self.retry_count = 0;
                        self.last_progress = Instant::now();
                        self.checkpoint.last_successful_progress = Utc::now();
                        let phase = self.runtime.market_session_phase(Utc::now());
                        if phase.is_trading_active() {
                            self.transition_to(
                                SupervisorState::Trading,
                                "prepared_and_market_open",
                            );
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
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "bot_manufacturing_failed",
                            );
                        }
                    }
                    Err(_) => {
                        let elapsed = mfg_start.elapsed().as_millis();
                        println!(
                            "HIVE_OPERATION_TIMEOUT operation=ensure_bots_manufactured elapsed_ms={} session_id={} trading_enabled=false",
                            elapsed, session_id
                        );
                        self.checkpoint.last_error = Some("manufacturing_timeout".into());
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "bot_manufacturing_timeout",
                            );
                        }
                    }
                }
            }

            SupervisorState::Trading => {
                // Hard Readiness Invariants check.
                if self.runtime.bot_plans.is_empty() {
                    println!("TRADING_BLOCKED reason=ZERO_ACTIVE_BOTS");
                    self.runtime.active = false;
                    self.transition_to(
                        SupervisorState::PreparingSession,
                        "zero_active_bots_in_trading_state",
                    );
                    return Ok(());
                }

                // Priority: stale-data kill-switch check.
                if let Ok(h) = self.runtime.health_snapshot(now).await {
                    if !h.data_healthy && self.runtime.stats.last_market_event.is_some() {
                        self.runtime.execution.risk_mut().engage_kill_switch();
                        self.runtime.active = false;
                        self.halt_reason = Some("market_data_unhealthy_kill_switch".into());
                        self.transition_to(
                            SupervisorState::Halted,
                            "data_unhealthy_kill_switch_engaged",
                        );
                        return Ok(());
                    }
                }

                if !self.runtime.active {
                    let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                    if let Err(e) = self.runtime.activate_market_open(&session_id).await {
                        println!("MARKET_OPEN_ACTIVATION_FAILED error={}", e);
                        self.transition_to(
                            SupervisorState::Recovering,
                            "market_open_activation_failed",
                        );
                        return Ok(());
                    }
                }

                let current_phase = self.runtime.market_session_phase(now);
                if current_phase == SessionPhase::MarketClosing {
                    self.transition_to(
                        SupervisorState::FinalizingSession,
                        "phase_transition_to_closing",
                    );
                    return Ok(());
                } else if !current_phase.is_trading_active() {
                    self.transition_to(
                        SupervisorState::FinalizingSession,
                        "phase_transition_past_open",
                    );
                    return Ok(());
                }

                // Execute tick.
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
                                let event_id = format!(
                                    "{}:{}",
                                    symbol,
                                    b.ts.timestamp_nanos_opt().unwrap_or(0)
                                );
                                if let Err(e) =
                                    self.runtime.on_market_bar_at(&event_id, b, end).await
                                {
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

                // Record domain progress so the watchdog stays satisfied.
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

            // Hardened finalization: cancel orders first, verify flatness after.
            SupervisorState::FinalizingSession => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                println!("SESSION_FINALIZATION_STARTED session_id={}", session_id);
                self.runtime.active = false;

                // Step 1: Cancel all working orders before attempting to close positions.
                let cancelled = self
                    .runtime
                    .execution
                    .broker_ref()
                    .cancel_all_orders()
                    .await
                    .unwrap_or_else(|e| {
                        println!(
                            "CANCEL_ALL_ORDERS_ERROR session_id={} error={} continuing",
                            session_id, e
                        );
                        vec![]
                    });
                if !cancelled.is_empty() {
                    println!(
                        "WORKING_ORDERS_CANCELLED count={} session_id={}",
                        cancelled.len(),
                        session_id
                    );
                }

                // Step 2: Flatten all open positions — fail-closed on error.
                if let Err(e) = self.runtime.execute_market_closing(&session_id).await {
                    println!("MARKET_CLOSING_ERROR error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to(SupervisorState::Recovering, "market_closing_failed");
                    }
                    return Ok(());
                }

                // Step 3: Verify broker reports zero open positions.
                match self.runtime.execution.positions().await {
                    Ok(positions) if positions.is_empty() => {
                        println!(
                            "FINALIZATION_VERIFIED positions=0 session_id={}",
                            session_id
                        );
                    }
                    Ok(positions) => {
                        println!(
                            "FINALIZATION_VERIFICATION_FAILED open_positions={} session_id={}",
                            positions.len(),
                            session_id
                        );
                        self.checkpoint.last_error = Some(format!(
                            "open_positions_remain_after_close: {}",
                            positions.len()
                        ));
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "positions_remain_after_finalization",
                            );
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        println!(
                            "FINALIZATION_POSITION_CHECK_ERROR error={} session_id={}",
                            e, session_id
                        );
                        self.checkpoint.last_error = Some(format!("position_check_failed: {e}"));
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to(
                                SupervisorState::Recovering,
                                "position_check_failed_after_close",
                            );
                        }
                        return Ok(());
                    }
                }

                // Step 4: Post-market reconciliation — fail-closed on error.
                let active_symbols = self.runtime.active_universe.clone();
                if let Err(e) = self
                    .runtime
                    .execute_post_market(&session_id, &active_symbols, now)
                    .await
                {
                    println!("POST_MARKET_ERROR error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to(SupervisorState::Recovering, "post_market_failed");
                    }
                    return Ok(());
                }

                // All steps verified: safe to declare the session complete.
                println!("SESSION_FINALIZED session_id={}", session_id);
                self.retry_count = 0;
                self.transition_to(
                    SupervisorState::Learning,
                    "session_finalized_ready_for_learning",
                );
            }

            SupervisorState::Learning => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                let active_symbols = self.runtime.active_universe.clone();
                println!("LEARNING_STARTED session_id={}", session_id);

                if let Err(e) = self
                    .runtime
                    .execute_learning(&session_id, &active_symbols, now)
                    .await
                {
                    println!("LEARNING_FAILED_OR_SKIPPED reason={} - continuing_24_7", e);
                } else {
                    println!("LEARNING_COMPLETED session_id={}", session_id);
                }

                // Retire session bots and clear session plans.
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();
                self.runtime.open_trades.clear();

                let next_open = self.runtime.market_clock.next_market_open(now);
                self.checkpoint.target_next_session = Some(next_open);
                println!(
                    "NEXT_SESSION_SCHEDULED target_next_session={}",
                    next_open.to_rfc3339()
                );
                println!("WAITING_FOR_NEXT_SESSION session_id={}", session_id);

                self.transition_to(
                    SupervisorState::WaitingForSession,
                    "learning_concluded_next_session_scheduled",
                );
            }

            // D1: Recovery is only declared successful after reconcile returns Ok(true).
            SupervisorState::Recovering | SupervisorState::Degraded => {
                println!(
                    "RECOVERY_ATTEMPT attempt={} max={}",
                    self.retry_count, self.config.max_retries
                );
                let delay = self.calculate_backoff_delay();
                println!("SUPERVISOR_RETRY_SCHEDULED delay_ms={}", delay.as_millis());
                tokio::time::sleep(delay).await;

                // Step 1: Test broker connectivity.
                match self.runtime.execution.broker().await {
                    Ok(_) => {
                        // Step 2: Reconcile — result must be checked.
                        println!("RECONCILIATION_STARTED phase=recovery");
                        match self.runtime.reconcile().await {
                            Ok(true) => {
                                // Verified recovery.
                                self.retry_count = 0;
                                self.last_progress = Instant::now();
                                self.checkpoint.last_successful_progress = Utc::now();
                                self.checkpoint.last_error = None;
                                println!("RECONCILIATION_SUCCEEDED phase=recovery");
                                println!("RECOVERY_SUCCEEDED");
                                let phase = self.runtime.market_session_phase(Utc::now());
                                if phase.is_trading_active() {
                                    self.transition_to(
                                        SupervisorState::PreparingSession,
                                        "recovered_during_market_open",
                                    );
                                } else {
                                    self.transition_to(
                                        SupervisorState::WaitingForSession,
                                        "recovered_outside_market_hours",
                                    );
                                }
                            }
                            Ok(false) => {
                                self.retry_count += 1;
                                self.checkpoint.last_error =
                                    Some("reconciliation_mismatch_in_recovery".into());
                                println!(
                                    "RECONCILIATION_FAILED phase=recovery reason=position_mismatch"
                                );
                                println!(
                                    "RECOVERY_FAILED reason=reconciliation_mismatch attempt={}",
                                    self.retry_count
                                );
                                self.check_retry_budget();
                            }
                            Err(e) => {
                                self.retry_count += 1;
                                self.checkpoint.last_error =
                                    Some(format!("reconciliation_error_in_recovery: {e}"));
                                println!("RECONCILIATION_FAILED phase=recovery error={}", e);
                                println!(
                                    "RECOVERY_FAILED reason=reconciliation_error attempt={}",
                                    self.retry_count
                                );
                                self.check_retry_budget();
                            }
                        }
                    }
                    Err(e) => {
                        self.retry_count += 1;
                        self.checkpoint.last_error =
                            Some(format!("broker_unreachable_in_recovery: {e}"));
                        println!("SESSION_RECOVERY_RETRY_FAILED error={}", e);
                        println!(
                            "RECOVERY_FAILED reason=broker_unreachable attempt={}",
                            self.retry_count
                        );
                        self.check_retry_budget();
                    }
                }
            }

            SupervisorState::Halted => {
                // Safety halt: keep supervisor alive, do NOT trade.
                println!(
                    "SUPERVISOR_HALTED halt_reason=\"{}\" operator_intervention_required=true",
                    self.halt_reason.as_deref().unwrap_or("UNKNOWN")
                );
                tokio::time::sleep(self.config.heartbeat_interval).await;
            }

            SupervisorState::ShuttingDown => {
                self.watchdog.notify_stopping();
                self.runtime.active = false;
                self.persist_checkpoint();
                self.should_stop = true;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // D4, D12: Structured graceful shutdown
    // -----------------------------------------------------------------------

    pub async fn step_shutdown(&mut self) {
        println!("SHUTDOWN_STARTED");
        self.transition_to(SupervisorState::ShuttingDown, "graceful_shutdown");
        self.runtime.active = false;

        // D4: Check the reconciliation result during shutdown.
        println!("RECONCILIATION_STARTED phase=shutdown");
        match self.runtime.reconcile().await {
            Ok(true) => println!("RECONCILIATION_SUCCEEDED phase=shutdown"),
            Ok(false) => println!("RECONCILIATION_FAILED phase=shutdown reason=position_mismatch"),
            Err(e) => println!("RECONCILIATION_FAILED phase=shutdown error={}", e),
        }

        // Persist final checkpoint; failure is logged but must not block exit.
        if let Err(e) = self.persist_checkpoint_critical() {
            println!("CHECKPOINT_PERSIST_FAILED phase=shutdown error={}", e);
        }

        self.watchdog.notify_stopping();
        self.should_stop = true;
        println!("SHUTDOWN_COMPLETED");
    }

    pub async fn run_supervised(&mut self, max_ticks: Option<usize>) -> Result<(), RuntimeError> {
        self.initialize_and_recover().await?;

        let mut tick_count = 0usize;
        let mut loop_count = 0usize;
        while !self.should_stop {
            if let Err(e) = self.step(max_ticks, &mut tick_count).await {
                println!("SUPERVISOR_STEP_ERROR error={} state={}", e, self.state);
                self.checkpoint.last_error = Some(e.to_string());
                self.retry_count += 1;
                self.check_retry_budget();
                if self.state != SupervisorState::Halted {
                    self.transition_to(SupervisorState::Recovering, "step_error");
                }
            }

            loop_count += 1;

            if let Some(max) = max_ticks {
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
