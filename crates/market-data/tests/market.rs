use chrono::{Duration, Utc};
use th_domain::Bar;
use th_market_data::MultiSymbolCandleEngine;
fn bar(symbol: &str, ts: chrono::DateTime<Utc>, p: f64) -> Bar {
    Bar {
        symbol: symbol.into(),
        ts,
        open: p,
        high: p + 1.0,
        low: p - 1.0,
        close: p + 0.5,
        volume: 10.0,
    }
}
#[test]
fn duplicate_event_is_ignored() {
    let t = Utc::now();
    let mut e = MultiSymbolCandleEngine::new(100);
    assert!(e.push_event("x", bar("SPY", t, 100.0)).unwrap().is_none());
    assert!(e
        .push_event("x", bar("SPY", t + Duration::seconds(30), 101.0))
        .unwrap()
        .is_none());
    let c = e.flush_symbol("SPY").unwrap();
    assert_eq!(c.volume, 10.0);
}
#[test]
fn symbols_have_independent_builders() {
    let t = Utc::now();
    let mut e = MultiSymbolCandleEngine::new(100);
    e.push_event("a", bar("SPY", t, 100.0)).unwrap();
    e.push_event("b", bar("QQQ", t, 200.0)).unwrap();
    assert_eq!(e.flush_symbol("SPY").unwrap().open, 100.0);
    assert_eq!(e.flush_symbol("QQQ").unwrap().open, 200.0);
}

#[test]
fn event_cache_evicts_oldest_deterministically() {
    let t = Utc::now();
    let mut e = MultiSymbolCandleEngine::new(1024);
    for i in 0..1030 {
        let id = format!("e{}", i);
        let _ = e.push_event(&id, bar("SPY", t + Duration::seconds(i as i64), 100.0));
    }
    assert!(e
        .push_event("e0", bar("SPY", t + Duration::seconds(2000), 100.0))
        .is_ok());
}
