use chrono::{DateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use th_deployment::{BotCreationPlan, BotFleet};
use th_domain::{Bar, OrderIntent, OrderSide, SessionPhase};
use th_execution::{order_hash, reconcile_positions, Broker, ExecutionEngine};
use th_hive::{
    manufacture_promoted_bots, run_analysis_with_q_and_trades, AnalysisBundle,
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
        }
    }
}
impl RuntimeConfig {
    /// Explicit test configuration. Production configuration must come from environment variables.
    pub fn testing() -> Self {
        Self {
            stop_loss_pct: 0.05,
            take_profit_pct: 0.0,
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
        let daily_key = now.with_timezone(&cfg.tz()).date_naive().to_string();
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
        let bot_plans = store
            .load_bot_plans()
            .map_err(|e| RuntimeError::Storage(e.to_string()))?
            .into_iter()
            .map(|p| (p.bot_id.clone(), p))
            .collect::<HashMap<_, _>>();
        let mut fleet = BotFleet::default();
        for p in bot_plans.values() {
            if fleet
                .create(
                    &p.bot_id,
                    &p.strategy_id,
                    &p.config_version,
                    p.capital_allocated,
                )
                .is_ok()
            {
                let _ = fleet.promote_paper(&p.bot_id);
                let _ = fleet.activate(&p.bot_id);
            }
        }
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
                    },
                );
            }
        }
        Ok(Self {
            strategies,
            execution: ExecutionEngine::new(
                broker,
                RiskGovernor::new(RiskLimits::from_env().unwrap_or_default()),
            ),
            provider,
            store,
            bars,
            active: cfg.phase_at(now) == SessionPhase::Trading,
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
            cfg: RuntimeConfig {
                config_version: active_version,
                ..cfg
            },
        })
    }
    pub fn phase_at(&self, dt: DateTime<Utc>) -> SessionPhase {
        self.cfg.phase_at(dt)
    }
    pub fn phase(&self) -> SessionPhase {
        self.phase_at(Utc::now())
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
        let key = now.with_timezone(&self.cfg.tz()).date_naive().to_string();
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
        let history = self.bars.entry(candle.symbol.clone()).or_default();
        history.push(candle.clone());
        if history.len() > self.cfg.max_bars_memory {
            let n = history.len() - self.cfg.max_bars_memory;
            history.drain(0..n);
        }
        if self.cfg.phase_at(now) != SessionPhase::Trading
            || !self.active
            || !th_domain::MarketSessionClock::default().is_open(now)
        {
            return Ok(());
        }
        let snapshot = history.clone();
        let state = classify_regime(&snapshot);
        self.manage_open_trade(&candle.symbol, &state).await?;
        let mut signals = Vec::new();
        for strategy in self.strategies.iter_mut() {
            if let Some(sig) = strategy.update(&candle, &state) {
                println!(
                    "SIGNAL_GENERATED symbol={} strategy_id={} side={:?} strength={:.2}",
                    sig.symbol, sig.strategy_id, sig.side, sig.strength
                );
                signals.push(sig)
            }
        }
        for sig in signals {
            self.store
                .signal(&sig)
                .map_err(|e| RuntimeError::Storage(e.to_string()))?;
            if let Err(e) = self.handle_signal(sig).await {
                self.stats.rejected_orders += 1;
                self.store
                    .event(
                        "ORDER_REJECTED",
                        &serde_json::json!({"error":e.to_string()}),
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
            if ret <= -t.stop_loss_pct
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
                    };
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
            .find(|p| p.strategy_id == sig.strategy_id && p.underlying == sig.symbol)
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
        let market_clock = th_domain::MarketSessionClock::default();
        if !market_clock.is_open(now) {
            println!(
                "MARKET_CLOSED session_state={:?}",
                market_clock.session_state_at(now)
            );
            return Err(RuntimeError::Market("MARKET_CLOSED".into()));
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
            0.05
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
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(u32::MAX);
        self.execution
            .risk_mut()
            .register_strategy_risk(th_risk::StrategyRiskAllocation {
                strategy_id: plan.strategy_id.clone(),
                risk_pct: plan.risk_pct,
                capital_allocated: plan.capital_allocated,
                risk_budget: plan.risk_budget,
            });
        let max_trade_risk_pct = std::env::var("RISK_MAX_TRADE_RISK_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.02);
        let max_portfolio_risk_pct = std::env::var("RISK_MAX_PORTFOLIO_RISK_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.10);
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
            plan_capital_allocated: plan.capital_allocated,
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
        };
        order.order_hash = order_hash(&order);
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
            "ORDER_SUBMITTED broker_order_id={} status={:?} symbol={} qty={}",
            bo.broker_order_id, bo.status, order.symbol, order.qty
        );
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
                let opened = OpenTrade {
                    symbol: order.symbol.clone(),
                    underlying: sig.symbol.clone(),
                    strategy_id: order.strategy_id.clone(),
                    entry_price: fp,
                    entry_ts: Utc::now(),
                    stop_loss_pct: self.cfg.stop_loss_pct,
                    take_profit_pct: self.cfg.take_profit_pct,
                    qty: bo.filled_qty,
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
                    })
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                self.open_trades
                    .insert(order.symbol.clone(), opened.clone());
                self.stats.trades_opened += 1;
                println!(
                    "POSITION_OPENED symbol={} underlying={} strategy_id={} qty={} entry_price={:.2}",
                    opened.symbol, opened.underlying, opened.strategy_id, opened.qty, opened.entry_price
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
            })
            .collect::<Vec<_>>();
        let report = reconcile_positions(&internal, &broker);
        self.store
            .event("RECONCILIATION", &report)
            .map_err(|e| RuntimeError::Storage(e.to_string()))?;
        if !report.matched {
            self.active = false;
            self.health = RuntimeHealth::Degraded;
            self.execution.risk_mut().engage_kill_switch();
        }
        Ok(report.matched)
    }
    fn prior_q_from_active(&self) -> Option<QLearning> {
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
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let risk_fraction = std::env::var("HIVE_RISK_FRACTION")
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_RISK_FRACTION missing".into()))?
            .parse::<f64>()
            .map_err(|_| RuntimeError::InvalidConfig("HIVE_RISK_FRACTION invalid".into()))?;
        let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
        Ok(HiveManufacturingPolicy {
            total_capital: total,
            max_bots,
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
        if self.cfg.phase_at(now) != SessionPhase::Analysis {
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
    pub async fn ensure_bots_manufactured(
        &mut self,
        symbols: &[String],
    ) -> Result<usize, RuntimeError> {
        let now = Utc::now();
        if !self.bot_plans.is_empty() {
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
            for symbol in symbols {
                let mut bs = Vec::with_capacity(120);
                let p = 500.0;
                for i in 0..120 {
                    let ts = now - chrono::Duration::minutes((120 - i) as i64);
                    bs.push(th_domain::Bar {
                        symbol: symbol.clone(),
                        ts,
                        open: p + (i as f64 * 0.05),
                        high: p + (i as f64 * 0.05) + 0.2,
                        low: p + (i as f64 * 0.05) - 0.2,
                        close: p + (i as f64 * 0.05) + 0.1,
                        volume: 1000.0,
                    });
                }
                self.bars.insert(symbol.clone(), bs.clone());
                histories.insert(symbol.clone(), bs);
            }
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
            if let Ok(chain) = self.provider.option_chain(symbol, now).await {
                chains.insert(symbol.clone(), chain);
            } else {
                chains.insert(
                    symbol.clone(),
                    th_market_data::synthetic_option_chain(symbol, 500.0, now),
                );
            }
        }

        let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
        let policy =
            Self::manufacturing_policy_from_env().unwrap_or(th_hive::HiveManufacturingPolicy {
                total_capital: 1000000.0,
                max_bots: 20,
                risk_fraction: 0.05,
                min_expiry_minutes: expiry_policy.min_expiry_minutes,
                max_expiry_minutes: expiry_policy.max_expiry_minutes.unwrap_or(u32::MAX),
            });

        let plans = manufacture_promoted_bots(&bundle, &histories, &chains, &policy, now);
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
        let plans = manufacture_promoted_bots(
            &bundle,
            &histories,
            &chains,
            &Self::manufacturing_policy_from_env()?,
            now,
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

    pub async fn run_session(
        &mut self,
        symbols: &[String],
        max_ticks: Option<usize>,
    ) -> Result<SessionStats, RuntimeError> {
        if symbols.is_empty() {
            return Err(RuntimeError::InvalidConfig("no symbols configured".into()));
        }

        let now = Utc::now();
        let phase = self.phase_at(now);
        let start = current_analysis_start(&self.cfg, now);
        let deadline = research_deadline(&self.cfg, start);

        println!(
            "SESSION_STARTED phase={:?} symbols={:?} analysis_start={} deadline={:?}",
            phase, symbols, start, deadline
        );

        self.ensure_bots_manufactured(symbols).await?;

        if !self.reconcile().await? {
            println!("SESSION_ERROR reconciliation failed on startup");
            return Err(RuntimeError::ReconciliationFailed);
        }

        if phase == SessionPhase::Trading && !self.execution.is_killed() {
            self.active = true;
            self.health = RuntimeHealth::Healthy;
        }

        println!(
            "MARKET_DATA_READY active={} health={:?} open_trades={}",
            self.active,
            self.health,
            self.open_trades.len()
        );

        let mut last_phase = phase;
        let mut tick_count: usize = 0;

        loop {
            let now = Utc::now();
            let current_phase = self.phase_at(now);

            if current_phase != last_phase {
                self.store
                    .event(
                        "SESSION_PHASE",
                        &serde_json::json!({
                            "from": format!("{:?}", last_phase),
                            "to": format!("{:?}", current_phase),
                            "at": now
                        }),
                    )
                    .map_err(|e| RuntimeError::Storage(e.to_string()))?;

                if current_phase == SessionPhase::Trading {
                    if !self.reconcile().await? {
                        return Err(RuntimeError::ReconciliationFailed);
                    }
                    if !self.execution.is_killed() {
                        self.active = true;
                        self.health = RuntimeHealth::Healthy;
                    }
                } else {
                    self.active = false;
                }
                last_phase = current_phase;
            }

            if current_phase == SessionPhase::Analysis {
                let a_start = current_analysis_start(&self.cfg, now);
                let a_deadline = research_deadline(&self.cfg, a_start);
                let key = format!("analysis_completed:{}", a_start.timestamp());

                if now < a_deadline.research_cutoff
                    && self
                        .store
                        .checkpoint_value(&key)
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?
                        .is_none()
                {
                    let staged_any = self
                        .run_analysis_window_and_manufacture(symbols, now, &a_start)
                        .await?;
                    self.store
                        .checkpoint(&key, if staged_any { "1" } else { "NO_CANDIDATE" })
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?;
                } else if a_deadline.promotion_allowed(now) {
                    let since = a_start.to_rfc3339();
                    if let Some((version, _)) = self
                        .store
                        .latest_inactive_config_since(&since)
                        .map_err(|e| RuntimeError::Storage(e.to_string()))?
                    {
                        let _ = self.activate_research_config(&version, now)?;
                    }
                }
            } else if self.active {
                let clock = self
                    .execution
                    .clock()
                    .await
                    .map_err(|e| RuntimeError::Execution(e.to_string()))?;

                if clock.is_open {
                    for symbol in symbols {
                        let now_tick = Utc::now();
                        let end = now_tick
                            - chrono::Duration::seconds(now_tick.timestamp().rem_euclid(60))
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
                                self.store
                                    .event(
                                        "MARKET_DATA_ERROR",
                                        &serde_json::json!({"symbol": symbol, "error": e.to_string()}),
                                    )
                                    .map_err(|x| RuntimeError::Storage(x.to_string()))?;
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
            "SESSION_COMPLETED trades_opened={} trades_closed={} realized_pnl={:.2} rejected_orders={}",
            self.stats.trades_opened,
            self.stats.trades_closed,
            self.stats.realized_pnl,
            self.stats.rejected_orders
        );

        Ok(self.stats.clone())
    }

    pub async fn run_forever(&mut self, symbols: &[String]) -> Result<(), RuntimeError> {
        self.run_session(symbols, None).await.map(|_| ())
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
