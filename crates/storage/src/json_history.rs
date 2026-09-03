use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum JsonHistoryError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid history: {0}")]
    Invalid(String),
}

fn with_lock<T, F: FnOnce() -> Result<T, JsonHistoryError>>(
    path: &Path,
    f: F,
) -> Result<T, JsonHistoryError> {
    let lock = path.with_extension(format!(
        "{}.lock",
        path.extension().and_then(|x| x.to_str()).unwrap_or("json")
    ));
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let mut acquired = false;
    for _ in 0..100 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => {
                acquired = true;
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e.into()),
        }
    }
    if !acquired {
        return Err(JsonHistoryError::Invalid(format!(
            "could not acquire history lock: {}",
            lock.display()
        )));
    }
    let result = f();
    let _ = fs::remove_file(&lock);
    result
}

fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T, JsonHistoryError> {
    if !path.exists() {
        return Ok(T::default());
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok(T::default());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn atomic_write<T: Serialize>(path: &Path, value: &T) -> Result<(), JsonHistoryError> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("history"),
        Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, bytes)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BotsHistory {
    pub schema_version: u32,
    pub bots: Vec<BotHistoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotHistoryRecord {
    pub bot_id: String,
    pub manifest: Value,
    pub state: Value,
    pub positions: Vec<Value>,
    pub signals: Vec<Value>,
    pub orders: Vec<Value>,
    pub trades: Vec<Value>,
    pub lifecycle: Vec<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HiveManufacturingHistory {
    pub schema_version: u32,
    pub manufacturing_runs: Vec<HiveManufacturingRun>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveManufacturingRun {
    pub manufacturing_id: String,
    pub timestamp: DateTime<Utc>,
    pub input: Value,
    pub discovery: Value,
    pub strategy_selection: Value,
    pub capital_allocation: Value,
    pub option_selection: Value,
    pub bot_manifest: Value,
    pub risk_authorization: Value,
    pub manufacturing_result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReinforcementLearningHistory {
    pub schema_version: u32,
    pub rl_sessions: Vec<RlSessionHistory>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlSessionHistory {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub seed_library_before: Vec<Value>,
    pub seed_count_before: usize,
    pub bot_history_summary: Value,
    pub market_input: Value,
    pub observations: Vec<Value>,
    pub actions: Vec<Value>,
    pub rewards: Vec<Value>,
    pub q_learning: Value,
    pub q_table_snapshot: Vec<Value>,
    pub strategy_ranking: Vec<Value>,
    pub candidate_generation: Value,
    pub validation: Value,
    pub output: Value,
    pub seed_library_after: Vec<Value>,
    pub seed_count_after: usize,
}

#[derive(Debug, Clone)]
pub struct JsonHistoryStore {
    root: PathBuf,
}
impl JsonHistoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, JsonHistoryError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
    fn bots_path(&self) -> PathBuf {
        self.root.join("bots_history.json")
    }
    fn manufacturing_path(&self) -> PathBuf {
        self.root.join("hive_manufacturing_history.json")
    }
    fn rl_path(&self) -> PathBuf {
        self.root.join("reinforcement_learning_history.json")
    }

    pub fn upsert_bot(&self, record: BotHistoryRecord) -> Result<(), JsonHistoryError> {
        let path = self.bots_path();
        with_lock(&path, || {
            let mut h: BotsHistory = read_or_default(&path)?;
            h.schema_version = 1;
            if let Some(existing) = h.bots.iter_mut().find(|b| b.bot_id == record.bot_id) {
                *existing = record;
            } else {
                h.bots.push(record);
            }
            atomic_write(&path, &h)
        })
    }
    pub fn append_bot_event(
        &self,
        bot_id: &str,
        section: &str,
        event: Value,
    ) -> Result<(), JsonHistoryError> {
        let path = self.bots_path();
        with_lock(&path, || {
            let mut h: BotsHistory = read_or_default(&path)?;
            let b = h
                .bots
                .iter_mut()
                .find(|b| b.bot_id == bot_id)
                .ok_or_else(|| JsonHistoryError::Invalid(format!("unknown bot {}", bot_id)))?;
            match section {
                "positions" => b.positions.push(event),
                "signals" => b.signals.push(event),
                "orders" => b.orders.push(event),
                "trades" => b.trades.push(event),
                "lifecycle" => b.lifecycle.push(event),
                "state" => b.state = event,
                _ => {
                    return Err(JsonHistoryError::Invalid(format!(
                        "unknown bot section {}",
                        section
                    )))
                }
            };
            b.updated_at = Utc::now();
            atomic_write(&path, &h)
        })
    }
    pub fn bot_context(&self, bot_id: &str) -> Result<Option<BotHistoryRecord>, JsonHistoryError> {
        let h: BotsHistory = read_or_default(&self.bots_path())?;
        Ok(h.bots.into_iter().find(|b| b.bot_id == bot_id))
    }

    pub fn record_manufacturing(&self, run: HiveManufacturingRun) -> Result<(), JsonHistoryError> {
        let path = self.manufacturing_path();
        with_lock(&path, || {
            let mut h: HiveManufacturingHistory = read_or_default(&path)?;
            h.schema_version = 1;
            h.manufacturing_runs.push(run);
            atomic_write(&path, &h)
        })
    }
    pub fn record_rl_session(&self, session: RlSessionHistory) -> Result<(), JsonHistoryError> {
        let path = self.rl_path();
        with_lock(&path, || {
            let mut h: ReinforcementLearningHistory = read_or_default(&path)?;
            h.schema_version = 1;
            h.rl_sessions.push(session);
            atomic_write(&path, &h)
        })
    }
    pub fn latest_seed_snapshot(&self) -> Result<Option<Vec<Value>>, JsonHistoryError> {
        let h: ReinforcementLearningHistory = read_or_default(&self.rl_path())?;
        Ok(h.rl_sessions.last().map(|s| s.seed_library_after.clone()))
    }

    pub fn latest_rl_session(&self) -> Result<Option<RlSessionHistory>, JsonHistoryError> {
        let h: ReinforcementLearningHistory = read_or_default(&self.rl_path())?;
        Ok(h.rl_sessions.last().cloned())
    }

    pub fn record_session_dataset(&self, dataset: Value) -> Result<(), JsonHistoryError> {
        let path = self.root.join("session_history.json");
        with_lock(&path, || {
            let mut list: Vec<Value> = read_or_default(&path)?;
            list.push(dataset);
            atomic_write(&path, &list)
        })
    }
}

impl BotHistoryRecord {
    pub fn from_manifest(bot_id: &str, manifest: Value, now: DateTime<Utc>) -> Self {
        Self {
            bot_id: bot_id.into(),
            manifest,
            state: json!({"status":"MANUFACTURED"}),
            positions: vec![],
            signals: vec![],
            orders: vec![],
            trades: vec![],
            lifecycle: vec![json!({"event":"MANUFACTURED","timestamp":now})],
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_file_bot_history_round_trip() {
        let dir = std::env::temp_dir().join(format!("th-json-{}", Uuid::new_v4()));
        let store = JsonHistoryStore::new(&dir).unwrap();
        let now = Utc::now();
        store
            .upsert_bot(BotHistoryRecord::from_manifest(
                "BOT-001",
                json!({"strategy_id":"STRAT-01","capital":1000}),
                now,
            ))
            .unwrap();
        store
            .append_bot_event("BOT-001", "signals", json!({"signal":"TRADE"}))
            .unwrap();
        assert_eq!(
            store.bot_context("BOT-001").unwrap().unwrap().signals.len(),
            1
        );
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn rl_history_keeps_seed_snapshot() {
        let dir = std::env::temp_dir().join(format!("th-rl-{}", Uuid::new_v4()));
        let store = JsonHistoryStore::new(&dir).unwrap();
        let seeds = vec![json!({"strategy_id":"STRAT-01"})];
        store
            .record_rl_session(RlSessionHistory {
                session_id: "RL-1".into(),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                seed_library_before: seeds.clone(),
                seed_count_before: 1,
                bot_history_summary: json!({}),
                market_input: json!({}),
                observations: vec![],
                actions: vec![],
                rewards: vec![],
                q_learning: json!({}),
                q_table_snapshot: vec![],
                strategy_ranking: vec![],
                candidate_generation: json!({}),
                validation: json!({}),
                output: json!({"strategy_id":"STRAT-31"}),
                seed_library_after: vec![
                    json!({"strategy_id":"STRAT-01"}),
                    json!({"strategy_id":"STRAT-31"}),
                ],
                seed_count_after: 2,
            })
            .unwrap();
        assert_eq!(store.latest_seed_snapshot().unwrap().unwrap().len(), 2);
        let _ = fs::remove_dir_all(dir);
    }
}
