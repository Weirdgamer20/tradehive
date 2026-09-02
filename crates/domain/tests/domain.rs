use chrono::Utc;
use th_domain::{Bar, CandleBuilder};
fn b(t: i64, p: f64, v: f64) -> Bar {
    Bar {
        symbol: "SPY".into(),
        ts: chrono::DateTime::from_timestamp(t, 0).unwrap(),
        open: p,
        high: p + 1.0,
        low: p - 1.0,
        close: p + 0.5,
        volume: v,
    }
}
#[test]
fn aggregates_first_volume() {
    let mut c = CandleBuilder::new("SPY");
    let t = (Utc::now().timestamp() / 300) * 300;
    assert!(c.push(b(t, 100.0, 7.0)).unwrap().is_none());
    assert!(c.push(b(t + 60, 101.0, 3.0)).unwrap().is_none());
    let x = c.flush().unwrap();
    assert_eq!(x.volume, 10.0);
    assert_eq!(x.open, 100.0);
    assert_eq!(x.close, 101.5)
}
#[test]
fn rejects_bad_ohlc() {
    let x = Bar {
        symbol: "SPY".into(),
        ts: Utc::now(),
        open: 1.0,
        high: 0.5,
        low: 0.8,
        close: 1.0,
        volume: 1.0,
    };
    assert!(x.validate().is_err())
}
#[test]
fn black_scholes_call_is_positive() {
    let p = th_domain::black_scholes::price(
        100.0,
        100.0,
        30.0 / 365.0,
        0.04,
        0.25,
        th_domain::OptionType::Call,
    )
    .unwrap();
    assert!(p > 0.0)
}
#[test]
fn parses_occ_symbol() {
    let x = th_domain::occ::parse("SPY260828C00400000").unwrap();
    assert_eq!(x.underlying, "SPY");
    assert_eq!(x.strike, 400.0)
}

#[test]
fn black_scholes_cdf_is_monotonic_and_price_positive() {
    use th_domain::black_scholes::price;
    use th_domain::OptionType;
    let a = price(100.0, 100.0, 30.0 / 365.0, 0.04, 0.25, OptionType::Call).unwrap();
    let b = price(110.0, 100.0, 30.0 / 365.0, 0.04, 0.25, OptionType::Call).unwrap();
    assert!(a > 0.0 && b > a);
}
#[test]
fn future_quote_is_not_tradeable() {
    use chrono::{Duration, Utc};
    use th_domain::{OptionQuote, OptionType};
    let now = Utc::now();
    let q = OptionQuote {
        symbol: "X".into(),
        underlying: "SPY".into(),
        option_type: OptionType::Call,
        strike: 100.0,
        expiry: now + Duration::days(2),
        bid: 1.0,
        ask: 1.1,
        last: 1.05,
        iv: 0.2,
        greeks: None,
        open_interest: 100,
        volume: 100,
        quote_ts: now + Duration::seconds(1),
    };
    assert!(!q.is_tradeable(now, 30));
}
