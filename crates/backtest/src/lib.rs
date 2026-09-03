use serde::{Deserialize, Serialize};
use th_domain::{Bar, OptionQuote, OptionType, SignalSide};
use th_strategy::{classify_regime, Strategy};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExecutionModel {
    Exploratory,
    #[default]
    RealisticBar,
    RealisticQuote,
    #[serde(alias = "Naive")]
    Naive,
    #[serde(alias = "Realistic")]
    Realistic,
}

impl ExecutionModel {
    pub fn is_quote_realistic(&self) -> bool {
        matches!(self, Self::RealisticQuote)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExecutionOrdering {
    #[default]
    ExitFirst,
    Fifo,
    Sequential,
}

fn default_execution_model() -> ExecutionModel {
    ExecutionModel::RealisticBar
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
fn default_target_delta() -> f64 {
    0.50
}
fn default_assumed_iv() -> f64 {
    0.25
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
    #[serde(default)]
    pub execution_ordering: ExecutionOrdering,
    #[serde(default = "default_target_delta")]
    pub target_delta: f64,
    #[serde(default = "default_assumed_iv")]
    pub assumed_iv: f64,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            initial_cash: 10_000.0,
            fee_bps: 1.5,
            slippage_bps: 2.0,
            multiplier: 100.0,
            max_hold_bars: 12,
            execution_model: ExecutionModel::RealisticBar,
            spread_bps: 20.0,
            max_volume_participation_pct: 0.10,
            latency_bars: 1,
            execution_ordering: ExecutionOrdering::ExitFirst,
            target_delta: 0.50,
            assumed_iv: 0.25,
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
    #[serde(default)]
    pub contract_symbol: Option<String>,
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
    #[serde(default)]
    pub total_return: f64,
    #[serde(default)]
    pub cagr: f64,
    #[serde(default)]
    pub volatility: f64,
    #[serde(default)]
    pub sortino: f64,
    #[serde(default)]
    pub calmar: f64,
    #[serde(default)]
    pub average_drawdown: f64,
    #[serde(default)]
    pub drawdown_duration: usize,
    #[serde(default)]
    pub expectancy: f64,
    #[serde(default)]
    pub trade_count: usize,
    #[serde(default)]
    pub average_holding_time: f64,
    #[serde(default)]
    pub tail_loss: f64,
    #[serde(default)]
    pub var_95: f64,
    #[serde(default)]
    pub cvar_95: f64,
    #[serde(default)]
    pub gain_loss_ratio: f64,
}

impl BacktestReport {
    pub fn empty(strategy_id: &str, initial_cash: f64, execution_model: ExecutionModel) -> Self {
        Self {
            strategy_id: strategy_id.to_string(),
            initial_cash,
            final_cash: initial_cash,
            trades: Vec::new(),
            net_pnl: 0.0,
            win_rate: 0.0,
            profit_factor: 0.0,
            max_drawdown: 0.0,
            sharpe: 0.0,
            turnover: 0.0,
            execution_model,
            total_return: 0.0,
            cagr: 0.0,
            volatility: 0.0,
            sortino: 0.0,
            calmar: 0.0,
            average_drawdown: 0.0,
            drawdown_duration: 0,
            expectancy: 0.0,
            trade_count: 0,
            average_holding_time: 0.0,
            tail_loss: 0.0,
            var_95: 0.0,
            cvar_95: 0.0,
            gain_loss_ratio: 0.0,
        }
    }
}

#[derive(Debug, Error)]
pub enum BacktestError {
    #[error("insufficient data")]
    InsufficientData,
    #[error("invalid split")]
    InvalidSplit,
}

pub struct ComprehensiveStats {
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub average_drawdown: f64,
    pub drawdown_duration: usize,
    pub sharpe: f64,
    pub sortino: f64,
    pub calmar: f64,
    pub total_return: f64,
    pub cagr: f64,
    pub volatility: f64,
    pub expectancy: f64,
    pub turnover: f64,
    pub trade_count: usize,
    pub average_holding_time: f64,
    pub tail_loss: f64,
    pub var_95: f64,
    pub cvar_95: f64,
    pub gain_loss_ratio: f64,
}

pub fn calculate_comprehensive_stats(
    trades: &[TradeResult],
    initial: f64,
    total_bars: usize,
) -> ComprehensiveStats {
    let trade_count = trades.len();
    if trade_count == 0 || initial <= 0.0 {
        return ComprehensiveStats {
            win_rate: 0.0,
            profit_factor: 0.0,
            max_drawdown: 0.0,
            average_drawdown: 0.0,
            drawdown_duration: 0,
            sharpe: 0.0,
            sortino: 0.0,
            calmar: 0.0,
            total_return: 0.0,
            cagr: 0.0,
            volatility: 0.0,
            expectancy: 0.0,
            turnover: 0.0,
            trade_count: 0,
            average_holding_time: 0.0,
            tail_loss: 0.0,
            var_95: 0.0,
            cvar_95: 0.0,
            gain_loss_ratio: 0.0,
        };
    }

    let wins = trades.iter().filter(|t| t.pnl > 0.0).count();
    let losses = trades.iter().filter(|t| t.pnl < 0.0).count();
    let gains_sum: f64 = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    let losses_sum: f64 = trades.iter().filter(|t| t.pnl < 0.0).map(|t| -t.pnl).sum();

    let win_rate = wins as f64 / trade_count as f64;
    let profit_factor = if losses_sum > 0.0 {
        gains_sum / losses_sum
    } else if gains_sum > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let avg_win = if wins > 0 {
        gains_sum / wins as f64
    } else {
        0.0
    };
    let avg_loss = if losses > 0 {
        losses_sum / losses as f64
    } else {
        0.0
    };
    let gain_loss_ratio = if avg_loss > 0.0 {
        avg_win / avg_loss
    } else if avg_win > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    let expectancy = (win_rate * avg_win) - ((1.0 - win_rate) * avg_loss);

    let mut eq = initial;
    let mut peak = eq;
    let mut max_dd: f64 = 0.0;
    let mut current_dd_bars = 0usize;
    let mut max_dd_bars = 0usize;
    let mut dd_samples = Vec::new();
    let mut returns = Vec::with_capacity(trade_count);

    for t in trades {
        eq += t.pnl;
        if eq >= peak {
            peak = eq;
            current_dd_bars = 0;
        } else {
            let dd = peak - eq;
            max_dd = max_dd.max(dd);
            dd_samples.push(dd);
            current_dd_bars += t.bars_held.max(1);
            max_dd_bars = max_dd_bars.max(current_dd_bars);
        }
        returns.push(t.pnl / initial);
    }

    let average_drawdown = if !dd_samples.is_empty() {
        dd_samples.iter().sum::<f64>() / dd_samples.len() as f64
    } else {
        0.0
    };

    let total_pnl: f64 = trades.iter().map(|t| t.pnl).sum();
    let total_return = total_pnl / initial;

    // Approximate trading days (assuming 78 five-minute bars per regular session)
    let trading_days = (total_bars as f64 / 78.0).max(1.0);
    let years = (trading_days / 252.0).max(0.01);
    let cagr = if eq > 0.0 {
        (eq / initial).powf(1.0 / years) - 1.0
    } else {
        -1.0
    };

    let calmar = if max_dd > 0.0 {
        cagr / (max_dd / initial)
    } else {
        0.0
    };

    let mean_ret = returns.iter().sum::<f64>() / trade_count as f64;
    let var_ret = if trade_count > 1 {
        returns.iter().map(|r| (r - mean_ret).powi(2)).sum::<f64>() / (trade_count - 1) as f64
    } else {
        0.0
    };
    let std_ret = var_ret.sqrt();
    let annualization_factor =
        (252.0 * 78.0 / (total_bars.max(1) as f64 / trade_count as f64)).sqrt();
    let volatility = std_ret * annualization_factor;
    let sharpe = if std_ret > 1e-9 {
        (mean_ret / std_ret) * annualization_factor
    } else {
        0.0
    };

    let downside_var = returns
        .iter()
        .map(|r| if *r < 0.0 { r.powi(2) } else { 0.0 })
        .sum::<f64>()
        / trade_count.max(1) as f64;
    let downside_std = downside_var.sqrt();
    let sortino = if downside_std > 1e-9 {
        (mean_ret / downside_std) * annualization_factor
    } else {
        0.0
    };

    let total_notional: f64 = trades.iter().map(|t| (t.entry + t.exit) * 100.0).sum();
    let turnover = total_notional / initial;
    let average_holding_time =
        trades.iter().map(|t| t.bars_held).sum::<usize>() as f64 / trade_count as f64;

    // VaR 95 and CVaR 95
    let mut sorted_returns = returns.clone();
    sorted_returns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tail_loss = sorted_returns.first().cloned().unwrap_or(0.0);
    let var_idx = (sorted_returns.len() as f64 * 0.05).floor() as usize;
    let var_95 = sorted_returns.get(var_idx).cloned().unwrap_or(0.0).abs();
    let cvar_slice = &sorted_returns[..=var_idx.min(sorted_returns.len() - 1)];
    let cvar_95 = if !cvar_slice.is_empty() {
        (cvar_slice.iter().sum::<f64>() / cvar_slice.len() as f64).abs()
    } else {
        0.0
    };

    ComprehensiveStats {
        win_rate,
        profit_factor,
        max_drawdown: max_dd,
        average_drawdown,
        drawdown_duration: max_dd_bars,
        sharpe,
        sortino,
        calmar,
        total_return,
        cagr,
        volatility,
        expectancy,
        turnover,
        trade_count,
        average_holding_time,
        tail_loss,
        var_95,
        cvar_95,
        gain_loss_ratio,
    }
}

/// Computes Black-Scholes option price for synthetic or quote-less backtesting
fn option_mark_bs(
    spot: f64,
    strike: f64,
    remaining_days: f64,
    iv: f64,
    r: f64,
    ty: OptionType,
) -> f64 {
    th_domain::black_scholes::price(spot, strike, (remaining_days / 365.0).max(1e-6), r, iv, ty)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
struct OpenOptionPosition {
    side: SignalSide,
    entry_px: f64,
    strike: f64,
    ets: i64,
    ei: usize,
    entry_slip: f64,
    entry_spread: f64,
    opt_sym: Option<String>,
}

/// OptionBacktestEngine: Evaluates strategies directly on actual option instrument economics
pub struct OptionBacktestEngine {
    cfg: BacktestConfig,
}

impl OptionBacktestEngine {
    pub fn new(cfg: BacktestConfig) -> Self {
        Self { cfg }
    }

    pub fn run(
        &self,
        strategy: &mut dyn Strategy,
        bars: &[Bar],
    ) -> Result<BacktestReport, BacktestError> {
        self.run_with_quotes(strategy, bars, None)
    }

    pub fn run_with_quotes(
        &self,
        strategy: &mut dyn Strategy,
        bars: &[Bar],
        quotes_map: Option<&std::collections::HashMap<i64, Vec<OptionQuote>>>,
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
        let mut open: Option<OpenOptionPosition> = None;
        let mut pending: Option<(SignalSide, usize)> = None;
        let dte_days = 7.0; // Benchmark 7-day DTE option

        for i in 0..bars.len() {
            let bar = &bars[i];
            let bar_ts = bar.ts.timestamp();

            // Order Execution Ordering: EXIT_FIRST
            // 1. Process exits first
            if let Some(pos) = open.take() {
                let bars_held = i.saturating_sub(pos.ei);
                if bars_held >= self.cfg.max_hold_bars {
                    let ty = match pos.side {
                        SignalSide::LongCall => OptionType::Call,
                        SignalSide::LongPut => OptionType::Put,
                        SignalSide::Flat => OptionType::Call,
                    };

                    let (exit_px, exit_slip, exit_spread) = if let Some(q_map) = quotes_map {
                        if let Some(quotes) = q_map.get(&bar_ts) {
                            if let Some(q) = quotes.iter().find(|q| q.option_type == ty) {
                                let slip = q.bid * self.cfg.slippage_bps / 10_000.0;
                                (q.bid - slip, slip, q.spread())
                            } else {
                                self.compute_bs_exit(bar.open, pos.strike, bars_held, dte_days, ty)
                            }
                        } else {
                            self.compute_bs_exit(bar.open, pos.strike, bars_held, dte_days, ty)
                        }
                    } else {
                        self.compute_bs_exit(bar.open, pos.strike, bars_held, dte_days, ty)
                    };

                    let fees =
                        (pos.entry_px + exit_px) * self.cfg.multiplier * (self.cfg.fee_bps / 10_000.0);
                    let pnl = (exit_px - pos.entry_px) * self.cfg.multiplier - fees;

                    trades.push(TradeResult {
                        entry_ts: pos.ets,
                        exit_ts: bar_ts,
                        side: pos.side,
                        entry: pos.entry_px,
                        exit: exit_px,
                        pnl,
                        slippage_incurred: (pos.entry_slip + exit_slip) * self.cfg.multiplier,
                        spread_cost: (pos.entry_spread + exit_spread) * self.cfg.multiplier,
                        fees_paid: fees,
                        bars_held,
                        contract_symbol: pos.opt_sym,
                    });
                } else {
                    open = Some(pos);
                }
            }

            // 2. Process pending entries
            if let Some((side, signal_bar)) = pending {
                if i >= signal_bar + self.cfg.latency_bars.max(1) {
                    pending = None;
                    if let Some(pos) = open.take() {
                        if pos.side != side {
                            let ty = match pos.side {
                                SignalSide::LongCall => OptionType::Call,
                                SignalSide::LongPut => OptionType::Put,
                                SignalSide::Flat => OptionType::Call,
                            };
                            let bars_held = i.saturating_sub(pos.ei);
                            let (exit_px, exit_slip, exit_spread) =
                                self.compute_bs_exit(bar.open, pos.strike, bars_held, dte_days, ty);
                            let fees = (pos.entry_px + exit_px)
                                * self.cfg.multiplier
                                * (self.cfg.fee_bps / 10_000.0);
                            let pnl = (exit_px - pos.entry_px) * self.cfg.multiplier - fees;
                            trades.push(TradeResult {
                                entry_ts: pos.ets,
                                exit_ts: bar_ts,
                                side: pos.side,
                                entry: pos.entry_px,
                                exit: exit_px,
                                pnl,
                                slippage_incurred: (pos.entry_slip + exit_slip) * self.cfg.multiplier,
                                spread_cost: (pos.entry_spread + exit_spread) * self.cfg.multiplier,
                                fees_paid: fees,
                                bars_held,
                                contract_symbol: pos.opt_sym,
                            });
                        } else {
                            open = Some(pos);
                        }
                    }

                    if open.is_none() && side != SignalSide::Flat {
                        let ty = match side {
                            SignalSide::LongCall => OptionType::Call,
                            SignalSide::LongPut => OptionType::Put,
                            SignalSide::Flat => OptionType::Call,
                        };
                        let strike = bar.open.max(0.01);
                        let (entry_px, entry_slip, entry_spread, opt_sym) = if let Some(q_map) =
                            quotes_map
                        {
                            if let Some(quotes) = q_map.get(&bar_ts) {
                                if let Some(q) = quotes.iter().find(|q| q.option_type == ty) {
                                    let slip = q.ask * self.cfg.slippage_bps / 10_000.0;
                                    (q.ask + slip, slip, q.spread(), Some(q.symbol.clone()))
                                } else {
                                    let (p, s, sp) =
                                        self.compute_bs_entry(bar.open, strike, dte_days, ty);
                                    (p, s, sp, None)
                                }
                            } else {
                                let (p, s, sp) =
                                    self.compute_bs_entry(bar.open, strike, dte_days, ty);
                                (p, s, sp, None)
                            }
                        } else {
                            let (p, s, sp) = self.compute_bs_entry(bar.open, strike, dte_days, ty);
                            (p, s, sp, None)
                        };

                        open = Some(OpenOptionPosition {
                            side,
                            entry_px,
                            strike,
                            ets: bar_ts,
                            ei: i,
                            entry_slip,
                            entry_spread,
                            opt_sym,
                        });
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

        // Close final remaining open position at end of session
        if let Some(pos) = open {
            let Some(b) = bars.last() else {
                return Err(BacktestError::InsufficientData);
            };
            let ty = match pos.side {
                SignalSide::LongCall => OptionType::Call,
                SignalSide::LongPut => OptionType::Put,
                SignalSide::Flat => OptionType::Call,
            };
            let bars_held = bars.len().saturating_sub(pos.ei);
            let (exit_px, exit_slip, exit_spread) =
                self.compute_bs_exit(b.close, pos.strike, bars_held, dte_days, ty);
            let fees = (pos.entry_px + exit_px) * self.cfg.multiplier * (self.cfg.fee_bps / 10_000.0);
            let pnl = (exit_px - pos.entry_px) * self.cfg.multiplier - fees;
            trades.push(TradeResult {
                entry_ts: pos.ets,
                exit_ts: b.ts.timestamp(),
                side: pos.side,
                entry: pos.entry_px,
                exit: exit_px,
                pnl,
                slippage_incurred: (pos.entry_slip + exit_slip) * self.cfg.multiplier,
                spread_cost: (pos.entry_spread + exit_spread) * self.cfg.multiplier,
                fees_paid: fees,
                bars_held,
                contract_symbol: pos.opt_sym,
            });
        }

        let stats = calculate_comprehensive_stats(&trades, self.cfg.initial_cash, bars.len());
        let net = trades.iter().map(|t| t.pnl).sum::<f64>();
        let final_cash = self.cfg.initial_cash + net;

        Ok(BacktestReport {
            strategy_id: strategy.spec().id.clone(),
            initial_cash: self.cfg.initial_cash,
            final_cash,
            trades,
            net_pnl: net,
            win_rate: stats.win_rate,
            profit_factor: stats.profit_factor,
            max_drawdown: stats.max_drawdown,
            sharpe: stats.sharpe,
            turnover: stats.turnover,
            execution_model: self.cfg.execution_model,
            total_return: stats.total_return,
            cagr: stats.cagr,
            volatility: stats.volatility,
            sortino: stats.sortino,
            calmar: stats.calmar,
            average_drawdown: stats.average_drawdown,
            drawdown_duration: stats.drawdown_duration,
            expectancy: stats.expectancy,
            trade_count: stats.trade_count,
            average_holding_time: stats.average_holding_time,
            tail_loss: stats.tail_loss,
            var_95: stats.var_95,
            cvar_95: stats.cvar_95,
            gain_loss_ratio: stats.gain_loss_ratio,
        })
    }

    fn compute_bs_entry(
        &self,
        spot: f64,
        strike: f64,
        dte_days: f64,
        ty: OptionType,
    ) -> (f64, f64, f64) {
        let theoretical =
            option_mark_bs(spot, strike, dte_days, self.cfg.assumed_iv, 0.04, ty).max(0.10);
        let spread_cost = theoretical * (self.cfg.spread_bps / 2.0) / 10_000.0;
        let slip_cost = theoretical * self.cfg.slippage_bps / 10_000.0;
        (
            theoretical + spread_cost + slip_cost,
            slip_cost,
            spread_cost,
        )
    }

    fn compute_bs_exit(
        &self,
        spot: f64,
        strike: f64,
        bars_held: usize,
        dte_days: f64,
        ty: OptionType,
    ) -> (f64, f64, f64) {
        let remaining_days = (dte_days - bars_held as f64 * 5.0 / 1440.0).max(0.01);
        let theoretical =
            option_mark_bs(spot, strike, remaining_days, self.cfg.assumed_iv, 0.04, ty).max(0.01);
        let spread_cost = theoretical * (self.cfg.spread_bps / 2.0) / 10_000.0;
        let slip_cost = theoretical * self.cfg.slippage_bps / 10_000.0;
        (
            theoretical - spread_cost - slip_cost,
            slip_cost,
            spread_cost,
        )
    }
}

/// UnderlyingBacktestEngine: Specifically for strategies trading underlying equity shares
pub struct UnderlyingBacktestEngine {
    cfg: BacktestConfig,
}

impl UnderlyingBacktestEngine {
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
            let bar_ts = bar.ts.timestamp();

            if let Some((old, entry_px, ets, ei, entry_slip, entry_spread)) = open.take() {
                let bars_held = i.saturating_sub(ei);
                if bars_held >= self.cfg.max_hold_bars {
                    let spread_cost = bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                    let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                    let exit_px = bar.open - spread_cost - slip_cost;
                    let fees = (entry_px + exit_px) * (self.cfg.fee_bps / 10_000.0);
                    let pnl = exit_px - entry_px - fees;

                    trades.push(TradeResult {
                        entry_ts: ets,
                        exit_ts: bar_ts,
                        side: old,
                        entry: entry_px,
                        exit: exit_px,
                        pnl,
                        slippage_incurred: entry_slip + slip_cost,
                        spread_cost: entry_spread + spread_cost,
                        fees_paid: fees,
                        bars_held,
                        contract_symbol: None,
                    });
                } else {
                    open = Some((old, entry_px, ets, ei, entry_slip, entry_spread));
                }
            }

            if let Some((side, signal_bar)) = pending {
                if i >= signal_bar + self.cfg.latency_bars.max(1) {
                    pending = None;
                    if let Some((old, entry_px, ets, ei, entry_slip, entry_spread)) = open.take() {
                        if old != side {
                            let spread_cost = bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                            let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                            let exit_px = bar.open - spread_cost - slip_cost;
                            let fees = (entry_px + exit_px) * (self.cfg.fee_bps / 10_000.0);
                            let pnl = exit_px - entry_px - fees;
                            trades.push(TradeResult {
                                entry_ts: ets,
                                exit_ts: bar_ts,
                                side: old,
                                entry: entry_px,
                                exit: exit_px,
                                pnl,
                                slippage_incurred: entry_slip + slip_cost,
                                spread_cost: entry_spread + spread_cost,
                                fees_paid: fees,
                                bars_held: i.saturating_sub(ei),
                                contract_symbol: None,
                            });
                        } else {
                            open = Some((old, entry_px, ets, ei, entry_slip, entry_spread));
                        }
                    }

                    if open.is_none() && side != SignalSide::Flat {
                        let spread_cost = bar.open * (self.cfg.spread_bps / 2.0) / 10_000.0;
                        let slip_cost = bar.open * self.cfg.slippage_bps / 10_000.0;
                        let entry_px = bar.open + spread_cost + slip_cost;
                        open = Some((side, entry_px, bar_ts, i, slip_cost, spread_cost));
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

        let stats = calculate_comprehensive_stats(&trades, self.cfg.initial_cash, bars.len());
        let net = trades.iter().map(|t| t.pnl).sum::<f64>();
        let final_cash = self.cfg.initial_cash + net;

        Ok(BacktestReport {
            strategy_id: strategy.spec().id.clone(),
            initial_cash: self.cfg.initial_cash,
            final_cash,
            trades,
            net_pnl: net,
            win_rate: stats.win_rate,
            profit_factor: stats.profit_factor,
            max_drawdown: stats.max_drawdown,
            sharpe: stats.sharpe,
            turnover: stats.turnover,
            execution_model: self.cfg.execution_model,
            total_return: stats.total_return,
            cagr: stats.cagr,
            volatility: stats.volatility,
            sortino: stats.sortino,
            calmar: stats.calmar,
            average_drawdown: stats.average_drawdown,
            drawdown_duration: stats.drawdown_duration,
            expectancy: stats.expectancy,
            trade_count: stats.trade_count,
            average_holding_time: stats.average_holding_time,
            tail_loss: stats.tail_loss,
            var_95: stats.var_95,
            cvar_95: stats.cvar_95,
            gain_loss_ratio: stats.gain_loss_ratio,
        })
    }
}

/// Unified Backtester entrypoint
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
        let engine = OptionBacktestEngine::new(self.cfg.clone());
        engine.run(strategy, bars)
    }

    pub fn run_options(
        &self,
        strategy: &mut dyn Strategy,
        bars: &[Bar],
        quotes_map: Option<&std::collections::HashMap<i64, Vec<OptionQuote>>>,
    ) -> Result<BacktestReport, BacktestError> {
        let engine = OptionBacktestEngine::new(self.cfg.clone());
        engine.run_with_quotes(strategy, bars, quotes_map)
    }

    pub fn run_underlying(
        &self,
        strategy: &mut dyn Strategy,
        bars: &[Bar],
    ) -> Result<BacktestReport, BacktestError> {
        let engine = UnderlyingBacktestEngine::new(self.cfg.clone());
        engine.run(strategy, bars)
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
    train_pct: f64,
    val_pct: f64,
) -> Result<ChronologicalSplit, BacktestError> {
    if !train_pct.is_finite()
        || !val_pct.is_finite()
        || train_pct <= 0.0
        || val_pct <= 0.0
        || train_pct + val_pct >= 1.0
        || bars.len() < 30
    {
        return Err(BacktestError::InvalidSplit);
    }
    let n = bars.len();
    let i1 = (n as f64 * train_pct).floor() as usize;
    let i2 = (n as f64 * (train_pct + val_pct)).floor() as usize;
    if i1 == 0 || i2 <= i1 || i2 >= n {
        return Err(BacktestError::InvalidSplit);
    }
    Ok(ChronologicalSplit {
        train: bars[..i1].to_vec(),
        validation: bars[i1..i2].to_vec(),
        test: bars[i2..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use th_domain::{MarketState, Signal, Uuid};
    use th_strategy::{Strategy, StrategySpec};

    struct DummyMovingStrategy {
        spec: StrategySpec,
        bars_seen: usize,
    }
    impl DummyMovingStrategy {
        fn new() -> Self {
            Self {
                spec: StrategySpec {
                    id: "dummy_moving".into(),
                    name: "Dummy Moving".into(),
                    version: 1,
                    warmup: 5,
                    max_hold_bars: 3,
                    enabled: true,
                    description: "test".into(),
                },
                bars_seen: 0,
            }
        }
    }
    impl Strategy for DummyMovingStrategy {
        fn spec(&self) -> &StrategySpec {
            &self.spec
        }
        fn update(&mut self, bar: &Bar, _state: &MarketState) -> Option<Signal> {
            self.bars_seen += 1;
            if self.bars_seen.is_multiple_of(5) {
                Some(Signal {
                    id: Uuid::new_v4(),
                    strategy_id: self.spec.id.clone(),
                    symbol: bar.symbol.clone(),
                    side: SignalSide::LongCall,
                    strength: 1.0,
                    reason: "test".into(),
                    generated_at: bar.ts,
                    config_version: "v1".into(),
                    session_id: None,
                    bot_id: None,
                    candidate_id: None,
                })
            } else {
                None
            }
        }
    }

    fn make_test_bars(n: usize) -> Vec<Bar> {
        let base = Utc.with_ymd_and_hms(2026, 9, 4, 14, 30, 0).unwrap();
        (0..n)
            .map(|i| {
                let px = 500.0 + (i as f64) * 0.2;
                Bar {
                    symbol: "SPY".into(),
                    open: px,
                    high: px + 0.5,
                    low: px - 0.5,
                    close: px + 0.1,
                    volume: 10_000.0,
                    ts: base + Duration::minutes(5 * i as i64),
                }
            })
            .collect()
    }

    #[test]
    fn test_option_backtest_engine_execution() {
        let bars = make_test_bars(40);
        let mut strat = DummyMovingStrategy::new();
        let engine = OptionBacktestEngine::new(BacktestConfig::default());
        let report = engine
            .run(&mut strat, &bars)
            .expect("option backtest must succeed");

        assert_eq!(report.strategy_id, "dummy_moving");
        assert!(!report.trades.is_empty(), "must produce option trades");
        assert!(report.trade_count > 0);
        assert!(report.sharpe.is_finite());
        assert!(report.total_return.is_finite());
        assert!(report.win_rate >= 0.0 && report.win_rate <= 1.0);
    }

    #[test]
    fn test_underlying_backtest_engine_execution() {
        let bars = make_test_bars(40);
        let mut strat = DummyMovingStrategy::new();
        let engine = UnderlyingBacktestEngine::new(BacktestConfig::default());
        let report = engine
            .run(&mut strat, &bars)
            .expect("underlying backtest must succeed");

        assert_eq!(report.strategy_id, "dummy_moving");
        assert!(!report.trades.is_empty());
        assert!(report.max_drawdown >= 0.0);
    }

    #[test]
    fn test_comprehensive_stats_calculations() {
        let trades = vec![
            TradeResult {
                entry_ts: 0,
                exit_ts: 1,
                side: SignalSide::LongCall,
                entry: 5.0,
                exit: 6.0,
                pnl: 100.0,
                slippage_incurred: 1.0,
                spread_cost: 2.0,
                fees_paid: 1.0,
                bars_held: 3,
                contract_symbol: None,
            },
            TradeResult {
                entry_ts: 2,
                exit_ts: 3,
                side: SignalSide::LongCall,
                entry: 5.0,
                exit: 4.5,
                pnl: -50.0,
                slippage_incurred: 1.0,
                spread_cost: 2.0,
                fees_paid: 1.0,
                bars_held: 2,
                contract_symbol: None,
            },
        ];

        let stats = calculate_comprehensive_stats(&trades, 10_000.0, 78);
        assert_eq!(stats.trade_count, 2);
        assert_eq!(stats.win_rate, 0.50);
        assert_eq!(stats.profit_factor, 2.0);
        assert_eq!(stats.total_return, 50.0 / 10_000.0);
        assert!(stats.var_95 >= 0.0);
        assert!(stats.cvar_95 >= 0.0);
    }

    #[test]
    fn test_split_validation() {
        let bars = make_test_bars(40);
        let res = split(&bars, 0.6, 0.2).expect("valid split");
        assert_eq!(res.train.len(), 24);
        assert_eq!(res.validation.len(), 8);
        assert_eq!(res.test.len(), 8);

        // Invalid split
        assert!(split(&bars, 0.8, 0.3).is_err());
    }
}
