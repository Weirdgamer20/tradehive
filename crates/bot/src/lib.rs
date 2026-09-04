use chrono::{DateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use th_deployment::{BotCreationPlan, BotFleet};
use th_domain::{Bar, OptionOrderAction, OrderIntent, OrderSide, OrderStatus, SessionPhase};
use th_execution::{order_hash, reconcile_positions, Broker, ExecutionEngine};
use th_hive::{
    manufacture_promoted_bots_for_session, run_analysis_with_q_and_trades, AnalysisBundle,
    HiveManufacturingPolicy, QLearning,
};
use th_market_data::{classify_news_risk, MarketDataProvider, MultiSymbolCandleEngine, NewsRisk};
use th_memory::{ExperienceStore, TradeRecord};
use th_risk::{PortfolioRisk, RiskGovernor, RiskLimits};
use th_storage::{
    BotHistoryRecord, ExecutionFeedbackRecord, HiveManufacturingRun, JsonHistoryStore,
    OpenTradeRecord, Store,
};
use th_strategy::{classify_regime, Strategy, StrategyRegistry};
use thiserror::Error;
use uuid::Uuid;

pub mod sizing;
pub use sizing::*;

pub mod supervisor;
pub use supervisor::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub analysis_start_hour: u32,
    pub analysis_hours: u32,
    pub market_timezone: String,
    pub max_bars_memory: usize,
    pub max_event_ids: usize,
    pub database_path: String,
    pub config_version: String,
    pub max_quote_age_secs: i64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub bot_max_hold_minutes: u32,
    pub candidate_universe: Vec<String>,
    pub max_universe_size: usize,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            analysis_start_hour: 20,
            analysis_hours: 4,
            market_timezone: "America/New_York".into(),
            max_bars_memory: 1000,
            max_event_ids: 100_000,
            database_path: "trading_hive.sqlite".into(),
            config_version: "production-v1".into(),
            max_quote_age_secs: 30,
            stop_loss_pct: 0.0,
            take_profit_pct: 0.0,
            bot_max_hold_minutes: 180,
            candidate_universe: Vec::new(),
            max_universe_size: 10,
        }
    }
}
impl RuntimeConfig {
    /// Explicit test configuration. Production configuration must come from environment variables.
    pub fn testing() -> Self {
        Self {
            stop_loss_pct: 0.05,
            take_profit_pct: 0.0,
            candidate_universe: vec!["SPY".into()],
            max_universe_size: 2,
            ..Self::default()
        }
    }
    pub fn from_env() -> Result<Self, RuntimeError> {
        let mut c = Self::default();
        if let Ok(v) = std::env::var("TRADING_HIVE_DB") {
            c.database_path = v
        }
        if let Ok(v) = std::env::var("TRADING_HIVE_CONFIG_VERSION") {
            c.config_version = v
        }
        if let Ok(v) = std::env::var("TRADING_STOP_LOSS_PCT") {
            if let Ok(p) = v.parse::<f64>() {
                c.stop_loss_pct = p;
            }
        }
        if let Ok(v) = std::env::var("TRADING_TAKE_PROFIT_PCT") {
            if let Ok(p) = v.parse::<f64>() {
                c.take_profit_pct = p;
            }
        }
        if let Ok(v) = std::env::var("HIVE_MAX_UNIVERSE_SIZE") {
            if let Ok(n) = v.parse::<usize>() {
                c.max_universe_size = n;
            }
        }
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.analysis_hours != 4 {
            return Err(RuntimeError::InvalidConfig(
                "analysis_hours must be exactly 4".into(),
            ));
        }
        if self.analysis_start_hour != 20 {
            return Err(RuntimeError::InvalidConfig(
                "analysis_start_hour must be exactly 20".into(),
            ));
        }
        if self.analysis_start_hour >= 24 {
            return Err(RuntimeError::InvalidConfig(
                "analysis_start_hour must be 0..23".into(),
            ));
        }
        if self.max_bars_memory < 100 {
            return Err(RuntimeError::InvalidConfig(
                "max_bars_memory too small".into(),
            ));
        }
        if self.max_event_ids < 1024 {
            return Err(RuntimeError::InvalidConfig(
                "max_event_ids too small".into(),
            ));
        }
        if self.max_quote_age_secs <= 0 {
            return Err(RuntimeError::InvalidConfig(
                "max_quote_age_secs must be positive".into(),
            ));
        }
        if !self.stop_loss_pct.is_finite() || self.stop_loss_pct <= 0.0 || self.stop_loss_pct >= 1.0
        {
            return Err(RuntimeError::InvalidConfig(
                "stop loss must be finite and strictly between 0 and 1".into(),
            ));
        }
        if !self.take_profit_pct.is_finite() || self.take_profit_pct < 0.0 {
            return Err(RuntimeError::InvalidConfig(
                "take profit must be finite and non-negative".into(),
            ));
        }
        if self.bot_max_hold_minutes != 180 {
            return Err(RuntimeError::InvalidConfig(
                "bot_max_hold_minutes must be exactly 180".into(),
            ));
        }
        match self.market_timezone.as_str() {
            "Asia/Kolkata" | "UTC" | "America/New_York" => Ok(()),
            _ => Err(RuntimeError::InvalidConfig(format!(
                "unsupported market timezone: {}",
                self.market_timezone
            ))),
        }
    }
    fn tz(&self) -> Tz {
        match self.market_timezone.as_str() {
            "Asia/Kolkata" => chrono_tz::Asia::Kolkata,
            "UTC" => chrono_tz::UTC,
            _ => chrono_tz::America::New_York,
        }
    }
    pub fn phase_at(&self, dt: DateTime<Utc>) -> SessionPhase {
        let local = dt.with_timezone(&self.tz());
        let start = self.analysis_start_hour % 24;
        let end = (start + self.analysis_hours.min(24)) % 24;
        if self.analysis_hours >= 24 {
            return SessionPhase::Analysis;
        }
        if start < end {
            if local.hour() >= start && local.hour() < end {
                SessionPhase::Analysis
            } else {
                SessionPhase::Trading
            }
        } else if local.hour() >= start || local.hour() < end {
            SessionPhase::Analysis
        } else {
            SessionPhase::Trading
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeHealth {
    Healthy,
    Degraded,
    Halted,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BotSizing {
    pub quantity: u32,
    pub capital_capacity: u32,
    pub risk_capacity: u32,
}

/// Worker-side position sizing with signal strength scaling. Hive supplies only capital/risk budgets; the worker
/// uses the live option ask, strategy signal strength, and risk budget to decide quantity.
pub fn calculate_worker_quantity_with_strength(
    capital_allocated: f64,
    risk_budget: f64,
    option_ask: f64,
    stop_loss_pct: f64,
    multiplier: f64,
    signal_strength: f64,
) -> Result<BotSizing, RuntimeError> {
    if !capital_allocated.is_finite()
        || capital_allocated <= 0.0
        || !risk_budget.is_finite()
        || risk_budget <= 0.0
    {
        return Err(RuntimeError::InvalidConfig(
            "invalid bot capital/risk budget".into(),
        ));
    }
    if !option_ask.is_finite()
        || option_ask <= 0.0
        || !stop_loss_pct.is_finite()
        || stop_loss_pct <= 0.0
        || stop_loss_pct >= 1.0
        || !multiplier.is_finite()
        || multiplier <= 0.0
    {
        return Err(RuntimeError::InvalidConfig(
            "invalid live option sizing inputs".into(),
        ));
    }
    let strength = if signal_strength.is_finite() && signal_strength > 0.0 {
        signal_strength.clamp(0.1, 1.0)
    } else {
        1.0
    };
    let contract_cost = option_ask * multiplier;
    let risk_per_contract = contract_cost * stop_loss_pct;
    let capital_capacity = (capital_allocated / contract_cost).floor() as u32;
    let risk_capacity = (risk_budget / risk_per_contract).floor() as u32;
    let raw_qty = capital_capacity.min(risk_capacity);
    let scaled_qty = ((raw_qty as f64) * strength).floor() as u32;
    let quantity = if raw_qty > 0 && scaled_qty == 0 {
        1
    } else {
        scaled_qty
    };
    Ok(BotSizing {
        quantity,
        capital_capacity,
        risk_capacity,
    })
}

/// Backward-compatible worker-side position sizing (defaults to full strength 1.0).
pub fn calculate_worker_quantity(
    capital_allocated: f64,
    risk_budget: f64,
    option_ask: f64,
    stop_loss_pct: f64,
    multiplier: f64,
) -> Result<BotSizing, RuntimeError> {
    calculate_worker_quantity_with_strength(
        capital_allocated,
        risk_budget,
        option_ask,
        stop_loss_pct,
        multiplier,
        1.0,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTrade {
    pub symbol: String,
    pub underlying: String,
    pub strategy_id: String,
    pub entry_price: f64,
    pub entry_ts: DateTime<Utc>,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub qty: u32,
    /// Broker order ID of the protective sell-to-close limit order submitted
    /// immediately after the entry fill. `None` means the protective order
    /// has not yet been submitted or submission failed (the process-level
    /// `manage_open_trade` exit loop remains the fallback safety net).
    /// NOTE: Alpaca does not support OTO/bracket/stop orders for options,
    /// so this is a separate limit order submitted post-fill.
    #[serde(default)]
    pub protective_order_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub trades_opened: u64,
    pub trades_closed: u64,
    pub realized_pnl: f64,
    pub rejected_orders: u64,
    pub last_market_event: Option<DateTime<Utc>>,
}

fn strategies_from_report(report: &AnalysisBundle) -> Vec<Box<dyn Strategy>> {
    let registry = StrategyRegistry::new();
    let mut selected = Vec::new();
    for p in &report.promoted {
        if let Ok(s) = registry.create(&p.strategy_id) {
            selected.push(s);
            continue;
        }
        if let Some(g) = report
            .symbols
            .iter()
            .filter_map(|x| x.report.generated_strategy.as_ref())
            .find(|g| {
                g.blueprint.id == p.strategy_id
                    && g.validation.as_ref().map(|v| v.accepted).unwrap_or(false)
            })
        {
            if let Ok(s) = registry.create_synthesized(&g.blueprint) {
                selected.push(s);
                continue;
            }
        }
        // Promoted strategies from previous RL sessions are persisted in the seed snapshot.
        // Reconstruct any validated synthesized strategy without special-casing STRAT-31.
        if let Ok(store) = JsonHistoryStore::new(
            std::env::var("TRADING_HIVE_HISTORY_DIR").unwrap_or_else(|_| "data".into()),
        ) {
            if let Ok(Some(snapshot)) = store.latest_seed_snapshot() {
                if let Some(b) = snapshot
                    .iter()
                    .find(|v| v.get("strategy_id").and_then(|x| x.as_str()) == Some(&p.strategy_id))
                    .and_then(|v| v.get("blueprint"))
                    .cloned()
                {
                    if let Ok(bp) = serde_json::from_value::<th_strategy::StrategyBlueprint>(b) {
                        if let Ok(s) = registry.create_synthesized(&bp) {
                            selected.push(s);
                        }
                    }
                }
            }
        }
    }
    selected
}

pub struct TradingRuntime<B: Broker, P: MarketDataProvider> {
    pub cfg: RuntimeConfig,
    pub strategies: Vec<Box<dyn Strategy>>,
    pub execution: ExecutionEngine<B>,
    pub provider: P,
    pub store: Store,
    pub bars: HashMap<String, Vec<Bar>>,
    pub active: bool,
    pub candles: MultiSymbolCandleEngine,
    pub open_trades: HashMap<String, OpenTrade>,
    pub bot_plans: HashMap<String, BotCreationPlan>,
    pub fleet: BotFleet,
    pub experience: ExperienceStore,
    pub daily_realized: f64,
    pub daily_key: String,
    pub stats: SessionStats,
    pub health: RuntimeHealth,
    pub control_epoch: u64,
    pub json_history: JsonHistoryStore,
    pub market_clock: th_domain::MarketSessionClock,
    pub current_session_id: Option<String>,
    pub active_universe: Vec<String>,
    pub phase_override: Option<SessionPhase>,
    /// Live Q-policy loaded from the previous session. Consulted at every trading tick.
    pub q_policy: Option<QLearning>,
}
impl<B: Broker, P: MarketDataProvider> TradingRuntime<B, P> {
    pub fn new(cfg: RuntimeConfig, broker: B, provider: P) -> Result<Self, RuntimeError> {
        cfg.validate()?;
        let store =
            Store::open(&cfg.database_path).map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let history_root = std::env::var("TRADING_HIVE_HISTORY_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("data"));
        let json_history = JsonHistoryStore::new(history_root)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let now = Utc::now();
        let daily_key = now.date_naive().to_string();
        let daily_name = format!("daily_realized:{}", daily_key);
        let daily_realized = store
            .checkpoint_value(&daily_name)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        if store
            .active_config()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
            .is_none()
        {
            let payload =
                serde_json::to_string(&cfg).map_err(|e| RuntimeError::Storage(e.to_string()))?;
            store
                .save_config(&cfg.config_version, &payload, true)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        }
        let mut strategies = StrategyRegistry::new().all();
        let mut active_version = cfg.config_version.clone();
        if let Some((version, payload)) = store
            .active_config()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            active_version = version;
            if let Ok(report) = serde_json::from_str::<AnalysisBundle>(&payload) {
                let selected = strategies_from_report(&report);
                if !selected.is_empty() {
                    strategies = selected;
                }
            }
        }
        let mut open_trades = HashMap::new();
        let mut bars: HashMap<String, Vec<Bar>> = HashMap::new();
        for symbol in store
            .distinct_symbols()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            if let Ok(history) = store.recent_candles(&symbol, cfg.max_bars_memory) {
                bars.insert(symbol, history);
            }
        }
        let bot_plans = HashMap::new();
        let fleet = BotFleet::default();
        for row in store
            .load_open_trades()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            if let Ok(entry_ts) =
                DateTime::parse_from_rfc3339(&row.entry_ts).map(|x| x.with_timezone(&Utc))
            {
                open_trades.insert(
                    row.symbol.clone(),
                    OpenTrade {
                        symbol: row.symbol,
                        underlying: row.underlying,
                        strategy_id: row.strategy_id,
                        entry_price: row.entry_price,
                        entry_ts,
                        stop_loss_pct: row.stop_loss_pct,
                        take_profit_pct: row.take_profit_pct,
                        qty: row.qty,
                        protective_order_id: row.protective_order_id,
                    },
                );
            }
        }
        let risk_limits =
            RiskLimits::from_env().map_err(|e| RuntimeError::InvalidConfig(e.to_string()))?;
        let execution = ExecutionEngine::new(broker, RiskGovernor::new(risk_limits));
        let market_clock = th_domain::MarketSessionClock::from_env();
        let initial_active = market_clock.phase_at(now).allows_new_entries();
        Ok(Self {
            strategies,
            execution,
            provider,
            store,
            bars,
            active: initial_active,
            candles: MultiSymbolCandleEngine::new(cfg.max_event_ids),
            open_trades,
            bot_plans,
            fleet,
            experience: ExperienceStore::default(),
            daily_realized,
            daily_key,
            stats: SessionStats {
                trades_opened: 0,
                trades_closed: 0,
                realized_pnl: 0.0,
                rejected_orders: 0,
                last_market_event: None,
            },
            health: RuntimeHealth::Healthy,
            control_epoch: 0,
            json_history,
            market_clock,
            current_session_id: None,
            active_universe: Vec::new(),
            phase_override: None,
            q_policy: None,
            cfg: RuntimeConfig {
                config_version: active_version,
                ..cfg
            },
        })
    }
    pub fn phase_at(&self, dt: DateTime<Utc>) -> SessionPhase {
        self.market_session_phase(dt)
    }
    pub fn phase(&self) -> SessionPhase {
        self.market_session_phase(Utc::now())
    }
    pub fn market_session_phase(&self, dt: DateTime<Utc>) -> SessionPhase {
        if let Some(p) = self.phase_override {
            p
        } else {
            self.market_clock.phase_at(dt)
        }
    }
    pub fn halt(&mut self) {
        self.active = false;
        self.health = RuntimeHealth::Halted;
        self.execution.risk_mut().engage_kill_switch();
        self.control_epoch += 1
    }
    pub fn resume_trading(&mut self) {
        if self.health != RuntimeHealth::Halted {
            self.active = true;
            self.health = RuntimeHealth::Healthy;
            self.control_epoch += 1
        }
    }
    pub fn roll_daily_risk(&mut self, now: DateTime<Utc>) -> Result<(), RuntimeError> {
        let key = now.date_naive().to_string();
        if key != self.daily_key {
            self.daily_key = key.clone();
            self.daily_realized = 0.0;
            self.store
                .checkpoint(&format!("daily_realized:{}", key), "0")
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        }
        Ok(())
    }
    pub async fn on_market_bar(&mut self, event_id: &str, bar: Bar) -> Result<(), RuntimeError> {
        self.on_market_bar_at(event_id, bar, Utc::now()).await
    }
    pub async fn on_market_bar_at(
        &mut self,
        event_id: &str,
        bar: Bar,
        now: DateTime<Utc>,
    ) -> Result<(), RuntimeError> {
        self.roll_daily_risk(now)?;
        bar.validate()
            .map_err(|e| RuntimeError::Market(e.to_string()))?;
        if bar.ts > now + chrono::Duration::minutes(5) {
            return Err(RuntimeError::Market("future market bar".into()));
        }
        if self.health == RuntimeHealth::Halted {
            return Ok(());
        }
        let market_key = format!(
            "market:{}:{}",
            bar.symbol,
            bar.ts.timestamp_nanos_opt().unwrap_or(0)
        );
        if !self
            .store
            .reserve_market_event(&market_key)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            return Ok(());
        }
        self.stats.last_market_event = Some(bar.ts);
        let Some(candle) = self
            .candles
            .push_event(event_id, bar)
            .map_err(|e| RuntimeError::Market(e.to_string()))?
        else {
            return Ok(());
        };
        self.store
            .candle(&candle)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        {
            let history = self.bars.entry(candle.symbol.clone()).or_default();
            history.push(candle.clone());
            if history.len() > self.cfg.max_bars_memory {
                let n = history.len() - self.cfg.max_bars_memory;
                history.drain(0..n);
            }
        }
        if self.market_session_phase(now) != SessionPhase::MarketOpen || !self.active {
            return Ok(());
        }
        let snapshot = self.bars.get(&candle.symbol).cloned().unwrap_or_default();
        let state = classify_regime(&snapshot);
        self.manage_open_trade(&candle.symbol, &state).await?;
        // Consult the live Q-policy to determine the preferred action for the current regime.
        // The Q-policy suppresses or permits buy/sell signals; it does NOT bypass risk controls.
        let state_key = th_hive::StateKey::from_state(&state);
        let q_action = self.q_policy.as_mut().and_then(|q| {
            q.choose(
                &state_key,
                &[
                    "Buy".to_string(),
                    "Sell".to_string(),
                    "Hold".to_string(),
                    "Exit".to_string(),
                ],
            )
        });
        let q_permits_buy = q_action.as_deref() != Some("Hold")
            && q_action.as_deref() != Some("Exit")
            && q_action.as_deref() != Some("Sell");
        let mut signals = Vec::new();
        // Strategy-to-bot routing: evaluate only strategies assigned to active bots for this symbol
        let assigned_strategies: Vec<String> = self
            .bot_plans
            .values()
            .filter(|p| {
                let session_match = match &self.current_session_id {
                    Some(curr) if !p.session_id.is_empty() => curr == &p.session_id,
                    _ => false,
                };
                session_match && p.underlying == candle.symbol
            })
            .map(|p| p.strategy_id.clone())
            .collect();

        for strategy in self.strategies.iter_mut() {
            if assigned_strategies.contains(&strategy.spec().id) {
                if let Some(sig) = strategy.update(&candle, &state) {
                    // Gate long entries by Q-policy. Short/flat signals always pass through.
                    let is_long_entry = matches!(
                        sig.side,
                        th_domain::SignalSide::LongCall | th_domain::SignalSide::LongPut
                    );
                    if is_long_entry && !q_permits_buy {
                        println!(
                            "SIGNAL_SUPPRESSED_BY_Q symbol={} strategy_id={} side={:?} q_action={:?}",
                            sig.symbol, sig.strategy_id, sig.side, q_action
                        );
                        continue;
                    }
                    println!(
                        "SIGNAL_GENERATED symbol={} strategy_id={} side={:?} strength={:.2} q_action={:?}",
                        sig.symbol, sig.strategy_id, sig.side, sig.strength, q_action
                    );
                    signals.push(sig);
                }
            }
        }
        for sig in signals {
            let sig_symbol = sig.symbol.clone();
            let sig_strat = sig.strategy_id.clone();
            self.store
                .signal(&sig)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            if let Err(e) = self.handle_signal(sig).await {
                self.stats.rejected_orders += 1;
                let reason = match &e {
                    RuntimeError::NoOption => "NO_OPTION".to_string(),
                    RuntimeError::NewsRisk => "NEWS_RISK".to_string(),
                    RuntimeError::Market(msg) => format!("MARKET_{msg}"),
                    RuntimeError::Execution(msg) => format!("EXECUTION_{msg}"),
                    _ => e.to_string(),
                };
                println!(
                    "SIGNAL_REJECTED symbol={} strategy_id={} reason={}",
                    sig_symbol, sig_strat, reason
                );
                self.store
                    .event(
                        "SIGNAL_REJECTED",
                        &serde_json::json!({
                            "symbol": sig_symbol,
                            "strategy_id": sig_strat,
                            "reason": reason,
                            "error": e.to_string(),
                        }),
                    )
                    .map_err(|x| RuntimeError::Storage(x.to_string()))?;
            }
        }
        Ok(())
    }
    async fn manage_open_trade(
        &mut self,
        underlying: &str,
        _state: &th_domain::MarketState,
    ) -> Result<(), RuntimeError> {
        let keys = self
            .open_trades
            .iter()
            .filter(|(_, t)| t.underlying == underlying)
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let Some(t) = self.open_trades.get(&key).cloned() else {
                continue;
            };
            let chain = self
                .provider
                .option_chain(&t.underlying, Utc::now())
                .await
                .map_err(|e| RuntimeError::Market(e.to_string()))?;
            let Some(q) = chain.quotes.iter().find(|q| {
                q.symbol == t.symbol && q.is_tradeable(Utc::now(), self.cfg.max_quote_age_secs)
            }) else {
                continue;
            };
            let mark = q.bid;
            let ret = mark / t.entry_price - 1.0;
            let age = (Utc::now() - t.entry_ts).num_minutes();

            // Check liveness of the broker-side protective order.
            // If it is still working at the broker, the stop-loss leg is
            // protected broker-side; skip the local stop-loss check for
            // this trade (take-profit and time-limit checks still run).
            // If it is missing/cancelled/rejected, re-submit it.
            let protective_order_working = if let Some(ref pid) = t.protective_order_id {
                match self.execution.broker_ref().get_order(pid).await {
                    Ok(pbo) => {
                        if matches!(
                            pbo.status,
                            th_domain::OrderStatus::New
                                | th_domain::OrderStatus::Accepted
                                | th_domain::OrderStatus::PartiallyFilled
                        ) {
                            true // Still working — broker will execute the stop
                        } else {
                            // Terminal or unknown state — need to re-submit
                            println!(
                                "PROTECTIVE_ORDER_TERMINAL symbol={} broker_order_id={} status={:?} will_resubmit=true",
                                t.symbol, pid, pbo.status
                            );
                            false
                        }
                    }
                    Err(e) => {
                        println!(
                            "PROTECTIVE_ORDER_CHECK_FAILED symbol={} broker_order_id={} error={}",
                            t.symbol, pid, e
                        );
                        // Treat as not working; fall through to process-level check.
                        false
                    }
                }
            } else {
                false
            };

            // If the protective order is no longer working, try to re-submit it.
            if !protective_order_working && t.protective_order_id.is_some() {
                let sl_price = (t.entry_price * (1.0 - t.stop_loss_pct)).max(0.01);
                let reprotect = OrderIntent {
                    client_order_id: Uuid::new_v4(),
                    symbol: t.symbol.clone(),
                    side: OrderSide::Sell,
                    qty: t.qty,
                    limit_price: Some(sl_price),
                    reduce_only: true,
                    strategy_id: t.strategy_id.clone(),
                    created_at: Utc::now(),
                    order_hash: String::new(),
                    bot_id: Some(t.strategy_id.clone()),
                    session_id: self.current_session_id.clone(),
                    decision_id: None,
                    oms_state: Some(th_domain::OmsState::IntentCreated),
                    option_action: Some(OptionOrderAction::SellToClose),
                };
                match self.execution.broker_ref().submit(&reprotect).await {
                    Ok(pbo) => {
                        println!(
                            "PROTECTIVE_ORDER_RESUBMITTED symbol={} broker_order_id={} sl_price={:.2}",
                            t.symbol, pbo.broker_order_id, sl_price
                        );
                        if let Some(trade) = self.open_trades.get_mut(&key) {
                            trade.protective_order_id = Some(pbo.broker_order_id.clone());
                            let _ = self.store.open_trade(&OpenTradeRecord {
                                symbol: trade.symbol.clone(),
                                underlying: trade.underlying.clone(),
                                strategy_id: trade.strategy_id.clone(),
                                entry_price: trade.entry_price,
                                entry_ts: trade.entry_ts.to_rfc3339(),
                                stop_loss_pct: trade.stop_loss_pct,
                                take_profit_pct: trade.take_profit_pct,
                                qty: trade.qty,
                                protective_order_id: trade.protective_order_id.clone(),
                            });
                        }
                    }
                    Err(e) => {
                        println!(
                            "PROTECTIVE_ORDER_RESUBMIT_FAILED symbol={} sl_price={:.2} error={}",
                            t.symbol, sl_price, e
                        );
                    }
                }
            }

            // Process-level exit: handles take-profit and time-limit unconditionally.
            // Also handles stop-loss when the protective order is not working.
            let stop_loss_triggered = !protective_order_working && ret <= -t.stop_loss_pct;
            if stop_loss_triggered
                || (t.take_profit_pct > 0.0 && ret >= t.take_profit_pct)
                || age >= self.cfg.bot_max_hold_minutes as i64
            {
                let broker_positions = self
                    .execution
                    .positions()
                    .await
                    .map_err(|e| RuntimeError::Execution(e.to_string()))?;
                let acct = self
                    .execution
                    .broker()
                    .await
                    .map_err(|e| RuntimeError::Execution(e.to_string()))?;
                let portfolio = PortfolioRisk {
                    cash: acct.cash,
                    realized_today: self.daily_realized,
                    positions: broker_positions,
                };
                let mut order = OrderIntent {
                    client_order_id: Uuid::new_v4(),
                    symbol: t.symbol.clone(),
                    side: OrderSide::Sell,
                    qty: t.qty,
                    limit_price: Some(mark),
                    reduce_only: true,
                    strategy_id: t.strategy_id.clone(),
                    created_at: Utc::now(),
                    order_hash: String::new(),
                    bot_id: Some(t.strategy_id.clone()),
                    session_id: self.current_session_id.clone(),
                    decision_id: None,
                    oms_state: Some(th_domain::OmsState::IntentCreated),
                    option_action: Some(OptionOrderAction::SellToClose),
                };
                order.order_hash = order_hash(&order);
                let spread = q.spread_bps();
                if let Some((broker_id, status)) = self
                    .store
                    .idempotency_status(order.client_order_id)
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?
                {
                    if broker_id.is_some() && status != "FAILED" {
                        continue;
                    }
                } else if !self
                    .store
                    .reserve_order(&order)
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?
                {
                    continue;
                }
                let (bo, _) = match self
                    .execution
                    .execute(order.clone(), mark, spread, &portfolio)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        self.store
                            .set_idempotency(order.client_order_id, "", "FAILED")
                            .map_err(|x| RuntimeError::Storage(x.to_string()))?;
                        return Err(RuntimeError::Execution(e.to_string()));
                    }
                };
                self.store
                    .set_idempotency(
                        order.client_order_id,
                        &bo.broker_order_id,
                        &format!("{:?}", bo.status),
                    )
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                    event_id: None,
                    timestamp: Utc::now(),
                    event_kind: "SELL_SUBMITTED".into(),
                    bot_id: t.strategy_id.clone(),
                    strategy_id: t.strategy_id.clone(),
                    option_symbol: t.symbol.clone(),
                    quantity: order.qty,
                    entry_price: Some(t.entry_price),
                    exit_price: Some(mark),
                    realized_pnl: None,
                    risk_pct: t.stop_loss_pct,
                    capital_allocated: t.entry_price * t.qty as f64 * 100.0,
                    rl_decision: Some("SellToClose".into()),
                    rl_confidence: Some(1.0),
                    execution_status: format!("{:?}", bo.status),
                    broker_order_id: Some(bo.broker_order_id.clone()),
                    payload: serde_json::json!({"mark": mark, "status": format!("{:?}", bo.status)}),
                });
                if let Some(fp) = bo.filled_avg_price {
                    let pnl = (fp - t.entry_price) * bo.filled_qty as f64 * 100.0;
                    self.daily_realized += pnl;
                    self.store
                        .checkpoint(
                            &format!("daily_realized:{}", self.daily_key),
                            &self.daily_realized.to_string(),
                        )
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    self.stats.realized_pnl += pnl;
                    self.stats.trades_closed += 1;
                    let fill = th_domain::Fill {
                        fill_id: Uuid::new_v4(),
                        client_order_id: order.client_order_id,
                        broker_order_id: bo.broker_order_id.clone(),
                        symbol: order.symbol.clone(),
                        side: order.side,
                        qty: bo.filled_qty,
                        price: fp,
                        fee: 0.0,
                        ts: Utc::now(),
                    };
                    self.store
                        .fill(&fill)
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                        event_id: None,
                        timestamp: Utc::now(),
                        event_kind: "SELL_FILLED".into(),
                        bot_id: t.strategy_id.clone(),
                        strategy_id: t.strategy_id.clone(),
                        option_symbol: t.symbol.clone(),
                        quantity: bo.filled_qty,
                        entry_price: Some(t.entry_price),
                        exit_price: Some(fp),
                        realized_pnl: Some(pnl),
                        risk_pct: t.stop_loss_pct,
                        capital_allocated: t.entry_price * t.qty as f64 * 100.0,
                        rl_decision: Some("SellFilled".into()),
                        rl_confidence: Some(1.0),
                        execution_status: "Filled".into(),
                        broker_order_id: Some(bo.broker_order_id.clone()),
                        payload: serde_json::json!({"fill_price": fp, "pnl": pnl}),
                    });
                    let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                        event_id: None,
                        timestamp: Utc::now(),
                        event_kind: "POSITION_CLOSED".into(),
                        bot_id: t.strategy_id.clone(),
                        strategy_id: t.strategy_id.clone(),
                        option_symbol: t.symbol.clone(),
                        quantity: bo.filled_qty,
                        entry_price: Some(t.entry_price),
                        exit_price: Some(fp),
                        realized_pnl: Some(pnl),
                        risk_pct: t.stop_loss_pct,
                        capital_allocated: t.entry_price * t.qty as f64 * 100.0,
                        rl_decision: Some("Closed".into()),
                        rl_confidence: Some(1.0),
                        execution_status: "PositionClosed".into(),
                        broker_order_id: Some(bo.broker_order_id.clone()),
                        payload: serde_json::json!({"reason": "exit_condition"}),
                    });
                    let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                        event_id: None,
                        timestamp: Utc::now(),
                        event_kind: "P&L_CALCULATED".into(),
                        bot_id: t.strategy_id.clone(),
                        strategy_id: t.strategy_id.clone(),
                        option_symbol: t.symbol.clone(),
                        quantity: bo.filled_qty,
                        entry_price: Some(t.entry_price),
                        exit_price: Some(fp),
                        realized_pnl: Some(pnl),
                        risk_pct: t.stop_loss_pct,
                        capital_allocated: t.entry_price * t.qty as f64 * 100.0,
                        rl_decision: Some("PnlCalculated".into()),
                        rl_confidence: Some(1.0),
                        execution_status: "PnlCalculated".into(),
                        broker_order_id: Some(bo.broker_order_id.clone()),
                        payload: serde_json::json!({"realized_pnl": pnl, "daily_realized": self.daily_realized}),
                    });
                    let trade = TradeRecord {
                        trade_id: format!("TRADE-{}", order.client_order_id),
                        symbol: t.underlying.clone(),
                        strategy_id: t.strategy_id.clone(),
                        session_id: self.current_session_id.clone().unwrap_or_default(),
                        entry: t.entry_ts,
                        exit: Some(Utc::now()),
                        pnl,
                        fees: 0.0,
                        reason: if ret <= -t.stop_loss_pct {
                            "stop_loss".into()
                        } else if t.take_profit_pct > 0.0 && ret >= t.take_profit_pct {
                            "take_profit".into()
                        } else {
                            "time_limit".into()
                        },
                        signal_price: Some(t.entry_price),
                        quote_spread_bps: None,
                        entry_fill_price: Some(t.entry_price),
                        exit_fill_price: Some(fp),
                        slippage_bps: Some(
                            ((fp - mark).abs() / mark.max(1e-9) * 10000.0).clamp(0.0, 1000.0),
                        ),
                        latency_ms: Some(10),
                        regime_at_entry: None,
                    };
                    // Online Q-table update: immediately reflect this trade outcome.
                    if let Some(q) = self.q_policy.as_mut() {
                        if let Some(bars) = self.bars.get(&t.underlying) {
                            let cur_state = th_hive::StateKey::from_state(&classify_regime(bars));
                            let action = if pnl > 0.0 { "Buy" } else { "Hold" };
                            let experience = th_hive::Experience {
                                state: cur_state.clone(),
                                action: action.to_string(),
                                reward: pnl.clamp(-1.0, 1.0),
                                next_state: cur_state,
                                terminal: true,
                                decision_ts: t.entry_ts,
                                outcome_ts: Utc::now(),
                            };
                            q.update(&experience);
                        }
                    }
                    self.experience.record_trade(trade.clone());
                    let autopsy = self.experience.autopsy(&trade);
                    self.store
                        .event_keyed(
                            &format!("TRADE_AUTOPSY:{}", trade.trade_id),
                            "TRADE_AUTOPSY",
                            &serde_json::json!({"trade":trade,"autopsy":autopsy}),
                        )
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    println!(
                        "POSITION_CLOSED symbol={} underlying={} strategy_id={} exit_price={:.2} pnl={:.2} reason={}",
                        t.symbol, t.underlying, t.strategy_id, fp, pnl, trade.reason
                    );
                    self.store
                        .update_order_status(
                            order.client_order_id,
                            Some(&bo.broker_order_id),
                            &format!("{:?}", bo.status),
                        )
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    if bo.filled_qty >= t.qty {
                        self.open_trades.remove(&key);
                        self.store
                            .delete_open_trade(&key)
                            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    } else if bo.filled_qty > 0 {
                        let mut remaining = t.clone();
                        remaining.qty -= bo.filled_qty;
                        self.open_trades.insert(key.clone(), remaining.clone());
                        self.store
                            .open_trade(&OpenTradeRecord {
                                symbol: remaining.symbol.clone(),
                                underlying: remaining.underlying.clone(),
                                strategy_id: remaining.strategy_id.clone(),
                                entry_price: remaining.entry_price,
                                entry_ts: remaining.entry_ts.to_rfc3339(),
                                stop_loss_pct: remaining.stop_loss_pct,
                                take_profit_pct: remaining.take_profit_pct,
                                qty: remaining.qty,
                                protective_order_id: remaining.protective_order_id.clone(),
                            })
                            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                    }
                }
            }
        }
        Ok(())
    }
    async fn handle_signal(&mut self, sig: th_domain::Signal) -> Result<(), RuntimeError> {
        let _history = self
            .bars
            .get(&sig.symbol)
            .ok_or(RuntimeError::InsufficientData)?;
        let plan = self
            .bot_plans
            .values()
            .find(|p| {
                let session_match = match &self.current_session_id {
                    Some(curr) if !p.session_id.is_empty() => curr == &p.session_id,
                    _ => false,
                };
                session_match && p.strategy_id == sig.strategy_id && p.underlying == sig.symbol
            })
            .cloned()
            .ok_or(RuntimeError::NoOption)?;
        let chain = self
            .provider
            .option_chain(&sig.symbol, Utc::now())
            .await
            .map_err(|e| RuntimeError::Market(e.to_string()))?;
        let wanted_type = match sig.side {
            th_domain::SignalSide::LongCall => th_domain::OptionType::Call,
            th_domain::SignalSide::LongPut => th_domain::OptionType::Put,
            th_domain::SignalSide::Flat => return Ok(()),
        };
        let now = Utc::now();
        if !self.market_session_phase(now).allows_new_entries() {
            println!("MARKET_NOT_OPEN phase={:?}", self.market_session_phase(now));
            return Err(RuntimeError::Market("MARKET_NOT_OPEN".into()));
        }
        let expiry_policy = th_domain::OptionExpiryPolicy::new(
            plan.min_expiry_minutes.max(180),
            if plan.max_expiry_minutes == 0 || plan.max_expiry_minutes == u32::MAX {
                None
            } else {
                Some(plan.max_expiry_minutes)
            },
        );
        let quote = chain
            .quotes
            .iter()
            .filter(|q| {
                q.underlying == sig.symbol
                    && q.option_type == wanted_type
                    && q.is_tradeable(now, self.cfg.max_quote_age_secs)
                    && expiry_policy.is_valid_expiry(now, q.expiry)
            })
            .min_by(|a, b| {
                a.spread_bps()
                    .partial_cmp(&b.spread_bps())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or_else(|| {
                chain.quotes.iter().find(|q| {
                    (q.symbol == plan.option_symbol
                        || (q.underlying == sig.symbol && q.option_type == wanted_type))
                        && q.is_tradeable(now, self.cfg.max_quote_age_secs)
                        && expiry_policy.is_valid_expiry(now, q.expiry)
                })
            })
            .cloned()
            .ok_or(RuntimeError::NoOption)?;

        println!(
            "OPTION_SELECTED symbol={} contract_symbol={} strike={:.2} expiry={} bid={:.2} ask={:.2} spread_bps={:.1}",
            sig.symbol, quote.symbol, quote.strike, quote.expiry, quote.bid, quote.ask, quote.spread_bps()
        );
        let news = self
            .provider
            .news(
                &sig.symbol,
                Utc::now() - chrono::Duration::minutes(30),
                Utc::now(),
            )
            .await
            .map_err(|e| RuntimeError::Market(e.to_string()))?;
        match classify_news_risk(&news) {
            NewsRisk::Block => return Err(RuntimeError::NewsRisk),
            NewsRisk::Elevated => self
                .store
                .event(
                    "NEWS_RISK_ELEVATED",
                    &serde_json::json!({"symbol":sig.symbol,"count":news.len()}),
                )
                .map_err(|e| RuntimeError::Storage(e.to_string()))?,
            NewsRisk::None => {}
        }
        if self.open_trades.contains_key(&quote.symbol) {
            return Ok(());
        }
        if !quote.is_tradeable(Utc::now(), self.cfg.max_quote_age_secs) {
            return Err(RuntimeError::NoOption);
        }
        let px = quote.ask;
        let sl_pct = if self.cfg.stop_loss_pct > 0.0 {
            self.cfg.stop_loss_pct
        } else {
            return Err(RuntimeError::InvalidConfig(
                "stop_loss_pct must be strictly positive".into(),
            ));
        };
        let positions = self
            .execution
            .positions()
            .await
            .map_err(|e| RuntimeError::Execution(e.to_string()))?;
        let acct = self
            .execution
            .broker()
            .await
            .map_err(|e| RuntimeError::Execution(e.to_string()))?;
        let portfolio = PortfolioRisk {
            cash: acct.cash,
            realized_today: self.daily_realized,
            positions,
        };

        // Compute volatility/ATR from bar history or option implied volatility
        let volatility_atr = if let Some(bars) = self.bars.get(&sig.symbol) {
            if bars.len() >= 15 {
                let current_close = bars.last().map(|b| b.close).unwrap_or(1.0);
                th_strategy::atr(bars, 14)
                    .map(|a| a / current_close.max(1e-6))
                    .unwrap_or(quote.iv)
            } else {
                quote.iv
            }
        } else {
            quote.iv
        };

        let safety_ceiling_qty = std::env::var("RISK_POSITION_SAFETY_CEILING")
            .or_else(|_| std::env::var("RISK_MAX_SINGLE_POSITION_QTY"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.execution.risk().limits().max_single_position_qty);
        self.execution
            .risk_mut()
            .register_strategy_risk(th_risk::StrategyRiskAllocation {
                strategy_id: plan.strategy_id.clone(),
                risk_pct: plan.risk_pct,
                capital_allocated: plan.capital_allocated,
                risk_budget: plan.risk_budget,
            });
        let max_trade_risk_pct = self.execution.risk().limits().max_trade_risk_pct;
        let max_portfolio_risk_pct = self.execution.risk().limits().max_portfolio_risk_pct;
        let ceiling_action = CeilingAction::from_env();

        let sizing_inputs = DynamicSizingInputs {
            account_equity: acct.equity,
            available_buying_power: acct.buying_power,
            option_ask: px,
            stop_loss_pct: sl_pct,
            multiplier: th_domain::CONTRACT_MULTIPLIER,
            strategy_confidence: sig.strength,
            volatility_atr,
            max_trade_risk_pct,
            max_portfolio_risk_pct,
            current_portfolio_risk: portfolio.total_notional() * sl_pct,
            plan_risk_budget: plan.risk_budget,
            plan_capital_allocated: if plan.capital_allocated > 0.0 {
                plan.capital_allocated
                    .min(self.execution.risk().limits().max_order_notional)
            } else {
                self.execution.risk().limits().max_order_notional
            },
            safety_ceiling_qty,
            ceiling_action,
        };

        let sizing = calculate_dynamic_risk_quantity(&sizing_inputs)?;

        println!(
            "DYNAMIC_SIZING equity={:.2} buying_power={:.2} ask={:.2} premium={:.2} atr={:.4} stop_dist={:.2} risk_budget={:.2} confidence={:.2} calculated_qty={} final_qty={} action={:?} reason=\"{}\"",
            sizing.account_equity,
            sizing.available_buying_power,
            sizing.instrument_price,
            sizing.contract_cost,
            sizing.volatility_atr,
            sizing.stop_distance,
            sizing.risk_budget,
            sizing.strategy_confidence,
            sizing.calculated_quantity,
            sizing.final_quantity,
            sizing.action_taken,
            sizing.reason
        );

        if sizing.action_taken == SizingAction::ResizedToCeiling {
            println!(
                "DYNAMIC_SIZING_RESIZED symbol={} calculated_qty={} final_qty={} ceiling={} reason=\"{}\"",
                sig.symbol, sizing.calculated_quantity, sizing.final_quantity, sizing.safety_ceiling, sizing.reason
            );
            let _ = self.store.event(
                "SIZING_RESIZED",
                &serde_json::json!({
                    "symbol": sig.symbol,
                    "calculated_qty": sizing.calculated_quantity,
                    "final_qty": sizing.final_quantity,
                    "ceiling": sizing.safety_ceiling,
                    "reason": sizing.reason
                }),
            );
        }

        let qty = sizing.final_quantity;
        if qty == 0 {
            println!(
                "DYNAMIC_SIZING_REJECTED symbol={} calculated_qty={} final_qty=0 reason=\"{}\"",
                sig.symbol, sizing.calculated_quantity, sizing.reason
            );
            let _ = self.store.event(
                "SIZING_REJECTED",
                &serde_json::json!({
                    "symbol": sig.symbol,
                    "calculated_qty": sizing.calculated_quantity,
                    "reason": sizing.reason
                }),
            );
            return Err(RuntimeError::NoOption);
        }
        println!(
            "RISK_APPROVED symbol={} contract_symbol={} qty={} limit_price={:.2}",
            sig.symbol, quote.symbol, qty, px
        );
        println!(
            "ORDER_REQUESTED symbol={} option_symbol={} qty={} limit_price={:.2} strategy_id={} strength={:.2}",
            sig.symbol, quote.symbol, qty, px, sig.strategy_id, sig.strength
        );
        let mut order = OrderIntent {
            client_order_id: Uuid::new_v4(),
            symbol: quote.symbol.clone(),
            side: OrderSide::Buy,
            qty,
            limit_price: Some(px),
            reduce_only: false,
            strategy_id: sig.strategy_id.clone(),
            created_at: Utc::now(),
            order_hash: String::new(),
            bot_id: sig.bot_id.clone(),
            session_id: sig
                .session_id
                .clone()
                .or_else(|| self.current_session_id.clone()),
            decision_id: Some(sig.id),
            oms_state: Some(th_domain::OmsState::IntentCreated),
            option_action: Some(OptionOrderAction::BuyToOpen),
        };
        order.order_hash = order_hash(&order);
        println!(
            "ORDER_INTENT_CREATED client_order_id={} symbol={} action={:?} qty={} limit_price={:.2}",
            order.client_order_id, order.symbol, order.option_action, order.qty, px
        );
        if let Some((broker_id, status)) = self
            .store
            .idempotency_status(order.client_order_id)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            if broker_id.is_some() && status != "FAILED" {
                return Ok(());
            }
        } else if !self
            .store
            .reserve_order(&order)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
        {
            return Ok(());
        }
        let (bo, _approval) = match self
            .execution
            .execute(order.clone(), px, quote.spread_bps(), &portfolio)
            .await
        {
            Ok(v) => {
                println!(
                    "RISK_ACCEPTED token={} order_hash={} notional={:.2}",
                    v.1.token,
                    v.1.order_hash,
                    px * qty as f64 * th_domain::CONTRACT_MULTIPLIER
                );
                v
            }
            Err(e) => {
                println!("RISK_REJECTED reason={}", e);
                self.store
                    .set_idempotency(order.client_order_id, "", "FAILED")
                    .map_err(|x| RuntimeError::Storage(x.to_string()))?;
                return Err(RuntimeError::Execution(e.to_string()));
            }
        };
        println!(
            "ORDER_SUBMITTED client_order_id={} broker_order_id={} status={:?} symbol={} qty={}",
            order.client_order_id, bo.broker_order_id, bo.status, order.symbol, order.qty
        );
        match bo.status {
            OrderStatus::Accepted | OrderStatus::New => {
                println!(
                    "ORDER_ACCEPTED client_order_id={} broker_order_id={}",
                    order.client_order_id, bo.broker_order_id
                );
            }
            OrderStatus::Rejected | OrderStatus::Cancelled => {
                println!(
                    "ORDER_REJECTED client_order_id={} broker_order_id={} status={:?}",
                    order.client_order_id, bo.broker_order_id, bo.status
                );
            }
            _ => {}
        }
        self.store
            .set_idempotency(
                order.client_order_id,
                &bo.broker_order_id,
                &format!("{:?}", bo.status),
            )
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        self.store
            .order(
                &order,
                Some(&bo.broker_order_id),
                Some(&format!("{:?}", bo.status)),
            )
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
            event_id: None,
            timestamp: Utc::now(),
            event_kind: "BUY_SUBMITTED".into(),
            bot_id: plan.bot_id.clone(),
            strategy_id: sig.strategy_id.clone(),
            option_symbol: quote.symbol.clone(),
            quantity: qty,
            entry_price: Some(px),
            exit_price: None,
            realized_pnl: None,
            risk_pct: plan.risk_pct,
            capital_allocated: plan.capital_allocated,
            rl_decision: Some("BuyToOpen".into()),
            rl_confidence: Some(sig.strength),
            execution_status: format!("{:?}", bo.status),
            broker_order_id: Some(bo.broker_order_id.clone()),
            payload: serde_json::json!({"status": format!("{:?}", bo.status)}),
        });
        if bo.filled_qty > 0 {
            if bo.filled_qty < order.qty {
                println!(
                    "ORDER_PARTIALLY_FILLED client_order_id={} broker_order_id={} filled_qty={} remaining={}",
                    order.client_order_id, bo.broker_order_id, bo.filled_qty, order.qty - bo.filled_qty
                );
            } else {
                println!(
                    "ORDER_FILLED client_order_id={} broker_order_id={} filled_qty={}",
                    order.client_order_id, bo.broker_order_id, bo.filled_qty
                );
            }
            if let Some(fp) = bo.filled_avg_price {
                let fill = th_domain::Fill {
                    fill_id: Uuid::new_v4(),
                    client_order_id: order.client_order_id,
                    broker_order_id: bo.broker_order_id.clone(),
                    symbol: order.symbol.clone(),
                    side: order.side,
                    qty: bo.filled_qty,
                    price: fp,
                    fee: 0.0,
                    ts: Utc::now(),
                };
                self.store
                    .fill(&fill)
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                    event_id: None,
                    timestamp: Utc::now(),
                    event_kind: "BUY_FILLED".into(),
                    bot_id: plan.bot_id.clone(),
                    strategy_id: sig.strategy_id.clone(),
                    option_symbol: quote.symbol.clone(),
                    quantity: bo.filled_qty,
                    entry_price: Some(fp),
                    exit_price: None,
                    realized_pnl: None,
                    risk_pct: plan.risk_pct,
                    capital_allocated: plan.capital_allocated,
                    rl_decision: Some("Filled".into()),
                    rl_confidence: Some(sig.strength),
                    execution_status: "Filled".into(),
                    broker_order_id: Some(bo.broker_order_id.clone()),
                    payload: serde_json::json!({"filled_qty": bo.filled_qty, "avg_price": fp}),
                });
                println!(
                    "FILL_RECEIVED fill_id={} broker_order_id={} symbol={} qty={} price={:.2}",
                    fill.fill_id, bo.broker_order_id, order.symbol, bo.filled_qty, fp
                );
                // Compute the stop-loss limit price from the actual fill price
                // (not the ask — the fill price is the authoritative cost basis).
                // NOTE: Alpaca does not support OTO/bracket/stop orders for options;
                // we must submit a separate sell-to-close limit order post-fill.
                let sl_price = (fp * (1.0 - self.cfg.stop_loss_pct)).max(0.01);
                let protective_intent = OrderIntent {
                    client_order_id: Uuid::new_v4(),
                    symbol: order.symbol.clone(),
                    side: OrderSide::Sell,
                    qty: bo.filled_qty,
                    limit_price: Some(sl_price),
                    reduce_only: true,
                    strategy_id: order.strategy_id.clone(),
                    created_at: Utc::now(),
                    order_hash: String::new(),
                    bot_id: order.bot_id.clone(),
                    session_id: order.session_id.clone(),
                    decision_id: None,
                    oms_state: Some(th_domain::OmsState::IntentCreated),
                    option_action: Some(OptionOrderAction::SellToClose),
                };
                let protective_broker_id = match self
                    .execution
                    .broker_ref()
                    .submit(&protective_intent)
                    .await
                {
                    Ok(pbo) => {
                        println!(
                            "PROTECTIVE_ORDER_SUBMITTED symbol={} broker_order_id={} limit_price={:.2} qty={}",
                            order.symbol, pbo.broker_order_id, sl_price, bo.filled_qty
                        );
                        Some(pbo.broker_order_id)
                    }
                    Err(e) => {
                        // Soft failure: the process-level manage_open_trade exit loop
                        // remains the safety net. The position is not abandoned.
                        println!(
                            "PROTECTIVE_ORDER_FAILED symbol={} sl_price={:.2} error={}",
                            order.symbol, sl_price, e
                        );
                        None
                    }
                };
                let opened = OpenTrade {
                    symbol: order.symbol.clone(),
                    underlying: sig.symbol.clone(),
                    strategy_id: order.strategy_id.clone(),
                    entry_price: fp,
                    entry_ts: Utc::now(),
                    stop_loss_pct: self.cfg.stop_loss_pct,
                    take_profit_pct: self.cfg.take_profit_pct,
                    qty: bo.filled_qty,
                    protective_order_id: protective_broker_id,
                };
                self.store
                    .open_trade(&OpenTradeRecord {
                        symbol: opened.symbol.clone(),
                        underlying: opened.underlying.clone(),
                        strategy_id: opened.strategy_id.clone(),
                        entry_price: opened.entry_price,
                        entry_ts: opened.entry_ts.to_rfc3339(),
                        stop_loss_pct: opened.stop_loss_pct,
                        take_profit_pct: opened.take_profit_pct,
                        qty: opened.qty,
                        protective_order_id: opened.protective_order_id.clone(),
                    })
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                self.open_trades
                    .insert(order.symbol.clone(), opened.clone());
                self.stats.trades_opened += 1;
                println!(
                    "POSITION_OPENED symbol={} underlying={} strategy_id={} qty={} entry_price={:.2} protective_order={}",
                    opened.symbol, opened.underlying, opened.strategy_id, opened.qty, opened.entry_price,
                    opened.protective_order_id.as_deref().unwrap_or("none")
                );
                let _ = self.store.record_feedback(&ExecutionFeedbackRecord {
                    event_id: None,
                    timestamp: Utc::now(),
                    event_kind: "POSITION_OPENED".into(),
                    bot_id: plan.bot_id.clone(),
                    strategy_id: sig.strategy_id.clone(),
                    option_symbol: quote.symbol.clone(),
                    quantity: bo.filled_qty,
                    entry_price: Some(fp),
                    exit_price: None,
                    realized_pnl: None,
                    risk_pct: plan.risk_pct,
                    capital_allocated: plan.capital_allocated,
                    rl_decision: Some("PositionActive".into()),
                    rl_confidence: Some(sig.strength),
                    execution_status: "PositionOpened".into(),
                    broker_order_id: Some(bo.broker_order_id.clone()),
                    payload: serde_json::json!({"stop_loss": sl_pct, "take_profit": self.cfg.take_profit_pct}),
                });
            }
        }
        Ok(())
    }
    pub async fn reconcile(&mut self) -> Result<bool, RuntimeError> {
        let broker = self
            .execution
            .positions()
            .await
            .map_err(|e| RuntimeError::Execution(e.to_string()))?;
        let internal = self
            .open_trades
            .values()
            .map(|t| th_domain::Position {
                symbol: t.symbol.clone(),
                qty: t.qty as i32,
                avg_price: t.entry_price,
                mark: t.entry_price,
                opened_at: t.entry_ts,
                contract: th_domain::OptionContract::from_occ(&t.symbol),
            })
            .collect::<Vec<_>>();
        // Crash-consistent in-flight order reconciliation
        if let Ok(reserved) = self.store.load_reserved_orders() {
            for cid in reserved {
                match self
                    .execution
                    .broker_ref()
                    .find_by_client_order_id(cid)
                    .await
                {
                    Ok(Some(bo)) => {
                        let _ = self.store.set_idempotency(
                            cid,
                            &bo.broker_order_id,
                            &format!("{:?}", bo.status),
                        );
                        println!(
                            "ORDER_RECONCILED client_order_id={} broker_id={} status={:?}",
                            cid, bo.broker_order_id, bo.status
                        );
                    }
                    Ok(None) => {
                        let _ = self.store.set_idempotency(cid, "", "FAILED");
                        println!("ORDER_RECONCILED_FAILED client_order_id={}", cid);
                    }
                    Err(e) => {
                        println!(
                            "ORDER_RECONCILIATION_ERROR client_order_id={} error={}",
                            cid, e
                        );
                    }
                }
            }
        }

        let report = reconcile_positions(&internal, &broker);
        self.store
            .event("RECONCILIATION", &report)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        if !report.matched {
            self.active = false;
            self.health = RuntimeHealth::Degraded;
            self.execution.risk_mut().engage_kill_switch();
            println!(
                "POSITION_MISMATCH missing_internal={:?} missing_broker={:?}",
                report.missing_internal, report.missing_broker
            );
            println!("KILL_SWITCH_ENGAGED reason=reconciliation_mismatch");
        } else {
            println!(
                "POSITION_RECONCILED count={} broker_count={}",
                report.internal_count, report.broker_count
            );
        }
        Ok(report.matched)
    }
    fn prior_q_from_active(&self) -> Option<QLearning> {
        if let Ok(Some(latest)) = self.json_history.latest_rl_session() {
            if let Ok(entries) = serde_json::from_value::<Vec<th_hive::QEntry>>(
                serde_json::Value::Array(latest.q_table_snapshot),
            ) {
                if !entries.is_empty() {
                    return Some(QLearning::from_entries(&entries));
                }
            }
        }
        self.store
            .active_config()
            .ok()
            .flatten()
            .and_then(|(_, payload)| serde_json::from_str::<AnalysisBundle>(&payload).ok())
            .and_then(|b| {
                b.symbols
                    .first()
                    .map(|s| QLearning::from_entries(&s.report.q_table))
            })
    }
    fn manufacturing_policy_from_env() -> Result<HiveManufacturingPolicy, RuntimeError> {
        let total = std::env::var("HIVE_TOTAL_CAPITAL")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_TOTAL_CAPITAL missing".into()))?
            .parse::<f64>()
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_TOTAL_CAPITAL invalid".into()))?;
        let max_bots = std::env::var("HIVE_MAX_BOTS")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_MAX_BOTS missing".into()))?
            .parse::<usize>()
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_MAX_BOTS invalid".into()))?;
        let max_bots_per_symbol = std::env::var("HIVE_MAX_BOTS_PER_SYMBOL")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_MAX_BOTS_PER_SYMBOL missing".into()))?
            .parse::<usize>()
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_MAX_BOTS_PER_SYMBOL invalid".into()))?;
        let max_symbol_capital_pct = std::env::var("HIVE_MAX_SYMBOL_CAPITAL_PCT")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_MAX_SYMBOL_CAPITAL_PCT missing".into()))?
            .parse::<f64>()
            .map_err(|_| {
                RuntimeError::InvalidConfig("HIVE_MAX_SYMBOL_CAPITAL_PCT invalid".into())
            })?;
        let risk_fraction = std::env::var("HIVE_RISK_FRACTION")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_RISK_FRACTION missing".into()))?
            .parse::<f64>()
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_RISK_FRACTION invalid".into()))?;
        let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
        Ok(HiveManufacturingPolicy {
            total_capital: total,
            max_bots,
            max_bots_per_symbol,
            max_symbol_capital_pct,
            risk_fraction,
            min_expiry_minutes: expiry_policy.min_expiry_minutes,
            max_expiry_minutes: expiry_policy.max_expiry_minutes.unwrap_or(u32::MAX),
        })
    }
    pub fn stage_research_config(
        &self,
        report: &AnalysisBundle,
        now: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        // Research config is staged during pre-market analysis or while waiting for next session.
        let phase = self.market_session_phase(now);
        if phase != SessionPhase::PreMarket
            && phase != SessionPhase::WaitingForNextSession
            && phase != SessionPhase::Analysis
        {
            return Err(RuntimeError::WrongPhase);
        }
        if report.promoted.is_empty() {
            return Ok(false);
        }
        let payload =
            serde_json::to_string(report).map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let version = report
            .symbols
            .first()
            .map(|s| s.report.config_version.clone())
            .unwrap_or_else(|| format!("research-{}", now.timestamp()));
        self.store
            .save_config(&version, &payload, false)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        self.store
            .event("RESEARCH_CANDIDATE", report)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        Ok(true)
    }
    pub fn activate_research_config(
        &mut self,
        version: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        if self.cfg.phase_at(now) != SessionPhase::Analysis {
            return Err(RuntimeError::WrongPhase);
        }
        let start = current_analysis_start(&self.cfg, now);
        let d = research_deadline(&self.cfg, start);
        if !d.promotion_allowed(now) {
            return Ok(false);
        }
        let ok = self
            .store
            .activate_config(version)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        if ok {
            if let Some(payload) = self
                .store
                .config(version)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?
            {
                if let Ok(report) = serde_json::from_str::<AnalysisBundle>(&payload) {
                    let promoted = strategies_from_report(&report);
                    if !promoted.is_empty() {
                        self.strategies = promoted;
                    }
                }
            }
            self.cfg.config_version = version.to_string();
            self.store
                .event(
                    "CONFIG_ACTIVATED",
                    &serde_json::json!({"version":version,"at":now}),
                )
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        }
        Ok(ok)
    }
    pub fn run_analysis_window(&self, now: DateTime<Utc>) -> Result<AnalysisBundle, RuntimeError> {
        if self.cfg.phase_at(now) != SessionPhase::Analysis {
            return Err(RuntimeError::WrongPhase);
        }
        let histories = self
            .bars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .filter(|(_, v)| v.len() >= 60)
            .collect::<HashMap<_, _>>();
        if histories.is_empty() {
            return Err(RuntimeError::InsufficientData);
        }
        let trades = self
            .store
            .trade_records_since(&current_analysis_start(&self.cfg, now).to_rfc3339())
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        Ok(run_analysis_with_q_and_trades(
            histories,
            self.prior_q_from_active(),
            &trades,
        ))
    }
}

