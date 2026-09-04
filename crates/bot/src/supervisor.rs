use crate::{RuntimeError, RuntimeHealth, TradingRuntime};
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
// Durable checkpoint & schema versioning
// ---------------------------------------------------------------------------

pub const SUPERVISOR_CHECKPOINT_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    SUPERVISOR_CHECKPOINT_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorCheckpoint {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
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
            schema_version: SUPERVISOR_CHECKPOINT_SCHEMA_VERSION,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointLoad {
    Missing,
    Valid(Box<SupervisorCheckpoint>),
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
    /// Event loop scheduling latency tolerance before declaring watchdog unhealthy.
    pub lag_tolerance: Duration,
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
            lag_tolerance: Duration::from_secs(2),
        }
    }
}

// ---------------------------------------------------------------------------
// WatchdogNotifier — uses sd-notify crate (no hand-rolled socket)
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
        let has_socket = std::env::var("NOTIFY_SOCKET").is_ok();
        let interval = {
            #[cfg(unix)]
            {
                if let Some((enabled, usec)) = sd_notify::watchdog_enabled(false) {
                    if enabled {
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

    /// READY=1 must only be sent after verified operational readiness.
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
// Monotonic Progress Lease & Managed Watchdog Task
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProgressLease {
    start: Instant,
    millis: Arc<AtomicU64>,
}

impl Default for ProgressLease {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressLease {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            millis: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn mark_progress(&self) {
        let elapsed = self.start.elapsed().as_millis() as u64;
        self.millis.store(elapsed, Ordering::Release);
    }

    pub fn elapsed_since_progress(&self) -> Duration {
        let current = self.start.elapsed().as_millis() as u64;
        let last = self.millis.load(Ordering::Acquire);
        Duration::from_millis(current.saturating_sub(last))
    }
}

pub struct WatchdogHandle {
    pub lease: Arc<ProgressLease>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    pub task: tokio::task::JoinHandle<()>,
}

impl WatchdogHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.task.await;
    }
}

pub fn spawn_managed_watchdog(
    mut watchdog: WatchdogNotifier,
    stall_threshold: Duration,
    lag_tolerance: Duration,
) -> WatchdogHandle {
    let lease = Arc::new(ProgressLease::new());
    let lease_clone = Arc::clone(&lease);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
    let poll_interval = watchdog.interval.min(Duration::from_secs(10));

    let task = tokio::spawn(async move {
        loop {
            let scheduled_wake = Instant::now() + poll_interval;
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    println!("WATCHDOG_TASK_SHUTDOWN");
                    break;
                }
                _ = tokio::time::sleep(poll_interval) => {
                    let actual_wake = Instant::now();
                    let wake_lag = actual_wake.saturating_duration_since(scheduled_wake);
                    let elapsed_stale = lease_clone.elapsed_since_progress();

                    if wake_lag > lag_tolerance {
                        println!(
                            "WATCHDOG_UNHEALTHY reason=\"event_loop_lag\" lag_ms={} tolerance_ms={}",
                            wake_lag.as_millis(),
                            lag_tolerance.as_millis()
                        );
                        watchdog.notify_status("TradingHive: watchdog unhealthy (event loop lag)");
                    } else if elapsed_stale > stall_threshold {
                        println!(
                            "WATCHDOG_UNHEALTHY reason=\"domain_stalled\" elapsed_secs={} threshold_secs={}",
                            elapsed_stale.as_secs(),
                            stall_threshold.as_secs()
                        );
                        watchdog.notify_status("TradingHive: watchdog unhealthy (domain stalled)");
                    } else {
                        watchdog.notify_watchdog();
                    }
                }
            }
        }
    });

    WatchdogHandle {
        lease,
        shutdown_tx,
        task,
    }
}

pub fn spawn_watchdog_task(
    watchdog: WatchdogNotifier,
    stall_threshold: Duration,
) -> Arc<ProgressLease> {
    let handle = spawn_managed_watchdog(watchdog, stall_threshold, Duration::from_secs(2));
    handle.lease
}

// ---------------------------------------------------------------------------
// Startup Watchdog Guard (OS-thread guard for pre-event-loop hangs)
// ---------------------------------------------------------------------------

pub struct StartupWatchdogGuard {
    disarmed: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StartupWatchdogGuard {
    pub fn arm(deadline: Duration) -> Self {
        let disarmed = Arc::new(AtomicBool::new(false));
        let disarmed_clone = Arc::clone(&disarmed);
        let start = Instant::now();
        let handle = std::thread::spawn(move || {
            let step = Duration::from_millis(100);
            while start.elapsed() < deadline {
                if disarmed_clone.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(step);
            }
            if !disarmed_clone.load(Ordering::SeqCst) {
                eprintln!(
                    "STARTUP_WATCHDOG_TIMEOUT elapsed_secs={} process_wedged_at_startup",
                    start.elapsed().as_secs()
                );
                std::process::exit(1);
            }
        });
        Self {
            disarmed,
            handle: Some(handle),
        }
    }

    pub fn disarm(&mut self) {
        self.disarmed.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StartupWatchdogGuard {
    fn drop(&mut self) {
        self.disarm();
    }
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
    // P0: Transactional state transition (Durability First)
    // -----------------------------------------------------------------------

    pub fn transition_to_verified(
        &mut self,
        next: SupervisorState,
        reason: &str,
    ) -> Result<(), RuntimeError> {
        if self.state == next {
            return Ok(());
        }
        let previous = self.state;
        let mut candidate = self.checkpoint.clone();
        candidate.schema_version = SUPERVISOR_CHECKPOINT_SCHEMA_VERSION;
        candidate.supervisor_state = next;
        candidate.last_transition_time = Utc::now();
        candidate.heartbeat_timestamp = Utc::now();
        candidate.active_bots = self.runtime.bot_plans.len();
        if candidate.active_universe.is_empty() && !self.runtime.active_universe.is_empty() {
            candidate.active_universe = self.runtime.active_universe.clone();
        }
        if let Some(sid) = &self.runtime.current_session_id {
            candidate.session_id = sid.clone();
        }
        if next == SupervisorState::Halted && candidate.last_error.is_none() {
            candidate.last_error = self.halt_reason.clone();
        }

        let payload = serde_json::to_string(&candidate)
            .map_err(|e| RuntimeError::Storage(format!("checkpoint_serialize: {e}")))?;

        // Durability Invariant: persist to disk BEFORE committing in-memory transition.
        if let Err(e) = self
            .runtime
            .store
            .save_checkpoint("SUPERVISOR_CHECKPOINT", &payload)
        {
            println!(
                "CHECKPOINT_PERSIST_FAILED from={} to={} error={}",
                previous, next, e
            );
            self.checkpoint.last_error = Some(format!("checkpoint_persist_failed: {e}"));
            self.runtime.active = false;
            return Err(RuntimeError::Storage(format!(
                "checkpoint_persist_failed: {e}"
            )));
        }

        // Commit in-memory transition only after verified durable persistence.
        self.checkpoint = candidate;
        self.state = next;
        self.last_progress = Instant::now();
        self.checkpoint.last_successful_progress = Utc::now();

        println!(
            "SUPERVISOR_STATE_CHANGED from={} to={} reason=\"{}\"",
            previous, next, reason
        );
        Ok(())
    }

    pub fn transition_to(&mut self, next: SupervisorState, reason: &str) {
        if let Err(e) = self.transition_to_verified(next, reason) {
            println!(
                "TRANSITION_FAILED target={} reason=\"{}\" error={}",
                next, reason, e
            );
        }
    }

    // -----------------------------------------------------------------------
    // Checkpoint persistence & typed loading
    // -----------------------------------------------------------------------

    pub fn persist_checkpoint_critical(&mut self) -> Result<(), RuntimeError> {
        self.checkpoint.schema_version = SUPERVISOR_CHECKPOINT_SCHEMA_VERSION;
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

    pub fn persist_checkpoint(&mut self) {
        if let Err(e) = self.persist_checkpoint_critical() {
            println!("CHECKPOINT_PERSIST_FAILED error={}", e);
        }
    }

    pub fn load_checkpoint_typed(&mut self) -> Result<CheckpointLoad, RuntimeError> {
        match self.runtime.store.get_checkpoint("SUPERVISOR_CHECKPOINT") {
            Ok(Some(payload)) => {
                let cp: SupervisorCheckpoint = serde_json::from_str(&payload)
                    .map_err(|e| RuntimeError::Storage(format!("checkpoint_corrupted: {e}")))?;
                if cp.schema_version > SUPERVISOR_CHECKPOINT_SCHEMA_VERSION {
                    return Err(RuntimeError::Storage(format!(
                        "unsupported_checkpoint_schema_version: {} > {}",
                        cp.schema_version, SUPERVISOR_CHECKPOINT_SCHEMA_VERSION
                    )));
                }
                Ok(CheckpointLoad::Valid(Box::new(cp)))
            }
            Ok(None) => Ok(CheckpointLoad::Missing),
            Err(e) => Err(RuntimeError::Storage(format!("checkpoint_read_error: {e}"))),
        }
    }

    pub fn load_checkpoint(&mut self) -> Option<SupervisorCheckpoint> {
        match self.load_checkpoint_typed() {
            Ok(CheckpointLoad::Valid(cp)) => Some(*cp),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // Startup & recovery
    // -----------------------------------------------------------------------

    pub async fn initialize_and_recover(&mut self) -> Result<(), RuntimeError> {
        println!(
            "SUPERVISOR_STARTED version={}",
            self.runtime.cfg.config_version
        );

        match self.load_checkpoint_typed() {
            Ok(CheckpointLoad::Valid(cp)) => {
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
                    let _ = self.transition_to_verified(
                        SupervisorState::Halted,
                        "restored_halted_state_requires_operator_action",
                    );
                    return Ok(());
                }
                self.checkpoint = *cp;
                self.checkpoint.restart_count = restart_count;
            }
            Ok(CheckpointLoad::Missing) => {
                self.checkpoint.restart_count = 1;
            }
            Err(e) => {
                println!("CHECKPOINT_LOAD_ERROR error={}", e);
                self.checkpoint.last_error = Some(format!("checkpoint_load_error: {e}"));
                self.halt_reason = Some(format!("corrupt_or_incompatible_checkpoint: {e}"));
                let _ = self.transition_to_verified(
                    SupervisorState::Halted,
                    "checkpoint_load_failed_safety_halt",
                );
                return Err(e);
            }
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
                let _ = self
                    .transition_to_verified(SupervisorState::Degraded, "broker_unhealthy_at_init");
                return Ok(());
            }
        }

        // Reconcile on startup.
        println!("RECONCILIATION_STARTED phase=startup");
        match self.runtime.reconcile().await {
            Ok(true) => {
                println!("RECONCILIATION_SUCCEEDED phase=startup");
            }
            Ok(false) => {
                println!("RECONCILIATION_FAILED phase=startup reason=position_mismatch");
                self.checkpoint.last_error = Some("reconciliation_mismatch_at_startup".into());
                let _ = self.transition_to_verified(
                    SupervisorState::Recovering,
                    "reconciliation_failed_at_startup",
                );
                return Ok(());
            }
            Err(e) => {
                println!("RECONCILIATION_FAILED phase=startup error={}", e);
                self.checkpoint.last_error = Some(format!("reconciliation_error_at_startup: {e}"));
                let _ = self.transition_to_verified(
                    SupervisorState::Recovering,
                    "reconciliation_error_at_startup",
                );
                return Ok(());
            }
        }

        // Reconstruct active bot plans from store without swallowing storage errors.
        if !self.checkpoint.session_id.is_empty() {
            match self
                .runtime
                .store
                .load_bot_plans_for_session(&self.checkpoint.session_id)
            {
                Ok(plans) => {
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
                Err(e) => {
                    println!("SESSION_BOTS_RESTORE_ERROR error={}", e);
                    self.checkpoint.last_error = Some(format!("load_bot_plans_failed: {e}"));
                    let _ = self.transition_to_verified(
                        SupervisorState::Recovering,
                        "bot_plans_restore_failed",
                    );
                    return Ok(());
                }
            }
        }

        // Send READY=1 only after verified readiness.
        self.watchdog.notify_ready();
        self.watchdog
            .notify_status("TradingHive: initialized and reconciled");
        self.last_progress = Instant::now();
        self.checkpoint.last_successful_progress = Utc::now();
        let _ = self.transition_to_verified(SupervisorState::Starting, "initialization_complete");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Heartbeat
    // -----------------------------------------------------------------------

    pub fn emit_heartbeat(&mut self, now: DateTime<Utc>) {
        if self.last_heartbeat.elapsed() < self.config.heartbeat_interval {
            return;
        }
        self.last_heartbeat = Instant::now();

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
            let _ = self.transition_to_verified(SupervisorState::Recovering, "runtime_stalled");
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
    // Backoff with ±20% jitter
    // -----------------------------------------------------------------------

    pub fn calculate_backoff_delay(&self) -> Duration {
        let base_ms = self.config.initial_retry_delay.as_millis().max(1) as u64;
        let exp = 2u64.saturating_pow(self.retry_count.min(6));
        let max_ms = self.config.max_retry_delay.as_millis().max(1) as u64;
        let delay_ms = base_ms.saturating_mul(exp).min(max_ms);

        let jitter_range = (delay_ms / 5).max(1);
        let jitter_offset = rand::thread_rng().gen_range(0..jitter_range * 2);
        let jittered_ms = delay_ms.saturating_sub(jitter_range) + jitter_offset;
        Duration::from_millis(jittered_ms.min(max_ms))
    }

    // -----------------------------------------------------------------------
    // Enforce restart budget — enter Halted on meltdown
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
            let _ = self.transition_to_verified(SupervisorState::Halted, "retry_budget_exhausted");
        }
    }

    // -----------------------------------------------------------------------
    // Main step
    // -----------------------------------------------------------------------

    pub async fn step(
        &mut self,
        _max_ticks: Option<usize>,
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
                        self.transition_to_verified(
                            SupervisorState::PreparingSession,
                            "pre_market_detected",
                        )?;
                    }
                    SessionPhase::MarketOpen | SessionPhase::Trading => {
                        if self.runtime.bot_plans.is_empty() {
                            self.transition_to_verified(
                                SupervisorState::PreparingSession,
                                "market_open_needs_preparation",
                            )?;
                        } else {
                            self.transition_to_verified(
                                SupervisorState::Trading,
                                "market_open_ready_to_trade",
                            )?;
                        }
                    }
                    SessionPhase::MarketClosing => {
                        self.transition_to_verified(
                            SupervisorState::FinalizingSession,
                            "market_closing_detected",
                        )?;
                    }
                    SessionPhase::PostMarket => {
                        self.transition_to_verified(
                            SupervisorState::FinalizingSession,
                            "post_market_detected",
                        )?;
                    }
                    SessionPhase::Learning => {
                        self.transition_to_verified(
                            SupervisorState::Learning,
                            "learning_phase_detected",
                        )?;
                    }
                    SessionPhase::WaitingForNextSession | SessionPhase::Analysis => {
                        let next_open = self.runtime.market_clock.next_market_open(now);
                        self.checkpoint.target_next_session = Some(next_open);
                        self.transition_to_verified(
                            SupervisorState::WaitingForSession,
                            "market_closed_waiting_for_next_session",
                        )?;
                    }
                }
            }

            SupervisorState::WaitingForSession => {
                self.runtime.active = false;
                let current_phase = self.runtime.market_session_phase(now);

                let next_open = self.runtime.market_clock.next_market_open(now);
                self.checkpoint.target_next_session = Some(next_open);

                let prep_window = Duration::from_secs(
                    (self.config.pre_market_prep_window_minutes * 60).max(60) as u64,
                );
                let is_prep_window = if next_open > now {
                    (next_open - now).to_std().unwrap_or(Duration::ZERO) <= prep_window
                } else {
                    false
                };

                if current_phase == SessionPhase::PreMarket
                    || current_phase.is_trading_active()
                    || is_prep_window
                {
                    self.transition_to_verified(
                        SupervisorState::PreparingSession,
                        "session_prep_window_reached",
                    )?;
                } else {
                    tokio::time::sleep(self.config.tick_interval).await;
                }
            }

            // PreparingSession: Reconciles BEFORE mutating fleet/plans.
            SupervisorState::PreparingSession => {
                let current_date = now
                    .with_timezone(&self.runtime.market_clock.config.timezone)
                    .format("%Y%m%d")
                    .to_string();
                let session_id = format!("SESSION-{}", current_date);
                self.runtime.current_session_id = Some(session_id.clone());
                self.checkpoint.session_id = session_id.clone();
                self.checkpoint.session_date = current_date;

                println!("SESSION_PREPARATION_STARTED session_id={}", session_id);

                // Step 1: Reconcile before clearing state.
                println!("RECONCILIATION_STARTED phase=preparation");
                match self.runtime.reconcile().await {
                    Ok(true) => {
                        println!("RECONCILIATION_SUCCEEDED phase=preparation");
                    }
                    Ok(false) => {
                        println!(
                            "RECONCILIATION_FAILED phase=preparation reason=position_mismatch"
                        );
                        self.checkpoint.last_error =
                            Some("reconciliation_mismatch_before_session_prep".into());
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to_verified(
                                SupervisorState::Recovering,
                                "reconciliation_failed_before_session_prep",
                            )?;
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        println!("RECONCILIATION_FAILED phase=preparation error={}", e);
                        self.checkpoint.last_error =
                            Some(format!("reconciliation_error_before_session_prep: {e}"));
                        self.retry_count += 1;
                        self.check_retry_budget();
                        if self.state != SupervisorState::Halted {
                            self.transition_to_verified(
                                SupervisorState::Recovering,
                                "reconciliation_error_before_session_prep",
                            )?;
                        }
                        return Ok(());
                    }
                }

                // Step 2: Now safe to retire old fleet and clear bot plans.
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();

                // Step 3: Discover active universe.
                let active_symbols = match self.runtime.select_trading_universe(now).await {
                    Ok(symbols) if !symbols.is_empty() => symbols,
                    _ => self.runtime.cfg.candidate_universe.clone(),
                };

                if active_symbols.is_empty() {
                    println!("UNIVERSE_DISCOVERY_FAILED fallback_exhausted");
                    self.checkpoint.last_error = Some("empty_active_universe".into());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "empty_universe_during_preparation",
                        )?;
                    }
                    return Ok(());
                }

                self.runtime.active_universe = active_symbols.clone();
                self.checkpoint.active_universe = active_symbols.clone();

                // Step 4: Manufacture bots for session.
                if let Err(e) = self.runtime.ensure_bots_manufactured(&active_symbols).await {
                    println!("BOT_MANUFACTURING_FAILED error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "bot_manufacturing_failed",
                        )?;
                    }
                    return Ok(());
                }

                if self.runtime.bot_plans.is_empty() {
                    println!("ZERO_BOTS_MANUFACTURED session_id={}", session_id);
                    self.checkpoint.last_error = Some("zero_bots_manufactured".into());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "zero_bots_manufactured",
                        )?;
                    }
                    return Ok(());
                }

                if let Err(e) = self.runtime.activate_market_open(&session_id).await {
                    println!("ACTIVATE_MARKET_OPEN_FAILED error={}", e);
                }

                self.checkpoint.active_bots = self.runtime.bot_plans.len();
                self.last_progress = Instant::now();
                self.checkpoint.last_successful_progress = Utc::now();
                self.retry_count = 0;

                println!(
                    "SESSION_PREPARED session_id={} active_bots={} active_universe={:?}",
                    session_id,
                    self.runtime.bot_plans.len(),
                    active_symbols
                );

                let phase = self.runtime.market_session_phase(now);
                if phase.is_trading_active() {
                    self.transition_to_verified(
                        SupervisorState::Trading,
                        "session_prep_complete_market_open",
                    )?;
                } else {
                    self.transition_to_verified(
                        SupervisorState::WaitingForSession,
                        "session_prep_complete_waiting_for_open",
                    )?;
                }
            }

            SupervisorState::Trading => {
                if self.runtime.bot_plans.is_empty() {
                    println!("NO_ACTIVE_BOTS_IN_TRADING transitioning_to_preparation");
                    self.transition_to_verified(
                        SupervisorState::PreparingSession,
                        "no_active_bots_in_trading",
                    )?;
                    return Ok(());
                }

                if let Ok(h) = self.runtime.health_snapshot(Utc::now()).await {
                    if !h.data_healthy && self.runtime.stats.last_market_event.is_some() {
                        self.runtime.execution.risk_mut().engage_kill_switch();
                        self.runtime.active = false;
                        self.runtime.health = RuntimeHealth::Halted;
                        self.halt_reason = Some("market_data_unhealthy_kill_switch".into());
                        self.transition_to_verified(
                            SupervisorState::Halted,
                            "market_data_unhealthy",
                        )?;
                        return Ok(());
                    }
                }

                let current_phase = self.runtime.market_session_phase(now);
                if !current_phase.is_trading_active() {
                    println!(
                        "MARKET_SESSION_CLOSED phase={:?} initiating_finalization",
                        current_phase
                    );
                    self.transition_to_verified(
                        SupervisorState::FinalizingSession,
                        "market_session_ended",
                    )?;
                    return Ok(());
                }

                self.runtime.active = true;
                let active_symbols = if self.runtime.active_universe.is_empty() {
                    self.runtime.cfg.candidate_universe.clone()
                } else {
                    self.runtime.active_universe.clone()
                };

                let clock = self
                    .runtime
                    .execution
                    .clock()
                    .await
                    .map_err(|e| RuntimeError::Execution(e.to_string()))?;

                if clock.is_open {
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
                                        println!(
                                            "ON_MARKET_BAR_ERROR symbol={} error={}",
                                            symbol, e
                                        );
                                    } else {
                                        self.last_progress = Instant::now();
                                        self.checkpoint.last_successful_progress = Utc::now();
                                    }
                                }
                            }
                            Err(e) => {
                                println!("MARKET_DATA_BARS_ERROR symbol={} error={}", symbol, e);
                            }
                        }
                    }
                }

                *tick_count += 1;
                tokio::time::sleep(self.config.tick_interval).await;
            }

            // P0: Fail-closed finalization
            SupervisorState::FinalizingSession => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                println!("SESSION_FINALIZATION_STARTED session_id={}", session_id);
                self.runtime.active = false;

                // Step 1: Cancel all working orders before attempting to close positions.
                let cancel_res = self
                    .runtime
                    .execution
                    .broker_ref()
                    .cancel_all_orders()
                    .await;

                match cancel_res {
                    Ok(cancelled) => {
                        if !cancelled.is_empty() {
                            println!(
                                "WORKING_ORDERS_CANCELLED count={} session_id={}",
                                cancelled.len(),
                                session_id
                            );
                        }
                    }
                    Err(e) => {
                        println!(
                            "CANCEL_ALL_ORDERS_ERROR session_id={} error={}",
                            session_id, e
                        );
                        // Query open orders to verify if any working orders actually remain
                        match self.runtime.execution.broker_ref().list_open_orders().await {
                            Ok(open_orders) if open_orders.is_empty() => {
                                println!(
                                    "WORKING_ORDERS_VERIFIED_EMPTY after_cancel_error session_id={}",
                                    session_id
                                );
                            }
                            Ok(open_orders) => {
                                println!(
                                    "WORKING_ORDERS_REMAIN count={} session_id={}",
                                    open_orders.len(),
                                    session_id
                                );
                                self.checkpoint.last_error = Some(format!(
                                    "cancel_failed_open_orders_remain: {}",
                                    open_orders.len()
                                ));
                                self.retry_count += 1;
                                self.check_retry_budget();
                                if self.state != SupervisorState::Halted {
                                    self.transition_to_verified(
                                        SupervisorState::Recovering,
                                        "open_orders_remain_after_cancel_failure",
                                    )?;
                                }
                                return Ok(());
                            }
                            Err(list_err) => {
                                println!(
                                    "LIST_OPEN_ORDERS_ERROR session_id={} error={}",
                                    session_id, list_err
                                );
                                self.checkpoint.last_error =
                                    Some(format!("cancel_and_list_orders_failed: {list_err}"));
                                self.retry_count += 1;
                                self.check_retry_budget();
                                if self.state != SupervisorState::Halted {
                                    self.transition_to_verified(
                                        SupervisorState::Recovering,
                                        "cancel_and_list_orders_failed",
                                    )?;
                                }
                                return Ok(());
                            }
                        }
                    }
                }

                // Step 2: Flatten all open positions — fail-closed on error.
                if let Err(e) = self.runtime.execute_market_closing(&session_id).await {
                    println!("MARKET_CLOSING_ERROR error={}", e);
                    self.checkpoint.last_error = Some(e.to_string());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "market_closing_failed",
                        )?;
                    }
                    return Ok(());
                }

                // Step 3: Verify broker reports zero open positions and zero open working orders.
                let (open_orders_empty, open_orders_err) =
                    match self.runtime.execution.broker_ref().list_open_orders().await {
                        Ok(orders) => (orders.is_empty(), None),
                        Err(e) => (false, Some(e.to_string())),
                    };

                if let Some(err) = open_orders_err {
                    println!(
                        "FINALIZATION_ORDER_CHECK_ERROR error={} session_id={}",
                        err, session_id
                    );
                    self.checkpoint.last_error = Some(format!("open_order_check_failed: {err}"));
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "open_order_check_failed",
                        )?;
                    }
                    return Ok(());
                }

                if !open_orders_empty {
                    println!(
                        "FINALIZATION_VERIFICATION_FAILED working_orders_remain session_id={}",
                        session_id
                    );
                    self.checkpoint.last_error = Some("working_orders_remain_after_close".into());
                    self.retry_count += 1;
                    self.check_retry_budget();
                    if self.state != SupervisorState::Halted {
                        self.transition_to_verified(
                            SupervisorState::Recovering,
                            "working_orders_remain",
                        )?;
                    }
                    return Ok(());
                }

                match self.runtime.execution.positions().await {
                    Ok(positions) if positions.is_empty() => {
                        println!(
                            "FINALIZATION_VERIFIED positions=0 open_orders=0 session_id={}",
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
                            self.transition_to_verified(
                                SupervisorState::Recovering,
                                "positions_remain_after_finalization",
                            )?;
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
                            self.transition_to_verified(
                                SupervisorState::Recovering,
                                "position_check_failed_after_close",
                            )?;
                        }
                        return Ok(());
                    }
                }

                // Step 4: Post-market execution
                let active_symbols = self.runtime.active_universe.clone();
                if let Err(e) = self
                    .runtime
                    .execute_post_market(&session_id, &active_symbols, now)
                    .await
                {
                    println!("POST_MARKET_ERROR error={}", e);
                }

                println!("SESSION_FINALIZED session_id={}", session_id);
                self.transition_to_verified(
                    SupervisorState::Learning,
                    "session_finalization_complete",
                )?;
            }

            SupervisorState::Learning => {
                let session_id = self.runtime.current_session_id.clone().unwrap_or_default();
                let active_symbols = self.runtime.active_universe.clone();
                println!("SESSION_LEARNING_STARTED session_id={}", session_id);

                if let Err(e) = self
                    .runtime
                    .execute_learning(&session_id, &active_symbols, now)
                    .await
                {
                    println!(
                        "LEARNING_NON_FATAL_ERROR session_id={} error={} continuing_24_7",
                        session_id, e
                    );
                } else {
                    println!("SESSION_LEARNING_SUCCEEDED session_id={}", session_id);
                }

                // Retire session bots and clear plans after learning
                self.runtime.fleet.retire_all();
                self.runtime.bot_plans.clear();

                let next_open = self.runtime.market_clock.next_market_open(now);
                self.checkpoint.target_next_session = Some(next_open);
                self.last_progress = Instant::now();
                self.checkpoint.last_successful_progress = Utc::now();
                self.retry_count = 0;

                self.transition_to_verified(
                    SupervisorState::WaitingForSession,
                    "learning_concluded_next_session_scheduled",
                )?;
            }

            // Gated recovery: Only success upon Ok(true).
            SupervisorState::Recovering | SupervisorState::Degraded => {
                println!(
                    "RECOVERY_ATTEMPT attempt={} max={}",
                    self.retry_count, self.config.max_retries
                );
                let delay = self.calculate_backoff_delay();
                println!("SUPERVISOR_RETRY_SCHEDULED delay_ms={}", delay.as_millis());
                tokio::time::sleep(delay).await;

                match self.runtime.execution.broker().await {
                    Ok(_) => {
                        println!("RECONCILIATION_STARTED phase=recovery");
                        match self.runtime.reconcile().await {
                            Ok(true) => {
                                self.retry_count = 0;
                                self.last_progress = Instant::now();
                                self.checkpoint.last_successful_progress = Utc::now();
                                self.checkpoint.last_error = None;
                                println!("RECONCILIATION_SUCCEEDED phase=recovery");
                                println!("RECOVERY_SUCCEEDED");
                                let phase = self.runtime.market_session_phase(Utc::now());
                                if phase.is_trading_active() {
                                    self.transition_to_verified(
                                        SupervisorState::PreparingSession,
                                        "recovered_during_market_open",
                                    )?;
                                } else {
                                    self.transition_to_verified(
                                        SupervisorState::WaitingForSession,
                                        "recovered_outside_market_hours",
                                    )?;
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
                println!(
                    "SUPERVISOR_HALTED operator_intervention_required reason=\"{}\"",
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
    // Structured graceful shutdown
    // -----------------------------------------------------------------------

    pub async fn step_shutdown(&mut self) {
        println!("SHUTDOWN_STARTED");
        let _ = self.transition_to_verified(SupervisorState::ShuttingDown, "graceful_shutdown");
        self.runtime.active = false;

        println!("RECONCILIATION_STARTED phase=shutdown");
        match self.runtime.reconcile().await {
            Ok(true) => println!("RECONCILIATION_SUCCEEDED phase=shutdown"),
            Ok(false) => println!("RECONCILIATION_FAILED phase=shutdown reason=position_mismatch"),
            Err(e) => println!("RECONCILIATION_FAILED phase=shutdown error={}", e),
        }

        if !self.runtime.open_trades.is_empty() {
            println!(
                "SHUTDOWN_OPEN_TRADES_DETECTED count={} initiating_emergency_liquidation",
                self.runtime.open_trades.len()
            );
            if let Err(e) = self.runtime.emergency_liquidate_all().await {
                println!("SHUTDOWN_LIQUIDATION_FAILED error={}", e);
            }
        }

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
                    let _ = self.transition_to_verified(SupervisorState::Recovering, "step_error");
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
