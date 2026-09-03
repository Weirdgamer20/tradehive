use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use th_domain::{OrderIntent, OrderSide, OrderStatus, Position, CONTRACT_MULTIPLIER};
pub use th_risk::{PortfolioRisk, RiskApproval, RiskGovernor};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("broker: {0}")]
    Broker(String),
    #[error("risk: {0}")]
    Risk(String),
    #[error("duplicate order: {0}")]
    Duplicate(Uuid),
    #[error("kill switch active")]
    KillSwitch,
    #[error("invalid order: {0}")]
    Invalid(String),
    #[error("unsafe live configuration: {0}")]
    UnsafeLive(String),
    #[error("market is closed: {0}")]
    MarketClosed(String),
}
#[async_trait]
pub trait Broker: Send + Sync {
    async fn submit(&self, order: &OrderIntent) -> Result<BrokerOrder, ExecutionError>;
    async fn get_order(&self, broker_order_id: &str) -> Result<BrokerOrder, ExecutionError>;
    async fn find_by_client_order_id(
        &self,
        client_order_id: Uuid,
    ) -> Result<Option<BrokerOrder>, ExecutionError>;
    async fn cancel(&self, broker_order_id: &str) -> Result<(), ExecutionError>;
    /// Returns all working (non-terminal) orders known to the broker.
    async fn list_open_orders(&self) -> Result<Vec<BrokerOrder>, ExecutionError>;
    /// Attempts to cancel all working orders. Returns the broker_order_ids of
    /// orders that were successfully cancelled. Partial success is accepted;
    /// callers must verify with `list_open_orders` afterward.
    async fn cancel_all_orders(&self) -> Result<Vec<String>, ExecutionError>;
    async fn positions(&self) -> Result<Vec<Position>, ExecutionError>;
    async fn account(&self) -> Result<AccountSnapshot, ExecutionError>;
    async fn clock(&self) -> Result<MarketClock, ExecutionError>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerOrder {
    pub broker_order_id: String,
    pub client_order_id: Uuid,
    pub status: OrderStatus,
    pub filled_qty: u32,
    pub filled_avg_price: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub equity: f64,
    pub cash: f64,
    pub buying_power: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketClock {
    pub is_open: bool,
    pub timestamp: chrono::DateTime<Utc>,
    pub next_open: Option<chrono::DateTime<Utc>>,
    pub next_close: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperExecutionConfig {
    pub slippage_bps: f64,
    pub spread_bps: f64,
    pub partial_fill_pct: Option<f64>,
    pub reject_probability: f64,
    pub simulate_latency_ms: u64,
}

impl Default for PaperExecutionConfig {
    fn default() -> Self {
        Self {
            slippage_bps: 2.0,
            spread_bps: 10.0,
            partial_fill_pct: None,
            reject_probability: 0.0,
            simulate_latency_ms: 0,
        }
    }
}

#[derive(Clone)]
pub struct PaperBroker {
    inner: Arc<Mutex<PaperState>>,
}
#[derive(Default)]
struct PaperState {
    orders: HashMap<Uuid, BrokerOrder>,
    positions: HashMap<String, Position>,
    cash: f64,
    mock_clock_open: Option<bool>,
    config: PaperExecutionConfig,
}
impl PaperBroker {
    pub fn new(cash: f64) -> Self {
        Self::with_config(cash, PaperExecutionConfig::default())
    }

    pub fn with_config(cash: f64, config: PaperExecutionConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PaperState {
                cash,
                config,
                ..Default::default()
            })),
        }
    }
    pub fn set_mock_clock(&self, is_open: bool) {
        if let Ok(mut state) = self.inner.lock() {
            state.mock_clock_open = Some(is_open);
        }
    }
    pub fn seed_position(&self, p: Position) {
        if let Ok(mut state) = self.inner.lock() {
            state.positions.insert(p.symbol.clone(), p);
        }
    }
}
#[async_trait]
impl Broker for PaperBroker {
    async fn submit(&self, o: &OrderIntent) -> Result<BrokerOrder, ExecutionError> {
        o.validate()
            .map_err(|e| ExecutionError::Invalid(e.to_string()))?;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?;
        if let Some(existing) = s.orders.get(&o.client_order_id) {
            return Err(ExecutionError::Duplicate(existing.client_order_id));
        }

        let base_px = o
            .limit_price
            .ok_or_else(|| ExecutionError::Invalid("paper broker requires limit price".into()))?;
        if !base_px.is_finite() || base_px <= 0.0 {
            return Err(ExecutionError::Invalid(
                "invalid paper execution price".into(),
            ));
        }

        // Apply realistic slippage & spread crossing
        let slip_bps = s.config.slippage_bps;
        let spread_bps = s.config.spread_bps;
        let px = match o.side {
            OrderSide::Buy => base_px * (1.0 + (slip_bps + spread_bps / 2.0) / 10_000.0),
            OrderSide::Sell => base_px * (1.0 - (slip_bps + spread_bps / 2.0) / 10_000.0),
        };

        let existing_qty = s.positions.get(&o.symbol).map(|p| p.qty).unwrap_or(0);
        if o.reduce_only && o.side != OrderSide::Sell {
            return Err(ExecutionError::Invalid(
                "reduce-only orders must be sells".into(),
            ));
        }
        if o.reduce_only && existing_qty < o.qty as i32 {
            return Err(ExecutionError::Invalid(
                "reduce-only quantity exceeds position".into(),
            ));
        }
        if o.side == OrderSide::Sell && !o.reduce_only {
            return Err(ExecutionError::Invalid(
                "naked option sells are disabled".into(),
            ));
        }

        // Calculate fill quantity (handling realistic partial fills)
        let fill_qty = if let Some(pct) = s.config.partial_fill_pct {
            ((o.qty as f64 * pct).floor() as u32).clamp(1, o.qty)
        } else {
            o.qty
        };

        let status = if fill_qty < o.qty {
            OrderStatus::PartiallyFilled
        } else {
            OrderStatus::Filled
        };

        let cash_delta = px * fill_qty as f64 * CONTRACT_MULTIPLIER;
        if o.side == OrderSide::Buy && s.cash < cash_delta {
            return Err(ExecutionError::Broker("insufficient cash".into()));
        }
        if o.side == OrderSide::Buy {
            s.cash -= cash_delta
        } else {
            s.cash += cash_delta
        };
        let now = Utc::now();
        let bo = BrokerOrder {
            broker_order_id: Uuid::new_v4().to_string(),
            client_order_id: o.client_order_id,
            status,
            filled_qty: fill_qty,
            filled_avg_price: Some(px),
        };
        if o.side == OrderSide::Buy {
            let e = s.positions.entry(o.symbol.clone()).or_insert(Position {
                symbol: o.symbol.clone(),
                qty: 0,
                avg_price: px,
                mark: px,
                opened_at: now,
                contract: th_domain::OptionContract::from_occ(&o.symbol),
            });
            let old = e.qty.max(0) as f64;
            let fill_f = fill_qty as f64;
            e.avg_price = if old > 0.0 {
                (e.avg_price * old + px * fill_f) / (old + fill_f)
            } else {
                px
            };
            e.qty += fill_qty as i32;
            e.mark = px
        } else if let Some(e) = s.positions.get_mut(&o.symbol) {
            e.qty -= fill_qty as i32;
            e.mark = px;
            if e.qty <= 0 {
                s.positions.remove(&o.symbol);
            }
        }
        s.orders.insert(o.client_order_id, bo.clone());
        Ok(bo)
    }
    async fn get_order(&self, id: &str) -> Result<BrokerOrder, ExecutionError> {
        self.inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?
            .orders
            .values()
            .find(|x| x.broker_order_id == id)
            .cloned()
            .ok_or_else(|| ExecutionError::Broker("order not found".into()))
    }
    async fn find_by_client_order_id(
        &self,
        id: Uuid,
    ) -> Result<Option<BrokerOrder>, ExecutionError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?
            .orders
            .get(&id)
            .cloned())
    }
    async fn cancel(&self, id: &str) -> Result<(), ExecutionError> {
        let mut s = self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?;
        if let Some(x) = s.orders.values_mut().find(|x| x.broker_order_id == id) {
            if matches!(x.status, OrderStatus::New | OrderStatus::Accepted) {
                x.status = OrderStatus::Cancelled
            };
            Ok(())
        } else {
            Err(ExecutionError::Broker("order not found".into()))
        }
    }
    async fn positions(&self) -> Result<Vec<Position>, ExecutionError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?
            .positions
            .values()
            .cloned()
            .collect())
    }
    async fn clock(&self) -> Result<MarketClock, ExecutionError> {
        let now = Utc::now();
        let s = self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?;
        let is_open = s
            .mock_clock_open
            .unwrap_or_else(|| th_domain::MarketSessionClock::default().is_open(now));
        Ok(MarketClock {
            is_open,
            timestamp: now,
            next_open: None,
            next_close: None,
        })
    }
    async fn account(&self) -> Result<AccountSnapshot, ExecutionError> {
        let s = self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?;
        let pos_value = s
            .positions
            .values()
            .map(|p| p.mark * p.qty as f64 * CONTRACT_MULTIPLIER)
            .sum::<f64>();
        Ok(AccountSnapshot {
            equity: s.cash + pos_value,
            cash: s.cash,
            buying_power: s.cash.max(0.0),
        })
    }
    async fn list_open_orders(&self) -> Result<Vec<BrokerOrder>, ExecutionError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?
            .orders
            .values()
            .filter(|o| {
                matches!(
                    o.status,
                    OrderStatus::New | OrderStatus::Accepted | OrderStatus::PartiallyFilled
                )
            })
            .cloned()
            .collect())
    }
    async fn cancel_all_orders(&self) -> Result<Vec<String>, ExecutionError> {
        let mut s = self
            .inner
            .lock()
            .map_err(|_| ExecutionError::Broker("paper mutex poisoned".into()))?;
        let mut cancelled = Vec::new();
        for order in s.orders.values_mut() {
            if matches!(
                order.status,
                OrderStatus::New | OrderStatus::Accepted | OrderStatus::PartiallyFilled
            ) {
                order.status = OrderStatus::Cancelled;
                cancelled.push(order.broker_order_id.clone());
            }
        }
        Ok(cancelled)
    }
}

