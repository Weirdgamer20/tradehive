use chrono::{Duration, TimeZone, Utc};
use chrono_tz::America::New_York;
use th_domain::{Bar, MarketState, Regime, SignalSide};
use th_strategy::{
    MultiHorizonMomentumConfig, MultiHorizonMomentumStrategy, Strategy, StrategyRegistry,
};

fn generate_market_bars(start: chrono::DateTime<Utc>, n: usize, trend: f64) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut px = 500.0;
    for i in 0..n {
        let open = px;
        px = (px + trend + ((i as f64 * 0.5).sin() * 0.1)).max(1.0);
        let close = px;
        let high = open.max(close) + 0.2;
        let low = open.min(close) - 0.2;
        bars.push(Bar {
            symbol: "SPY".into(),
            ts: start + Duration::minutes(i as i64),
            open,
            high,
            low,
            close,
            volume: 10_000.0,
        });
    }
    bars
}

#[test]
fn strategy_registry_creates_multi_horizon_momentum() {
    let registry = StrategyRegistry::new();
    assert!(registry
        .ids()
        .contains(&"multi_horizon_momentum".to_string()));

    let strat = registry.create("multi_horizon_momentum");
    assert!(strat.is_ok());
    assert_eq!(strat.unwrap().spec().id, "multi_horizon_momentum");

    // Ensure seed_ids is still exactly 30
    assert_eq!(
        registry.seed_ids().len(),
        30,
        "Seed registry must preserve 30 canonical seeds"
    );
}

#[test]
fn multi_horizon_momentum_blocks_outside_market_hours() {
    let mut strat = MultiHorizonMomentumStrategy::new();
    let dummy_state = MarketState {
        symbol: "SPY".into(),
        regime: Regime::TrendingBull,
        volatility: 0.15,
        momentum: 0.05,
        volume_ratio: 1.0,
        as_of: Utc::now(),
    };

    // 1. Outside market hours: 8:00 AM ET (BeforeMarketOpen, ends at 9:19 AM ET)
    let pre_market = New_York
        .with_ymd_and_hms(2026, 6, 1, 8, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    let bars = generate_market_bars(pre_market, 80, 0.5);
    for b in &bars[..79] {
        let _ = strat.update(b, &dummy_state);
    }
    let sig = strat
        .update(&bars[79], &dummy_state)
        .expect("should produce market closed signal");
    assert_eq!(sig.side, SignalSide::Flat);
    assert_eq!(sig.reason, "MARKET_CLOSED");

    // 2. Weekend: Saturday 11:00 AM ET
    strat.reset();
    let weekend = New_York
        .with_ymd_and_hms(2026, 6, 6, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let weekend_bars = generate_market_bars(weekend, 80, 0.5);
    for b in &weekend_bars[..79] {
        let _ = strat.update(b, &dummy_state);
    }
    let sig_wknd = strat
        .update(&weekend_bars[79], &dummy_state)
        .expect("weekend signal");
    assert_eq!(sig_wknd.side, SignalSide::Flat);
    assert_eq!(sig_wknd.reason, "MARKET_CLOSED");
}

#[test]
fn multi_horizon_momentum_generates_bullish_consensus() {
    let mut strat = MultiHorizonMomentumStrategy::with_config(MultiHorizonMomentumConfig {
        short_lookback: 5,
        medium_lookback: 20,
        long_lookback: 50,
        short_weight: 0.25,
        medium_weight: 0.40,
        long_weight: 0.35,
        min_agreement_ratio: 0.66,
        min_signal_strength: 0.05,
        min_expiry_minutes: 180,
    });

    let dummy_state = MarketState {
        symbol: "SPY".into(),
        regime: Regime::TrendingBull,
        volatility: 0.15,
        momentum: 0.05,
        volume_ratio: 1.0,
        as_of: Utc::now(),
    };

    // Monday, June 1, 2026 starting at 10:00 AM ET (well within regular session)
    let in_session = New_York
        .with_ymd_and_hms(2026, 6, 1, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    // Strong upward trend
    let bars = generate_market_bars(in_session, 70, 0.4);
    for b in &bars[..69] {
        let _ = strat.update(b, &dummy_state);
    }

    let last_bar = &bars[69];
    let sig = strat
        .update(last_bar, &dummy_state)
        .expect("signal must be generated");
    assert_eq!(sig.side, SignalSide::LongCall);
    assert!(sig.strength > 0.05);
    assert!(sig.reason.contains("multi-horizon bullish consensus"));

    // Verify feature extraction at the same timestamp
    let features = strat
        .extract_features(&bars, last_bar.ts, None)
        .expect("features should be extracted");
    assert!(features.short_momentum > 0.0);
    assert!(features.medium_momentum > 0.0);
    assert!(features.long_momentum > 0.0);
    assert!(features.consensus);
    assert!(features.session_state.is_open());
}

#[test]
fn multi_horizon_momentum_generates_bearish_consensus() {
    let mut strat = MultiHorizonMomentumStrategy::new();
    let dummy_state = MarketState {
        symbol: "SPY".into(),
        regime: Regime::TrendingBear,
        volatility: 0.15,
        momentum: -0.05,
        volume_ratio: 1.0,
        as_of: Utc::now(),
    };

    // Tuesday, June 2, 2026 at 11:00 AM ET (in-session)
    let in_session = New_York
        .with_ymd_and_hms(2026, 6, 2, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    // Strong downward trend
    let bars = generate_market_bars(in_session, 80, -0.4);
    for b in &bars[..79] {
        let _ = strat.update(b, &dummy_state);
    }

    let sig = strat.update(&bars[79], &dummy_state).expect("signal");
    assert_eq!(sig.side, SignalSide::LongPut);
    assert!(sig.strength > 0.05);
    assert!(sig.reason.contains("multi-horizon bearish consensus"));
}
