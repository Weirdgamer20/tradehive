use chrono::{Duration, Utc};
use th_domain::Bar;
use th_strategy::{classify_regime, StrategyRegistry};
fn bars(n: usize) -> Vec<Bar> {
    let s = Utc::now() - Duration::minutes(n as i64);
    (0..n)
        .map(|i| {
            let p = 100.0 + i as f64 * 0.2;
            Bar {
                symbol: "SPY".into(),
                ts: s + Duration::minutes(i as i64),
                open: p,
                high: p + 0.5,
                low: p - 0.2,
                close: p + 0.2,
                volume: 1000.0,
            }
        })
        .collect()
}
#[test]
fn registry_has_no_scalpers() {
    let ids = StrategyRegistry::new().ids();
    assert!(!ids.iter().any(|x| x.contains("scalp")));
    assert!(ids.len() >= 10)
}
#[test]
fn strategies_are_executable() {
    let bs = bars(100);
    let state = classify_regime(&bs);
    for mut s in StrategyRegistry::new().all() {
        for b in &bs {
            let _ = s.update(b, &state);
        }
    }
}
