use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use uuid::Uuid;

pub mod expiry_policy;
pub mod market_session;

pub use expiry_policy::OptionExpiryPolicy;
pub use market_session::{
    HolidayCalendar, MarketClosedReason, MarketSessionClock, MarketSessionConfig,
    MarketSessionState,
};

pub const CONTRACT_MULTIPLIER: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeFrame {
    Min5,
}
impl TimeFrame {
    pub const fn seconds(self) -> i64 {
        300
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bar {
    pub symbol: String,
    pub ts: DateTime<Utc>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}
impl Bar {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.symbol.trim().is_empty() {
            return Err(DomainError::Invalid("empty symbol".into()));
        }
        if ![self.open, self.high, self.low, self.close, self.volume]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(DomainError::Invalid("non-finite OHLCV".into()));
        }
        if self.open <= 0.0
            || self.high <= 0.0
            || self.low <= 0.0
            || self.close <= 0.0
            || self.volume < 0.0
        {
            return Err(DomainError::Invalid("invalid OHLCV".into()));
        }
        if self.high < self.open
            || self.high < self.close
            || self.high < self.low
            || self.low > self.open
            || self.low > self.close
        {
            return Err(DomainError::Invalid("OHLC invariant violated".into()));
        }
        Ok(())
    }
    pub fn range(&self) -> f64 {
        self.high - self.low
    }
    pub fn body(&self) -> f64 {
        (self.close - self.open).abs()
    }
    pub fn returns_from(&self, prev: &Bar) -> f64 {
        if prev.close > 0.0 {
            self.close / prev.close - 1.0
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketEventKind {
    Bar,
    Trade,
    Quote,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketEvent {
    pub event_id: String,
    pub symbol: String,
    pub ts: DateTime<Utc>,
    pub kind: MarketEventKind,
    pub bar: Option<Bar>,
}
impl MarketEvent {
    pub fn key(&self) -> String {
        self.event_id.clone()
    }
}

#[derive(Debug, Clone)]
pub struct CandleBuilder {
    symbol: String,
    current: Option<Bar>,
    timeframe: TimeFrame,
    last_event_ts: Option<DateTime<Utc>>,
    _last_event_key: Option<String>,
}
impl CandleBuilder {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            current: None,
            timeframe: TimeFrame::Min5,
            last_event_ts: None,
            _last_event_key: None,
        }
    }
    pub fn bucket(ts: DateTime<Utc>) -> DateTime<Utc> {
        let s = ts.timestamp();
        ts - chrono::Duration::seconds(s.rem_euclid(300))
    }
    pub fn push(&mut self, bar: Bar) -> Result<Option<Bar>, DomainError> {
        bar.validate()?;
        if bar.symbol != self.symbol {
            return Err(DomainError::Invalid("symbol mismatch".into()));
        }
        if let Some(last) = self.last_event_ts {
            if bar.ts < last {
                return Ok(None);
            }
        }
        let bucket = Self::bucket(bar.ts);
        self.last_event_ts = Some(bar.ts);
        match &mut self.current {
            None => {
                self.current = Some(Bar {
                    symbol: self.symbol.clone(),
                    ts: bucket,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: bar.volume,
                });
                Ok(None)
            }
            Some(c) if bucket < c.ts => Ok(None),
            Some(c) if bucket == c.ts => {
                c.high = c.high.max(bar.high);
                c.low = c.low.min(bar.low);
                c.close = bar.close;
                c.volume += bar.volume;
                Ok(None)
            }
            Some(c) => {
                let out = c.clone();
                self.current = Some(Bar {
                    symbol: self.symbol.clone(),
                    ts: bucket,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: bar.volume,
                });
                Ok(Some(out))
            }
        }
    }
    pub fn flush(&mut self) -> Option<Bar> {
        self.current.take()
    }
    pub fn timeframe(&self) -> TimeFrame {
        self.timeframe
    }
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionType {
    Call,
    Put,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Greeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}
impl Greeks {
    pub fn valid(&self) -> bool {
        [self.delta, self.gamma, self.theta, self.vega, self.rho]
            .iter()
            .all(|v| v.is_finite())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionQuote {
    pub symbol: String,
    pub underlying: String,
    pub option_type: OptionType,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub iv: f64,
    pub greeks: Option<Greeks>,
    pub open_interest: u64,
    pub volume: u64,
    pub quote_ts: DateTime<Utc>,
}
impl OptionQuote {
    pub fn mid(&self) -> f64 {
        if self.bid > 0.0 && self.ask >= self.bid {
            (self.bid + self.ask) / 2.0
        } else {
            self.last.max(0.0)
        }
    }
    pub fn spread(&self) -> f64 {
        if self.ask >= self.bid {
            self.ask - self.bid
        } else {
            f64::INFINITY
        }
    }
    pub fn spread_bps(&self) -> f64 {
        let m = self.mid();
        if m > 0.0 {
            self.spread() / m * 10_000.0
        } else {
            f64::INFINITY
        }
    }
    pub fn dte(&self, now: DateTime<Utc>) -> f64 {
        (self.expiry - now).num_seconds().max(0) as f64 / 86_400.0
    }
    pub fn is_tradeable(&self, now: DateTime<Utc>, max_quote_age_secs: i64) -> bool {
        let age = (now - self.quote_ts).num_seconds();
        self.bid.is_finite()
            && self.ask.is_finite()
            && self.bid > 0.0
            && self.ask >= self.bid
            && self.last.is_finite()
            && self.dte(now) > 0.0
            && age >= 0
            && age <= max_quote_age_secs.max(0)
    }
    pub fn is_valid_expiry(&self, decision_ts: DateTime<Utc>, policy: &OptionExpiryPolicy) -> bool {
        policy.is_valid_expiry(decision_ts, self.expiry)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionChain {
    pub underlying: String,
    pub as_of: DateTime<Utc>,
    pub quotes: Vec<OptionQuote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalSide {
    LongCall,
    LongPut,
    Flat,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: Uuid,
    pub strategy_id: String,
    pub symbol: String,
    pub side: SignalSide,
    pub strength: f64,
    pub reason: String,
    pub generated_at: DateTime<Utc>,
    pub config_version: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub candidate_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionOrderAction {
    BuyToOpen,
    BuyToClose,
    SellToOpen,
    SellToClose,
}

impl OptionOrderAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BuyToOpen => "buy_to_open",
            Self::BuyToClose => "buy_to_close",
            Self::SellToOpen => "sell_to_open",
            Self::SellToClose => "sell_to_close",
        }
    }

    pub fn order_side(&self) -> OrderSide {
        match self {
            Self::BuyToOpen | Self::BuyToClose => OrderSide::Buy,
            Self::SellToOpen | Self::SellToClose => OrderSide::Sell,
        }
    }

    pub fn is_opening(&self) -> bool {
        matches!(self, Self::BuyToOpen | Self::SellToOpen)
    }

    pub fn is_closing(&self) -> bool {
        matches!(self, Self::BuyToClose | Self::SellToClose)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    New,
    Accepted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Expired,
    Replaced,
    PendingCancel,
    Unknown,
}

impl OrderStatus {
    pub fn is_working(&self) -> bool {
        matches!(
            self,
            Self::New | Self::Accepted | Self::PartiallyFilled | Self::PendingCancel
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Cancelled | Self::Rejected | Self::Expired
        )
    }

    pub fn requires_reconciliation(&self) -> bool {
        matches!(self, Self::Unknown | Self::Replaced)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OmsState {
    IntentCreated,
    RiskReserved,
    Submitting,
    Submitted,
    Accepted,
    PartiallyFilled,
    Filled,
    CancelRequested,
    Cancelled,
    Rejected,
    Expired,
    Failed,
    Unknown,
    Reconciling,
    Reconciled,
}

impl OmsState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Filled
                | Self::Cancelled
                | Self::Rejected
                | Self::Expired
                | Self::Failed
                | Self::Reconciled
        )
    }

    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Unknown | Self::Reconciling)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceChain {
    pub research_run_id: String,
    pub candidate_id: String,
    pub strategy_id: String,
    pub strategy_version: u32,
    pub bot_id: String,
    pub session_id: String,
    pub decision_id: Uuid,
    pub client_order_id: Uuid,
    pub broker_order_id: Option<String>,
    pub fill_id: Option<Uuid>,
    pub trade_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub client_order_id: Uuid,
    pub symbol: String,
    pub side: OrderSide,
    pub qty: u32,
    pub limit_price: Option<f64>,
    pub reduce_only: bool,
    pub strategy_id: String,
    pub created_at: DateTime<Utc>,
    pub order_hash: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub decision_id: Option<Uuid>,
    #[serde(default)]
    pub oms_state: Option<OmsState>,
    #[serde(default)]
    pub option_action: Option<OptionOrderAction>,
}
impl OrderIntent {
    pub fn resolve_option_action(&self) -> OptionOrderAction {
        if let Some(action) = self.option_action {
            return action;
        }
        match (self.side, self.reduce_only) {
            (OrderSide::Buy, false) => OptionOrderAction::BuyToOpen,
            (OrderSide::Buy, true) => OptionOrderAction::BuyToClose,
            (OrderSide::Sell, false) => OptionOrderAction::SellToOpen,
            (OrderSide::Sell, true) => OptionOrderAction::SellToClose,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.qty == 0 {
            return Err(DomainError::Invalid("zero quantity".into()));
        }
        if self.symbol.trim().is_empty() {
            return Err(DomainError::Invalid("empty order symbol".into()));
        }
        if let Some(p) = self.limit_price {
            if !p.is_finite() || p <= 0.0 {
                return Err(DomainError::Invalid("invalid limit price".into()));
            }
        }
        if self.strategy_id.trim().is_empty() {
            return Err(DomainError::Invalid("empty strategy id".into()));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub fill_id: Uuid,
    pub client_order_id: Uuid,
    pub broker_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub qty: u32,
    pub price: f64,
    pub fee: f64,
    pub ts: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OptionContract {
    pub symbol: String,
    pub underlying: String,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub option_type: OptionType,
    pub multiplier: f64,
    pub exchange: String,
    pub currency: String,
}

impl OptionContract {
    pub fn from_occ(symbol: &str) -> Option<Self> {
        use chrono::TimeZone;
        let parsed = occ::parse(symbol)?;
        let naive_date =
            chrono::NaiveDate::from_ymd_opt(parsed.year as i32, parsed.month, parsed.day)?;
        let naive_dt = naive_date.and_hms_opt(16, 0, 0)?; // 4:00 PM Eastern Time expiration
        let expiry = chrono_tz::America::New_York
            .from_local_datetime(&naive_dt)
            .single()?
            .with_timezone(&Utc);
        Some(Self {
            symbol: symbol.to_string(),
            underlying: parsed.underlying,
            strike: parsed.strike,
            expiry,
            option_type: parsed.option_type,
            multiplier: CONTRACT_MULTIPLIER,
            exchange: "OPRA".into(),
            currency: "USD".into(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetClass {
    Equity,
    Option,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: u64,
    pub ask_size: u64,
    pub ts: DateTime<Utc>,
    pub source: String,
    pub feed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTick {
    pub symbol: String,
    pub price: f64,
    pub size: u64,
    pub ts: DateTime<Utc>,
    pub conditions: Vec<String>,
    pub exchange: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSnapshot {
    pub symbol: String,
    pub as_of: DateTime<Utc>,
    pub features: std::collections::HashMap<String, f64>,
    pub quality: DataQualityStatus,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRun {
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub dataset_hash: String,
    pub total_trials: usize,
    pub candidates_evaluated: usize,
    pub candidates_promoted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyGenome {
    pub genome_id: String,
    pub strategy_id: String,
    pub generation: u32,
    pub parent_a: Option<String>,
    pub parent_b: Option<String>,
    pub mutation_type: String,
    pub parameters: std::collections::HashMap<String, f64>,
    pub genome_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate_id: String,
    pub genome: StrategyGenome,
    pub symbol: String,
    pub status: String,
    pub score: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub candidate_id: String,
    pub in_sample_sharpe: f64,
    pub out_of_sample_sharpe: f64,
    pub max_drawdown: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub psr: f64,
    pub dsr: f64,
    pub pbo: f64,
    pub promoted: bool,
    pub rejection_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioDecision {
    pub decision_id: Uuid,
    pub session_id: String,
    pub bot_id: String,
    pub symbol: String,
    pub target_exposure: f64,
    pub allocated_capital: f64,
    pub risk_budget: f64,
    pub priority: u32,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskDecision {
    pub decision_id: Uuid,
    pub approved: bool,
    pub reserved_capital: f64,
    pub reason: String,
    pub limits_checked: Vec<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRecord {
    pub client_order_id: Uuid,
    pub broker_order_id: Option<String>,
    pub symbol: String,
    pub side: OrderSide,
    pub qty: u32,
    pub filled_qty: u32,
    pub avg_fill_price: Option<f64>,
    pub status: OrderStatus,
    pub oms_state: OmsState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub execution_id: String,
    pub client_order_id: Uuid,
    pub broker_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub last_qty: u32,
    pub last_price: f64,
    pub cum_qty: u32,
    pub leaves_qty: u32,
    pub status: OrderStatus,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioSnapshot {
    pub as_of: DateTime<Utc>,
    pub cash: f64,
    pub equity: f64,
    pub buying_power: f64,
    pub gross_exposure: f64,
    pub net_exposure: f64,
    pub positions: Vec<Position>,
    pub daily_realized: f64,
    pub daily_unrealized: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationRecord {
    pub session_id: String,
    pub reconciled_at: DateTime<Utc>,
    pub broker_positions_count: usize,
    pub internal_positions_count: usize,
    pub matched: bool,
    pub discrepancies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningExperience {
    pub state_id: String,
    pub action: String,
    pub reward: f64,
    pub next_state_id: String,
    pub pnl: f64,
    pub slippage: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImprovementCandidate {
    pub version: u32,
    pub rationale: String,
    pub proposed_changes: Vec<String>,
    pub validation_status: String,
    pub promoted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceDecision {
    pub action: AuthorizationClass,
    pub authorized: bool,
    pub operator: String,
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

pub fn validate_point_in_time_event(
    event_ts: DateTime<Utc>,
    cutoff_ts: DateTime<Utc>,
    price: f64,
    bid: Option<f64>,
    ask: Option<f64>,
) -> Result<(), DomainError> {
    if event_ts > cutoff_ts {
        return Err(DomainError::Invalid(format!(
            "Point-in-time violation: event_ts {} > cutoff_ts {}",
            event_ts, cutoff_ts
        )));
    }
    if !price.is_finite() || price <= 0.0 {
        return Err(DomainError::Invalid(format!(
            "Invalid non-positive price: {}",
            price
        )));
    }
    if let (Some(b), Some(a)) = (bid, ask) {
        if !b.is_finite() || !a.is_finite() || b < 0.0 || a <= 0.0 {
            return Err(DomainError::Invalid("Invalid bid/ask quote values".into()));
        }
        if b > a {
            return Err(DomainError::Invalid(format!(
                "Crossed market detected: bid {} > ask {}",
                b, a
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: i32,
    pub avg_price: f64,
    pub mark: f64,
    pub opened_at: DateTime<Utc>,
    #[serde(default)]
    pub contract: Option<OptionContract>,
}
impl Position {
    pub fn new(
        symbol: impl Into<String>,
        qty: i32,
        avg_price: f64,
        mark: f64,
        opened_at: DateTime<Utc>,
    ) -> Self {
        let sym = symbol.into();
        let contract = OptionContract::from_occ(&sym);
        Self {
            symbol: sym,
            qty,
            avg_price,
            mark,
            opened_at,
            contract,
        }
    }

    pub fn unrealized_pnl(&self) -> f64 {
        let mult = self
            .contract
            .as_ref()
            .map(|c| c.multiplier)
            .unwrap_or(CONTRACT_MULTIPLIER);
        (self.mark - self.avg_price) * self.qty as f64 * mult
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
    Unknown,
    TrendingBull,
    TrendingBear,
    Range,
    HighVol,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub symbol: String,
    pub regime: Regime,
    pub volatility: f64,
    pub momentum: f64,
    pub volume_ratio: f64,
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    PreMarket,
    MarketOpen,
    MarketClosing,
    PostMarket,
    Learning,
    WaitingForNextSession,
    Trading,
    Analysis,
}
impl SessionPhase {
    pub fn at_hour(h: u32) -> Self {
        if h < 20 {
            Self::Trading
        } else {
            Self::Analysis
        }
    }

    pub fn is_trading_active(&self) -> bool {
        matches!(self, Self::MarketOpen | Self::Trading)
    }

    pub fn allows_new_entries(&self) -> bool {
        matches!(self, Self::MarketOpen | Self::Trading)
    }

    pub fn is_market_closing(&self) -> bool {
        matches!(self, Self::MarketClosing)
    }

    pub fn is_pre_market(&self) -> bool {
        matches!(self, Self::PreMarket | Self::Analysis)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorizationClass {
    ReadOnly,
    Research,
    Simulation,
    PaperExecution,
    LiveExecution,
    ImprovementProposal,
    ImprovementValidation,
    PromotionAuthorized,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernancePolicy {
    pub max_live_capital: f64,
    pub allow_live_execution: bool,
    pub allow_self_improvement: bool,
    pub max_daily_drawdown_limit: f64,
    pub require_red_team_approval: bool,
    pub active_authorizations: Vec<AuthorizationClass>,
}

impl Default for GovernancePolicy {
    fn default() -> Self {
        Self {
            max_live_capital: 0.0,
            allow_live_execution: false,
            allow_self_improvement: true,
            max_daily_drawdown_limit: 0.05,
            require_red_team_approval: true,
            active_authorizations: vec![
                AuthorizationClass::ReadOnly,
                AuthorizationClass::Research,
                AuthorizationClass::Simulation,
                AuthorizationClass::PaperExecution,
                AuthorizationClass::ImprovementProposal,
                AuthorizationClass::ImprovementValidation,
            ],
        }
    }
}

impl GovernancePolicy {
    pub fn is_authorized(&self, auth: AuthorizationClass) -> bool {
        self.active_authorizations.contains(&auth)
    }

    pub fn validate_execution_mode(&self, live: bool) -> Result<(), DomainError> {
        if live
            && (!self.allow_live_execution
                || !self.is_authorized(AuthorizationClass::LiveExecution))
        {
            return Err(DomainError::Invalid(
                "GOVERNANCE_BLOCKED: Live execution not authorized".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HiveState {
    Starting,
    PreMarket,
    Researching,
    Evaluating,
    Manufacturing,
    Ready,
    MarketOpen,
    Trading,
    Halting,
    MarketClosing,
    Reconciling,
    PostMarket,
    Learning,
    Improvement,
    Finalized,
    Degraded,
    Halted,
}

impl HiveState {
    pub fn is_operational(&self) -> bool {
        !matches!(self, Self::Degraded | Self::Halted)
    }

    pub fn can_trade(&self) -> bool {
        matches!(self, Self::Trading | Self::MarketOpen)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataQualityStatus {
    Valid,
    Stale,
    Duplicate,
    Anomaly,
    MissingField,
    FutureTimestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataProvenance {
    pub source: String,
    pub feed: String,
    pub request_ts: DateTime<Utc>,
    pub market_ts: DateTime<Utc>,
    pub symbol: String,
    pub quality: DataQualityStatus,
    pub latency_ms: i64,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid domain data: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

pub mod black_scholes {
    use super::{Greeks, OptionType};
    fn phi(x: f64) -> f64 {
        (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt()
    }
    // Abramowitz-Stegun 7.1.26 approximation. Maximum absolute error is ~7.5e-8.
    fn cdf(x: f64) -> f64 {
        if x.is_nan() {
            return f64::NAN;
        }
        if x == 0.0 {
            return 0.5;
        }
        let z = x.abs();
        let t = 1.0 / (1.0 + 0.2316419 * z);
        let poly = ((((1.330274429 * t - 1.821255978) * t + 1.781477937) * t - 0.356563782) * t
            + 0.319381530)
            * t;
        let v = 1.0 - phi(z) * poly;
        if x > 0.0 {
            v
        } else {
            1.0 - v
        }
    }
    fn valid_inputs(s: f64, k: f64, t: f64, r: f64, sigma: f64) -> bool {
        [s, k, t, r, sigma].iter().all(|v| v.is_finite())
            && s > 0.0
            && k > 0.0
            && t > 0.0
            && sigma > 0.0
    }
    pub fn price(s: f64, k: f64, t: f64, r: f64, sigma: f64, ty: OptionType) -> Option<f64> {
        if !valid_inputs(s, k, t, r, sigma) {
            return None;
        }
        let st = sigma * t.sqrt();
        let d1 = ((s / k).ln() + (r + 0.5 * sigma * sigma) * t) / st;
        let d2 = d1 - st;
        let disc = (-r * t).exp();
        let p = match ty {
            OptionType::Call => s * cdf(d1) - k * disc * cdf(d2),
            OptionType::Put => k * disc * cdf(-d2) - s * cdf(-d1),
        };
        p.is_finite().then_some(p.max(0.0))
    }
    pub fn greeks(s: f64, k: f64, t: f64, r: f64, sigma: f64, ty: OptionType) -> Option<Greeks> {
        price(s, k, t, r, sigma, ty)?;
        let st = sigma * t.sqrt();
        let d1 = ((s / k).ln() + (r + 0.5 * sigma * sigma) * t) / st;
        let d2 = d1 - st;
        let disc = (-r * t).exp();
        let nd = phi(d1);
        let delta = match ty {
            OptionType::Call => cdf(d1),
            OptionType::Put => cdf(d1) - 1.0,
        };
        let gamma = nd / (s * st);
        let vega = s * nd * t.sqrt() / 100.0;
        let theta_year = match ty {
            OptionType::Call => -(s * nd * sigma) / (2.0 * t.sqrt()) - r * k * disc * cdf(d2),
            OptionType::Put => -(s * nd * sigma) / (2.0 * t.sqrt()) + r * k * disc * cdf(-d2),
        };
        let rho_year = match ty {
            OptionType::Call => k * t * disc * cdf(d2) / 100.0,
            OptionType::Put => -k * t * disc * cdf(-d2) / 100.0,
        };
        let g = Greeks {
            delta,
            gamma,
            theta: theta_year / 365.0,
            vega,
            rho: rho_year,
        };
        g.valid().then_some(g)
    }
    pub fn implied_volatility(
        market: f64,
        s: f64,
        k: f64,
        t: f64,
        r: f64,
        ty: OptionType,
    ) -> Option<f64> {
        if !market.is_finite() || market <= 0.0 || !valid_inputs(s, k, t, r, 0.2) {
            return None;
        }
        let intrinsic = match ty {
            OptionType::Call => (s - k).max(0.0),
            OptionType::Put => (k - s).max(0.0),
        };
        if market + 1e-10 < intrinsic {
            return None;
        }
        let mut lo = 1e-6;
        let mut hi = 5.0;
        for _ in 0..100 {
            let mid = (lo + hi) / 2.0;
            let p = price(s, k, t, r, mid, ty)?;
            if (p - market).abs() < 1e-8 {
                return Some(mid);
            }
            if p > market {
                hi = mid
            } else {
                lo = mid
            }
        }
        Some((lo + hi) / 2.0)
    }
}

pub mod occ {
    use super::OptionType;
    #[derive(Debug, Clone, PartialEq)]
    pub struct Parsed {
        pub underlying: String,
        pub year: u32,
        pub month: u32,
        pub day: u32,
        pub option_type: OptionType,
        pub strike: f64,
    }
    pub fn parse(symbol: &str) -> Option<Parsed> {
        if symbol.len() < 15 {
            return None;
        }
        let off = symbol.len() - 15;
        let root = symbol[..off].trim().to_string();
        let date = &symbol[off..off + 6];
        let typ = &symbol[off + 6..off + 7];
        let strike_raw = &symbol[off + 7..];
        let year = 2000 + date[0..2].parse::<u32>().ok()?;
        let month = date[2..4].parse::<u32>().ok()?;
        let day = date[4..6].parse::<u32>().ok()?;
        let option_type = match typ {
            "C" => OptionType::Call,
            "P" => OptionType::Put,
            _ => return None,
        };
        let strike = strike_raw.parse::<u64>().ok()? as f64 / 1000.0;
        Some(Parsed {
            underlying: root,
            year,
            month,
            day,
            option_type,
            strike,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_occ_parsing_and_contract_generation() {
        let sym = "SPY260904C00500000";
        let parsed = occ::parse(sym).expect("must parse standard OCC");
        assert_eq!(parsed.underlying, "SPY");
        assert_eq!(parsed.year, 2026);
        assert_eq!(parsed.month, 9);
        assert_eq!(parsed.day, 4);
        assert_eq!(parsed.option_type, OptionType::Call);
        assert_eq!(parsed.strike, 500.0);

        let contract = OptionContract::from_occ(sym).expect("must generate OptionContract");
        assert_eq!(contract.underlying, "SPY");
        assert_eq!(contract.strike, 500.0);
        assert_eq!(contract.multiplier, 100.0);
        assert_eq!(contract.option_type, OptionType::Call);

        let pos = Position::new(sym, 2, 5.0, 6.0, Utc::now());
        assert_eq!(pos.unrealized_pnl(), (6.0 - 5.0) * 2.0 * 100.0);
    }

    #[test]
    fn test_point_in_time_event_validation() {
        let now = Utc::now();
        let past = now - chrono::Duration::minutes(5);
        let future = now + chrono::Duration::minutes(5);

        // Valid past event
        assert!(validate_point_in_time_event(past, now, 100.0, Some(99.5), Some(100.5)).is_ok());

        // Future timestamp violation
        assert!(validate_point_in_time_event(future, now, 100.0, Some(99.5), Some(100.5)).is_err());

        // Negative price violation
        assert!(validate_point_in_time_event(past, now, -10.0, None, None).is_err());

        // Crossed market violation
        assert!(validate_point_in_time_event(past, now, 100.0, Some(101.0), Some(99.0)).is_err());
    }

    #[test]
    fn test_option_order_action_mapping() {
        let bto = OptionOrderAction::BuyToOpen;
        assert_eq!(bto.as_str(), "buy_to_open");
        assert_eq!(bto.order_side(), OrderSide::Buy);
        assert!(bto.is_opening());
        assert!(!bto.is_closing());

        let stc = OptionOrderAction::SellToClose;
        assert_eq!(stc.as_str(), "sell_to_close");
        assert_eq!(stc.order_side(), OrderSide::Sell);
        assert!(!stc.is_opening());
        assert!(stc.is_closing());

        let intent_open = OrderIntent {
            client_order_id: Uuid::new_v4(),
            symbol: "SPY260904C00500000".into(),
            side: OrderSide::Buy,
            qty: 1,
            limit_price: Some(5.0),
            reduce_only: false,
            strategy_id: "strat1".into(),
            created_at: Utc::now(),
            order_hash: "hash".into(),
            bot_id: None,
            session_id: None,
            decision_id: None,
            oms_state: None,
            option_action: None,
        };
        assert_eq!(
            intent_open.resolve_option_action(),
            OptionOrderAction::BuyToOpen
        );

        let intent_close = OrderIntent {
            reduce_only: true,
            ..intent_open
        };
        assert_eq!(
            intent_close.resolve_option_action(),
            OptionOrderAction::BuyToClose
        );
    }

    #[test]
    fn test_black_scholes_pricing_and_greeks() {
        let spot = 100.0;
        let strike = 100.0;
        let t = 30.0 / 365.0;
        let r = 0.05;
        let sigma = 0.20;

        let call_price = black_scholes::price(spot, strike, t, r, sigma, OptionType::Call).unwrap();
        let put_price = black_scholes::price(spot, strike, t, r, sigma, OptionType::Put).unwrap();
        assert!(call_price > 0.0);
        assert!(put_price > 0.0);

        let call_greeks =
            black_scholes::greeks(spot, strike, t, r, sigma, OptionType::Call).unwrap();
        assert!(call_greeks.delta > 0.45 && call_greeks.delta < 0.65);
        assert!(call_greeks.gamma > 0.0);
        assert!(call_greeks.vega > 0.0);

        let put_greeks = black_scholes::greeks(spot, strike, t, r, sigma, OptionType::Put).unwrap();
        assert!(put_greeks.delta < 0.0 && put_greeks.delta > -1.0);
    }
}
