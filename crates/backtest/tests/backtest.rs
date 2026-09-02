use chrono::{Duration, Utc};
use th_backtest::{BacktestConfig, Backtester};
use th_domain::Bar;
use th_strategy::Momentum;
#[test]
fn runs_without_lookahead() {
    let s = Utc::now();
    let bs = (0..80)
        .map(|i| {
            let p = 100.0 + i as f64 * 0.3;
            Bar {
                symbol: "SPY".into(),
                ts: s + Duration::minutes(i),
                open: p,
                high: p + 0.4,
                low: p - 0.1,
                close: p + 0.2,
                volume: 1000.0,
            }
        })
        .collect::<Vec<_>>();
    let mut st = Momentum::default();
    let r = Backtester::new(BacktestConfig::default())
        .run(&mut st, &bs)
        .unwrap();
    assert!(r.strategy_id == "momentum")
}
