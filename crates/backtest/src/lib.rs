use serde::{Deserialize, Serialize};
use th_domain::{Bar, OptionType, SignalSide};
use th_strategy::{classify_regime, Strategy};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub initial_cash: f64,
    pub fee_bps: f64,
    pub slippage_bps: f64,
    pub multiplier: f64,
    pub max_hold_bars: usize,
}
impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            initial_cash: 10_000.0,
            fee_bps: 1.5,
            slippage_bps: 2.0,
            multiplier: 100.0,
            max_hold_bars: 12,
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
}
#[derive(Debug, Error)]
pub enum BacktestError {
    #[error("insufficient data")]
    InsufficientData,
    #[error("invalid split")]
    InvalidSplit,
}
fn cost(entry: f64, exit: f64, cfg: &BacktestConfig) -> f64 {
    (entry + exit) * cfg.multiplier * (cfg.fee_bps + cfg.slippage_bps) / 10_000.0
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
        } else {
            if gains > 0.0 {
                f64::INFINITY
            } else {
                0.0
            }
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
        let mut open: Option<(SignalSide, f64, i64, usize)> = None;
        let mut pending: Option<SignalSide> = None;
        for i in 0..bars.len() {
            let bar = &bars[i];
            if let Some((old, entry, ets, ei)) = open.take() {
                if i.saturating_sub(ei) >= self.cfg.max_hold_bars {
                    let exit = bar.open;
                    let raw = match old {
                        SignalSide::LongCall => exit - entry,
                        SignalSide::LongPut => entry - exit,
                        SignalSide::Flat => 0.0,
                    };
                    let pnl = raw * self.cfg.multiplier - cost(entry, exit, &self.cfg);
                    trades.push(TradeResult {
                        entry_ts: ets,
                        exit_ts: bar.ts.timestamp(),
                        side: old,
                        entry,
                        exit,
                        pnl,
                    });
                } else {
                    open = Some((old, entry, ets, ei));
                }
            }
            // A signal formed on bar i is executable no earlier than bar i+1. This prevents same-bar look-ahead.
            if let Some(side) = pending.take() {
                if i > 0 {
                    if let Some((old, entry, ets, ei)) = open.take() {
                        if old != side {
                            let exit = bar.open;
                            let raw = match old {
                                SignalSide::LongCall => exit - entry,
                                SignalSide::LongPut => entry - exit,
                                SignalSide::Flat => 0.0,
                            };
                            let pnl = raw * self.cfg.multiplier - cost(entry, exit, &self.cfg);
                            trades.push(TradeResult {
                                entry_ts: ets,
                                exit_ts: bar.ts.timestamp(),
                                side: old,
                                entry,
                                exit,
                                pnl,
                            });
                        } else {
                            open = Some((old, entry, ets, ei));
                        }
                    }
                    if open.is_none() && side != SignalSide::Flat {
                        open = Some((side, bar.open, bar.ts.timestamp(), i));
                    }
                }
            }
            let state = classify_regime(&bars[..=i]);
            if let Some(sig) = strategy.update(bar, &state) {
                if sig.side != SignalSide::Flat {
                    pending = Some(sig.side)
                }
            }
        }
        if let Some((side, entry, ets, _)) = open {
            let Some(b) = bars.last() else {
                return Err(BacktestError::InsufficientData);
            };
            let exit = b.close;
            let raw = match side {
                SignalSide::LongCall => exit - entry,
                SignalSide::LongPut => entry - exit,
                SignalSide::Flat => 0.0,
            };
            let pnl = raw * self.cfg.multiplier - cost(entry, exit, &self.cfg);
            trades.push(TradeResult {
                entry_ts: ets,
                exit_ts: b.ts.timestamp(),
                side,
                entry,
                exit,
                pnl,
            });
        }
        let net = trades.iter().map(|t| t.pnl).sum::<f64>();
        let final_cash = self.cfg.initial_cash + net;
        let (win, pf, dd, sharpe) = stats(&trades, self.cfg.initial_cash);
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
