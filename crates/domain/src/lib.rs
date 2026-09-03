use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
pub enum OrderStatus {
    New,
    Accepted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
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
}
impl OrderIntent {
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: i32,
    pub avg_price: f64,
    pub mark: f64,
    pub opened_at: DateTime<Utc>,
}
impl Position {
    pub fn unrealized_pnl(&self) -> f64 {
        (self.mark - self.avg_price) * self.qty as f64 * CONTRACT_MULTIPLIER
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