impl<B: Broker, P: MarketDataProvider> TradingRuntime<B, P> {
    pub async fn health_snapshot(
        &self,
        now: DateTime<Utc>,
    ) -> Result<HealthSnapshot, RuntimeError> {
        let broker_healthy = self.execution.broker().await.is_ok();
        let storage_healthy = self.store.health_check().is_ok();
        let data_healthy = DataHealth {
            last_event: self.stats.last_market_event,
            max_age_secs: self.cfg.max_quote_age_secs,
        }
        .healthy(now);
        Ok(HealthSnapshot {
            phase: self.phase_at(now),
            runtime_active: self.active,
            data_healthy,
            broker_healthy,
            storage_healthy,
            risk_killed: self.execution.is_killed(),
            open_trades: self.open_trades.len(),
        })
    }
    pub async fn select_trading_universe(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, RuntimeError> {
        let discovery_limit = std::env::var("HIVE_DISCOVERY_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(99)
            .clamp(1, 99);
        let universe_size = std::env::var("HIVE_UNIVERSE_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(self.cfg.max_universe_size.max(1));

        println!("UNIVERSE_DISCOVERY_STARTED discovery_limit={discovery_limit} universe_size={universe_size}");

        // Stage 1: Discover candidates from Alpaca most-actives screener.
        let candidates = match self.provider.most_actives(discovery_limit).await {
            Ok(syms) if !syms.is_empty() => {
                println!("UNIVERSE_DISCOVERY_COMPLETED candidates={}", syms.len());
                syms
            }
            Ok(_) => {
                return Err(RuntimeError::Market(
                    "most_actives returned empty list; failing closed".into(),
                ));
            }
            Err(e) => {
                return Err(RuntimeError::Market(format!(
                    "UNIVERSE_DISCOVERY_FAILED: {e}; failing closed"
                )));
            }
        };

        // Stage 2: Fetch bars for each candidate and rank by volume.
        println!("UNIVERSE_SELECTION_STARTED candidates={:?}", candidates);
        let mut liquid_symbols: Vec<(String, f64)> = Vec::new();
        let target_pool = (universe_size * 2).clamp(15, 30);
        for sym in &candidates {
            if liquid_symbols.len() >= target_pool {
                break;
            }
            let end = now;
            let start = end - chrono::Duration::days(5);
            match self.provider.bars(sym, start, end).await {
                Ok(bars) if !bars.is_empty() => {
                    let total_vol: f64 = bars.iter().map(|b| b.volume).sum();
                    self.bars.insert(sym.clone(), bars);
                    liquid_symbols.push((sym.clone(), total_vol));
                }
                Ok(_) => {
                    println!("UNIVERSE_CANDIDATE_SKIPPED symbol={sym} reason=NO_BARS");
                }
                Err(e) => {
                    println!("UNIVERSE_CANDIDATE_ERROR symbol={sym} error={e}");
                }
            }
        }

        if liquid_symbols.is_empty() {
            return Err(RuntimeError::Market(
                "No valid market data available for any candidate symbols; failing closed".into(),
            ));
        }

        liquid_symbols.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let selected: Vec<String> = liquid_symbols
            .into_iter()
            .take(universe_size)
            .map(|(s, _)| s)
            .collect();

        println!("UNIVERSE_DISCOVERED symbols={:?}", selected);
        println!("UNIVERSE_SELECTED symbols={:?}", selected);
        self.active_universe = selected.clone();
        Ok(selected)
    }

    pub async fn ensure_bots_manufactured(
        &mut self,
        symbols: &[String],
    ) -> Result<usize, RuntimeError> {
        let now = Utc::now();
        if !self.bot_plans.is_empty() {
            let session_valid = self.current_session_id.as_deref().is_none_or(|sid| {
                self.bot_plans
                    .values()
                    .all(|p| p.session_id.is_empty() || p.session_id == sid)
            });
            let universe_valid = symbols.is_empty()
                || self
                    .bot_plans
                    .values()
                    .any(|p| symbols.contains(&p.underlying));
            if session_valid && universe_valid {
                println!("HIVE_INITIALIZED bots={}", self.bot_plans.len());
                let seed_ids = th_strategy::StrategyRegistry::new().seed_ids();
                println!("SEED_LIBRARY_LOADED count={}", seed_ids.len());
                for plan in self.bot_plans.values() {
                    println!(
                        "BOT_STARTED bot_id={} strategy_id={} underlying={} capital={} risk_budget={}",
                        plan.bot_id,
                        plan.strategy_id,
                        plan.underlying,
                        plan.capital_allocated,
                        plan.risk_budget
                    );
                }
                return Ok(self.bot_plans.len());
            } else {
                self.fleet.retire_all();
                self.bot_plans.clear();
            }
        }

        println!("HIVE_INITIALIZED status=manufacturing_initial_bots");
        let seed_ids = th_strategy::StrategyRegistry::new().seed_ids();
        println!("SEED_LIBRARY_LOADED count={}", seed_ids.len());

        let mut histories = HashMap::new();
        for symbol in symbols {
            if let Some(h) = self.bars.get(symbol).filter(|h| h.len() >= 60) {
                histories.insert(symbol.clone(), h.clone());
                continue;
            }
            let end = now;
            let start = end - chrono::Duration::days(30);
            if let Ok(bs) = self.provider.bars(symbol, start, end).await {
                if bs.len() >= 60 {
                    self.bars.insert(symbol.clone(), bs.clone());
                    histories.insert(symbol.clone(), bs);
                }
            }
        }

        if histories.is_empty() {
            return Err(RuntimeError::Market(
                "Alpaca returned no historical market bars for universe; failing closed".into(),
            ));
        }

        let trade_records = self
            .store
            .trade_records_since(&(now - chrono::Duration::days(7)).to_rfc3339())
            .unwrap_or_default();
        let bundle = run_analysis_with_q_and_trades(
            histories.clone(),
            self.prior_q_from_active(),
            &trade_records,
        );

        let base_seeds = th_strategy::StrategyRegistry::new().seed_ids();
        let seed_before = self
            .json_history
            .latest_seed_snapshot()
            .unwrap_or_default()
            .unwrap_or_else(|| {
                base_seeds
                    .iter()
                    .map(|id| serde_json::json!({"strategy_id": id, "type": "seed"}))
                    .collect()
            });
        let mut seed_after = seed_before.clone();
        for sa in &bundle.symbols {
            if let Some(g) = &sa.report.generated_strategy {
                if g.validation.as_ref().map(|v| v.accepted).unwrap_or(false) {
                    seed_after.push(serde_json::json!({"strategy_id": g.blueprint.id, "type": "rl_promoted", "blueprint": g.blueprint}));
                }
            }
        }

        let _ = th_hive::persist_rl_history(
            &self.json_history,
            &bundle,
            serde_json::json!({"trade_records_used": trade_records.len()}),
            seed_before,
            seed_after,
            serde_json::json!({"symbols": histories.keys().collect::<Vec<_>>()}),
        );

        let mut chains = HashMap::new();
        for symbol in histories.keys() {
            let chain = self.provider.option_chain(symbol, now).await.map_err(|e| {
                RuntimeError::Market(format!("Failed to retrieve option chain for {symbol}: {e}"))
            })?;
            chains.insert(symbol.clone(), chain);
        }

        let policy = Self::manufacturing_policy_from_env()?;

        let mfg_now = Utc::now();
        let plans = th_hive::manufacture_promoted_bots_for_session(
            &bundle,
            &histories,
            &chains,
            &policy,
            mfg_now,
            self.current_session_id.as_deref(),
        );
        if plans.is_empty() {
            println!("BOT_MANUFACTURING_BLOCKED reason=NO_BOT_PLANS_MANUFACTURED");
            return Err(RuntimeError::Market(
                "BOT_MANUFACTURING_BLOCKED: failed to manufacture any active bots".into(),
            ));
        }
        for plan in &plans {
            self.store
                .save_bot_plan(plan)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            if self
                .fleet
                .create(
                    &plan.bot_id,
                    &plan.strategy_id,
                    &plan.config_version,
                    plan.capital_allocated,
                )
                .is_ok()
            {
                let _ = self.fleet.promote_paper(&plan.bot_id);
                let _ = self.fleet.activate(&plan.bot_id);
            }
            self.bot_plans.insert(plan.bot_id.clone(), plan.clone());
            let _ = self.store.event_keyed(
                &format!("BOT_MANUFACTURED:{}", plan.fingerprint),
                "BOT_MANUFACTURED",
                plan,
            );
            let manifest = serde_json::to_value(plan).unwrap_or_default();
            let _ = self
                .json_history
                .upsert_bot(BotHistoryRecord::from_manifest(
                    &plan.bot_id,
                    manifest.clone(),
                    now,
                ));
            let _ = self.json_history.record_manufacturing(HiveManufacturingRun {
                manufacturing_id: plan.plan_id.clone(),
                timestamp: now,
                input: serde_json::json!({"capital_allocated_to_bot": plan.capital_allocated}),
                discovery: serde_json::json!({"underlying": plan.underlying}),
                strategy_selection: serde_json::json!({"strategy_id": plan.strategy_id, "version": plan.strategy_version}),
                capital_allocation: serde_json::json!({"allocated": plan.capital_allocated, "risk_budget": plan.risk_budget}),
                option_selection: serde_json::json!({"contract": plan.option_symbol, "expiry": plan.expiry, "option_type": plan.option_type, "strike": plan.strike}),
                bot_manifest: manifest,
                risk_authorization: serde_json::json!({"risk_budget": plan.risk_budget}),
                manufacturing_result: serde_json::json!({"status": "MANUFACTURED", "bot_id": plan.bot_id}),
            });

            println!(
                "BOT_MANUFACTURED bot_id={} strategy_id={} underlying={} capital={} risk_budget={} option={} expiry={}",
                plan.bot_id,
                plan.strategy_id,
                plan.underlying,
                plan.capital_allocated,
                plan.risk_budget,
                plan.option_symbol,
                plan.expiry
            );
            println!("BOT_STARTED bot_id={} status=ACTIVE", plan.bot_id);
        }

        let _ = self.stage_research_config(&bundle, now);
        Ok(self.bot_plans.len())
    }

    pub async fn run_analysis_window_and_manufacture(
        &mut self,
        symbols: &[String],
        now: DateTime<Utc>,
        start: &DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        let histories = symbols
            .iter()
            .filter_map(|symbol| {
                self.bars
                    .get(symbol)
                    .filter(|h| h.len() >= 60)
                    .map(|h| (symbol.clone(), h.clone()))
            })
            .collect::<HashMap<_, _>>();
        if histories.is_empty() {
            return Ok(false);
        }
        let trade_records = self
            .store
            .trade_records_since(&start.to_rfc3339())
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let bundle = run_analysis_with_q_and_trades(
            histories.clone(),
            self.prior_q_from_active(),
            &trade_records,
        );
        let base_seeds = th_strategy::StrategyRegistry::new().seed_ids();
        let seed_before = self
            .json_history
            .latest_seed_snapshot()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
            .unwrap_or_else(|| {
                base_seeds
                    .iter()
                    .map(|id| serde_json::json!({"strategy_id": id, "type": "seed"}))
                    .collect()
            });
        let mut seed_after = seed_before.clone();
        for sa in &bundle.symbols {
            if let Some(g) = &sa.report.generated_strategy {
                if g.validation.as_ref().map(|v| v.accepted).unwrap_or(false) {
                    seed_after.push(serde_json::json!({"strategy_id": g.blueprint.id, "type": "rl_promoted", "blueprint": g.blueprint}));
                }
            }
        }
        th_hive::persist_rl_history(
            &self.json_history,
            &bundle,
            serde_json::json!({"trade_records_used": trade_records.len()}),
            seed_before,
            seed_after,
            serde_json::json!({"symbols": histories.keys().collect::<Vec<_>>()}),
        )
        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        let mut chains = HashMap::new();
        for symbol in histories.keys() {
            if let Ok(chain) = self.provider.option_chain(symbol, now).await {
                chains.insert(symbol.clone(), chain);
            }
        }
        let policy = Self::manufacturing_policy_from_env()?;
        let mfg_now = Utc::now();
        let plans = manufacture_promoted_bots_for_session(
            &bundle,
            &histories,
            &chains,
            &policy,
            mfg_now,
            self.current_session_id.as_deref(),
        );
        for plan in &plans {
            self.store
                .save_bot_plan(plan)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            if self
                .fleet
                .create(
                    &plan.bot_id,
                    &plan.strategy_id,
                    &plan.config_version,
                    plan.capital_allocated,
                )
                .is_ok()
            {
                self.fleet
                    .promote_paper(&plan.bot_id)
                    .map_err(|e| RuntimeError::InvalidConfig(e.to_string()))?;
                self.fleet
                    .activate(&plan.bot_id)
                    .map_err(|e| RuntimeError::InvalidConfig(e.to_string()))?;
            }
            self.bot_plans.insert(plan.bot_id.clone(), plan.clone());
            self.store
                .event_keyed(
                    &format!("BOT_MANUFACTURED:{}", plan.fingerprint),
                    "BOT_MANUFACTURED",
                    plan,
                )
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            let manifest =
                serde_json::to_value(plan).map_err(|e| RuntimeError::Storage(e.to_string()))?;
            self.json_history
                .upsert_bot(BotHistoryRecord::from_manifest(
                    &plan.bot_id,
                    manifest.clone(),
                    now,
                ))
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            self.json_history.record_manufacturing(HiveManufacturingRun {
                manufacturing_id: plan.plan_id.clone(),
                timestamp: now,
                input: serde_json::json!({"capital_allocated_to_bot": plan.capital_allocated}),
                discovery: serde_json::json!({"underlying": plan.underlying}),
                strategy_selection: serde_json::json!({"strategy_id": plan.strategy_id, "version": plan.strategy_version}),
                capital_allocation: serde_json::json!({"allocated": plan.capital_allocated, "risk_budget": plan.risk_budget}),
                option_selection: serde_json::json!({"contract": plan.option_symbol, "expiry": plan.expiry, "option_type": plan.option_type, "strike": plan.strike}),
                bot_manifest: manifest,
                risk_authorization: serde_json::json!({"risk_budget": plan.risk_budget}),
                manufacturing_result: serde_json::json!({"status": "MANUFACTURED", "bot_id": plan.bot_id}),
            })
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            println!(
                "BOT_MANUFACTURED bot_id={} strategy_id={} underlying={} capital={} risk_budget={} option={} expiry={}",
                plan.bot_id,
                plan.strategy_id,
                plan.underlying,
                plan.capital_allocated,
                plan.risk_budget,
                plan.option_symbol,
                plan.expiry
            );
            println!("BOT_STARTED bot_id={} status=ACTIVE", plan.bot_id);
        }
        let staged = self.stage_research_config(&bundle, now)?;
        Ok(staged && !plans.is_empty())
    }

    pub async fn execute_pre_market(
        &mut self,
        session_id: &str,
        symbols: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, RuntimeError> {
        self.current_session_id = Some(session_id.to_string());
        println!("PRE_MARKET_ANALYSIS_STARTED session_id={session_id}");

        // Reconcile broker positions and retire previous session bots before manufacturing
        match self.reconcile().await {
            Ok(true) => {}
            Ok(false) => {
                return Err(RuntimeError::Execution(
                    "reconciliation_position_mismatch_at_pre_market".into(),
                ));
            }
            Err(e) => {
                return Err(RuntimeError::Execution(format!(
                    "reconciliation_error_at_pre_market: {e}"
                )));
            }
        }
        self.fleet.retire_all();
        self.bot_plans.clear();

        let prior_rl = self.json_history.latest_rl_session().unwrap_or(None);

        // Load Q-policy from the previous session for live decision coupling.
        self.q_policy = prior_rl.as_ref().and_then(|r| {
            serde_json::from_value::<Vec<th_hive::QEntry>>(serde_json::Value::Array(
                r.q_table_snapshot.clone(),
            ))
            .ok()
            .filter(|entries| !entries.is_empty())
            .map(|entries| QLearning::from_entries(&entries))
        });
        let q_entries = self.q_policy.as_ref().map(|q| q.q.len()).unwrap_or(0);
        println!("RL_STATE_LOADED entries={q_entries}");

        // Use previous session's trade records for pre-market analysis context.
        let prior_session_id = prior_rl.as_ref().map(|r| r.session_id.clone());
        let trade_records = if let Some(ref sid) = prior_session_id {
            self.store
                .trade_records_for_session(sid)
                .unwrap_or_default()
        } else {
            // First ever run: scan last 7 days as bootstrap
            self.store
                .trade_records_since(&(now - chrono::Duration::days(7)).to_rfc3339())
                .unwrap_or_default()
        };
        let portfolio = self
            .execution
            .broker()
            .await
            .map(|a| a.equity)
            .unwrap_or(0.0);
        println!(
            "PREVIOUS_SESSION_LOADED prior_rl={} trade_records={} equity={:.2}",
            prior_rl.is_some(),
            trade_records.len(),
            portfolio
        );

        let selected = if symbols.is_empty() {
            self.select_trading_universe(now).await?
        } else {
            symbols.to_vec()
        };

        println!("BOT_MANUFACTURING_STARTED session_id={session_id}");
        self.ensure_bots_manufactured(&selected).await?;
        println!(
            "BOT_MANUFACTURING_COMPLETED session_id={session_id} bots={}",
            self.bot_plans.len()
        );

        Ok(selected)
    }

    pub async fn activate_market_open(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        self.current_session_id = Some(session_id.to_string());
        if !self.reconcile().await? {
            println!("SESSION_ERROR reconciliation failed at market open");
            return Err(RuntimeError::ReconciliationFailed);
        }
        if !self.execution.is_killed() {
            self.active = true;
            self.health = RuntimeHealth::Healthy;
            self.fleet.activate_all();
            println!("BOT_ACTIVATED count={}", self.bot_plans.len());
            println!("BOTS_ACTIVATED count={}", self.bot_plans.len());
            println!("MARKET_OPEN session_id={session_id}");
            println!("TRADING_ENABLED session_id={session_id}");
        }
        Ok(())
    }

    pub async fn execute_market_closing(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        println!("MARKET_CLOSE session_id={session_id}");
        println!("MARKET_CLOSING session_id={session_id}");
        self.active = false;
        println!("TRADING_DISABLED session_id={session_id}");

        // Flatten all open positions before the session is finalized.
        // Each forced close is recorded as a TRADE_AUTOPSY with reason="session_end"
        // and tagged with the current session_id so RL can consume them.
        let open_keys: Vec<String> = self.open_trades.keys().cloned().collect();
        println!(
            "FLATTENING_POSITIONS count={} session_id={session_id}",
            open_keys.len()
        );
        for key in open_keys {
            let Some(t) = self.open_trades.get(&key).cloned() else {
                continue;
            };
            let chain = self
                .provider
                .option_chain(&t.underlying, Utc::now())
                .await
                .map_err(|e| RuntimeError::Market(e.to_string()))?;
            let quote = chain
                .quotes
                .iter()
                .find(|q| {
                    q.symbol == t.symbol && q.is_tradeable(Utc::now(), self.cfg.max_quote_age_secs)
                })
                .ok_or_else(|| {
                    RuntimeError::Market(format!(
                        "cannot flatten {}: no fresh executable quote",
                        t.symbol
                    ))
                })?;
            let mark = quote.bid;
            let spread = quote.spread_bps();

            let broker_positions = self
                .execution
                .positions()
                .await
                .map_err(|e| RuntimeError::Execution(e.to_string()))?;
            let acct = self
                .execution
                .broker()
                .await
                .map_err(|e| RuntimeError::Execution(e.to_string()))?;
            let portfolio = PortfolioRisk {
                cash: acct.cash,
                realized_today: self.daily_realized,
                positions: broker_positions,
            };
            let mut order = OrderIntent {
                client_order_id: Uuid::new_v4(),
                symbol: t.symbol.clone(),
                side: OrderSide::Sell,
                qty: t.qty,
                limit_price: Some(mark),
                reduce_only: true,
                strategy_id: t.strategy_id.clone(),
                created_at: Utc::now(),
                order_hash: String::new(),
                bot_id: Some(t.strategy_id.clone()),
                session_id: self.current_session_id.clone(),
                decision_id: None,
                oms_state: Some(th_domain::OmsState::IntentCreated),
                option_action: Some(OptionOrderAction::SellToClose),
            };
            order.order_hash = order_hash(&order);

            if let Some((broker_id, status)) = self
                .store
                .idempotency_status(order.client_order_id)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?
            {
                if broker_id.is_some() && status != "FAILED" {
                    continue;
                }
            } else if !self
                .store
                .reserve_order(&order)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?
            {
                continue;
            }

            println!(
                "FLATTEN_SUBMITTED client_order_id={} symbol={} qty={}",
                order.client_order_id, order.symbol, order.qty
            );
            let (mut bo, _) = self
                .execution
                .execute(order.clone(), mark, spread, &portfolio)
                .await
                .map_err(|e| RuntimeError::Execution(e.to_string()))?;

            if bo.status == OrderStatus::New || bo.status == OrderStatus::Accepted {
                if let Ok(resolved) = self.execution.reconcile_order(&bo.broker_order_id).await {
                    bo = resolved;
                }
            }

            let _ = self.store.set_idempotency(
                order.client_order_id,
                &bo.broker_order_id,
                &format!("{:?}", bo.status),
            );

            match bo.status {
                OrderStatus::Filled => {
                    println!(
                        "FLATTEN_FILLED client_order_id={} broker_order_id={} filled_qty={}",
                        order.client_order_id, bo.broker_order_id, bo.filled_qty
                    );
                    let fp = bo.filled_avg_price.unwrap_or(mark);
                    let pnl = (fp - t.entry_price)
                        * bo.filled_qty as f64
                        * th_domain::CONTRACT_MULTIPLIER;
                    self.daily_realized += pnl;
                    let _ = self.store.checkpoint(
                        &format!("daily_realized:{}", self.daily_key),
                        &self.daily_realized.to_string(),
                    );
                    self.stats.realized_pnl += pnl;
                    self.stats.trades_closed += 1;
                    let fill = th_domain::Fill {
                        fill_id: Uuid::new_v4(),
                        client_order_id: order.client_order_id,
                        broker_order_id: bo.broker_order_id.clone(),
                        symbol: order.symbol.clone(),
                        side: order.side,
                        qty: bo.filled_qty,
                        price: fp,
                        fee: 0.0,
                        ts: Utc::now(),
                    };
                    let _ = self.store.fill(&fill);
                    let trade = TradeRecord {
                        trade_id: format!("TRADE-EOD-{}-{}", session_id, t.symbol),
                        symbol: t.underlying.clone(),
                        strategy_id: t.strategy_id.clone(),
                        session_id: session_id.to_string(),
                        entry: t.entry_ts,
                        exit: Some(Utc::now()),
                        pnl,
                        fees: 0.0,
                        reason: "session_end".into(),
                        signal_price: Some(t.entry_price),
                        quote_spread_bps: None,
                        entry_fill_price: Some(t.entry_price),
                        exit_fill_price: Some(fp),
                        slippage_bps: Some(0.0),
                        latency_ms: Some(10),
                        regime_at_entry: None,
                    };
                    self.experience.record_trade(trade.clone());
                    let autopsy = self.experience.autopsy(&trade);
                    let _ = self.store.event_keyed(
                        &format!("TRADE_AUTOPSY:{}", trade.trade_id),
                        "TRADE_AUTOPSY",
                        &serde_json::json!({"trade": trade, "autopsy": autopsy}),
                    );
                    self.open_trades.remove(&key);
                    let _ = self.store.delete_open_trade(&key);
                    println!(
                        "POSITION_FORCE_CLOSED symbol={} underlying={} filled_qty={} pnl={:.2} reason=session_end",
                        t.symbol, t.underlying, bo.filled_qty, pnl
                    );
                }
                OrderStatus::PartiallyFilled => {
                    let fp = bo.filled_avg_price.unwrap_or(mark);
                    let pnl = (fp - t.entry_price)
                        * bo.filled_qty as f64
                        * th_domain::CONTRACT_MULTIPLIER;
                    self.daily_realized += pnl;
                    let _ = self.store.checkpoint(
                        &format!("daily_realized:{}", self.daily_key),
                        &self.daily_realized.to_string(),
                    );
                    self.stats.realized_pnl += pnl;
                    let fill = th_domain::Fill {
                        fill_id: Uuid::new_v4(),
                        client_order_id: order.client_order_id,
                        broker_order_id: bo.broker_order_id.clone(),
                        symbol: order.symbol.clone(),
                        side: order.side,
                        qty: bo.filled_qty,
                        price: fp,
                        fee: 0.0,
                        ts: Utc::now(),
                    };
                    let _ = self.store.fill(&fill);
                    let mut remaining = t.clone();
                    remaining.qty = remaining.qty.saturating_sub(bo.filled_qty);
                    self.open_trades.insert(key.clone(), remaining.clone());
                    let _ = self.store.open_trade(&OpenTradeRecord {
                        symbol: remaining.symbol.clone(),
                        underlying: remaining.underlying.clone(),
                        strategy_id: remaining.strategy_id.clone(),
                        entry_price: remaining.entry_price,
                        entry_ts: remaining.entry_ts.to_rfc3339(),
                        stop_loss_pct: remaining.stop_loss_pct,
                        take_profit_pct: remaining.take_profit_pct,
                        qty: remaining.qty,
                        protective_order_id: remaining.protective_order_id.clone(),
                    });
                    println!(
                        "POSITION_PARTIALLY_CLOSED symbol={} underlying={} filled_qty={} remaining_qty={} pnl={:.2}",
                        t.symbol, t.underlying, bo.filled_qty, remaining.qty, pnl
                    );
                }
                OrderStatus::New | OrderStatus::Accepted | OrderStatus::PendingCancel => {
                    println!(
                        "POSITION_CLOSE_PENDING symbol={} underlying={} status={:?}",
                        t.symbol, t.underlying, bo.status
                    );
                }
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    println!(
                        "POSITION_CLOSE_REJECTED symbol={} underlying={} status={:?}",
                        t.symbol, t.underlying, bo.status
                    );
                    self.active = false;
                    self.health = RuntimeHealth::Degraded;
                    self.execution.risk_mut().engage_kill_switch();
                }
                OrderStatus::Replaced | OrderStatus::Unknown => {
                    println!(
                        "POSITION_CLOSE_AMBIGUOUS symbol={} underlying={} status={:?}",
                        t.symbol, t.underlying, bo.status
                    );
                    self.active = false;
                    self.health = RuntimeHealth::Degraded;
                    self.execution.risk_mut().engage_kill_switch();
                }
            }
        }

        // Mandatory EOD reconciliation
        if !self.reconcile().await? {
            self.active = false;
            self.health = RuntimeHealth::Degraded;
            self.execution.risk_mut().engage_kill_switch();
            return Err(RuntimeError::ReconciliationFailed);
        }

        let broker_positions = self
            .execution
            .positions()
            .await
            .map_err(|e| RuntimeError::Execution(e.to_string()))?;
        if !self.open_trades.is_empty() || !broker_positions.is_empty() {
            self.active = false;
            self.health = RuntimeHealth::Degraded;
            self.execution.risk_mut().engage_kill_switch();
            return Err(RuntimeError::ReconciliationFailed);
        }

        println!("POSITIONS_FLATTENED session_id={session_id}");
        Ok(())
    }

    pub async fn execute_post_market(
        &mut self,
        session_id: &str,
        symbols: &[String],
        now: DateTime<Utc>,
    ) -> Result<(), RuntimeError> {
        println!("POST_MARKET session_id={session_id}");

        match self.reconcile().await {
            Ok(true) => {}
            Ok(false) => {
                return Err(RuntimeError::Execution(
                    "reconciliation_position_mismatch_at_post_market".into(),
                ));
            }
            Err(e) => {
                return Err(RuntimeError::Execution(format!(
                    "reconciliation_error_at_post_market: {e}"
                )));
            }
        }
        let account = self.execution.broker().await.ok();
        let equity = account.as_ref().map(|a| a.equity).unwrap_or(0.0);
        let cash = account.as_ref().map(|a| a.cash).unwrap_or(0.0);

        let dataset = serde_json::json!({
            "session_id": session_id,
            "market_date": now.with_timezone(&self.market_clock.config.timezone).format("%Y-%m-%d").to_string(),
            "timestamp": now.to_rfc3339(),
            "symbols": symbols,
            "bot_count": self.bot_plans.len(),
            "open_trades": self.open_trades.len(),
            "trades_opened": self.stats.trades_opened,
            "trades_closed": self.stats.trades_closed,
            "realized_pnl": self.stats.realized_pnl,
            "rejected_orders": self.stats.rejected_orders,
            "account_equity": equity,
            "account_cash": cash,
            "status": "FINALIZED"
        });

        let _ = self.json_history.record_session_dataset(dataset.clone());
        let _ = self.store.event("SESSION_FINALIZED", &dataset);

        let count = self.bot_plans.len();
        self.fleet.retire_all();
        self.bot_plans.clear();
        self.open_trades.clear();

        println!("BOTS_RETIRED count={}", count);
        println!("SESSION_FINALIZED session_id={session_id}");
        Ok(())
    }

    pub async fn execute_learning(
        &mut self,
        session_id: &str,
        symbols: &[String],
        now: DateTime<Utc>,
    ) -> Result<(), RuntimeError> {
        println!("RL_STARTED session_id={session_id}");

        // Use session-scoped trade records — immune to clock drift and stale data.
        let trade_records = self
            .store
            .trade_records_for_session(session_id)
            .unwrap_or_default();

        if trade_records.is_empty() {
            println!(
                "RL_STATUS=INSUFFICIENT_DATA session_id={session_id} reason=no_session_trades"
            );
            // Still persist the current Q-policy even if no trades occurred (live updates may exist).
        } else {
            let mut histories = HashMap::new();
            for sym in symbols {
                if let Some(bars) = self.bars.get(sym) {
                    histories.insert(sym.clone(), bars.clone());
                }
            }

            if histories.is_empty() {
                println!(
                    "RL_STATUS=INSUFFICIENT_DATA session_id={session_id} reason=no_market_histories"
                );
            } else {
                // Use the live q_policy (which has online updates) as the seed for batch analysis.
                let bundle = run_analysis_with_q_and_trades(
                    histories.clone(),
                    self.q_policy.clone(),
                    &trade_records,
                );

                let base_seeds = th_strategy::StrategyRegistry::new().seed_ids();
                let seed_before = self
                    .json_history
                    .latest_seed_snapshot()
                    .unwrap_or_default()
                    .unwrap_or_else(|| {
                        base_seeds
                            .iter()
                            .map(|id| serde_json::json!({"strategy_id": id, "type": "seed"}))
                            .collect()
                    });
                let mut seed_after = seed_before.clone();
                for sa in &bundle.symbols {
                    if let Some(g) = &sa.report.generated_strategy {
                        if g.validation.as_ref().map(|v| v.accepted).unwrap_or(false) {
                            seed_after.push(serde_json::json!({
                                "strategy_id": g.blueprint.id,
                                "type": "rl_promoted",
                                "blueprint": g.blueprint
                            }));
                        }
                    }
                }

                let _ = th_hive::persist_rl_history(
                    &self.json_history,
                    &bundle,
                    serde_json::json!({"trade_records_used": trade_records.len(), "session_id": session_id}),
                    seed_before,
                    seed_after,
                    serde_json::json!({"symbols": histories.keys().collect::<Vec<_>>()}),
                );

                println!("RL_COMPLETED session_id={session_id}");
            }
        }

        // Persist the current live Q-policy (including online updates) as a standalone snapshot.
        // This is what execute_pre_market reloads for the next session.
        if let Some(q) = &self.q_policy {
            let entries = q.entries();
            if !entries.is_empty() {
                let snapshot: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                let _ = self.store.event(
                    "Q_POLICY_SNAPSHOT",
                    &serde_json::json!({
                        "session_id": session_id,
                        "timestamp": now.to_rfc3339(),
                        "entries": snapshot.len(),
                        "q_table": snapshot
                    }),
                );
                println!(
                    "Q_POLICY_PERSISTED session_id={session_id} entries={}",
                    entries.len()
                );
            }
        }

        println!("RL_STATE_PERSISTED session_id={session_id}");
        Ok(())
    }

    pub async fn run_session(
        &mut self,
        symbols: &[String],
        max_ticks: Option<usize>,
    ) -> Result<SessionStats, RuntimeError> {
        let now = Utc::now();
        let mut current_session_date = now
            .with_timezone(&self.market_clock.config.timezone)
            .format("%Y%m%d")
            .to_string();
        let mut session_id = format!("SESSION-{}", current_session_date);
        self.current_session_id = Some(session_id.clone());

        let initial_phase = self.market_session_phase(now);
        println!("MARKET_SESSION_CHANGED from=NONE to={:?}", initial_phase);
        println!("SESSION_PHASE_CHANGED from=NONE to={:?}", initial_phase);
        let mut last_phase = Some(initial_phase);

        let mut active_symbols = if !symbols.is_empty() {
            symbols.to_vec()
        } else {
            match self.select_trading_universe(now).await {
                Ok(s) => s,
                Err(e) => {
                    println!("INITIAL_UNIVERSE_SELECTION_DEFERRED reason={e}");
                    vec![]
                }
            }
        };

        let mut pre_market_done = false;
        let mut open_done = false;
        let mut closing_done = false;
        let mut post_market_done = false;
        let mut learning_done = false;

        match initial_phase {
            SessionPhase::PreMarket => {
                active_symbols = self
                    .execute_pre_market(&session_id, &active_symbols, now)
                    .await?;
                pre_market_done = true;
            }
            SessionPhase::MarketOpen | SessionPhase::Trading => {
                if active_symbols.is_empty() {
                    active_symbols = self.select_trading_universe(now).await?;
                }
                self.ensure_bots_manufactured(&active_symbols).await?;
                self.activate_market_open(&session_id).await?;
                open_done = true;
            }
            SessionPhase::MarketClosing => {
                self.execute_market_closing(&session_id).await?;
                closing_done = true;
            }
            SessionPhase::PostMarket => {
                self.execute_post_market(&session_id, &active_symbols, now)
                    .await?;
                post_market_done = true;
            }
            SessionPhase::Learning => {
                self.execute_learning(&session_id, &active_symbols, now)
                    .await?;
                learning_done = true;
            }
            SessionPhase::WaitingForNextSession | SessionPhase::Analysis => {
                self.active = false;
                println!("WAITING_FOR_NEXT_SESSION session_id={session_id}");
                println!("HIVE_HEARTBEAT status=WAITING_FOR_NEXT_SESSION market_open=false active_bots=0");
            }
        }

        let mut tick_count: usize = 0;

        loop {
            let now = Utc::now();
            let new_date = now
                .with_timezone(&self.market_clock.config.timezone)
                .format("%Y%m%d")
                .to_string();
            if new_date != current_session_date {
                current_session_date = new_date;
                session_id = format!("SESSION-{}", current_session_date);
                self.current_session_id = Some(session_id.clone());
                pre_market_done = false;
                open_done = false;
                closing_done = false;
                post_market_done = false;
                learning_done = false;
                if symbols.is_empty() {
                    active_symbols.clear();
                }
                println!("DAY_ROLLED_OVER new_session_id={session_id}");
            }

            let current_phase = self.market_session_phase(now);
            if Some(current_phase) != last_phase {
                if let Some(prev) = last_phase {
                    println!(
                        "MARKET_SESSION_CHANGED from={:?} to={:?}",
                        prev, current_phase
                    );
                    println!(
                        "SESSION_PHASE_CHANGED from={:?} to={:?}",
                        prev, current_phase
                    );
                }
                last_phase = Some(current_phase);
            }

            match current_phase {
                SessionPhase::PreMarket => {
                    if !pre_market_done {
                        active_symbols = self
                            .execute_pre_market(&session_id, &active_symbols, now)
                            .await?;
                        pre_market_done = true;
                    }
                }
                SessionPhase::MarketOpen | SessionPhase::Trading => {
                    if !open_done {
                        if active_symbols.is_empty() {
                            active_symbols = self.select_trading_universe(now).await?;
                        }
                        self.ensure_bots_manufactured(&active_symbols).await?;
                        self.activate_market_open(&session_id).await?;
                        open_done = true;
                    }

                    if self.active {
                        let clock = self
                            .execution
                            .clock()
                            .await
                            .map_err(|e| RuntimeError::Execution(e.to_string()))?;

                        if clock.is_open {
                            for symbol in &active_symbols {
                                let now_tick = Utc::now();
                                let end = now_tick
                                    - chrono::Duration::seconds(
                                        now_tick.timestamp().rem_euclid(60),
                                    )
                                    - chrono::Duration::minutes(1);
                                let start = end - chrono::Duration::minutes(6);
                                match self.provider.bars(symbol, start, end).await {
                                    Ok(bs) => {
                                        for b in bs {
                                            let event_id = format!(
                                                "{}:{}",
                                                symbol,
                                                b.ts.timestamp_nanos_opt().unwrap_or(0)
                                            );
                                            self.on_market_bar_at(&event_id, b, end).await?;
                                        }
                                    }
                                    Err(e) => {
                                        self.health = RuntimeHealth::Degraded;
                                        let _ = self.store.event(
                                            "MARKET_DATA_ERROR",
                                            &serde_json::json!({"symbol": symbol, "error": e.to_string()}),
                                        );
                                    }
                                }
                            }

                            println!(
                                "BOT_EVALUATION phase={:?} active_bots={} open_trades={} trades_opened={} trades_closed={} realized_pnl={:.2}",
                                current_phase,
                                self.bot_plans.len(),
                                self.open_trades.len(),
                                self.stats.trades_opened,
                                self.stats.trades_closed,
                                self.stats.realized_pnl
                            );
                        } else {
                            println!("MARKET_CLOSED is_open=false clock={:?}", clock);
                        }

                        if let Ok(h) = self.health_snapshot(Utc::now()).await {
                            if !h.data_healthy && self.stats.last_market_event.is_some() {
                                self.execution.risk_mut().engage_kill_switch();
                                self.active = false;
                                self.health = RuntimeHealth::Halted;
                            }
                        }
                    }
                }
                SessionPhase::MarketClosing => {
                    if !closing_done {
                        self.execute_market_closing(&session_id).await?;
                        closing_done = true;
                    }
                }
                SessionPhase::PostMarket => {
                    if !post_market_done {
                        self.execute_post_market(&session_id, &active_symbols, now)
                            .await?;
                        post_market_done = true;
                    }
                }
                SessionPhase::Learning => {
                    if !learning_done {
                        self.execute_learning(&session_id, &active_symbols, now)
                            .await?;
                        learning_done = true;
                    }
                }
                SessionPhase::WaitingForNextSession | SessionPhase::Analysis => {
                    self.active = false;
                    if tick_count.is_multiple_of(12) {
                        println!("HIVE_HEARTBEAT status=WAITING_FOR_NEXT_SESSION market_open=false active_bots=0");
                    }
                }
            }

            tick_count += 1;
            if let Some(max) = max_ticks {
                if tick_count >= max {
                    println!("SESSION_STOPPING max_ticks reached ({})", max);
                    break;
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        println!(
            "SESSION_COMPLETE trades_opened={} trades_closed={} realized_pnl={:.2} rejected_orders={}",
            self.stats.trades_opened,
            self.stats.trades_closed,
            self.stats.realized_pnl,
            self.stats.rejected_orders
        );
        println!(
            "SESSION_COMPLETED trades_opened={} trades_closed={} realized_pnl={:.2} rejected_orders={}",
            self.stats.trades_opened,
            self.stats.trades_closed,
            self.stats.realized_pnl,
            self.stats.rejected_orders
        );

        Ok(self.stats.clone())
    }

    pub async fn run_forever(&mut self, _symbols: &[String]) -> Result<(), RuntimeError> {
        let mut supervisor = HiveSupervisor::new(self, SupervisorConfig::default());
        supervisor.run_supervised(None).await
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("market data: {0}")]
    Market(String),
    #[error("execution: {0}")]
    Execution(String),
    #[error("wrong session phase")]
    WrongPhase,
    #[error("insufficient data")]
    InsufficientData,
    #[error("no tradeable option")]
    NoOption,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("broker reconciliation failed")]
    ReconciliationFailed,
    #[error("material news risk")]
    NewsRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub phase: SessionPhase,
}
pub fn session_windows(cfg: &RuntimeConfig, day: DateTime<Utc>) -> Vec<SessionWindow> {
    let tz = cfg.tz();
    let local = day.with_timezone(&tz);
    let d = local.date_naive();
    let start_hour = cfg.analysis_start_hour % 24;
    let _end_hour = (start_hour + cfg.analysis_hours.min(24)) % 24;
    let analysis_start_local = tz
        .from_local_datetime(&d.and_hms_opt(start_hour, 0, 0).unwrap_or_default())
        .single()
        .unwrap_or_else(|| tz.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()));
    let analysis_end_local =
        analysis_start_local + chrono::Duration::hours(cfg.analysis_hours.min(24) as i64);
    let trading_start_local = analysis_end_local - chrono::Duration::hours(20);
    vec![
        SessionWindow {
            start: trading_start_local.with_timezone(&Utc),
            end: analysis_start_local.with_timezone(&Utc),
            phase: SessionPhase::Trading,
        },
        SessionWindow {
            start: analysis_start_local.with_timezone(&Utc),
            end: analysis_end_local.with_timezone(&Utc),
            phase: SessionPhase::Analysis,
        },
    ]
}

pub fn current_analysis_start(cfg: &RuntimeConfig, now: DateTime<Utc>) -> DateTime<Utc> {
    let tz = cfg.tz();
    let local = now.with_timezone(&tz);
    let d = local.date_naive();
    let h = cfg.analysis_start_hour % 24;
    let naive = d
        .and_hms_opt(h, 0, 0)
        .unwrap_or_else(|| d.and_time(chrono::NaiveTime::MIN));
    let today = tz
        .from_local_datetime(&naive)
        .single()
        .unwrap_or_else(|| tz.from_utc_datetime(&naive));
    if now >= today {
        today.with_timezone(&Utc)
    } else {
        (today - chrono::Duration::days(1)).with_timezone(&Utc)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchDeadline {
    pub analysis_start: DateTime<Utc>,
    pub hard_stop: DateTime<Utc>,
    pub research_cutoff: DateTime<Utc>,
    pub promotion_cutoff: DateTime<Utc>,
}
pub fn research_deadline(cfg: &RuntimeConfig, analysis_start: DateTime<Utc>) -> ResearchDeadline {
    let hard = analysis_start + chrono::Duration::hours(cfg.analysis_hours.min(4) as i64);
    let research_cutoff = hard - chrono::Duration::minutes(30);
    let promotion_cutoff = hard - chrono::Duration::minutes(10);
    ResearchDeadline {
        analysis_start,
        hard_stop: hard,
        research_cutoff,
        promotion_cutoff,
    }
}
impl ResearchDeadline {
    pub fn research_allowed(&self, now: DateTime<Utc>) -> bool {
        now >= self.analysis_start && now < self.research_cutoff
    }
    pub fn promotion_allowed(&self, now: DateTime<Utc>) -> bool {
        now >= self.research_cutoff && now < self.promotion_cutoff
    }
    pub fn trading_boundary_reached(&self, now: DateTime<Utc>) -> bool {
        now >= self.hard_stop
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataHealth {
    pub last_event: Option<DateTime<Utc>>,
    pub max_age_secs: i64,
}
impl DataHealth {
    pub fn healthy(&self, now: DateTime<Utc>) -> bool {
        self.last_event
            .map(|t| {
                let age = (now - t).num_seconds();
                age >= 0 && age <= self.max_age_secs.max(0)
            })
            .unwrap_or(false)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub phase: SessionPhase,
    pub runtime_active: bool,
    pub data_healthy: bool,
    pub broker_healthy: bool,
    pub storage_healthy: bool,
    pub risk_killed: bool,
    pub open_trades: usize,
}
impl HealthSnapshot {
    pub fn fail_closed(&self) -> bool {
        !self.runtime_active
            || !self.data_healthy
            || !self.broker_healthy
            || !self.storage_healthy
            || self.risk_killed
    }
}

#[cfg(test)]
mod generated_strategy_activation_tests {
    use super::*;
    use th_hive::{
        AnalysisReport, GeneratedStrategyRecord, GeneratedStrategyValidation, PromotionRecord,
        SymbolAnalysis,
    };
    #[test]
    fn accepted_strat31_is_reconstructed_as_worker_strategy() {
        let now = Utc::now();
        let blueprint = th_strategy::StrategyBlueprint {
            id: "STRAT-31".into(),
            version: 1,
            parent_a: "momentum".into(),
            parent_b: "ema_trend".into(),
            confidence: 0.8,
            weight_a: 0.6,
            weight_b: 0.4,
            agreement_threshold: 0.55,
            rationale: "test".into(),
        };
        let generated = GeneratedStrategyRecord {
            blueprint: blueprint.clone(),
            generated_from_q: 1.2,
            generated_at: now,
            validation: Some(GeneratedStrategyValidation {
                train_pnl: 10.0,
                validation_pnl: 5.0,
                oos_pnl: 4.0,
                oos_sharpe: 1.0,
                profit_factor: 1.5,
                max_drawdown: 10.0,
                trades: 20,
                accepted: true,
            }),
        };
        let report = AnalysisBundle {
            started: now,
            finished: now,
            dataset_hash: "x".into(),
            symbols: vec![SymbolAnalysis {
                symbol: "SPY".into(),
                report: AnalysisReport {
                    started: now,
                    finished: now,
                    evaluations: vec![],
                    promoted: vec![PromotionRecord {
                        symbol: "SPY".into(),
                        strategy_id: "STRAT-31".into(),
                        version: 1,
                        fingerprint: "fp".into(),
                        promoted: true,
                        reason: "GENERATED_STRATEGY_PASSED_RESEARCH_GATES".into(),
                        created_at: now,
                    }],
                    variables: vec![],
                    learning_updates: 1,
                    config_version: "v13".into(),
                    q_table: vec![],
                    dataset_hash: "x".into(),
                    generated_strategy: Some(generated),
                    experiences: vec![],
                },
            }],
            promoted: vec![PromotionRecord {
                symbol: "SPY".into(),
                strategy_id: "STRAT-31".into(),
                version: 1,
                fingerprint: "fp".into(),
                promoted: true,
                reason: "GENERATED_STRATEGY_PASSED_RESEARCH_GATES".into(),
                created_at: now,
            }],
        };
        let strategies = strategies_from_report(&report);
        assert_eq!(strategies.len(), 1);
        assert_eq!(strategies[0].spec().id, "STRAT-31");
    }
}
