use chrono::{Duration, TimeZone, Utc};
use chrono_tz::America::New_York;
use th_domain::{Bar, MarketSessionClock};
use th_hive::{discover_variables, run_analysis_with_q};

fn make_bars(start: chrono::DateTime<Utc>, n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut px = 500.0;
    for i in 0..n {
        let wave = ((i as f64) / 5.0).sin() * 0.5;
        let open = px;
        px = (px + 0.1 + wave * 0.2).max(1.0);
        let close = px;
        let high = open.max(close) + 0.3;
        let low = open.min(close) - 0.3;
        bars.push(Bar {
            symbol: "SPY".into(),
            ts: start + Duration::minutes(i as i64),
            open,
            high,
            low,
            close,
            volume: 1500.0,
        });
    }
    bars
}

#[test]
fn discover_variables_exposes_multi_horizon_momentum_and_session_state() {
    // In-session timestamp: Monday, June 1, 2026 at 10:30 AM ET
    let in_session = New_York
        .with_ymd_and_hms(2026, 6, 1, 10, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    let bars = make_bars(in_session, 80);
    let vars = discover_variables(&bars);

    assert!(!vars.is_empty());
    let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();

    assert!(names.contains(&"MOMENTUM_SHORT"));
    assert!(names.contains(&"MOMENTUM_MEDIUM"));
    assert!(names.contains(&"MOMENTUM_LONG"));
    assert!(names.contains(&"MOMENTUM_COMPOSITE"));
    assert!(names.contains(&"MOMENTUM_CONSENSUS"));
    assert!(names.contains(&"MOMENTUM_CONFIDENCE"));
    assert!(names.contains(&"SESSION_STATE"));
    assert!(names.contains(&"EXPIRY_VALIDITY"));

    let session_var = vars.iter().find(|v| v.name == "SESSION_STATE").unwrap();
    assert_eq!(session_var.value, 1.0);

    let expiry_var = vars.iter().find(|v| v.name == "EXPIRY_VALIDITY").unwrap();
    assert_eq!(expiry_var.value, 1.0);
}

#[test]
fn rl_simulation_gates_on_market_session() {
    // 1. All bars outside market hours (Sunday, June 7, 2026)
    let weekend = New_York
        .with_ymd_and_hms(2026, 6, 7, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let weekend_bars = make_bars(weekend, 100);
    let report_weekend = run_analysis_with_q(&weekend_bars, None);

    // No experiences generated because market was closed
    assert_eq!(report_weekend.learning_updates, 0);
    assert!(report_weekend.experiences.is_empty());

    // 2. Bars during official market hours (Monday, June 1, 2026 at 10:00 AM ET)
    let weekday = New_York
        .with_ymd_and_hms(2026, 6, 1, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let weekday_bars = make_bars(weekday, 100);
    let report_weekday = run_analysis_with_q(&weekday_bars, None);

    // Learning updates executed during open market session
    assert!(report_weekday.learning_updates > 0);
    assert!(!report_weekday.experiences.is_empty());

    // Verify all generated experiences have in-session decision timestamps
    let clock = MarketSessionClock::default();
    for exp in &report_weekday.experiences {
        assert!(clock.is_open(exp.decision_ts));
    }
}
