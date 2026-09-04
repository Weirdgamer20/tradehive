use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration as StdDuration;
use th_domain::{Bar, CandleBuilder, Greeks, OptionChain, OptionQuote};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketDataError {
    #[error("http: {0}")]
    Http(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("domain: {0}")]
    Domain(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("stale data: {0}")]
    Stale(String),
}
#[async_trait]
pub trait MarketDataProvider: Send + Sync {
    async fn bars(
        &self,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Bar>, MarketDataError>;
    async fn option_chain(
        &self,
        underlying: &str,
        as_of: DateTime<Utc>,
    ) -> Result<OptionChain, MarketDataError>;
    async fn news(
        &self,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<NewsEvent>, MarketDataError>;
    /// Returns the most active tradable US equity symbols by volume.
    /// Used by the Hive for Stage 1 universe discovery during pre-market.
    async fn most_actives(&self, limit: usize) -> Result<Vec<String>, MarketDataError>;
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewsEvent {
    pub id: String,
    pub symbol: String,
    pub headline: String,
    pub summary: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub url: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRisk {
    None,
    Elevated,
    Block,
}
pub fn classify_news_risk(events: &[NewsEvent]) -> NewsRisk {
    let hard = [
        "halt",
        "bankrupt",
        "bankruptcy",
        "fraud",
        "restatement",
        "delist",
        "offering",
        "sec charges",
        "fda rejects",
        "merger termination",
    ];
    let elevated = [
        "earnings",
        "guidance",
        "acquisition",
        "merger",
        "lawsuit",
        "downgrade",
        "upgrade",
        "investigation",
        "offering",
    ];
    let mut level = NewsRisk::None;
    for e in events {
        let text = format!("{} {}", e.headline.to_lowercase(), e.summary.to_lowercase());
        if hard.iter().any(|k| text.contains(k)) {
            return NewsRisk::Block;
        }
        if elevated.iter().any(|k| text.contains(k)) {
            level = NewsRisk::Elevated;
        }
    }
    level
}
#[derive(Debug, Clone)]
pub struct AlpacaConfig {
    pub key: String,
    pub secret: String,
    pub data_url: String,
    pub news_url: String,
    pub options_feed: Option<String>,
    pub stocks_feed: Option<String>,
}
impl AlpacaConfig {
    pub fn from_env() -> Result<Self, MarketDataError> {
        let key = std::env::var("APCA_API_KEY_ID")
            .map_err(|_| MarketDataError::Unavailable("APCA_API_KEY_ID missing".into()))?;
        let secret = std::env::var("APCA_API_SECRET_KEY")
            .map_err(|_| MarketDataError::Unavailable("APCA_API_SECRET_KEY missing".into()))?;
        let stocks_feed = std::env::var("ALPACA_STOCKS_FEED")
            .ok()
            .or_else(|| std::env::var("ALPACA_FEED").ok())
            .or_else(|| Some("iex".into()));
        Ok(Self {
            key,
            secret,
            data_url: std::env::var("ALPACA_DATA_URL")
                .unwrap_or_else(|_| "https://data.alpaca.markets".into()),
            news_url: std::env::var("ALPACA_NEWS_URL")
                .unwrap_or_else(|_| "https://data.alpaca.markets".into()),
            options_feed: std::env::var("ALPACA_OPTIONS_FEED").ok(),
            stocks_feed,
        })
    }
}
#[derive(Clone)]
pub struct AlpacaProvider {
    client: Client,
    cfg: AlpacaConfig,
    max_retries: u8,
}
impl AlpacaProvider {
    pub fn new(cfg: AlpacaConfig) -> Result<Self, MarketDataError> {
        let client = Client::builder()
            .timeout(StdDuration::from_secs(15))
            .build()
            .map_err(|e| MarketDataError::Http(e.to_string()))?;
        Ok(Self {
            client,
            cfg,
            max_retries: 3,
        })
    }
    fn req(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header("APCA-API-KEY-ID", &self.cfg.key)
            .header("APCA-API-SECRET-KEY", &self.cfg.secret)
    }
    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> Result<T, MarketDataError> {
        let mut last = String::new();
        for attempt in 0..=self.max_retries {
            match self.req(url).send().await {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        return r
                            .json::<T>()
                            .await
                            .map_err(|e| MarketDataError::Decode(e.to_string()));
                    }
                    let body = r.text().await.unwrap_or_default();
                    last = format!("HTTP {}: {}", status, body);
                    if status != StatusCode::TOO_MANY_REQUESTS && status.is_client_error() {
                        break;
                    }
                }
                Err(e) => last = e.to_string(),
            }
            if attempt < self.max_retries {
                tokio::time::sleep(StdDuration::from_millis(250u64 * (1u64 << attempt))).await;
            }
        }
        Err(MarketDataError::Http(last))
    }
}
#[derive(Deserialize)]
struct AlpacaBars {
    bars: Vec<AlpacaBar>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct AlpacaBar {
    t: String,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
}
#[derive(Deserialize)]
struct AlpacaOptionSnapshot {
    #[serde(rename = "latestTrade")]
    latest_trade: Option<AlpacaTrade>,
    #[serde(rename = "latestQuote")]
    latest_quote: Option<AlpacaQuote>,
    greeks: Option<AlpacaGreeks>,
    #[serde(rename = "impliedVolatility")]
    implied_volatility: Option<f64>,
    open_interest: Option<u64>,
    volume: Option<u64>,
}
#[derive(Deserialize)]
struct AlpacaOptionPage {
    snapshots: HashMap<String, AlpacaOptionSnapshot>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct AlpacaTrade {
    p: Option<f64>,
    #[serde(rename = "t")]
    _t: Option<String>,
}
#[derive(Deserialize)]
struct AlpacaQuote {
    bp: Option<f64>,
    ap: Option<f64>,
    t: Option<String>,
}
#[derive(Deserialize)]
struct AlpacaGreeks {
    delta: Option<f64>,
    gamma: Option<f64>,
    theta: Option<f64>,
    vega: Option<f64>,
    rho: Option<f64>,
}
#[derive(Deserialize)]
struct NewsPage {
    news: Vec<AlpacaNews>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct AlpacaNews {
    id: String,
    headline: String,
    summary: Option<String>,
    source: String,
    created_at: String,
    url: Option<String>,
    symbols: Option<Vec<String>>,
}
fn parse_ts(s: &str) -> Result<DateTime<Utc>, MarketDataError> {
    DateTime::parse_from_rfc3339(s)
        .map(|x| x.with_timezone(&Utc))
        .map_err(|e| MarketDataError::Decode(e.to_string()))
}
fn parse_occ_expiry(symbol: &str) -> Option<DateTime<Utc>> {
    let p = th_domain::occ::parse(symbol)?;
    Utc.with_ymd_and_hms(p.year as i32, p.month, p.day, 16, 0, 0)
        .single()
}
#[async_trait]
impl MarketDataProvider for AlpacaProvider {
    async fn bars(
        &self,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Bar>, MarketDataError> {
        let mut token: Option<String> = None;
        let mut page_tokens = HashSet::new();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        loop {
            let mut url = format!(
                "{}/v2/stocks/{}/bars?timeframe=1Min&start={}&end={}&limit=10000",
                self.cfg.data_url.trim_end_matches('/'),
                symbol,
                start.format("%Y-%m-%dT%H:%M:%SZ"),
                end.format("%Y-%m-%dT%H:%M:%SZ")
            );
            if let Some(feed) = &self.cfg.stocks_feed {
                url.push_str("&feed=");
                url.push_str(feed);
            }
            if let Some(t) = &token {
                if !page_tokens.insert(t.clone()) {
                    return Err(MarketDataError::Decode("pagination token repeated".into()));
                }
                url.push_str("&page_token=");
                url.push_str(t);
            }
            let page: AlpacaBars = self.get_json(&url).await?;
            for b in page.bars {
                let ts = parse_ts(&b.t)?;
                let key = format!("{}:{}", symbol, ts.timestamp_nanos_opt().unwrap_or(0));
                if seen.insert(key) {
                    out.push(Bar {
                        symbol: symbol.into(),
                        ts,
                        open: b.o,
                        high: b.h,
                        low: b.l,
                        close: b.c,
                        volume: b.v,
                    });
                }
            }
            match page.next_page_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        out.sort_by_key(|b| b.ts);
        Ok(out)
    }
    async fn option_chain(
        &self,
        underlying: &str,
        as_of: DateTime<Utc>,
    ) -> Result<OptionChain, MarketDataError> {
        let mut token: Option<String> = None;
        let mut page_tokens = HashSet::new();
        let mut quotes = Vec::new();
        let mut seen = HashSet::new();
        loop {
            let mut url = format!(
                "{}/v1beta1/options/snapshots/{}?limit=1000",
                self.cfg.data_url.trim_end_matches('/'),
                underlying
            );
            if let Some(feed) = &self.cfg.options_feed {
                url.push_str("&feed=");
                url.push_str(feed);
            }
            if let Some(t) = &token {
                if !page_tokens.insert(t.clone()) {
                    return Err(MarketDataError::Decode("pagination token repeated".into()));
                }
                url.push_str("&page_token=");
                url.push_str(t);
            }
            let page: AlpacaOptionPage = self.get_json(&url).await?;
            for (symbol, x) in page.snapshots {
                if !seen.insert(symbol.clone()) {
                    continue;
                }
                let Some(expiry) = parse_occ_expiry(&symbol) else {
                    continue;
                };
                let Some(parsed) = th_domain::occ::parse(&symbol) else {
                    continue;
                };
                let Some(q) = x.latest_quote else { continue };
                let Some(bid) = q.bp else { continue };
                let Some(ask) = q.ap else { continue };
                if bid <= 0.0 || ask < bid {
                    continue;
                }
                let last = x
                    .latest_trade
                    .as_ref()
                    .and_then(|z| z.p)
                    .unwrap_or((bid + ask) / 2.0);
                let Some(quote_ts) = q.t.as_deref().and_then(|s| parse_ts(s).ok()) else {
                    continue;
                };
                // Hard invariant: never substitute 0.0 for IV. Only include if IV is valid and positive.
                let Some(iv) = x.implied_volatility else {
                    continue;
                };
                if !iv.is_finite() || iv <= 0.0 {
                    continue;
                }
                // Hard invariant: never substitute 0.0 for Greeks. If any Greek component is missing, set greeks to None.
                let g = x
                    .greeks
                    .and_then(|z| match (z.delta, z.gamma, z.theta, z.vega, z.rho) {
                        (Some(delta), Some(gamma), Some(theta), Some(vega), Some(rho)) => {
                            Some(Greeks {
                                delta,
                                gamma,
                                theta,
                                vega,
                                rho,
                            })
                        }
                        _ => None,
                    });
                quotes.push(OptionQuote {
                    symbol,
                    underlying: underlying.into(),
                    option_type: parsed.option_type,
                    strike: parsed.strike,
                    expiry,
                    bid,
                    ask,
                    last,
                    iv,
                    greeks: g,
                    open_interest: x.open_interest.unwrap_or(0),
                    volume: x.volume.unwrap_or(0),
                    quote_ts,
                });
            }
            match page.next_page_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        Ok(OptionChain {
            underlying: underlying.into(),
            as_of,
            quotes,
        })
    }
    async fn news(
        &self,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<NewsEvent>, MarketDataError> {
        let mut token: Option<String> = None;
        let mut page_tokens = HashSet::new();
        let mut out = Vec::new();
        loop {
            let mut url = format!(
                "{}/v1beta1/news?symbols={}&start={}&end={}&limit=50",
                self.cfg.news_url.trim_end_matches('/'),
                symbol,
                start.format("%Y-%m-%dT%H:%M:%SZ"),
                end.format("%Y-%m-%dT%H:%M:%SZ")
            );
            if let Some(t) = &token {
                if !page_tokens.insert(t.clone()) {
                    return Err(MarketDataError::Decode("pagination token repeated".into()));
                }
                url.push_str("&page_token=");
                url.push_str(t);
            }
            let page: NewsPage = self.get_json(&url).await?;
            for n in page.news {
                out.push(NewsEvent {
                    id: n.id,
                    symbol: n
                        .symbols
                        .and_then(|x| x.into_iter().find(|s| s == symbol))
                        .unwrap_or_else(|| symbol.into()),
                    headline: n.headline,
                    summary: n.summary.unwrap_or_default(),
                    source: n.source,
                    created_at: parse_ts(&n.created_at)?,
                    url: n.url,
                });
            }
            match page.next_page_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        out.sort_by_key(|n| n.created_at);
        Ok(out)
    }
    async fn most_actives(&self, limit: usize) -> Result<Vec<String>, MarketDataError> {
        let top = limit.clamp(1, 99);
        let url = format!(
            "{}/v1beta1/screener/stocks/most-actives?top={}&by=volume",
            self.cfg.data_url.trim_end_matches('/'),
            top
        );
        let resp: MostActivesResponse = self.get_json(&url).await?;
        Ok(resp.most_actives.into_iter().map(|e| e.symbol).collect())
    }
}
#[derive(Deserialize)]
struct MostActivesResponse {
    most_actives: Vec<MostActiveEntry>,
}
#[derive(Deserialize)]
struct MostActiveEntry {
    symbol: String,
}
#[derive(Debug)]
pub struct MultiSymbolCandleEngine {
    builders: HashMap<String, CandleBuilder>,
    seen_events: HashSet<String>,
    event_order: VecDeque<String>,
    max_seen: usize,
}
impl MultiSymbolCandleEngine {
    pub fn new(max_seen: usize) -> Self {
        Self {
            builders: HashMap::new(),
            seen_events: HashSet::new(),
            event_order: VecDeque::new(),
            max_seen: max_seen.max(1024),
        }
    }
    pub fn push_event(&mut self, event_id: &str, bar: Bar) -> Result<Option<Bar>, MarketDataError> {
        if !self.seen_events.insert(event_id.to_string()) {
            return Ok(None);
        }
        self.event_order.push_back(event_id.to_string());
        while self.seen_events.len() > self.max_seen {
            if let Some(old) = self.event_order.pop_front() {
                self.seen_events.remove(&old);
            }
        }
        let b = self
            .builders
            .entry(bar.symbol.clone())
            .or_insert_with(|| CandleBuilder::new(bar.symbol.clone()));
        b.push(bar)
            .map_err(|e| MarketDataError::Domain(e.to_string()))
    }
    pub fn flush_symbol(&mut self, symbol: &str) -> Option<Bar> {
        self.builders.get_mut(symbol).and_then(|b| b.flush())
    }
}
pub fn aggregate_5m(symbol: &str, mut bars: Vec<Bar>) -> Result<Vec<Bar>, MarketDataError> {
    bars.sort_by_key(|b| b.ts);
    let mut b = CandleBuilder::new(symbol);
    let mut out = Vec::new();
    for bar in bars {
        if let Some(c) = b
            .push(bar)
            .map_err(|e| MarketDataError::Domain(e.to_string()))?
        {
            out.push(c)
        }
    }
    if let Some(c) = b.flush() {
        out.push(c)
    }
    Ok(out)
}
