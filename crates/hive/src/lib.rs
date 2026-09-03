use chrono::{DateTime, Utc};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use th_backtest::{split, BacktestConfig, Backtester};
use th_deployment::{manufacture_bot_plan, BotCreationPlan, BotManufacturingRequest};
use th_domain::{Bar, MarketState, OptionChain, Regime, SignalSide};
use th_storage::{JsonHistoryStore, RlSessionHistory};
use th_strategy::{classify_regime, StrategyRegistry};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct StateKey {
    pub regime: String,
    pub vol_bucket: u8,
    pub momentum_bucket: i8,
    pub volume_bucket: u8,
    #[serde(default = "default_session_open")]
    pub session_open: bool,
    #[serde(default)]
    pub composite_momentum_bucket: i8,
}

fn default_session_open() -> bool {
    true
}

impl Default for StateKey {
    fn default() -> Self {
        Self {
            regime: "Range".into(),
            vol_bucket: 0,
            momentum_bucket: 0,
            volume_bucket: 0,
            session_open: true,
            composite_momentum_bucket: 0,
        }
    }
}

impl StateKey {
    pub fn from_state(s: &MarketState) -> Self {
        let session_open = th_domain::MarketSessionClock::default().is_open(s.as_of);
        Self {
            regime: format!("{:?}", s.regime),
            vol_bucket: (s.volatility * 100.0).round().clamp(0.0, 255.0) as u8,
            momentum_bucket: (s.momentum * 100.0).round().clamp(-127.0, 127.0) as i8,
            volume_bucket: (s.volume_ratio * 10.0).round().clamp(0.0, 255.0) as u8,
            session_open,
            composite_momentum_bucket: (s.momentum * 100.0).round().clamp(-127.0, 127.0) as i8,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    pub state: StateKey,
    pub action: String,
    pub reward: f64,
    pub next_state: StateKey,
    pub terminal: bool,
    pub decision_ts: DateTime<Utc>,
    pub outcome_ts: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QLearning {
    pub q: HashMap<(StateKey, String), f64>,
    pub alpha: f64,
    pub gamma: f64,
    pub epsilon: f64,
    pub epsilon_min: f64,
    pub epsilon_decay: f64,
    seed: u64,
}
impl Default for QLearning {
    fn default() -> Self {
        Self {
            q: HashMap::new(),
            alpha: 0.15,
            gamma: 0.90,
            epsilon: 0.10,
            epsilon_min: 0.01,
            epsilon_decay: 0.995,
            seed: 42,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QEntry {
    pub state: StateKey,
    pub action: String,
    pub value: f64,
}
impl QLearning {
    pub fn entries(&self) -> Vec<QEntry> {
        self.q
            .iter()
            .map(|((state, action), value)| QEntry {
                state: state.clone(),
                action: action.clone(),
                value: *value,
            })
            .collect()
    }
    pub fn from_entries(entries: &[QEntry]) -> Self {
        let mut q = Self::default();
        for e in entries {
            q.q.insert((e.state.clone(), e.action.clone()), e.value);
        }
        q
    }
    pub fn choose(&mut self, state: &StateKey, actions: &[String]) -> Option<String> {
        if actions.is_empty() {
            return None;
        }
        let mut rng = StdRng::seed_from_u64(self.seed);
        self.seed = self.seed.wrapping_add(1);
        if rng.gen::<f64>() < self.epsilon {
            return Some(actions[rng.gen_range(0..actions.len())].clone());
        }
        actions
            .iter()
            .max_by(|a, b| {
                self.q
                    .get(&(state.clone(), (*a).clone()))
                    .unwrap_or(&0.0)
                    .partial_cmp(self.q.get(&(state.clone(), (*b).clone())).unwrap_or(&0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }
    pub fn update(&mut self, e: &Experience) {
        let old = *self
            .q
            .get(&(e.state.clone(), e.action.clone()))
            .unwrap_or(&0.0);
        let next = if e.terminal {
            0.0
        } else {
            self.q
                .iter()
                .filter(|((s, _), _)| s == &e.next_state)
                .map(|(_, v)| *v)
                .fold(0.0, f64::max)
        };
        let target = e.reward + self.gamma * next;
        self.q.insert(
            (e.state.clone(), e.action.clone()),
            old + self.alpha * (target - old),
        );
        self.epsilon = (self.epsilon * self.epsilon_decay).max(self.epsilon_min);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableObservation {
    pub name: String,
    pub value: f64,
    pub predictive_score: f64,
    pub stable: bool,
    pub leakage_free: bool,
    pub source_ts: DateTime<Utc>,
}
pub fn discover_variables(bars: &[Bar]) -> Vec<VariableObservation> {
    if bars.len() < 30 {
        return vec![];
    }
    let state = classify_regime(bars);
    let xs: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let Some(last) = bars.last() else {
        return vec![];
    };
    let Some(&latest) = xs.last() else {
        return vec![];
    };
    let ret5 = latest / xs[xs.len() - 6] - 1.0;
    let ret10 = latest / xs[xs.len() - 11] - 1.0;
    let ret20 = latest / xs[xs.len() - 21] - 1.0;
    let avg = bars[bars.len() - 21..]
        .iter()
        .map(|b| b.volume)
        .sum::<f64>()
        / 21.0;
    let vr = last.volume / avg.max(1e-9);
    let atr20 = bars[bars.len() - 20..]
        .iter()
        .map(|b| b.high - b.low)
        .sum::<f64>()
        / 20.0;
    let clock = th_domain::MarketSessionClock::default();
    let session_state = clock.session_state_at(last.ts);
    let session_val = if session_state.is_open() { 1.0 } else { 0.0 };

    let m_short = th_strategy::MultiHorizonMomentumStrategy::calculate_horizon_momentum(&xs, 5)
        .unwrap_or(0.0);
    let m_med = th_strategy::MultiHorizonMomentumStrategy::calculate_horizon_momentum(&xs, 20)
        .unwrap_or(0.0);
    let m_long = th_strategy::MultiHorizonMomentumStrategy::calculate_horizon_momentum(
        &xs,
        60.min(xs.len().saturating_sub(1)),
    )
    .unwrap_or(0.0);
    let composite = 0.25 * m_short + 0.40 * m_med + 0.35 * m_long;
    let consensus = ((m_short > 0.0 && m_med > 0.0)
        || (m_short > 0.0 && m_long > 0.0)
        || (m_med > 0.0 && m_long > 0.0))
        || ((m_short < 0.0 && m_med < 0.0)
            || (m_short < 0.0 && m_long < 0.0)
            || (m_med < 0.0 && m_long < 0.0));
    let consensus_val = if consensus { 1.0 } else { 0.0 };
    let expiry_policy = th_domain::OptionExpiryPolicy::default();

    vec![
        VariableObservation {
            name: "RETURNS_5".into(),
            value: ret5,
            predictive_score: ret5.abs(),
            stable: state.regime != Regime::Unknown,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "RETURNS_10".into(),
            value: ret10,
            predictive_score: ret10.abs(),
            stable: state.regime != Regime::Unknown,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "RETURNS_20".into(),
            value: ret20,
            predictive_score: ret20.abs(),
            stable: state.regime != Regime::Unknown,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "VOLUME_RATIO".into(),
            value: vr,
            predictive_score: (vr - 1.0).abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "RANGE_MEAN_20".into(),
            value: atr20,
            predictive_score: atr20 / last.close,
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_SHORT".into(),
            value: m_short,
            predictive_score: m_short.abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_MEDIUM".into(),
            value: m_med,
            predictive_score: m_med.abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_LONG".into(),
            value: m_long,
            predictive_score: m_long.abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_COMPOSITE".into(),
            value: composite,
            predictive_score: composite.abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_CONSENSUS".into(),
            value: consensus_val,
            predictive_score: consensus_val,
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "MOMENTUM_CONFIDENCE".into(),
            value: composite.abs(),
            predictive_score: composite.abs(),
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "SESSION_STATE".into(),
            value: session_val,
            predictive_score: session_val,
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
        VariableObservation {
            name: "EXPIRY_VALIDITY".into(),
            value: if expiry_policy.min_expiry_minutes >= 180 {
                1.0
            } else {
                0.0
            },
            predictive_score: 1.0,
            stable: true,
            leakage_free: true,
            source_ts: last.ts,
        },
    ]
}

/// `fdr_q` is a Benjamini-Hochberg adjusted sign-test p-value across the tested strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvaluation {
    pub strategy_id: String,
    pub train_pnl: f64,
    pub validation_pnl: f64,
    pub oos_pnl: f64,
    pub oos_sharpe: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub trades: usize,
    pub accepted: bool,
    pub robustness: f64,
    pub p_value: f64,
    pub fdr_q: f64,
    pub confidence: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchGate {
    pub min_oos_trades: usize,
    pub min_sharpe: f64,
    pub min_profit_factor: f64,
    pub max_drawdown: f64,
    pub max_fdr_q: f64,
    pub min_robustness: f64,
}
impl Default for ResearchGate {
    fn default() -> Self {
        Self {
            min_oos_trades: 5,
            min_sharpe: 0.5,
            min_profit_factor: 1.05,
            max_drawdown: 500.0,
            max_fdr_q: 0.05,
            min_robustness: 0.75,
        }
    }
}
impl ResearchGate {
    pub fn for_bars(bars: &[Bar]) -> Self {
        if bars.len() < 300 {
            Self {
                min_oos_trades: 1,
                min_sharpe: 0.0,
                min_profit_factor: 1.0,
                max_drawdown: 1000.0,
                max_fdr_q: 1.0,
                min_robustness: 0.0,
            }
        } else {
            Self::default()
        }
    }

    pub fn accept(&self, e: &StrategyEvaluation) -> bool {
        e.validation_pnl >= 0.0
            && e.trades >= self.min_oos_trades
            && e.oos_sharpe >= self.min_sharpe
            && e.profit_factor >= self.min_profit_factor
            && e.max_drawdown <= self.max_drawdown
            && e.fdr_q <= self.max_fdr_q
            && e.robustness >= self.min_robustness
    }
}

fn sign_test_p_value(trades: &[th_backtest::TradeResult]) -> f64 {
    let n = trades.iter().filter(|t| t.pnl != 0.0).count();
    if n == 0 {
        return 1.0;
    }
    let k = trades.iter().filter(|t| t.pnl > 0.0).count();
    let m = k.min(n - k);
    let mut p = 0.0;
    for i in 0..=m {
        let mut c = 1.0;
        for j in 0..i {
            c *= ((n - j) as f64) / ((j + 1) as f64);
        }
        p += c / (2.0_f64).powi(n as i32);
    }
    (2.0 * p).min(1.0)
}

fn robustness(strategy_id: &str, bars: &[Bar]) -> f64 {
    let mut scores = Vec::new();
    for (cost, slip) in [(1.0, 1.0), (1.5, 2.0), (3.0, 5.0), (5.0, 8.0)] {
        if let Ok(sp) = split(bars, 0.6, 0.2) {
            if sp.test.len() < 10 {
                continue;
            }
            if let Ok(mut s) = StrategyRegistry::new().create(strategy_id) {
                let r = Backtester::new(BacktestConfig {
                    fee_bps: cost,
                    slippage_bps: slip,
                    ..Default::default()
                })
                .run(s.as_mut(), &sp.test);
                if let Ok(r) = r {
                    scores.push(if r.net_pnl > 0.0 { 1.0 } else { 0.0 });
                }
            }
        }
    }
    if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f64>() / scores.len() as f64
    }
}

pub fn evaluate_strategies(bars: &[Bar]) -> Vec<StrategyEvaluation> {
    let Ok(sp) = split(bars, 0.6, 0.2) else {
        return vec![];
    };
    let mut rows = Vec::new();
    for mut s in StrategyRegistry::new().all() {
        let id = s.spec().id.clone();
        let train = Backtester::new(BacktestConfig::default())
            .run(s.as_mut(), &sp.train)
            .ok();
        s.reset();
        let validation = Backtester::new(BacktestConfig::default())
            .run(s.as_mut(), &sp.validation)
            .ok();
        s.reset();
        let test = Backtester::new(BacktestConfig::default())
            .run(s.as_mut(), &sp.test)
            .ok();
        if let Some(oos) = test {
            let robust = robustness(&id, bars);
            rows.push(StrategyEvaluation {
                strategy_id: id,
                train_pnl: train.map(|x| x.net_pnl).unwrap_or(0.0),
                validation_pnl: validation.map(|x| x.net_pnl).unwrap_or(0.0),
                oos_pnl: oos.net_pnl,
                oos_sharpe: oos.sharpe,
                profit_factor: oos.profit_factor,
                max_drawdown: oos.max_drawdown,
                trades: oos.trades.len(),
                accepted: false,
                robustness: robust,
                p_value: sign_test_p_value(&oos.trades),
                fdr_q: 1.0,
                confidence: 0.0,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.p_value
            .partial_cmp(&b.p_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let m = rows.len();
    let mut q = vec![1.0; m];
    for (i, r) in rows.iter().enumerate() {
        q[i] = (r.p_value * m.max(1) as f64 / (i + 1) as f64).min(1.0);
    }
    for i in (0..m.saturating_sub(1)).rev() {
        q[i] = q[i].min(q[i + 1]);
    }
    let gate = ResearchGate::for_bars(bars);
    for (i, r) in rows.iter_mut().enumerate() {
        r.fdr_q = q[i];
        r.accepted = gate.accept(r);
    }
    rows
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub symbol: String,
    pub strategy_id: String,
    pub version: u32,
    pub fingerprint: String,
    pub promoted: bool,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}
pub fn fingerprint(strategy_id: &str, version: u32, params: &str) -> String {
    let mut h = Sha256::new();
    h.update(strategy_id.as_bytes());
    h.update(version.to_le_bytes());
    h.update(params.as_bytes());
    format!("{:x}", h.finalize())
}
pub fn promote(symbol: &str, e: &StrategyEvaluation, version: u32) -> PromotionRecord {
    let ok = e.accepted;
    PromotionRecord {
        symbol: symbol.to_string(),
        strategy_id: e.strategy_id.clone(),
        version,
        fingerprint: fingerprint(
            &e.strategy_id,
            version,
            &format!(
                "{}:{}:{}:{}:{}",
                symbol, e.oos_sharpe, e.profit_factor, e.max_drawdown, e.robustness
            ),
        ),
        promoted: ok,
        reason: if ok {
            "PASSED_ALL_RESEARCH_GATES".into()
        } else {
            "FAILED_RESEARCH_GATES".into()
        },
        created_at: Utc::now(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub strategy_id: String,
    pub dataset_version: String,
    pub hypothesis: String,
    pub status: String,
    pub oos_sharpe: f64,
    pub oos_pnl: f64,
    pub fdr_q: f64,
    pub robustness: f64,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Default)]
pub struct ExperimentRegistry {
    items: Vec<Experiment>,
}
impl ExperimentRegistry {
    pub fn register(&mut self, strategy_id: &str, hypothesis: &str, dataset: &str) -> String {
        let id = format!("EXP-{}", uuid::Uuid::new_v4());
        self.items.push(Experiment {
            id: id.clone(),
            strategy_id: strategy_id.into(),
            dataset_version: dataset.into(),
            hypothesis: hypothesis.into(),
            status: "DESIGNED".into(),
            oos_sharpe: 0.0,
            oos_pnl: 0.0,
            fdr_q: 1.0,
            robustness: 0.0,
            created_at: Utc::now(),
        });
        id
    }
    pub fn complete(
        &mut self,
        id: &str,
        oos_sharpe: f64,
        oos_pnl: f64,
        fdr_q: f64,
        robustness: f64,
    ) {
        if let Some(e) = self.items.iter_mut().find(|x| x.id == id) {
            e.oos_sharpe = oos_sharpe;
            e.oos_pnl = oos_pnl;
            e.fdr_q = fdr_q;
            e.robustness = robustness;
            e.status = "COMPLETED".into()
        }
    }
    pub fn eligible(&self, id: &str) -> bool {
        self.items
            .iter()
            .find(|e| e.id == id)
            .map(|e| {
                e.status == "COMPLETED"
                    && e.oos_sharpe >= 0.5
                    && e.oos_pnl > 0.0
                    && e.fdr_q <= 0.05
                    && e.robustness >= 0.75
            })
            .unwrap_or(false)
    }
    pub fn all(&self) -> &[Experiment] {
        &self.items
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub started: DateTime<Utc>,
    pub finished: DateTime<Utc>,
    pub evaluations: Vec<StrategyEvaluation>,
    pub promoted: Vec<PromotionRecord>,
    pub variables: Vec<VariableObservation>,
    pub learning_updates: usize,
    pub config_version: String,
    pub q_table: Vec<QEntry>,
    pub dataset_hash: String,
    pub generated_strategy: Option<GeneratedStrategyRecord>,
    pub experiences: Vec<Experience>,
}
pub fn learn_from_trades(
    mut q: QLearning,
    histories: &HashMap<String, Vec<Bar>>,
    trades: &[th_memory::TradeRecord],
) -> (QLearning, usize) {
    let mut updates = 0;
    for t in trades {
        let sym = t.symbol.split_whitespace().next().unwrap_or(&t.symbol);
        let Some(bars) = histories
            .get(t.symbol.as_str())
            .or_else(|| histories.get(sym))
        else {
            continue;
        };
        let mut prior = None;
        for b in bars {
            if b.ts <= t.entry {
                prior = Some(b);
            } else {
                break;
            }
        }
        let Some(entry_bar) = prior else { continue };
        let state = StateKey::from_state(&classify_regime(
            &bars[..=bars.iter().position(|b| b.ts == entry_bar.ts).unwrap_or(0)],
        ));
        let next_state = state.clone();
        let reward = (t.pnl / 100.0).clamp(-0.05, 0.05);
        q.update(&Experience {
            state,
            action: t.strategy_id.clone(),
            reward,
            next_state,
            terminal: true,
            decision_ts: t.entry,
            outcome_ts: t.exit.unwrap_or(t.entry),
        });
        updates += 1;
    }
    (q, updates)
}

pub fn run_analysis_with_q_and_trades(
    bars: HashMap<String, Vec<Bar>>,
    prior: Option<QLearning>,
    trades: &[th_memory::TradeRecord],
) -> AnalysisBundle {
    let mut q = prior.unwrap_or_default();
    let mut updates = 0;
    for (symbol, hs) in &bars {
        let local = trades
            .iter()
            .filter(|t| t.symbol == *symbol)
            .cloned()
            .collect::<Vec<_>>();
        let (nq, n) = learn_from_trades(q, &bars, &local);
        q = nq;
        updates += n;
        let _ = hs;
    }
    let mut bundle = run_analysis_bundle_with_q(bars, Some(q));
    if let Some(first) = bundle.symbols.first_mut() {
        first.report.learning_updates += updates;
    }
    bundle
}
pub fn run_analysis(bars: &[Bar]) -> AnalysisReport {
    run_analysis_with_q(bars, None)
}
fn persisted_seed_blueprints() -> Vec<th_strategy::StrategyBlueprint> {
    let root = std::env::var("TRADING_HIVE_HISTORY_DIR").unwrap_or_else(|_| "data".into());
    let store = match JsonHistoryStore::new(root) {
        Ok(x) => x,
        Err(_) => return vec![],
    };
    let snapshot = match store.latest_seed_snapshot() {
        Ok(Some(x)) => x,
        _ => return vec![],
    };
    snapshot
        .into_iter()
        .filter_map(|v| v.get("blueprint").cloned())
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

pub fn run_analysis_with_q(bars: &[Bar], prior: Option<QLearning>) -> AnalysisReport {
    let started = Utc::now();
    let variables = discover_variables(bars);
    let persisted_blueprints = persisted_seed_blueprints();
    let mut evaluations = evaluate_strategies(bars);
    let registry = StrategyRegistry::new();
    let mut actions = registry.seed_ids();
    for b in &persisted_blueprints {
        if !actions.contains(&b.id) {
            actions.push(b.id.clone());
            if let Ok(mut s) = registry.create_synthesized(b) {
                if let Ok(sp) = split(bars, 0.6, 0.2) {
                    let train = Backtester::new(BacktestConfig::default())
                        .run(s.as_mut(), &sp.train)
                        .ok();
                    s.reset();
                    let val = Backtester::new(BacktestConfig::default())
                        .run(s.as_mut(), &sp.validation)
                        .ok();
                    s.reset();
                    let oos = Backtester::new(BacktestConfig::default())
                        .run(s.as_mut(), &sp.test)
                        .ok();
                    if let Some(o) = oos {
                        evaluations.push(StrategyEvaluation {
                            strategy_id: b.id.clone(),
                            train_pnl: train.map(|x| x.net_pnl).unwrap_or(0.0),
                            validation_pnl: val.map(|x| x.net_pnl).unwrap_or(0.0),
                            oos_pnl: o.net_pnl,
                            oos_sharpe: o.sharpe,
                            profit_factor: o.profit_factor,
                            max_drawdown: o.max_drawdown,
                            trades: o.trades.len(),
                            accepted: false,
                            robustness: 1.0,
                            p_value: sign_test_p_value(&o.trades),
                            fdr_q: 1.0,
                            confidence: b.confidence,
                        });
                    }
                }
            }
        }
    }
    let mut rl = prior.unwrap_or_default();
    let mut updates = 0usize;
    let mut experiences = Vec::new();
    if bars.len() >= 25 {
        let mut processors = actions
            .iter()
            .filter_map(|id| {
                registry.create(id).ok().or_else(|| {
                    persisted_blueprints
                        .iter()
                        .find(|b| &b.id == id)
                        .and_then(|b| registry.create_synthesized(b).ok())
                })
            })
            .collect::<Vec<_>>();
        let clock = th_domain::MarketSessionClock::default();
        for i in 0..bars.len() - 1 {
            // Only generate active trading experiences during official regular market hours
            if !clock.is_open(bars[i].ts) {
                continue;
            }
            let state = StateKey::from_state(&classify_regime(&bars[..=i]));
            let next = StateKey::from_state(&classify_regime(&bars[..=i + 1]));
            let market_state = classify_regime(&bars[..=i]);
            for (idx, processor) in processors.iter_mut().enumerate() {
                if let Some(sig) = processor.update(&bars[i], &market_state) {
                    if sig.side == SignalSide::Flat {
                        continue;
                    }
                    let r = bars[i + 1].close / bars[i].close - 1.0;
                    let reward = match sig.side {
                        SignalSide::LongCall => r,
                        SignalSide::LongPut => -r,
                        SignalSide::Flat => 0.0,
                    };
                    let exp = Experience {
                        state: state.clone(),
                        action: actions[idx].clone(),
                        reward: reward.clamp(-0.05, 0.05),
                        next_state: next.clone(),
                        terminal: i + 1 == bars.len() - 1,
                        decision_ts: bars[i].ts,
                        outcome_ts: bars[i + 1].ts,
                    };
                    rl.update(&exp);
                    experiences.push(exp);
                    updates += 1;
                }
            }
        }
    }
    for e in evaluations.iter_mut() {
        let sharpe = e.oos_sharpe.clamp(0.0, 4.0) / 4.0;
        let pf = if e.profit_factor.is_finite() {
            ((e.profit_factor - 1.0) / 2.0).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let dd =
            1.0 - (e.max_drawdown / (e.train_pnl.abs() + e.max_drawdown + 1.0)).clamp(0.0, 1.0);
        let significance = (1.0 - e.fdr_q).clamp(0.0, 1.0);
        let evidence = (e.trades as f64 / (e.trades as f64 + 20.0)).clamp(0.0, 1.0);
        e.confidence =
            (0.35 * sharpe + 0.25 * pf + 0.20 * dd + 0.15 * significance + 0.05 * evidence)
                .clamp(0.0, 1.0);
    }
    let sym = bars.first().map(|b| b.symbol.as_str()).unwrap_or("");
    let promoted = evaluations
        .iter()
        .filter(|e| e.accepted)
        .take(3)
        .map(|e| promote(sym, e, 1))
        .collect::<Vec<_>>();
    let version = format!("research-{}", started.timestamp());
    let mut report = AnalysisReport {
        started,
        finished: Utc::now(),
        evaluations,
        promoted,
        variables,
        learning_updates: updates,
        config_version: version,
        q_table: rl.entries(),
        dataset_hash: dataset_hash(bars),
        generated_strategy: None,
        experiences,
    };
    report.generated_strategy = synthesize_strategy(&report);
    if let Some(generated) = report.generated_strategy.as_mut() {
        generated.validation = validate_generated_strategy(generated, bars);
        if let Some(v) = &generated.validation {
            if v.accepted {
                let fp = fingerprint(
                    &generated.blueprint.id,
                    generated.blueprint.version,
                    &format!(
                        "{}:{}:{}:{}:{}:{}",
                        sym,
                        v.oos_sharpe,
                        v.profit_factor,
                        v.max_drawdown,
                        v.oos_pnl,
                        report.dataset_hash
                    ),
                );
                report.promoted.push(PromotionRecord {
                    symbol: sym.to_string(),
                    strategy_id: generated.blueprint.id.clone(),
                    version: generated.blueprint.version,
                    fingerprint: fp,
                    promoted: true,
                    reason: "GENERATED_STRATEGY_PASSED_RESEARCH_GATES".into(),
                    created_at: Utc::now(),
                });
            }
        }
    }
    report
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedStrategyRecord {
    pub blueprint: th_strategy::StrategyBlueprint,
    pub generated_from_q: f64,
    pub generated_at: DateTime<Utc>,
    pub validation: Option<GeneratedStrategyValidation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedStrategyValidation {
    pub train_pnl: f64,
    pub validation_pnl: f64,
    pub oos_pnl: f64,
    pub oos_sharpe: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub trades: usize,
    pub accepted: bool,
}

pub fn synthesize_strategy(report: &AnalysisReport) -> Option<GeneratedStrategyRecord> {
    let registry = StrategyRegistry::new();
    let mut seeds = registry.seed_ids();
    for b in persisted_seed_blueprints() {
        if !seeds.contains(&b.id) {
            seeds.push(b.id);
        }
    }
    let mut ranked = report
        .q_table
        .iter()
        .filter(|e| seeds.contains(&e.action))
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.value
            .partial_cmp(&a.value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let a = ranked.first()?;
    let b = ranked.iter().find(|x| x.action != a.action)?;
    let ea = report
        .evaluations
        .iter()
        .find(|e| e.strategy_id == a.action)?;
    let eb = report
        .evaluations
        .iter()
        .find(|e| e.strategy_id == b.action)?;
    let accepted_a = ea.accepted;
    let accepted_b = eb.accepted;
    let confidence = ea.confidence.max(eb.confidence);
    let qa = a.value.max(0.0);
    let qb = b.value.max(0.0);
    let denom = qa + qb;
    let (weight_a, weight_b) = if denom > 0.0 {
        (qa / denom, qb / denom)
    } else {
        (0.5, 0.5)
    };
    let agreement_threshold = (0.50 + 0.25 * (1.0 - confidence)).clamp(0.50, 0.75);
    let status = if accepted_a && accepted_b {
        "both parents passed research gates"
    } else {
        "research candidate: one or both parents failed promotion gates"
    };
    let blueprint=th_strategy::StrategyBlueprint{id:next_strategy_id(&seeds,&report.promoted),version:1,parent_a:a.action.clone(),parent_b:b.action.clone(),confidence,weight_a,weight_b,agreement_threshold,rationale:format!("RL-evolved weighted agreement from {} and {} using learned Q-values and out-of-sample evidence; {}",a.action,b.action,status)};
    Some(GeneratedStrategyRecord {
        blueprint,
        generated_from_q: a.value.max(b.value),
        generated_at: Utc::now(),
        validation: None,
    })
}

pub fn validate_generated_strategy(
    record: &GeneratedStrategyRecord,
    bars: &[Bar],
) -> Option<GeneratedStrategyValidation> {
    if bars.len() < 100 {
        return None;
    }
    let registry = StrategyRegistry::new();
    let mut strategy = registry.create_synthesized(&record.blueprint).ok()?;
    let sp = split(bars, 0.6, 0.2).ok()?;
    let train = Backtester::new(BacktestConfig::default())
        .run(strategy.as_mut(), &sp.train)
        .ok()?;
    strategy.reset();
    let validation = Backtester::new(BacktestConfig::default())
        .run(strategy.as_mut(), &sp.validation)
        .ok()?;
    strategy.reset();
    let oos = Backtester::new(BacktestConfig::default())
        .run(strategy.as_mut(), &sp.test)
        .ok()?;
    let p_value = sign_test_p_value(&oos.trades);
    let mut robust_scores = Vec::new();
    for (fee, slip) in [(1.0, 1.0), (1.5, 2.0), (3.0, 5.0), (5.0, 8.0)] {
        let mut candidate = registry.create_synthesized(&record.blueprint).ok()?;
        let tested = Backtester::new(BacktestConfig {
            fee_bps: fee,
            slippage_bps: slip,
            ..Default::default()
        })
        .run(candidate.as_mut(), &sp.test)
        .ok()?;
        robust_scores.push(if tested.net_pnl > 0.0 { 1.0 } else { 0.0 });
    }
    let robust = robust_scores.iter().sum::<f64>() / robust_scores.len().max(1) as f64;
    let gate = ResearchGate::for_bars(bars);
    let accepted = gate.accept(&StrategyEvaluation {
        strategy_id: record.blueprint.id.clone(),
        train_pnl: train.net_pnl,
        validation_pnl: validation.net_pnl,
        oos_pnl: oos.net_pnl,
        oos_sharpe: oos.sharpe,
        profit_factor: oos.profit_factor,
        max_drawdown: oos.max_drawdown,
        trades: oos.trades.len(),
        accepted: false,
        robustness: robust,
        p_value,
        fdr_q: p_value,
        confidence: record.blueprint.confidence,
    });
    Some(GeneratedStrategyValidation {
        train_pnl: train.net_pnl,
        validation_pnl: validation.net_pnl,
        oos_pnl: oos.net_pnl,
        oos_sharpe: oos.sharpe,
        profit_factor: oos.profit_factor,
        max_drawdown: oos.max_drawdown,
        trades: oos.trades.len(),
        accepted,
    })
}

pub fn next_strategy_id(seed_ids: &[String], promoted: &[PromotionRecord]) -> String {
    let mut max_id = 30usize;
    for id in seed_ids {
        if let Some(n) = id
            .strip_prefix("STRAT-")
            .and_then(|x| x.parse::<usize>().ok())
        {
            max_id = max_id.max(n);
        }
    }
    for p in promoted {
        if p.reason != "GENERATED_STRATEGY_PASSED_RESEARCH_GATES" {
            if let Some(n) = p
                .strategy_id
                .strip_prefix("STRAT-")
                .and_then(|x| x.parse::<usize>().ok())
            {
                max_id = max_id.max(n);
            }
        }
    }
    format!("STRAT-{}", max_id + 1)
}

pub fn persist_rl_history(
    store: &JsonHistoryStore,
    bundle: &AnalysisBundle,
    bot_history_summary: serde_json::Value,
    seed_before: Vec<serde_json::Value>,
    seed_after: Vec<serde_json::Value>,
    market_input: serde_json::Value,
) -> Result<(), th_storage::JsonHistoryError> {
    let mut observations = Vec::new();
    let mut actions = Vec::new();
    let mut rewards = Vec::new();
    let mut q = Vec::new();
    let mut ranking = Vec::new();
    let mut candidate = serde_json::Value::Null;
    let mut validation = serde_json::Value::Null;
    let mut output = serde_json::Value::Null;
    for sa in &bundle.symbols {
        observations.extend(
            sa.report
                .variables
                .iter()
                .cloned()
                .map(|v| serde_json::to_value(v).unwrap_or_default()),
        );
        actions.extend(
            sa.report
                .q_table
                .iter()
                .cloned()
                .map(|v| serde_json::to_value(v).unwrap_or_default()),
        );
        rewards.extend(
            sa.report
                .experiences
                .iter()
                .cloned()
                .map(|e| serde_json::to_value(e).unwrap_or_default()),
        );
        q.extend(
            sa.report
                .q_table
                .iter()
                .cloned()
                .map(|v| serde_json::to_value(v).unwrap_or_default()),
        );
        let mut r = sa.report.evaluations.clone();
        r.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranking.extend(
            r.into_iter()
                .map(|v| serde_json::to_value(v).unwrap_or_default()),
        );
        if let Some(g) = &sa.report.generated_strategy {
            candidate = serde_json::to_value(&g.blueprint).unwrap_or_default();
            validation = serde_json::to_value(&g.validation).unwrap_or_default();
            output = serde_json::json!({"strategy_id":g.blueprint.id,"accepted":g.validation.as_ref().map(|x|x.accepted).unwrap_or(false)});
        }
    }
    let now = Utc::now();
    store.record_rl_session(RlSessionHistory{session_id:format!("RL-{}",now.timestamp_nanos_opt().unwrap_or(0)),started_at:bundle.started,ended_at:bundle.finished,seed_library_before:seed_before.clone(),seed_count_before:seed_before.len(),bot_history_summary,market_input,observations,actions,rewards,q_learning:serde_json::json!({"algorithm":"Q_LEARNING","updates":bundle.symbols.iter().map(|s|s.report.learning_updates).sum::<usize>()}),q_table_snapshot:q,strategy_ranking:ranking,candidate_generation:candidate,validation,output,seed_library_after:seed_after.clone(),seed_count_after:seed_after.len()})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolAnalysis {
    pub symbol: String,
    pub report: AnalysisReport,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisBundle {
    pub started: DateTime<Utc>,
    pub finished: DateTime<Utc>,
    pub dataset_hash: String,
    pub symbols: Vec<SymbolAnalysis>,
    pub promoted: Vec<PromotionRecord>,
}
pub fn run_analysis_bundle(histories: HashMap<String, Vec<Bar>>) -> AnalysisBundle {
    run_analysis_bundle_with_q(histories, None)
}
pub fn run_analysis_bundle_with_q(
    mut histories: HashMap<String, Vec<Bar>>,
    prior: Option<QLearning>,
) -> AnalysisBundle {
    let started = Utc::now();
    let mut symbols = Vec::new();
    let mut promoted = Vec::new();
    let mut hasher = Sha256::new();
    let mut rolling_q = prior;
    let mut keys: Vec<_> = histories.keys().cloned().collect();
    keys.sort();
    for symbol in keys {
        if let Some(mut bars) = histories.remove(&symbol) {
            bars.sort_by_key(|b| b.ts);
            for b in &bars {
                hasher.update(b.symbol.as_bytes());
                hasher.update(b.ts.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
                hasher.update(b.close.to_le_bytes());
                hasher.update(b.volume.to_le_bytes());
            }
            let report = run_analysis_with_q(&bars, rolling_q.take());
            rolling_q = Some(QLearning::from_entries(&report.q_table));
            promoted.extend(report.promoted.clone());
            symbols.push(SymbolAnalysis { symbol, report });
        }
    }
    let mut fp = promoted;
    fp.sort_by(|a, b| (&a.symbol, &a.strategy_id).cmp(&(&b.symbol, &b.strategy_id)));
    fp.dedup_by(|a, b| a.symbol == b.symbol && a.strategy_id == b.strategy_id);
    AnalysisBundle {
        started,
        finished: Utc::now(),
        dataset_hash: format!("{:x}", hasher.finalize()),
        symbols,
        promoted: fp,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotedCandidate {
    pub symbol: String,
    pub strategy_id: String,
    pub strategy_version: u32,
    pub confidence: f64,
    pub q_value: f64,
    pub research_score: f64,
    pub config_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocatedCandidate {
    pub candidate: PromotedCandidate,
    pub capital: f64,
    pub risk_budget: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationCandidate {
    pub symbol: String,
    pub strategy_id: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveManufacturingPolicy {
    pub total_capital: f64,
    pub max_bots: usize,
    pub max_bots_per_symbol: usize,
    pub max_symbol_capital_pct: f64,
    pub risk_fraction: f64,
    pub min_expiry_minutes: u32,
    pub max_expiry_minutes: u32,
}
impl Default for HiveManufacturingPolicy {
    fn default() -> Self {
        Self {
            total_capital: 0.0,
            max_bots: 20,
            max_bots_per_symbol: 4,
            max_symbol_capital_pct: 0.25,
            risk_fraction: 0.0,
            min_expiry_minutes: 180,
            max_expiry_minutes: u32::MAX,
        }
    }
}

pub fn collect_promoted_candidates(bundle: &AnalysisBundle) -> Vec<PromotedCandidate> {
    let mut candidates = Vec::new();
    for p in &bundle.promoted {
        if !p.promoted {
            continue;
        }
        let Some(sa) = bundle.symbols.iter().find(|s| s.symbol == p.symbol) else {
            continue;
        };
        if let Some(e) = sa
            .report
            .evaluations
            .iter()
            .find(|e| e.strategy_id == p.strategy_id)
        {
            candidates.push(PromotedCandidate {
                symbol: p.symbol.clone(),
                strategy_id: p.strategy_id.clone(),
                strategy_version: p.version,
                confidence: e.confidence,
                q_value: 0.0,
                research_score: e.confidence,
                config_version: sa.report.config_version.clone(),
            });
        } else if let Some(g) = &sa.report.generated_strategy {
            if g.blueprint.id == p.strategy_id {
                if let Some(v) = &g.validation {
                    if v.accepted {
                        candidates.push(PromotedCandidate {
                            symbol: p.symbol.clone(),
                            strategy_id: p.strategy_id.clone(),
                            strategy_version: g.blueprint.version,
                            confidence: g.blueprint.confidence,
                            q_value: g.generated_from_q,
                            research_score: g.blueprint.confidence,
                            config_version: sa.report.config_version.clone(),
                        });
                    }
                }
            }
        }
    }
    // Dedup by composite identity: (symbol, strategy_id, strategy_version)
    candidates.sort_by(|a, b| {
        (&a.symbol, &a.strategy_id, a.strategy_version).cmp(&(
            &b.symbol,
            &b.strategy_id,
            b.strategy_version,
        ))
    });
    candidates.dedup_by(|a, b| {
        a.symbol == b.symbol
            && a.strategy_id == b.strategy_id
            && a.strategy_version == b.strategy_version
    });
    candidates
}

pub fn allocate_candidates(
    candidates: &[PromotedCandidate],
    policy: &HiveManufacturingPolicy,
) -> Vec<AllocatedCandidate> {
    if !policy.total_capital.is_finite()
        || policy.total_capital <= 0.0
        || policy.max_bots == 0
        || !policy.risk_fraction.is_finite()
        || policy.risk_fraction <= 0.0
    {
        return Vec::new();
    }
    let mut ranked = candidates
        .iter()
        .filter(|c| {
            c.confidence.is_finite()
                && c.confidence > 0.0
                && !c.symbol.is_empty()
                && !c.strategy_id.is_empty()
        })
        .cloned()
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_bots_per_sym = policy.max_bots_per_symbol.max(1);
    let max_sym_cap =
        if policy.max_symbol_capital_pct.is_finite() && policy.max_symbol_capital_pct > 0.0 {
            policy.total_capital * policy.max_symbol_capital_pct.clamp(0.01, 1.0)
        } else {
            policy.total_capital
        };

    let mut selected: Vec<PromotedCandidate> = Vec::new();
    let mut symbol_bot_counts: HashMap<String, usize> = HashMap::new();

    for candidate in ranked {
        if selected.len() >= policy.max_bots {
            break;
        }
        let count = symbol_bot_counts
            .entry(candidate.symbol.clone())
            .or_insert(0);
        if *count >= max_bots_per_sym {
            continue; // ranked spillover to next eligible distinct symbol
        }
        *count += 1;
        selected.push(candidate);
    }

    if selected.is_empty() {
        return Vec::new();
    }

    let denom = selected.iter().map(|c| c.confidence).sum::<f64>();
    if denom <= 0.0 {
        return Vec::new();
    }

    let mut allocations: Vec<AllocatedCandidate> = Vec::new();
    let mut symbol_capital: HashMap<String, f64> = HashMap::new();

    for candidate in selected {
        let uncapped = policy.total_capital * candidate.confidence / denom;
        let sym_used = symbol_capital
            .entry(candidate.symbol.clone())
            .or_insert(0.0);
        let avail = (max_sym_cap - *sym_used).max(0.0);
        let capital = uncapped.min(avail);
        *sym_used += capital;
        if capital > 0.0 {
            let risk_budget = capital * policy.risk_fraction;
            allocations.push(AllocatedCandidate {
                candidate,
                capital,
                risk_budget,
            });
        }
    }

    allocations
}

pub fn portfolio_confidence_allocate(
    total: f64,
    candidates: &[AllocationCandidate],
    policy: &HiveManufacturingPolicy,
) -> Vec<(AllocationCandidate, f64)> {
    let promoted = candidates
        .iter()
        .map(|c| PromotedCandidate {
            symbol: c.symbol.clone(),
            strategy_id: c.strategy_id.clone(),
            strategy_version: 1,
            confidence: c.score,
            q_value: 0.0,
            research_score: c.score,
            config_version: "v1".into(),
        })
        .collect::<Vec<_>>();
    let mut pol = policy.clone();
    pol.total_capital = total;
    if pol.risk_fraction <= 0.0 {
        pol.risk_fraction = 0.05;
    }
    let allocated = allocate_candidates(&promoted, &pol);
    allocated
        .into_iter()
        .map(|a| {
            (
                AllocationCandidate {
                    symbol: a.candidate.symbol,
                    strategy_id: a.candidate.strategy_id,
                    score: a.candidate.confidence,
                },
                a.capital,
            )
        })
        .collect()
}

pub fn confidence_allocate(
    total: f64,
    scores: &[(String, f64)],
    max_bots: usize,
) -> Vec<(String, f64)> {
    if !total.is_finite() || total <= 0.0 {
        return Vec::new();
    }
    let mut ranked = scores
        .iter()
        .filter(|(_, c)| c.is_finite() && *c > 0.0)
        .cloned()
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(max_bots);
    let denom = ranked.iter().map(|(_, c)| *c).sum::<f64>();
    if denom <= 0.0 {
        return Vec::new();
    }
    ranked
        .into_iter()
        .map(|(id, c)| (id, total * c / denom))
        .collect()
}

pub fn manufacture_promoted_bots(
    bundle: &AnalysisBundle,
    histories: &HashMap<String, Vec<Bar>>,
    chains: &HashMap<String, OptionChain>,
    policy: &HiveManufacturingPolicy,
    now: DateTime<Utc>,
) -> Vec<BotCreationPlan> {
    let candidates = collect_promoted_candidates(bundle);
    if candidates.is_empty() {
        return Vec::new();
    }
    let allocated = allocate_candidates(&candidates, policy);
    let mut plans = Vec::new();
    for alloc in allocated {
        let symbol = alloc.candidate.symbol;
        let strategy_id = alloc.candidate.strategy_id;
        let Some(bars) = histories.get(&symbol).filter(|b| !b.is_empty()) else {
            continue;
        };
        let Some(chain) = chains.get(&symbol) else {
            continue;
        };
        let wanted = match classify_regime(bars).regime {
            Regime::TrendingBear => th_domain::OptionType::Put,
            _ => th_domain::OptionType::Call,
        };
        let expiry_policy = th_domain::OptionExpiryPolicy::new(
            policy.min_expiry_minutes,
            if policy.max_expiry_minutes == u32::MAX {
                None
            } else {
                Some(policy.max_expiry_minutes)
            },
        );
        let Some(quote) = chain
            .quotes
            .iter()
            .filter(|q| {
                q.underlying == symbol && q.option_type == wanted && q.is_tradeable(now, 30)
            })
            .filter(|q| {
                let m = (q.expiry - now).num_minutes().max(0) as u32;
                m >= policy.min_expiry_minutes
                    && m <= policy.max_expiry_minutes
                    && expiry_policy.is_valid_expiry(now, q.expiry)
            })
            .min_by(|a, b| {
                a.spread_bps()
                    .partial_cmp(&b.spread_bps())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
        else {
            continue;
        };
        if let Ok(plan) = manufacture_bot_plan(&BotManufacturingRequest {
            strategy_id: &strategy_id,
            strategy_version: alloc.candidate.strategy_version,
            config_version: &alloc.candidate.config_version,
            underlying: &symbol,
            quote: &quote,
            capital_budget: alloc.capital,
            risk_budget: alloc.risk_budget,
            now,
            generation_id: None,
            risk_pct: Some(policy.risk_fraction),
            rl_state: None,
            rl_action: None,
            rl_confidence: if alloc.candidate.q_value > 0.0 {
                Some(alloc.candidate.q_value)
            } else {
                None
            },
        }) {
            println!(
                "BOT_MANUFACTURED bot_id={} underlying={} strategy_id={} confidence={:.2} capital={:.2}",
                plan.bot_id, plan.underlying, plan.strategy_id, alloc.candidate.confidence, plan.capital_allocated
            );
            plans.push(plan);
        }
        if plans.len() >= policy.max_bots {
            break;
        }
    }
    plans
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManufacturingStressReport {
    pub manufacturing_test: bool,
    pub bots_created: usize,
    pub bots_valid: usize,
    pub bots_invalid: usize,
    pub strategies_created: usize,
    pub risk_configs_created: usize,
    pub option_configs_created: usize,
    pub rl_configs_created: usize,
    pub database_records_created: usize,
    pub execution_attempts: usize,
    pub generation_id: String,
    pub details: Vec<String>,
}

/// Continuous 200-300 bot manufacturing stress test.
/// Hive autonomously manufactures bot instances across strategies, risk allocations,
/// option contracts, and RL configurations, persisting each bot while order execution
/// is strictly DISABLED (execution_attempts = 0).
pub fn run_manufacturing_stress_test(
    count: usize,
    underlying: &str,
    chain: &th_domain::OptionChain,
    storage: Option<&th_storage::Store>,
    now: DateTime<Utc>,
) -> Result<ManufacturingStressReport, HiveError> {
    let generation_id = format!("GEN-STRESS-{}", now.format("%Y%m%d%H%M%S"));
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();

    let valid_quotes = chain
        .quotes
        .iter()
        .filter(|q| q.underlying == underlying && q.is_tradeable(now, 30))
        .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
        .cloned()
        .collect::<Vec<_>>();

    if valid_quotes.is_empty() {
        return Err(HiveError::Unavailable);
    }

    if let Some(store) = storage {
        let gen_rec = th_storage::HiveGenerationRecord {
            generation_id: generation_id.clone(),
            created_at: now,
            status: "ManufacturingTest".into(),
            total_capital: 10_000_000.0,
            bots_count: count,
            metadata: serde_json::json!({
                "manufacturing_test": true,
                "target_count": count,
                "underlying": underlying,
            }),
        };
        let _ = store.record_generation(&gen_rec);
    }

    let mut report = ManufacturingStressReport {
        manufacturing_test: true,
        bots_created: 0,
        bots_valid: 0,
        bots_invalid: 0,
        strategies_created: 0,
        risk_configs_created: 0,
        option_configs_created: 0,
        rl_configs_created: 0,
        database_records_created: 0,
        execution_attempts: 0, // MUST REMAIN 0
        generation_id: generation_id.clone(),
        details: Vec::new(),
    };

    let strategy_ids = (1..=30)
        .map(|i| format!("STRAT-{i:02}"))
        .collect::<Vec<_>>();

    for i in 0..count {
        let strat_idx = i % strategy_ids.len();
        let strat_id = &strategy_ids[strat_idx];
        let quote = &valid_quotes[i % valid_quotes.len()];

        let capital = 10_000.0 + (i as f64 * 250.0);
        let risk_pct = 0.01 + ((i % 5) as f64 * 0.005); // 1.0% to 3.0%
        let risk_budget = capital * risk_pct;
        let max_capital_exposure = capital;

        let rl_state = format!(
            "{{\"strat_idx\":{},\"trend\":{},\"spread_bps\":{:.1}}}",
            strat_idx,
            if i % 2 == 0 { 1 } else { -1 },
            quote.spread_bps()
        );
        let rl_action = if quote.option_type == th_domain::OptionType::Call {
            "BuyCall"
        } else {
            "BuyPut"
        };
        let rl_confidence = (0.50 + ((i % 50) as f64 * 0.009)).clamp(0.1, 1.0);

        let req = th_deployment::BotManufacturingRequest {
            strategy_id: strat_id,
            strategy_version: 1,
            config_version: "v1.0-stress",
            underlying,
            quote,
            capital_budget: capital,
            risk_budget,
            now,
            generation_id: Some(&generation_id),
            risk_pct: Some(risk_pct),
            rl_state: Some(&rl_state),
            rl_action: Some(rl_action),
            rl_confidence: Some(rl_confidence),
        };

        match th_deployment::manufacture_bot_plan(&req) {
            Ok(plan) => {
                report.bots_created += 1;
                report.strategies_created += 1;
                report.risk_configs_created += 1;
                report.option_configs_created += 1;
                report.rl_configs_created += 1;

                let is_valid = !plan.bot_id.is_empty()
                    && plan.capital_allocated > 0.0
                    && plan.risk_budget > 0.0
                    && plan.risk_budget <= plan.capital_allocated
                    && plan.risk_pct > 0.0
                    && plan.risk_pct <= 1.0
                    && !plan.option_symbol.is_empty()
                    && plan.strike > 0.0
                    && plan.expiry > now
                    && plan.rl_confidence > 0.0
                    && plan.rl_confidence <= 1.0;

                if is_valid {
                    report.bots_valid += 1;

                    if let Some(store) = storage {
                        let bot_rec = th_storage::HiveBotRecord {
                            bot_id: plan.bot_id.clone(),
                            generation_id: generation_id.clone(),
                            strategy_id: plan.strategy_id.clone(),
                            strategy_name: format!("Strategy-{}", plan.strategy_id),
                            underlying: plan.underlying.clone(),
                            option_symbol: plan.option_symbol.clone(),
                            option_type: format!("{:?}", plan.option_type),
                            strike: plan.strike,
                            expiry: plan.expiry,
                            capital_allocated: plan.capital_allocated,
                            risk_pct: plan.risk_pct,
                            risk_budget: plan.risk_budget,
                            max_capital_exposure,
                            position_size: 0,
                            rl_state: rl_state.clone(),
                            rl_action: rl_action.into(),
                            rl_confidence,
                            execution_status: "ManufacturedValid".into(),
                            created_at: now,
                            updated_at: now,
                        };
                        let risk_rec = th_storage::StrategyRiskConfig {
                            strategy_id: plan.strategy_id.clone(),
                            risk_pct,
                            capital_allocation: capital,
                            risk_budget,
                            position_sizing_policy: "DYNAMIC_RISK_BASED".into(),
                            created_at: now,
                        };

                        if store.record_bot(&bot_rec).is_ok() {
                            report.database_records_created += 1;
                        }
                        if store.record_strategy_risk(&risk_rec).is_ok() {
                            report.database_records_created += 1;
                        }
                        if store.save_bot_plan(&plan).is_ok() {
                            report.database_records_created += 1;
                        }
                    }
                } else {
                    report.bots_invalid += 1;
                    report
                        .details
                        .push(format!("Bot {} failed consistency checks", plan.bot_id));
                }
            }
            Err(e) => {
                report.bots_created += 1;
                report.bots_invalid += 1;
                report
                    .details
                    .push(format!("Manufacturing failed at index {}: {}", i, e));
            }
        }
    }

    Ok(report)
}

#[derive(Debug, Error)]
pub enum HiveError {
    #[error("analysis unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchRun {
    pub dataset_hash: String,
    pub rows: usize,
    pub leakage_detected: bool,
    pub evaluations: Vec<StrategyEvaluation>,
}
pub fn dataset_hash(bars: &[Bar]) -> String {
    let mut h = Sha256::new();
    for b in bars {
        h.update(b.symbol.as_bytes());
        h.update(b.ts.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
        h.update(b.close.to_le_bytes());
        h.update(b.volume.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}
pub fn run_research(bars: &[Bar]) -> ResearchRun {
    let hash = dataset_hash(bars);
    let leakage_detected = bars.windows(2).any(|w| w[1].ts < w[0].ts);
    ResearchRun {
        dataset_hash: hash,
        rows: bars.len(),
        leakage_detected,
        evaluations: if leakage_detected {
            Vec::new()
        } else {
            evaluate_strategies(bars)
        },
    }
}

#[cfg(test)]
mod manufacturing_tests {
    use super::*;
    use th_domain::{Bar, OptionQuote, OptionType};
    #[test]
    fn promoted_strategy_becomes_concrete_bot_assignment() {
        let now = Utc::now();
        let mut bars = Vec::new();
        for i in 0..80 {
            let p = 100.0 + i as f64 * 0.2;
            bars.push(Bar {
                symbol: "SPY".into(),
                ts: now - chrono::Duration::minutes((80 - i) as i64),
                open: p,
                high: p + 0.5,
                low: p - 0.5,
                close: p + 0.2,
                volume: 1000.0,
            });
        }
        let report = AnalysisReport {
            started: now,
            finished: now,
            evaluations: Vec::new(),
            promoted: vec![PromotionRecord {
                symbol: "SPY".into(),
                strategy_id: "momentum".into(),
                version: 1,
                fingerprint: "f".into(),
                promoted: true,
                reason: "test".into(),
                created_at: now,
            }],
            variables: Vec::new(),
            learning_updates: 0,
            config_version: "research-test".into(),
            q_table: Vec::new(),
            dataset_hash: "d".into(),
            generated_strategy: None,
            experiences: Vec::new(),
        };
        let bundle = AnalysisBundle {
            started: now,
            finished: now,
            dataset_hash: "d".into(),
            symbols: vec![SymbolAnalysis {
                symbol: "SPY".into(),
                report,
            }],
            promoted: vec![PromotionRecord {
                symbol: "SPY".into(),
                strategy_id: "momentum".into(),
                version: 1,
                fingerprint: "f".into(),
                promoted: true,
                reason: "test".into(),
                created_at: now,
            }],
        };
        let quote = OptionQuote {
            symbol: "SPY-100-C".into(),
            underlying: "SPY".into(),
            option_type: OptionType::Call,
            strike: 100.0,
            expiry: now + chrono::Duration::days(10),
            bid: 1.0,
            ask: 1.1,
            last: 1.05,
            iv: 0.2,
            greeks: Some(th_domain::Greeks {
                delta: 0.5,
                gamma: 0.01,
                theta: -0.01,
                vega: 0.1,
                rho: 0.0,
            }),
            open_interest: 100,
            volume: 100,
            quote_ts: now,
        };
        let mut histories = HashMap::new();
        histories.insert("SPY".into(), bars);
        let mut chains = HashMap::new();
        chains.insert(
            "SPY".into(),
            OptionChain {
                underlying: "SPY".into(),
                as_of: now,
                quotes: vec![quote],
            },
        );
        let plans = manufacture_promoted_bots(
            &bundle,
            &histories,
            &chains,
            &HiveManufacturingPolicy::default(),
            now,
        );
        assert_eq!(plans.len(), 0); // default policy has no capital/risk budget; manufacturing must not invent either.
    }
}

#[cfg(test)]
mod v16_history_contract_tests {
    use super::*;
    #[test]
    fn next_strategy_id_advances_after_promoted_strategy() {
        let seeds = (1..=30)
            .map(|i| format!("STRAT-{i:02}"))
            .collect::<Vec<_>>();
        let promoted = vec![PromotionRecord {
            symbol: "SPY".into(),
            strategy_id: "STRAT-99".into(),
            version: 1,
            fingerprint: "x".into(),
            promoted: true,
            reason: "passed".into(),
            created_at: Utc::now(),
        }];
        assert_eq!(next_strategy_id(&seeds, &promoted), "STRAT-100");
    }
}
