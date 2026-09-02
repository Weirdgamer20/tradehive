use chrono::Utc;
use th_domain::OptionType;
use th_options_data::*;
#[test]
fn rejects_cross_underlying() {
    let q = OptionQuote {
        symbol: "QQQ".into(),
        underlying: "QQQ".into(),
        option_type: OptionType::Call,
        strike: 100.0,
        expiry: Utc::now(),
        bid: 1.0,
        ask: 1.2,
        last: None,
        volume: 1,
        open_interest: 1,
        iv: Some(0.2),
        as_of: Utc::now(),
    };
    let c = OptionChain {
        underlying: "SPY".into(),
        spot: 100.0,
        as_of: Utc::now(),
        quotes: vec![q],
    };
    assert!(matches!(
        c.validate(),
        Err(OptionDataError::MixedUnderlying)
    ));
}
