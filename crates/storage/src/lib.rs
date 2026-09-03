use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use th_deployment::BotCreationPlan;
use th_domain::{Bar, Fill, OrderIntent, Position, Signal};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTradeRecord {
    pub symbol: String,
    pub underlying: String,
    pub strategy_id: String,
    pub entry_price: f64,
    pub entry_ts: String,
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub qty: u32,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("duplicate: {0}")]
    Duplicate(String),
}
pub struct Store {
    conn: Connection,
}
impl Store {
    pub fn open(path: &str) -> Result<Self, StorageError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.pragma_update(None, "foreign_keys", "ON")?;
        c.pragma_update(None, "synchronous", "NORMAL")?;
        let s = Self { conn: c };
        s.migrate()?;
        Ok(s)
    }
    fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(r#"
 CREATE TABLE IF NOT EXISTS candles(id INTEGER PRIMARY KEY,symbol TEXT NOT NULL,ts TEXT NOT NULL,open REAL NOT NULL,high REAL NOT NULL,low REAL NOT NULL,close REAL NOT NULL,volume REAL NOT NULL,UNIQUE(symbol,ts));
 CREATE TABLE IF NOT EXISTS signals(id TEXT PRIMARY KEY,strategy_id TEXT NOT NULL,symbol TEXT NOT NULL,side TEXT NOT NULL,strength REAL NOT NULL,reason TEXT NOT NULL,created_at TEXT NOT NULL,config_version TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS orders(client_order_id TEXT PRIMARY KEY,symbol TEXT NOT NULL,side TEXT NOT NULL,qty INTEGER NOT NULL,limit_price REAL,reduce_only INTEGER NOT NULL,strategy_id TEXT NOT NULL,created_at TEXT NOT NULL,order_hash TEXT NOT NULL,broker_order_id TEXT,status TEXT);
 CREATE TABLE IF NOT EXISTS fills(fill_id TEXT PRIMARY KEY,client_order_id TEXT NOT NULL,broker_order_id TEXT NOT NULL,symbol TEXT NOT NULL,side TEXT NOT NULL,qty INTEGER NOT NULL,price REAL NOT NULL,fee REAL NOT NULL,ts TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS positions(symbol TEXT PRIMARY KEY,qty INTEGER NOT NULL,avg_price REAL NOT NULL,mark REAL NOT NULL,opened_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS checkpoints(name TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS idempotency(client_order_id TEXT PRIMARY KEY,broker_order_id TEXT,status TEXT NOT NULL,updated_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS configs(version TEXT PRIMARY KEY,payload TEXT NOT NULL,active INTEGER NOT NULL,created_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS experiments(id TEXT PRIMARY KEY,payload TEXT NOT NULL,decision TEXT,created_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS open_trades(symbol TEXT PRIMARY KEY,underlying TEXT NOT NULL,strategy_id TEXT NOT NULL,entry_price REAL NOT NULL,entry_ts TEXT NOT NULL,stop_loss_pct REAL NOT NULL,take_profit_pct REAL NOT NULL,qty INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS event_keys(event_key TEXT PRIMARY KEY,created_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS bot_plans(plan_id TEXT PRIMARY KEY,bot_id TEXT NOT NULL,strategy_id TEXT NOT NULL,strategy_version INTEGER NOT NULL,config_version TEXT NOT NULL,underlying TEXT NOT NULL,option_symbol TEXT NOT NULL,option_type TEXT NOT NULL,strike REAL NOT NULL,expiry TEXT NOT NULL,capital_allocated REAL NOT NULL,risk_budget REAL NOT NULL DEFAULT 0,quantity INTEGER NOT NULL DEFAULT 0,entry_limit REAL NOT NULL DEFAULT 0,stop_loss_pct REAL NOT NULL DEFAULT 0,take_profit_pct REAL NOT NULL DEFAULT 0,min_expiry_minutes INTEGER NOT NULL DEFAULT 180,max_expiry_minutes INTEGER NOT NULL DEFAULT 0,allowed_direction TEXT NOT NULL DEFAULT 'Any',created_at TEXT NOT NULL,fingerprint TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL DEFAULT '');
 CREATE TABLE IF NOT EXISTS hive_generations(generation_id TEXT PRIMARY KEY,created_at TEXT NOT NULL,status TEXT NOT NULL,total_capital REAL NOT NULL,bots_count INTEGER NOT NULL,metadata TEXT NOT NULL DEFAULT '{}');
 CREATE TABLE IF NOT EXISTS hive_bots(bot_id TEXT PRIMARY KEY,generation_id TEXT NOT NULL,strategy_id TEXT NOT NULL,strategy_name TEXT NOT NULL,underlying TEXT NOT NULL,option_symbol TEXT NOT NULL,option_type TEXT NOT NULL,strike REAL NOT NULL,expiry TEXT NOT NULL,capital_allocated REAL NOT NULL,risk_pct REAL NOT NULL,risk_budget REAL NOT NULL,max_capital_exposure REAL NOT NULL,position_size INTEGER NOT NULL DEFAULT 0,rl_state TEXT NOT NULL DEFAULT '{}',rl_action TEXT NOT NULL DEFAULT '{}',rl_confidence REAL NOT NULL DEFAULT 0.0,execution_status TEXT NOT NULL DEFAULT 'Created',created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS strategy_risk_configs(strategy_id TEXT PRIMARY KEY,risk_pct REAL NOT NULL,capital_allocation REAL NOT NULL,risk_budget REAL NOT NULL,position_sizing_policy TEXT NOT NULL,created_at TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS execution_feedback(event_id INTEGER PRIMARY KEY AUTOINCREMENT,timestamp TEXT NOT NULL,event_kind TEXT NOT NULL,bot_id TEXT NOT NULL,strategy_id TEXT NOT NULL,option_symbol TEXT NOT NULL,quantity INTEGER NOT NULL,entry_price REAL,exit_price REAL,realized_pnl REAL,risk_pct REAL NOT NULL,capital_allocated REAL NOT NULL,rl_decision TEXT,rl_confidence REAL,execution_status TEXT NOT NULL,broker_order_id TEXT,payload TEXT NOT NULL DEFAULT '{}');
 CREATE TABLE IF NOT EXISTS generation_performance(generation_id TEXT PRIMARY KEY,total_trades INTEGER NOT NULL DEFAULT 0,winning_trades INTEGER NOT NULL DEFAULT 0,losing_trades INTEGER NOT NULL DEFAULT 0,realized_pnl REAL NOT NULL DEFAULT 0.0,win_rate REAL NOT NULL DEFAULT 0.0,updated_at TEXT NOT NULL);
 CREATE INDEX IF NOT EXISTS idx_candles_symbol_ts ON candles(symbol,ts);
 CREATE INDEX IF NOT EXISTS idx_orders_status ON orders(status);
 CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
 CREATE INDEX IF NOT EXISTS idx_hive_bots_gen ON hive_bots(generation_id);
 CREATE INDEX IF NOT EXISTS idx_execution_feedback_bot ON execution_feedback(bot_id);
 CREATE INDEX IF NOT EXISTS idx_execution_feedback_kind ON execution_feedback(event_kind);
 "#)?;
        for (name, ddl) in [
            (
                "risk_budget",
                "ALTER TABLE bot_plans ADD COLUMN risk_budget REAL NOT NULL DEFAULT 0",
            ),
            (
                "min_expiry_minutes",
                "ALTER TABLE bot_plans ADD COLUMN min_expiry_minutes INTEGER NOT NULL DEFAULT 120",
            ),
            (
                "max_expiry_minutes",
                "ALTER TABLE bot_plans ADD COLUMN max_expiry_minutes INTEGER NOT NULL DEFAULT 180",
            ),
            (
                "allowed_direction",
                "ALTER TABLE bot_plans ADD COLUMN allowed_direction TEXT NOT NULL DEFAULT 'Any'",
            ),
            (
                "session_id",
                "ALTER TABLE bot_plans ADD COLUMN session_id TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let exists = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('bot_plans') WHERE name=?")
                .and_then(|mut st| st.query_row([name], |r| r.get::<_, i64>(0)))
                .is_ok();
            if !exists {
                self.conn.execute(ddl, [])?;
            }
        }
        Ok(())
    }
    pub fn reserve_market_event(&self, event_key: &str) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO event_keys(event_key,created_at) VALUES(?,?)",
            params![event_key, Utc::now().to_rfc3339()],
        )?;
        Ok(n == 1)
    }
    pub fn candle(&self, b: &Bar) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR IGNORE INTO candles(symbol,ts,open,high,low,close,volume) VALUES(?,?,?,?,?,?,?)",params![b.symbol,b.ts.to_rfc3339(),b.open,b.high,b.low,b.close,b.volume])?;
        Ok(())
    }
    pub fn signal(&self, s: &Signal) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR IGNORE INTO signals(id,strategy_id,symbol,side,strength,reason,created_at,config_version) VALUES(?,?,?,?,?,?,?,?)",params![s.id.to_string(),s.strategy_id,s.symbol,format!("{:?}",s.side),s.strength,s.reason,s.generated_at.to_rfc3339(),s.config_version])?;
        Ok(())
    }
    pub fn order(
        &self,
        o: &OrderIntent,
        broker_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR IGNORE INTO orders(client_order_id,symbol,side,qty,limit_price,reduce_only,strategy_id,created_at,order_hash,broker_order_id,status) VALUES(?,?,?,?,?,?,?,?,?,?,?)",params![o.client_order_id.to_string(),o.symbol,format!("{:?}",o.side),o.qty,o.limit_price,if o.reduce_only{1}else{0},o.strategy_id,o.created_at.to_rfc3339(),o.order_hash,broker_id,status])?;
        Ok(())
    }
    pub fn update_order_status(
        &self,
        id: Uuid,
        broker_id: Option<&str>,
        status: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute("UPDATE orders SET broker_order_id=COALESCE(?,broker_order_id),status=? WHERE client_order_id=?",params![broker_id,status,id.to_string()])?;
        Ok(())
    }
    pub fn fill(&self, f: &Fill) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR IGNORE INTO fills(fill_id,client_order_id,broker_order_id,symbol,side,qty,price,fee,ts) VALUES(?,?,?,?,?,?,?,?,?)",params![f.fill_id.to_string(),f.client_order_id.to_string(),f.broker_order_id,f.symbol,format!("{:?}",f.side),f.qty,f.price,f.fee,f.ts.to_rfc3339()])?;
        Ok(())
    }
    pub fn open_trade(&self, trade: &OpenTradeRecord) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO open_trades(symbol,underlying,strategy_id,entry_price,entry_ts,stop_loss_pct,take_profit_pct,qty) VALUES(?,?,?,?,?,?,?,?)",
            params![
                trade.symbol,
                trade.underlying,
                trade.strategy_id,
                trade.entry_price,
                trade.entry_ts,
                trade.stop_loss_pct,
                trade.take_profit_pct,
                trade.qty
            ],
        )?;
        Ok(())
    }
    pub fn delete_open_trade(&self, symbol: &str) -> Result<(), StorageError> {
        self.conn
            .execute("DELETE FROM open_trades WHERE symbol=?", params![symbol])?;
        Ok(())
    }
    pub fn load_open_trades(&self) -> Result<Vec<OpenTradeRecord>, StorageError> {
        let mut st = self.conn.prepare(
            "SELECT symbol,underlying,strategy_id,entry_price,entry_ts,stop_loss_pct,take_profit_pct,qty FROM open_trades",
        )?;
        let rows = st.query_map([], |r| {
            Ok(OpenTradeRecord {
                symbol: r.get(0)?,
                underlying: r.get(1)?,
                strategy_id: r.get(2)?,
                entry_price: r.get(3)?,
                entry_ts: r.get(4)?,
                stop_loss_pct: r.get(5)?,
                take_profit_pct: r.get(6)?,
                qty: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?)
        }
        Ok(out)
    }
    pub fn position(&self, p: &Position) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR REPLACE INTO positions(symbol,qty,avg_price,mark,opened_at) VALUES(?,?,?,?,?)",params![p.symbol,p.qty,p.avg_price,p.mark,p.opened_at.to_rfc3339()])?;
        Ok(())
    }
    pub fn event<T: Serialize>(&self, kind: &str, payload: &T) -> Result<(), StorageError> {
        self.event_keyed(&format!("{}:{}", kind, Uuid::new_v4()), kind, payload)
    }
    pub fn event_keyed<T: Serialize>(
        &self,
        key: &str,
        kind: &str,
        payload: &T,
    ) -> Result<(), StorageError> {
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO event_keys(event_key,created_at) VALUES(?,?)",
            params![key, Utc::now().to_rfc3339()],
        )?;
        if inserted == 1 {
            tx.execute(
                "INSERT INTO events(kind,payload,created_at) VALUES(?,?,?)",
                params![
                    kind,
                    serde_json::to_string(payload)?,
                    Utc::now().to_rfc3339()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn checkpoint(&self, name: &str, value: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO checkpoints(name,value,updated_at) VALUES(?,?,?)",
            params![name, value, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
    pub fn checkpoint_value(&self, name: &str) -> Result<Option<String>, StorageError> {
        self.conn
            .query_row(
                "SELECT value FROM checkpoints WHERE name=?",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }
    pub fn idempotency_status(
        &self,
        id: Uuid,
    ) -> Result<Option<(Option<String>, String)>, StorageError> {
        self.conn
            .query_row(
                "SELECT broker_order_id,status FROM idempotency WHERE client_order_id=?",
                params![id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(StorageError::from)
    }
    pub fn reserve_idempotency(&self, id: Uuid) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO idempotency(client_order_id,status,updated_at) VALUES(?,?,?)",
            params![id.to_string(), "RESERVED", Utc::now().to_rfc3339()],
        )?;
        Ok(n == 1)
    }
    pub fn reserve_order(&self, o: &OrderIntent) -> Result<bool, StorageError> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO idempotency(client_order_id,status,updated_at) VALUES(?,?,?)",
            params![
                o.client_order_id.to_string(),
                "RESERVED",
                Utc::now().to_rfc3339()
            ],
        )?;
        if n == 1 {
            tx.execute("INSERT OR IGNORE INTO orders(client_order_id,symbol,side,qty,limit_price,reduce_only,strategy_id,created_at,order_hash,status) VALUES(?,?,?,?,?,?,?,?,?,?)",params![o.client_order_id.to_string(),o.symbol,format!("{:?}",o.side),o.qty,o.limit_price,if o.reduce_only{1}else{0},o.strategy_id,o.created_at.to_rfc3339(),o.order_hash,"RESERVED"])?;
            tx.commit()?;
            Ok(true)
        } else {
            tx.rollback()?;
            Ok(false)
        }
    }
    pub fn set_idempotency(
        &self,
        id: Uuid,
        broker_id: &str,
        status: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute("UPDATE idempotency SET broker_order_id=?,status=?,updated_at=? WHERE client_order_id=?",params![broker_id,status,Utc::now().to_rfc3339(),id.to_string()])?;
        Ok(())
    }
    pub fn load_reserved_orders(&self) -> Result<Vec<Uuid>, StorageError> {
        let mut st = self
            .conn
            .prepare("SELECT client_order_id FROM idempotency WHERE status='RESERVED'")?;
        let rows = st.query_map([], |r| {
            let s: String = r.get(0)?;
            Uuid::parse_str(&s).map_err(|_| rusqlite::Error::InvalidQuery)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    pub fn save_config(
        &self,
        version: &str,
        payload: &str,
        active: bool,
    ) -> Result<(), StorageError> {
        let tx = self.conn.unchecked_transaction()?;
        if active {
            tx.execute("UPDATE configs SET active=0 WHERE active=1", [])?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO configs(version,payload,active,created_at) VALUES(?,?,?,?)",
            params![
                version,
                payload,
                if active { 1 } else { 0 },
                Utc::now().to_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn activate_config(&self, version: &str) -> Result<bool, StorageError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE configs SET active=0 WHERE active=1", [])?;
        let n = tx.execute(
            "UPDATE configs SET active=1 WHERE version=?",
            params![version],
        )?;
        if n == 1 {
            tx.commit()?;
            Ok(true)
        } else {
            tx.rollback()?;
            Ok(false)
        }
    }
    pub fn latest_inactive_config_since(
        &self,
        after: &str,
    ) -> Result<Option<(String, String)>, StorageError> {
        self.conn.query_row("SELECT version,payload FROM configs WHERE active=0 AND created_at>=? ORDER BY created_at DESC LIMIT 1",params![after],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(StorageError::from)
    }
    pub fn latest_inactive_config(&self) -> Result<Option<(String, String)>, StorageError> {
        self.conn.query_row("SELECT version,payload FROM configs WHERE active=0 ORDER BY created_at DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(StorageError::from)
    }
    pub fn config(&self, version: &str) -> Result<Option<String>, StorageError> {
        self.conn
            .query_row(
                "SELECT payload FROM configs WHERE version=?",
                params![version],
                |r| r.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }
    pub fn active_config(&self) -> Result<Option<(String, String)>, StorageError> {
        self.conn.query_row("SELECT version,payload FROM configs WHERE active=1 ORDER BY created_at DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(StorageError::from)
    }
    pub fn integrity_check(&self) -> Result<bool, StorageError> {
        let v: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(v == "ok")
    }
    pub fn health_check(&self) -> Result<(), StorageError> {
        self.conn
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?;
        Ok(())
    }
    pub fn distinct_symbols(&self) -> Result<Vec<String>, StorageError> {
        let mut st = self
            .conn
            .prepare("SELECT DISTINCT symbol FROM candles ORDER BY symbol")?;
        let rows = st.query_map([], |r| r.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?)
        }
        Ok(out)
    }
    pub fn recent_candles(&self, symbol: &str, limit: usize) -> Result<Vec<Bar>, StorageError> {
        let mut st=self.conn.prepare("SELECT symbol,ts,open,high,low,close,volume FROM candles WHERE symbol=? ORDER BY ts DESC LIMIT ?")?;
        let rows = st.query_map(params![symbol, limit as i64], |r| {
            let ts: String = r.get(1)?;
            let ts = chrono::DateTime::parse_from_rfc3339(&ts)
                .map(|x| x.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(Bar {
                symbol: r.get(0)?,
                ts,
                open: r.get(2)?,
                high: r.get(3)?,
                low: r.get(4)?,
                close: r.get(5)?,
                volume: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?)
        }
        out.reverse();
        Ok(out)
    }
    pub fn trade_records_since(
        &self,
        after: &str,
    ) -> Result<Vec<th_memory::TradeRecord>, StorageError> {
        let mut st=self.conn.prepare("SELECT payload FROM events WHERE kind='TRADE_AUTOPSY' AND created_at>=? ORDER BY created_at")?;
        let rows = st.query_map(params![after], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let payload = row?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
                if let Some(t) = v.get("trade") {
                    if let Ok(t) = serde_json::from_value::<th_memory::TradeRecord>(t.clone()) {
                        out.push(t)
                    }
                }
            }
        }
        Ok(out)
    }
    pub fn trade_records_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<th_memory::TradeRecord>, StorageError> {
        let mut st = self
            .conn
            .prepare("SELECT payload FROM events WHERE kind='TRADE_AUTOPSY' ORDER BY created_at")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let payload = row?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
                if let Some(t) = v.get("trade") {
                    if let Ok(record) = serde_json::from_value::<th_memory::TradeRecord>(t.clone())
                    {
                        if record.session_id == session_id {
                            out.push(record);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
    pub fn save_bot_plan(&self, p: &BotCreationPlan) -> Result<(), StorageError> {
        self.conn.execute("INSERT OR REPLACE INTO bot_plans(plan_id,bot_id,strategy_id,strategy_version,config_version,underlying,option_symbol,option_type,strike,expiry,capital_allocated,risk_budget,quantity,entry_limit,stop_loss_pct,take_profit_pct,min_expiry_minutes,max_expiry_minutes,allowed_direction,created_at,fingerprint,session_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![p.plan_id,p.bot_id,p.strategy_id,p.strategy_version as i64,p.config_version,p.underlying,p.option_symbol,format!("{:?}",p.option_type),p.strike,p.expiry.to_rfc3339(),p.capital_allocated,p.risk_budget,p.quantity as i64,p.entry_limit,p.stop_loss_pct,p.take_profit_pct,p.min_expiry_minutes as i64,p.max_expiry_minutes as i64,"Any",p.created_at.to_rfc3339(),p.fingerprint,p.session_id])?;
        Ok(())
    }
    pub fn load_bot_plans(&self) -> Result<Vec<BotCreationPlan>, StorageError> {
        let mut st=self.conn.prepare("SELECT plan_id,bot_id,strategy_id,strategy_version,config_version,underlying,option_symbol,option_type,strike,expiry,capital_allocated,risk_budget,quantity,entry_limit,stop_loss_pct,take_profit_pct,min_expiry_minutes,max_expiry_minutes,allowed_direction,created_at,fingerprint,session_id FROM bot_plans ORDER BY created_at")?;
        let rows = st.query_map([], |r| {
            let expiry: String = r.get(9)?;
            let created: String = r.get(19)?;
            let expiry = chrono::DateTime::parse_from_rfc3339(&expiry)
                .map(|x| x.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created)
                .map(|x| x.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let option_type = match r.get::<_, String>(7)?.as_str() {
                "Call" => th_domain::OptionType::Call,
                "Put" => th_domain::OptionType::Put,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let capital_allocated: f64 = r.get(10)?;
            let risk_budget: f64 = r.get(11)?;
            let session_id: String = r.get::<_, Option<String>>(21)?.unwrap_or_default();
            Ok(BotCreationPlan {
                plan_id: r.get(0)?,
                bot_id: r.get(1)?,
                strategy_id: r.get(2)?,
                strategy_version: r.get::<_, i64>(3)? as u32,
                config_version: r.get(4)?,
                underlying: r.get(5)?,
                option_symbol: r.get(6)?,
                option_type,
                strike: r.get(8)?,
                expiry,
                capital_allocated,
                risk_budget,
                quantity: r.get::<_, i64>(12)? as u32,
                entry_limit: r.get(13)?,
                stop_loss_pct: r.get(14)?,
                take_profit_pct: r.get(15)?,
                min_expiry_minutes: r.get::<_, i64>(16)? as u32,
                max_expiry_minutes: r.get::<_, i64>(17)? as u32,
                created_at,
                fingerprint: r.get(20)?,
                generation_id: "GEN-DEFAULT".into(),
                risk_pct: if capital_allocated > 0.0 {
                    risk_budget / capital_allocated
                } else {
                    0.02
                },
                max_capital_exposure: capital_allocated,
                rl_state: None,
                rl_action: None,
                rl_confidence: 1.0,
                session_id,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
    pub fn load_bot_plans_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<BotCreationPlan>, StorageError> {
        let mut st = self.conn.prepare("SELECT plan_id,bot_id,strategy_id,strategy_version,config_version,underlying,option_symbol,option_type,strike,expiry,capital_allocated,risk_budget,quantity,entry_limit,stop_loss_pct,take_profit_pct,min_expiry_minutes,max_expiry_minutes,allowed_direction,created_at,fingerprint,session_id FROM bot_plans WHERE session_id=? ORDER BY created_at")?;
        let rows = st.query_map(params![session_id], |r| {
            let expiry: String = r.get(9)?;
            let created: String = r.get(19)?;
            let expiry = chrono::DateTime::parse_from_rfc3339(&expiry)
                .map(|x| x.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created)
                .map(|x| x.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let option_type = match r.get::<_, String>(7)?.as_str() {
                "Call" => th_domain::OptionType::Call,
                "Put" => th_domain::OptionType::Put,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let capital_allocated: f64 = r.get(10)?;
            let risk_budget: f64 = r.get(11)?;
            let session_id: String = r.get::<_, Option<String>>(21)?.unwrap_or_default();
            Ok(BotCreationPlan {
                plan_id: r.get(0)?,
                bot_id: r.get(1)?,
                strategy_id: r.get(2)?,
                strategy_version: r.get::<_, i64>(3)? as u32,
                config_version: r.get(4)?,
                underlying: r.get(5)?,
                option_symbol: r.get(6)?,
                option_type,
                strike: r.get(8)?,
                expiry,
                capital_allocated,
                risk_budget,
                quantity: r.get::<_, i64>(12)? as u32,
                entry_limit: r.get(13)?,
                stop_loss_pct: r.get(14)?,
                take_profit_pct: r.get(15)?,
                min_expiry_minutes: r.get::<_, i64>(16)? as u32,
                max_expiry_minutes: r.get::<_, i64>(17)? as u32,
                created_at,
                fingerprint: r.get(20)?,
                generation_id: "GEN-DEFAULT".into(),
                risk_pct: if capital_allocated > 0.0 {
                    risk_budget / capital_allocated
                } else {
                    0.02
                },
                max_capital_exposure: capital_allocated,
                rl_state: None,
                rl_action: None,
                rl_confidence: 1.0,
                session_id,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
    pub fn counts(&self) -> Result<(i64, i64, i64), StorageError> {
        Ok((
            self.conn
                .query_row("SELECT COUNT(*) FROM candles", [], |r| r.get(0))?,
            self.conn
                .query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0))?,
            self.conn
                .query_row("SELECT COUNT(*) FROM fills", [], |r| r.get(0))?,
        ))
    }

    pub fn record_generation(&self, g: &HiveGenerationRecord) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO hive_generations(generation_id,created_at,status,total_capital,bots_count,metadata) VALUES(?,?,?,?,?,?)",
            params![g.generation_id, g.created_at.to_rfc3339(), g.status, g.total_capital, g.bots_count as i64, g.metadata.to_string()],
        )?;
        Ok(())
    }

    pub fn record_bot(&self, b: &HiveBotRecord) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO hive_bots(bot_id,generation_id,strategy_id,strategy_name,underlying,option_symbol,option_type,strike,expiry,capital_allocated,risk_pct,risk_budget,max_capital_exposure,position_size,rl_state,rl_action,rl_confidence,execution_status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                b.bot_id,
                b.generation_id,
                b.strategy_id,
                b.strategy_name,
                b.underlying,
                b.option_symbol,
                b.option_type,
                b.strike,
                b.expiry.to_rfc3339(),
                b.capital_allocated,
                b.risk_pct,
                b.risk_budget,
                b.max_capital_exposure,
                b.position_size as i64,
                b.rl_state,
                b.rl_action,
                b.rl_confidence,
                b.execution_status,
                b.created_at.to_rfc3339(),
                b.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn record_strategy_risk(&self, r: &StrategyRiskConfig) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO strategy_risk_configs(strategy_id,risk_pct,capital_allocation,risk_budget,position_sizing_policy,created_at) VALUES(?,?,?,?,?,?)",
            params![r.strategy_id, r.risk_pct, r.capital_allocation, r.risk_budget, r.position_sizing_policy, r.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn record_feedback(&self, f: &ExecutionFeedbackRecord) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO execution_feedback(timestamp,event_kind,bot_id,strategy_id,option_symbol,quantity,entry_price,exit_price,realized_pnl,risk_pct,capital_allocated,rl_decision,rl_confidence,execution_status,broker_order_id,payload) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                f.timestamp.to_rfc3339(),
                f.event_kind,
                f.bot_id,
                f.strategy_id,
                f.option_symbol,
                f.quantity as i64,
                f.entry_price,
                f.exit_price,
                f.realized_pnl,
                f.risk_pct,
                f.capital_allocated,
                f.rl_decision,
                f.rl_confidence,
                f.execution_status,
                f.broker_order_id,
                f.payload.to_string(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_generation_bots(
        &self,
        generation_id: &str,
    ) -> Result<Vec<HiveBotRecord>, StorageError> {
        let mut st = self.conn.prepare(
            "SELECT bot_id,generation_id,strategy_id,strategy_name,underlying,option_symbol,option_type,strike,expiry,capital_allocated,risk_pct,risk_budget,max_capital_exposure,position_size,rl_state,rl_action,rl_confidence,execution_status,created_at,updated_at FROM hive_bots WHERE generation_id=? ORDER BY created_at"
        )?;
        let rows = st.query_map([generation_id], |r| {
            let exp_s: String = r.get(8)?;
            let cr_s: String = r.get(18)?;
            let up_s: String = r.get(19)?;
            let expiry = chrono::DateTime::parse_from_rfc3339(&exp_s)
                .map(|x| x.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let created_at = chrono::DateTime::parse_from_rfc3339(&cr_s)
                .map(|x| x.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let updated_at = chrono::DateTime::parse_from_rfc3339(&up_s)
                .map(|x| x.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(HiveBotRecord {
                bot_id: r.get(0)?,
                generation_id: r.get(1)?,
                strategy_id: r.get(2)?,
                strategy_name: r.get(3)?,
                underlying: r.get(4)?,
                option_symbol: r.get(5)?,
                option_type: r.get(6)?,
                strike: r.get(7)?,
                expiry,
                capital_allocated: r.get(9)?,
                risk_pct: r.get(10)?,
                risk_budget: r.get(11)?,
                max_capital_exposure: r.get(12)?,
                position_size: r.get::<_, i64>(13)? as u32,
                rl_state: r.get(14)?,
                rl_action: r.get(15)?,
                rl_confidence: r.get(16)?,
                execution_status: r.get(17)?,
                created_at,
                updated_at,
            })
        })?;
        let mut bots = Vec::new();
        for b in rows {
            bots.push(b?);
        }
        Ok(bots)
    }

    pub fn get_feedback_for_bot(
        &self,
        bot_id: &str,
    ) -> Result<Vec<ExecutionFeedbackRecord>, StorageError> {
        let mut st = self.conn.prepare(
            "SELECT event_id,timestamp,event_kind,bot_id,strategy_id,option_symbol,quantity,entry_price,exit_price,realized_pnl,risk_pct,capital_allocated,rl_decision,rl_confidence,execution_status,broker_order_id,payload FROM execution_feedback WHERE bot_id=? ORDER BY event_id"
        )?;
        let rows = st.query_map([bot_id], |r| {
            let ts_s: String = r.get(1)?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_s)
                .map(|x| x.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let payload_s: String = r.get(16)?;
            let payload = serde_json::from_str(&payload_s).unwrap_or_default();
            Ok(ExecutionFeedbackRecord {
                event_id: Some(r.get(0)?),
                timestamp,
                event_kind: r.get(2)?,
                bot_id: r.get(3)?,
                strategy_id: r.get(4)?,
                option_symbol: r.get(5)?,
                quantity: r.get::<_, i64>(6)? as u32,
                entry_price: r.get(7)?,
                exit_price: r.get(8)?,
                realized_pnl: r.get(9)?,
                risk_pct: r.get(10)?,
                capital_allocated: r.get(11)?,
                rl_decision: r.get(12)?,
                rl_confidence: r.get(13)?,
                execution_status: r.get(14)?,
                broker_order_id: r.get(15)?,
                payload,
            })
        })?;
        let mut items = Vec::new();
        for item in rows {
            items.push(item?);
        }
        Ok(items)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveGenerationRecord {
    pub generation_id: String,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub total_capital: f64,
    pub bots_count: usize,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveBotRecord {
    pub bot_id: String,
    pub generation_id: String,
    pub strategy_id: String,
    pub strategy_name: String,
    pub underlying: String,
    pub option_symbol: String,
    pub option_type: String,
    pub strike: f64,
    pub expiry: DateTime<Utc>,
    pub capital_allocated: f64,
    pub risk_pct: f64,
    pub risk_budget: f64,
    pub max_capital_exposure: f64,
    pub position_size: u32,
    pub rl_state: String,
    pub rl_action: String,
    pub rl_confidence: f64,
    pub execution_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskConfig {
    pub strategy_id: String,
    pub risk_pct: f64,
    pub capital_allocation: f64,
    pub risk_budget: f64,
    pub position_sizing_policy: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionFeedbackRecord {
    pub event_id: Option<i64>,
    pub timestamp: DateTime<Utc>,
    pub event_kind: String,
    pub bot_id: String,
    pub strategy_id: String,
    pub option_symbol: String,
    pub quantity: u32,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub realized_pnl: Option<f64>,
    pub risk_pct: f64,
    pub capital_allocated: f64,
    pub rl_decision: Option<String>,
    pub rl_confidence: Option<f64>,
    pub execution_status: String,
    pub broker_order_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationPerformanceRecord {
    pub generation_id: String,
    pub total_trades: u32,
    pub winning_trades: u32,
    pub losing_trades: u32,
    pub realized_pnl: f64,
    pub win_rate: f64,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_wal_and_idempotency_work() {
        let p = std::env::temp_dir().join(format!("th-test-{}.sqlite", Uuid::new_v4()));
        let s = Store::open(p.to_str().unwrap()).unwrap();
        let id = Uuid::new_v4();
        assert!(s.reserve_idempotency(id).unwrap());
        assert!(!s.reserve_idempotency(id).unwrap());
        s.set_idempotency(id, "broker", "FILLED").unwrap();
        assert_eq!(s.idempotency_status(id).unwrap().unwrap().1, "FILLED");
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn reserve_order_persists_ledger_and_is_idempotent() {
        let p = std::env::temp_dir().join(format!("th-order-test-{}.sqlite", Uuid::new_v4()));
        let s = Store::open(p.to_str().unwrap()).unwrap();
        let o = OrderIntent {
            client_order_id: Uuid::new_v4(),
            symbol: "SPY260828C00400000".into(),
            side: th_domain::OrderSide::Buy,
            qty: 1,
            limit_price: Some(2.0),
            reduce_only: false,
            strategy_id: "momentum".into(),
            created_at: Utc::now(),
            order_hash: "h".into(),
            bot_id: None,
            session_id: None,
            decision_id: None,
            oms_state: None,
        };
        assert!(s.reserve_order(&o).unwrap());
        assert!(!s.reserve_order(&o).unwrap());
        assert_eq!(
            s.idempotency_status(o.client_order_id).unwrap().unwrap().1,
            "RESERVED"
        );
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn hive_relational_models_round_trip() {
        let p = std::env::temp_dir().join(format!("th-hive-relational-{}.sqlite", Uuid::new_v4()));
        let s = Store::open(p.to_str().unwrap()).unwrap();

        let gen = HiveGenerationRecord {
            generation_id: "GEN-001".into(),
            created_at: Utc::now(),
            status: "Operating".into(),
            total_capital: 100_000.0,
            bots_count: 5,
            metadata: serde_json::json!({"market": "US_EQUITY"}),
        };
        s.record_generation(&gen).unwrap();

        let bot = HiveBotRecord {
            bot_id: "BOT-101".into(),
            generation_id: "GEN-001".into(),
            strategy_id: "STRAT-01".into(),
            strategy_name: "MultiHorizonMomentum".into(),
            underlying: "SPY".into(),
            option_symbol: "SPY260902C00500000".into(),
            option_type: "Call".into(),
            strike: 500.0,
            expiry: Utc::now() + chrono::Duration::hours(24),
            capital_allocated: 20_000.0,
            risk_pct: 0.02,
            risk_budget: 400.0,
            max_capital_exposure: 20_000.0,
            position_size: 2,
            rl_state: "{\"trend\":1}".into(),
            rl_action: "{\"action\":\"BuyCall\"}".into(),
            rl_confidence: 0.85,
            execution_status: "Active".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        s.record_bot(&bot).unwrap();

        let risk = StrategyRiskConfig {
            strategy_id: "STRAT-01".into(),
            risk_pct: 0.02,
            capital_allocation: 20_000.0,
            risk_budget: 400.0,
            position_sizing_policy: "DYNAMIC_RISK_BASED".into(),
            created_at: Utc::now(),
        };
        s.record_strategy_risk(&risk).unwrap();

        let feedback = ExecutionFeedbackRecord {
            event_id: None,
            timestamp: Utc::now(),
            event_kind: "BUY_FILLED".into(),
            bot_id: "BOT-101".into(),
            strategy_id: "STRAT-01".into(),
            option_symbol: "SPY260902C00500000".into(),
            quantity: 2,
            entry_price: Some(2.50),
            exit_price: None,
            realized_pnl: None,
            risk_pct: 0.02,
            capital_allocated: 20_000.0,
            rl_decision: Some("BuyCall".into()),
            rl_confidence: Some(0.85),
            execution_status: "Filled".into(),
            broker_order_id: Some("alpaca-order-123".into()),
            payload: serde_json::json!({"order_type": "limit"}),
        };
        s.record_feedback(&feedback).unwrap();

        let bots = s.get_generation_bots("GEN-001").unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].bot_id, "BOT-101");
        assert_eq!(bots[0].risk_budget, 400.0);

        let fb = s.get_feedback_for_bot("BOT-101").unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].event_kind, "BUY_FILLED");
        assert_eq!(fb[0].quantity, 2);

        let _ = std::fs::remove_file(p);
    }
}

pub mod json_history;
pub use json_history::{
    BotHistoryRecord, HiveManufacturingHistory, HiveManufacturingRun, JsonHistoryError,
    JsonHistoryStore, ReinforcementLearningHistory, RlSessionHistory,
};