#[derive(Clone)]
pub struct AlpacaBroker {
    client: Client,
    base_url: String,
    key: String,
    secret: String,
    live: bool,
}
impl AlpacaBroker {
    pub fn new(
        base_url: String,
        key: String,
        secret: String,
        live: bool,
    ) -> Result<Self, ExecutionError> {
        if key.trim().is_empty() || secret.trim().is_empty() {
            return Err(ExecutionError::UnsafeLive("missing credentials".into()));
        }
        if live && std::env::var("TRADING_HIVE_LIVE_CONFIRM").ok().as_deref() != Some("YES") {
            return Err(ExecutionError::UnsafeLive(
                "TRADING_HIVE_LIVE_CONFIRM=YES required".into(),
            ));
        }
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        let base_url = base_url
            .trim_end_matches('/')
            .trim_end_matches("/v2")
            .to_string();
        Ok(Self {
            client,
            base_url,
            key,
            secret,
            live,
        })
    }
    fn headers(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        r.header("APCA-API-KEY-ID", &self.key)
            .header("APCA-API-SECRET-KEY", &self.secret)
    }
}
#[derive(Deserialize)]
struct AlpacaOrderResp {
    id: String,
    client_order_id: String,
    status: String,
    filled_qty: String,
    filled_avg_price: Option<String>,
}
fn parse_status(s: &str) -> OrderStatus {
    match s {
        "new" => OrderStatus::New,
        "accepted" => OrderStatus::Accepted,
        "partially_filled" => OrderStatus::PartiallyFilled,
        "filled" => OrderStatus::Filled,
        "canceled" | "cancelled" => OrderStatus::Cancelled,
        "rejected" => OrderStatus::Rejected,
        "expired" => OrderStatus::Expired,
        "replaced" => OrderStatus::Replaced,
        "pending_cancel" => OrderStatus::PendingCancel,
        _ => OrderStatus::Unknown,
    }
}
#[async_trait]
impl Broker for AlpacaBroker {
    async fn submit(&self, o: &OrderIntent) -> Result<BrokerOrder, ExecutionError> {
        if self.live && std::env::var("TRADING_HIVE_LIVE_CONFIRM").ok().as_deref() != Some("YES") {
            return Err(ExecutionError::UnsafeLive(
                "live confirmation disappeared".into(),
            ));
        }
        let side = match o.side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        };
        let position_intent = match o.side {
            OrderSide::Buy => {
                if o.reduce_only {
                    "buy_to_close"
                } else {
                    "buy_to_open"
                }
            }
            OrderSide::Sell => {
                if o.reduce_only {
                    "sell_to_close"
                } else {
                    "sell_to_open"
                }
            }
        };
        let mut body = serde_json::json!({
            "symbol": o.symbol,
            "qty": o.qty.to_string(),
            "side": side,
            "type": if o.limit_price.is_some() { "limit" } else { "market" },
            "time_in_force": "day",
            "client_order_id": o.client_order_id.to_string(),
            "order_class": "simple",
            "position_intent": position_intent,
        });
        if let Some(p) = o.limit_price {
            body["limit_price"] = serde_json::json!(p);
        }
        let url = format!("{}/v2/orders", self.base_url.trim_end_matches('/'));
        let r = self
            .headers(self.client.post(url).json(&body))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "submit HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let x: AlpacaOrderResp = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        let client = Uuid::parse_str(&x.client_order_id)
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        Ok(BrokerOrder {
            broker_order_id: x.id,
            client_order_id: client,
            status: parse_status(&x.status),
            filled_qty: x
                .filled_qty
                .parse()
                .map_err(|_| ExecutionError::Broker("invalid filled_qty from broker".into()))?,
            filled_avg_price: x.filled_avg_price.and_then(|v| v.parse().ok()),
        })
    }
    async fn find_by_client_order_id(
        &self,
        id: Uuid,
    ) -> Result<Option<BrokerOrder>, ExecutionError> {
        let url = format!(
            "{}/v2/orders?status=all&limit=50&client_order_id={}",
            self.base_url.trim_end_matches('/'),
            id
        );
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "find client order HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let xs: Vec<AlpacaOrderResp> = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        let Some(x) = xs.into_iter().find(|x| x.client_order_id == id.to_string()) else {
            return Ok(None);
        };
        Ok(Some(BrokerOrder {
            broker_order_id: x.id,
            client_order_id: Uuid::parse_str(&x.client_order_id)
                .map_err(|e| ExecutionError::Broker(e.to_string()))?,
            status: parse_status(&x.status),
            filled_qty: x
                .filled_qty
                .parse()
                .map_err(|_| ExecutionError::Broker("invalid filled_qty from broker".into()))?,
            filled_avg_price: x.filled_avg_price.and_then(|v| v.parse().ok()),
        }))
    }
    async fn get_order(&self, id: &str) -> Result<BrokerOrder, ExecutionError> {
        let url = format!("{}/v2/orders/{}", self.base_url.trim_end_matches('/'), id);
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "get order HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let x: AlpacaOrderResp = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        Ok(BrokerOrder {
            broker_order_id: x.id,
            client_order_id: Uuid::parse_str(&x.client_order_id)
                .map_err(|e| ExecutionError::Broker(e.to_string()))?,
            status: parse_status(&x.status),
            filled_qty: x
                .filled_qty
                .parse()
                .map_err(|_| ExecutionError::Broker("invalid filled_qty from broker".into()))?,
            filled_avg_price: x.filled_avg_price.and_then(|v| v.parse().ok()),
        })
    }
    async fn cancel(&self, id: &str) -> Result<(), ExecutionError> {
        let url = format!("{}/v2/orders/{}", self.base_url.trim_end_matches('/'), id);
        let r = self
            .headers(self.client.delete(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "cancel HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }
    async fn positions(&self) -> Result<Vec<Position>, ExecutionError> {
        let url = format!("{}/v2/positions", self.base_url.trim_end_matches('/'));
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "positions HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let xs: Vec<serde_json::Value> = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        Ok(xs
            .into_iter()
            .filter_map(|x| {
                let sym = x.get("symbol")?.as_str()?.to_string();
                Some(Position {
                    qty: x.get("qty")?.as_str()?.parse().ok()?,
                    avg_price: x.get("avg_entry_price")?.as_str()?.parse().ok()?,
                    mark: x
                        .get("current_price")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.0),
                    opened_at: Utc::now(),
                    contract: th_domain::OptionContract::from_occ(&sym),
                    symbol: sym,
                })
            })
            .collect())
    }
    async fn clock(&self) -> Result<MarketClock, ExecutionError> {
        let url = format!("{}/v2/clock", self.base_url.trim_end_matches('/'));
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "clock HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let x: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        let parse_dt = |k: &str| {
            x.get(k)
                .and_then(|v| v.as_str())
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc))
        };
        Ok(MarketClock {
            is_open: x.get("is_open").and_then(|v| v.as_bool()).unwrap_or(false),
            timestamp: Utc::now(),
            next_open: parse_dt("next_open"),
            next_close: parse_dt("next_close"),
        })
    }
    async fn account(&self) -> Result<AccountSnapshot, ExecutionError> {
        let url = format!("{}/v2/account", self.base_url.trim_end_matches('/'));
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "account HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let x: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        let parse = |k: &str| {
            x.get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0)
        };
        Ok(AccountSnapshot {
            equity: parse("equity"),
            cash: parse("cash"),
            buying_power: parse("buying_power"),
        })
    }
    async fn list_open_orders(&self) -> Result<Vec<BrokerOrder>, ExecutionError> {
        let url = format!(
            "{}/v2/orders?status=open&limit=500",
            self.base_url.trim_end_matches('/')
        );
        let r = self
            .headers(self.client.get(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ExecutionError::Broker(format!(
                "list_open_orders HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        let xs: Vec<AlpacaOrderResp> = r
            .json()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        xs.into_iter()
            .map(|x| {
                Ok(BrokerOrder {
                    broker_order_id: x.id,
                    client_order_id: Uuid::parse_str(&x.client_order_id)
                        .map_err(|e| ExecutionError::Broker(e.to_string()))?,
                    status: parse_status(&x.status),
                    filled_qty: x
                        .filled_qty
                        .parse()
                        .map_err(|_| ExecutionError::Broker("invalid filled_qty".into()))?,
                    filled_avg_price: x.filled_avg_price.and_then(|v| v.parse().ok()),
                })
            })
            .collect()
    }
    async fn cancel_all_orders(&self) -> Result<Vec<String>, ExecutionError> {
        // Alpaca DELETE /v2/orders returns 207 Multi-Status with per-order results.
        let url = format!("{}/v2/orders", self.base_url.trim_end_matches('/'));
        let r = self
            .headers(self.client.delete(url))
            .send()
            .await
            .map_err(|e| ExecutionError::Broker(e.to_string()))?;
        // 207 is the success code for multi-status cancel. 422 = no open orders (success).
        if r.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Ok(vec![]);
        }
        if !r.status().is_success() && r.status().as_u16() != 207 {
            return Err(ExecutionError::Broker(format!(
                "cancel_all_orders HTTP {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            )));
        }
        // Parse 207 multi-status body: [{"id": "...", "status": 200}]
        let body: Vec<serde_json::Value> = r.json().await.unwrap_or_default();
        Ok(body
            .into_iter()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                let status = v.get("status")?.as_u64().unwrap_or(0);
                if status == 200 {
                    Some(id)
                } else {
                    None
                }
            })
            .collect())
    }
}

pub fn order_hash(o: &OrderIntent) -> String {
    let mut h = Sha256::new();
    h.update(o.symbol.as_bytes());
    h.update([0]);
    h.update(format!("{:?}", o.side).as_bytes());
    h.update([0]);
    h.update(o.qty.to_le_bytes());
    if let Some(p) = o.limit_price {
        h.update(p.to_le_bytes())
    }
    h.update([0]);
    h.update([o.reduce_only as u8]);
    h.update([0]);
    h.update(o.strategy_id.as_bytes());
    format!("{:x}", h.finalize())
}
pub fn make_order(mut o: OrderIntent) -> Result<OrderIntent, ExecutionError> {
    o.validate()
        .map_err(|e| ExecutionError::Invalid(e.to_string()))?;
    o.order_hash = order_hash(&o);
    if o.order_hash.len() != 64 {
        return Err(ExecutionError::Invalid(
            "failed to create order hash".into(),
        ));
    }
    Ok(o)
}

pub struct ExecutionEngine<B: Broker> {
    broker: B,
    risk: RiskGovernor,
    seen: std::collections::HashSet<Uuid>,
}
impl<B: Broker> ExecutionEngine<B> {
    pub fn new(broker: B, risk: RiskGovernor) -> Self {
        Self {
            broker,
            risk,
            seen: std::collections::HashSet::new(),
        }
    }
    pub fn broker_ref(&self) -> &B {
        &self.broker
    }
    pub fn risk(&self) -> &RiskGovernor {
        &self.risk
    }
    pub fn risk_mut(&mut self) -> &mut RiskGovernor {
        &mut self.risk
    }
    pub fn is_killed(&self) -> bool {
        self.risk.is_killed()
    }
    pub async fn execute(
        &mut self,
        o: OrderIntent,
        price: f64,
        spread_bps: f64,
        portfolio: &PortfolioRisk,
    ) -> Result<(BrokerOrder, RiskApproval), ExecutionError> {
        let broker_clock = self.broker.clock().await?;
        if !broker_clock.is_open && !o.reduce_only {
            return Err(ExecutionError::MarketClosed("MARKET_CLOSED".into()));
        }
        let o = make_order(o)?;
        if self.seen.contains(&o.client_order_id) {
            return Err(ExecutionError::Duplicate(o.client_order_id));
        }
        if let Some(existing) = self
            .broker
            .find_by_client_order_id(o.client_order_id)
            .await?
        {
            self.seen.insert(o.client_order_id);
            return Ok((
                existing,
                RiskApproval {
                    token: Uuid::nil(),
                    approved_at: Utc::now(),
                    expires_at: Utc::now(),
                    client_order_id: o.client_order_id,
                    order_hash: o.order_hash.clone(),
                    reason: "BROKER_RECONCILED_EXISTING_ORDER".into(),
                },
            ));
        }
        let a = self
            .risk
            .authorize(&o, price, spread_bps, portfolio)
            .map_err(|e| ExecutionError::Risk(e.to_string()))?;
        self.risk
            .validate_token(&a, o.client_order_id, &o.order_hash)
            .map_err(|e| ExecutionError::Risk(e.to_string()))?;
        let bo = self.broker.submit(&o).await?;
        self.seen.insert(o.client_order_id);
        Ok((bo, a))
    }
    pub async fn broker(&self) -> Result<AccountSnapshot, ExecutionError> {
        self.broker.account().await
    }
    pub async fn positions(&self) -> Result<Vec<Position>, ExecutionError> {
        self.broker.positions().await
    }
    pub async fn reconcile_order(
        &self,
        broker_order_id: &str,
    ) -> Result<BrokerOrder, ExecutionError> {
        self.broker.get_order(broker_order_id).await
    }
    pub async fn clock(&self) -> Result<MarketClock, ExecutionError> {
        self.broker.clock().await
    }
    pub async fn wait_for_fill(
        &self,
        broker_order_id: &str,
        timeout: std::time::Duration,
    ) -> Result<BrokerOrder, ExecutionError> {
        let start = std::time::Instant::now();
        let mut last_order = self.broker.get_order(broker_order_id).await?;
        while start.elapsed() < timeout {
            if last_order.status == OrderStatus::Filled
                || last_order.status == OrderStatus::Cancelled
                || last_order.status == OrderStatus::Rejected
            {
                return Ok(last_order);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            last_order = self.broker.get_order(broker_order_id).await?;
        }
        Ok(last_order)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub matched: bool,
    pub internal_count: usize,
    pub broker_count: usize,
    pub missing_internal: Vec<String>,
    pub missing_broker: Vec<String>,
}
pub fn reconcile_positions(internal: &[Position], broker: &[Position]) -> ReconciliationReport {
    let i: HashMap<_, _> = internal.iter().map(|p| (&p.symbol, p.qty)).collect();
    let b: HashMap<_, _> = broker.iter().map(|p| (&p.symbol, p.qty)).collect();
    let mut mi = Vec::new();
    let mut mb = Vec::new();
    for k in i.keys() {
        if !b.contains_key(k) || i[k] != b[k] {
            mi.push((*k).clone())
        }
    }
    for k in b.keys() {
        if !i.contains_key(k) {
            mb.push((*k).clone())
        }
    }
    ReconciliationReport {
        matched: mi.is_empty() && mb.is_empty(),
        internal_count: i.len(),
        broker_count: b.len(),
        missing_internal: mi,
        missing_broker: mb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_reconcile_positions_matching_and_mismatches() {
        let pos1 = Position::new("SPY260904C00500000", 2, 5.0, 5.5, Utc::now());
        let pos2 = Position::new("QQQ260904P00400000", 1, 3.0, 3.2, Utc::now());

        // Perfect match
        let rep1 = reconcile_positions(std::slice::from_ref(&pos1), std::slice::from_ref(&pos1));
        assert!(rep1.matched);
        assert_eq!(rep1.internal_count, 1);
        assert_eq!(rep1.broker_count, 1);

        // Discrepancy in quantity
        let mut pos1_diff = pos1.clone();
        pos1_diff.qty = 3;
        let rep2 = reconcile_positions(std::slice::from_ref(&pos1), &[pos1_diff]);
        assert!(!rep2.matched);
        assert_eq!(
            rep2.missing_internal,
            vec!["SPY260904C00500000".to_string()]
        );

        // Broker has extra position
        let rep3 = reconcile_positions(std::slice::from_ref(&pos1), &[pos1.clone(), pos2.clone()]);
        assert!(!rep3.matched);
        assert_eq!(rep3.missing_broker, vec!["QQQ260904P00400000".to_string()]);
    }

    #[tokio::test]
    async fn test_paper_broker_order_lifecycle() {
        let broker = PaperBroker::new(10_000.0);
        let intent = OrderIntent {
            client_order_id: Uuid::new_v4(),
            symbol: "SPY260904C00500000".into(),
            side: OrderSide::Buy,
            qty: 2,
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

        let submitted = broker.submit(&intent).await.expect("submit must succeed");
        assert_eq!(submitted.client_order_id, intent.client_order_id);

        let positions = broker.positions().await.expect("positions must succeed");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "SPY260904C00500000");
        assert_eq!(positions[0].qty, 2);
    }
}
