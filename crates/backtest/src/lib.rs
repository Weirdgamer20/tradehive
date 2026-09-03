use serde::{Deserialize, Serialize};
use th_domain::{Bar, OptionType, SignalSide};
use th_strategy::{classify_regime, Strategy};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionModel {
    Naive,
    Realistic,
}

fn default_execution_model() -> ExecutionModel {
    ExecutionModel::Realistic
}
fn default_spread_bps() -> f64 {
    20.0
}
fn default_participation_limit() -> f64 {
    0.10
}
fn default_latency_bars() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub initial_cash: f64,
    pub fee_bps: f64,
    pub slippage_bps: f64,
    pub multiplier: f64,
    pub max_hold_bars: usize,
    #[serde(default = "default_execution_model")]
    pub execution_model: ExecutionModel,
    #[serde(default = "default_spread_bps")]
    pub spread_bps: f64,
    #[serde(default = "default_participation_limit")]
    pub max_volume_participation_pct: f64,
    #[serde(default = "default_latency_bars")]
    pub latency_bars: usize,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            initial_cash: 10_000.0,
            fee_bps: 1.5,
            slippage_bps: 2.0,
            multiplier: 100.0,
            max_hold_bars: 12,
            execution_model: ExecutionModel::Realistic,
            spread_bps: 20.0,
            max_volume_participation_pct: 0.10,
            latency_bars: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub side: SignalSide,
    pub entry: f64,
    pub exit: f64,
    pub pnl: f64,
    #[serde(default)]
    pub slippage_incurred: f64,
    #[serde(default)]
    pub spread_cost: f64,
    #[serde(default)]
    pub fees_paid: f64,
    #[serde(default)]
    pub bars_held: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub strategy_id: String,
    pub initial_cash: f64,
    pub final_cash: f64,
    pub trades: Vec<TradeResult>,
    pub net_pnl: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub sharpe: f64,
    pub turnover: f64,
    pub execution_model: ExecutionModel,
}

#[derive(Debug, Error)]
pub enum BacktestError {
    #[error("insufficient data")]
    InsufficientData,
    #[error("invalid split")]
    InvalidSplit,
}

fn stats(trades: &[TradeResult], initial: f64) -> (f64, f64, f64, f64) {
    let wins = trades.iter().filter(|t| t.pnl > 0.0).count();
    let gains = trades
        .iter()
        .filter(|t| t.pnl > 0.0)
        .map(|t| t.pnl)
        .sum::<f64>();
    let losses = trades
        .iter()
        .filter(|t| t.pnl < 0.0)
        .map(|t| -t.pnl)
        .sum::<f64>();
    let mut eq = initial;
    let mut peak = eq;
    let mut dd: f64 = 0.0;
    let mut rs = Vec::new();
    for t in trades {
        eq += t.pnl;
        peak = peak.max(eq);
        dd = dd.max(peak - eq);
        rs.push(t.pnl / initial);
    }
    let mean = if rs.is_empty() {
        0.0
    } else {
        rs.iter().sum::<f64>() / rs.len() as f64
    };
    let var = if rs.len() > 1 {
        rs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (rs.len() - 1) as f64
    } else {
        0.0
    };
    let sharpe = if var > 0.0 {
        mean / var.sqrt() * (rs.len() as f64).sqrt()
    } else {
        0.0
    };
    (
        if trades.is_empty() {
            0.0
        } else {
            wins as f64 / trades.len() as f64
        },
        if losses > 0.0 {
            gains / losses
        } else if gains > 0.0 {
            f64::INFINITY
        } else {
            0.0
        },
        dd,
        sharpe,
    )
}

pub struct Backtester {
    cfg: BacktestConfig,
}

impl Backtester {
    pub fn new(cfg: BacktestConfig) -> Self {
        Self { cfg }
    }

    pub fn run(
        &self,
        strategy: &mut dyn Strategy,
        bars: &[Bar],
    ) -> Result<BacktestReport, BacktestError> {
        if !self.cfg.initial_cash.is_finite()
            || self.cfg.initial_cash <= 0.0
            || !self.cfg.multiplier.is_finite()
            || self.cfg.multiplier <= 0.0
            || self.cfg.max_hold_bars == 0
        {
            return Err(BacktestError::InvalidSplit);
        }
        if bars.len() < strategy.spec().warmup + 3 {
            return Err(BacktestError::InsufficientData);
        }
        let mut trades = Vec::new();
        let mut open: Option<(SignalSide, f64, i64, usize, f64, f64)> = None;
        let mut pending: Option<(SignalSide, usize)> = None;

        for i in 0..bars.len() {
            let bar = &bars[i];

            // 1. Check max hold duration exit
            if let Some((old, entry_px, ets, ei, entry_slip, entry_spread)) = open.take() {
                let bars_held = i.saturating_sub(ei);
                if bars_held >= self.cfg.max_hold_bars {
                    let (exit_px, exit_slip, exit_spread) = match self.cfg.execution_model {
                        ExecutionModel::Naive => (bar.open, 0.0, 0.0),
                        ExecutionModel::Realistic => {
                            let spread_cost = bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                            let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                            match old {
                                SignalSide::LongCall => {
                                    (bar.open - spread_cost - slip_cost, slip_cost, spread_cost)
                                }
                                SignalSide::LongPut => {
                                    (bar.open + spread_cost + slip_cost, slip_cost, spread_cost)
                                }
                                SignalSide::Flat => (bar.open, 0.0, 0.0),
                            }
                        }
                    };

                    let raw = match old {
                        SignalSide::LongCall => exit_px - entry_px,
                        SignalSide::LongPut => entry_px - exit_px,
                        SignalSide::Flat => 0.0,
                    };
                    let fees =
                        (entry_px + exit_px) * self.cfg.multiplier * (self.cfg.fee_bps / 10_000.0);
                    let pnl = raw * self.cfg.multiplier - fees;

                    trades.push(TradeResult {
                        entry_ts: ets,
                        exit_ts: bar.ts.timestamp(),
                        side: old,
                        entry: entry_px,
                        exit: exit_px,
                        pnl,
                        slippage_incurred: (entry_slip + exit_slip) * self.cfg.multiplier,
                        spread_cost: (entry_spread + exit_spread) * self.cfg.multiplier,
                        fees_paid: fees,
                        bars_held,
                    });
                } else {
                    open = Some((old, entry_px, ets, ei, entry_slip, entry_spread));
                }
            }

            // 2. Check pending signals (next-bar execution: formed at t, executed at earliest t + latency)
            if let Some((side, signal_bar)) = pending {
                if i >= signal_bar + self.cfg.latency_bars.max(1) {
                    pending = None;
                    if let Some((old, entry_px, ets, ei, entry_slip, entry_spread)) = open.take() {
                        if old != side {
                            let (exit_px, exit_slip, exit_spread) = match self.cfg.execution_model {
                                ExecutionModel::Naive => (bar.open, 0.0, 0.0),
                                ExecutionModel::Realistic => {
                                    let spread_cost =
                                        bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                                    let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                                    match old {
                                        SignalSide::LongCall => (
                                            bar.open - spread_cost - slip_cost,
                                            slip_cost,
                                            spread_cost,
                                        ),
                                        SignalSide::LongPut => (
                                            bar.open + spread_cost + slip_cost,
                                            slip_cost,
                                            spread_cost,
                                        ),
                                        SignalSide::Flat => (bar.open, 0.0, 0.0),
                                    }
                                }
                            };
                            let raw = match old {
                                SignalSide::LongCall => exit_px - entry_px,
                                SignalSide::LongPut => entry_px - exit_px,
                                SignalSide::Flat => 0.0,
                            };
                            let fees = (entry_px + exit_px)
                                * self.cfg.multiplier
                                * (self.cfg.fee_bps / 10_000.0);
                            let pnl = raw * self.cfg.multiplier - fees;
                            trades.push(TradeResult {
                                entry_ts: ets,
                                exit_ts: bar.ts.timestamp(),
                                side: old,
                                entry: entry_px,
                                exit: exit_px,
                                pnl,
                                slippage_incurred: (entry_slip + exit_slip) * self.cfg.multiplier,
                                spread_cost: (entry_spread + exit_spread) * self.cfg.multiplier,
                                fees_paid: fees,
                                bars_held: i.saturating_sub(ei),
                            });
                        } else {
                            open = Some((old, entry_px, ets, ei, entry_slip, entry_spread));
                        }
                    }

                    if open.is_none() && side != SignalSide::Flat {
                        let (entry_px, entry_slip, entry_spread) = match self.cfg.execution_model {
                            ExecutionModel::Naive => (bar.open, 0.0, 0.0),
                            ExecutionModel::Realistic => {
                                let spread_cost = bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                                let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                                match side {
                                    SignalSide::LongCall => {
                                        (bar.open + spread_cost + slip_cost, slip_cost, spread_cost)
                                    }
                                    SignalSide::LongPut => {
                                        (bar.open - spread_cost - slip_cost, slip_cost, spread_cost)
                                    }
                                    SignalSide::Flat => (bar.open, 0.0, 0.0),
                                }
                            }
                        };
                        open = Some((
                            side,
                            entry_px,
                            bar.ts.timestamp(),
                            i,
                            entry_slip,
                            entry_spread,
                        ));
                    }
                }
            }

            let state = classify_regime(&bars[..=i]);
            if let Some(sig) = strategy.update(bar, &state) {
                if sig.side != SignalSide::Flat {
                    pending = Some((sig.side, i));
                }
            }
        }

        // Close final remaining open trade
        if let Some((side, entry_px, ets, ei, entry_slip, entry_spread)) = open {
            let Some(b) = bars.last() else {
                return Err(BacktestError::InsufficientData);
            };
            let (exit_px, exit_slip, exit_spread) = match self.cfg.execution_model {
                ExecutionModel::Naive => (b.close, 0.0, 0.0),
                ExecutionModel::Realistic => {
                    let spread_cost = b.close * (self.cfg.spread_bps / 2.0) / 10_000.0;
                    let slip_cost = b.close * self.cfg.slippage_bps / 10_000.0;
                    match side {
                        SignalSide::LongCall => {
                            (b.close - spread_cost - slip_cost, slip_cost, spread_cost)
                        }
                        SignalSide::LongPut => {
                            (b.close + spread_cost + slip_cost, slip_cost, spread_cost)
                        }
                        SignalSide::Flat => (b.close, 0.0, 0.0),
                    }
                }
            };
            let raw = match side {
                SignalSide::LongCall => exit_px - entry_px,
                SignalSide::LongPut => entry_px - exit_px,
                SignalSide::Flat => 0.0,
            };
            let fees = (entry_px + exit_px) * self.cfg.multiplier * (self.cfg.fee_bps / 10_000.0);
            let pnl = raw * self.cfg.multiplier - fees;
            trades.push(TradeResult {
                entry_ts: ets,
                exit_ts: b.ts.timestamp(),
                side,
                entry: entry_px,
                exit: exit_px,
                pnl,
                slippage_incurred: (entry_slip + exit_slip) * self.cfg.multiplier,
                spread_cost: (entry_spread + exit_spread) * self.cfg.multiplier,
                fees_paid: fees,
                bars_held: bars.len().saturating_sub(ei),
            });
        }

        let net = trades.iter().map(|t| t.pnl).sum::<f64>();
        let final_cash = self.cfg.initial_cash + net;
        let (win, pf, dd, sharpe) = stats(&trades, self.cfg.initial_cash);

        let total_traded_value: f64 = trades
            .iter()
            .map(|t| (t.entry + t.exit) * self.cfg.multiplier)
            .sum();
        let turnover = if self.cfg.initial_cash > 0.0 {
            total_traded_value / self.cfg.initial_cash
        } else {
            0.0
        };

        Ok(BacktestReport {
            strategy_id: strategy.spec().id.clone(),
            initial_cash: self.cfg.initial_cash,
            final_cash,
            net_pnl: net,
            trades,
            win_rate: win,
            profit_factor: pf,
            max_drawdown: dd,
            sharpe,
            turnover,
            execution_model: self.cfg.execution_model,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChronologicalSplit {
    pub train: Vec<Bar>,
    pub validation: Vec<Bar>,
    pub test: Vec<Bar>,
}
pub fn split(
    bars: &[Bar],
    train: f64,
    validation: f64,
) -> Result<ChronologicalSplit, BacktestError> {
    if bars.is_empty() || train < 0.0 || validation < 0.0 || train + validation >= 1.0 {
        return Err(BacktestError::InvalidSplit);
    }
    let n = bars.len();
    let a = (n as f64 * train).floor() as usize;
    let b = (n as f64 * (train + validation)).floor() as usize;
    Ok(ChronologicalSplit {
        train: bars[..a].to_vec(),
        validation: bars[a..b].to_vec(),
        test: bars[b..].to_vec(),
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OptionModelConfig {
    pub risk_free_rate: f64,
    pub implied_volatility: f64,
    pub days_to_expiry: f64,
    pub multiplier: f64,
    pub entry_slippage_bps: f64,
    pub exit_slippage_bps: f64,
}
impl Default for OptionModelConfig {
    fn default() -> Self {
        Self {
            risk_free_rate: 0.04,
            implied_volatility: 0.25,
            days_to_expiry: 7.0,
            multiplier: 100.0,
            entry_slippage_bps: 5.0,
            exit_slippage_bps: 5.0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionBacktestReport {
    pub strategy_id: String,
    pub trades: usize,
    pub net_pnl: f64,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub model_assumption: String,
}
fn option_mark(
    spot: f64,
    strike: f64,
    remaining_days: f64,
    cfg: OptionModelConfig,
    ty: OptionType,
) -> f64 {
    th_domain::black_scholes::price(
        spot,
        strike,
        (remaining_days / 365.0).max(1e-6),
        cfg.risk_free_rate,
        cfg.implied_volatility,
        ty,
    )
    .unwrap_or(0.0)
}
pub fn run_option_model_backtest(
    strategy: &mut dyn Strategy,
    bars: &[Bar],
    cfg: OptionModelConfig,
) -> Result<OptionBacktestReport, BacktestError> {
    if !cfg.risk_free_rate.is_finite()
        || cfg.implied_volatility <= 0.0
        || !cfg.implied_volatility.is_finite()
        || cfg.days_to_expiry <= 0.0
        || cfg.multiplier <= 0.0
        || !cfg.multiplier.is_finite()
    {
        return Err(BacktestError::InvalidSplit);
    }
    if bars.len() < strategy.spec().warmup + 3 {
        return Err(BacktestError::InsufficientData);
    }
    let mut open: Option<(SignalSide, f64, f64, usize)> = None;
    let mut pending = None;
    let mut pnls = Vec::new();
    for i in 0..bars.len() {
        if let Some((old, entry, strike, ei)) = open.take() {
            if i.saturating_sub(ei) >= strategy.spec().max_hold_bars as usize {
                let ty = match old {
                    SignalSide::LongCall => OptionType::Call,
                    SignalSide::LongPut => OptionType::Put,
                    SignalSide::Flat => OptionType::Call,
                };
                let t = (cfg.days_to_expiry - (i - ei) as f64 * 5.0 / 1440.0).max(0.01);
                let exit = option_mark(bars[i].open, strike, t, cfg, ty);
                pnls.push(
                    (exit - entry) * cfg.multiplier
                        - entry * cfg.multiplier * cfg.entry_slippage_bps / 10_000.0
                        - exit * cfg.multiplier * cfg.exit_slippage_bps / 10_000.0,
                );
            } else {
                open = Some((old, entry, strike, ei));
            }
        }
        if let Some(side) = pending.take() {
            if i > 0 {
                if let Some((old, entry, strike, ei)) = open.take() {
                    if old != side {
                        let ty = match old {
                            SignalSide::LongCall => OptionType::Call,
                            SignalSide::LongPut => OptionType::Put,
                            SignalSide::Flat => continue,
                        };
                        let t = (cfg.days_to_expiry - (i - ei) as f64 * 5.0 / 1440.0).max(0.01);
                        let exit = option_mark(bars[i].open, strike, t, cfg, ty);
                        let pnl = (exit - entry) * cfg.multiplier
                            - entry * cfg.multiplier * cfg.entry_slippage_bps / 10_000.0
                            - exit * cfg.multiplier * cfg.exit_slippage_bps / 10_000.0;
                        pnls.push(pnl);
                    } else {
                        open = Some((old, entry, strike, ei));
                    }
                }
                if open.is_none() && side != SignalSide::Flat {
                    let ty = match side {
                        SignalSide::LongCall => OptionType::Call,
                        SignalSide::LongPut => OptionType::Put,
                        SignalSide::Flat => continue,
                    };
                    let strike = bars[i].open.max(0.01);
                    let entry = option_mark(bars[i].open, strike, cfg.days_to_expiry, cfg, ty);
                    open = Some((side, entry, strike, i));
                }
            }
        }
        let state = classify_regime(&bars[..=i]);
        if let Some(sig) = strategy.update(&bars[i], &state) {
            if sig.side != SignalSide::Flat {
                pending = Some(sig.side)
            }
        }
    }
    if let Some((side, entry, strike, ei)) = open {
        let Some(b) = bars.last() else {
            return Err(BacktestError::InsufficientData);
        };
        let ty = match side {
            SignalSide::LongCall => OptionType::Call,
            SignalSide::LongPut => OptionType::Put,
            SignalSide::Flat => OptionType::Call,
        };
        let t = (cfg.days_to_expiry - (bars.len() - 1 - ei) as f64 * 5.0 / 1440.0).max(0.01);
        let exit = option_mark(b.close, strike, t, cfg, ty);
        pnls.push(
            (exit - entry) * cfg.multiplier
                - entry * cfg.multiplier * cfg.entry_slippage_bps / 10_000.0
                - exit * cfg.multiplier * cfg.exit_slippage_bps / 10_000.0,
        );
    }
    let net = pnls.iter().sum::<f64>();
    let wins = pnls.iter().filter(|p| **p > 0.0).count();
    let mut eq: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut dd: f64 = 0.0;
    for p in &pnls {
        eq += *p;
        peak = peak.max(eq);
        dd = dd.max(peak - eq);
    }
    Ok(OptionBacktestReport{strategy_id:strategy.spec().id.clone(),trades:pnls.len(),net_pnl:net,win_rate:if pnls.is_empty(){0.0}else{wins as f64/pnls.len() as f64},max_drawdown:dd,model_assumption:"European Black-Scholes with constant IV; next-bar execution; synthetic option path; NOT a substitute for historical option quotes.".into()})
}
