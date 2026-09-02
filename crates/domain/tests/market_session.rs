use chrono::{TimeZone, Utc};
use chrono_tz::America::New_York;
use th_domain::{MarketClosedReason, MarketSessionClock, MarketSessionConfig, MarketSessionState};

#[test]
fn market_session_boundaries_regular_day() {
    let clock = MarketSessionClock::new(MarketSessionConfig::default());

    // Monday, June 1, 2026 (regular trading day, EDT, UTC-4)
    // 9:29 AM ET -> 13:29 UTC -> Rejected (BeforeMarketOpen)
    let t_929 = New_York
        .with_ymd_and_hms(2026, 6, 1, 9, 29, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(t_929),
        MarketSessionState::MarketClosed(MarketClosedReason::BeforeMarketOpen)
    );
    assert!(!clock.is_open(t_929));

    // 9:30 AM ET -> 13:30 UTC -> Allowed (Open)
    let t_930 = New_York
        .with_ymd_and_hms(2026, 6, 1, 9, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.session_state_at(t_930), MarketSessionState::Open);
    assert!(clock.is_open(t_930));

    // 3:59 PM ET -> 19:59 UTC -> Allowed (Open)
    let t_1559 = New_York
        .with_ymd_and_hms(2026, 6, 1, 15, 59, 59)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(clock.session_state_at(t_1559), MarketSessionState::Open);
    assert!(clock.is_open(t_1559));

    // 4:00 PM ET -> 20:00 UTC -> Rejected (AfterMarketClose)
    let t_1600 = New_York
        .with_ymd_and_hms(2026, 6, 1, 16, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(t_1600),
        MarketSessionState::MarketClosed(MarketClosedReason::AfterMarketClose)
    );
    assert!(!clock.is_open(t_1600));
}

#[test]
fn market_session_weekend_rejected() {
    let clock = MarketSessionClock::default();

    // Saturday, June 6, 2026 at 11:00 AM ET
    let sat = New_York
        .with_ymd_and_hms(2026, 6, 6, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(sat),
        MarketSessionState::MarketClosed(MarketClosedReason::Weekend)
    );
    assert!(!clock.is_open(sat));

    // Sunday, June 7, 2026 at 11:00 AM ET
    let sun = New_York
        .with_ymd_and_hms(2026, 6, 7, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(sun),
        MarketSessionState::MarketClosed(MarketClosedReason::Weekend)
    );
    assert!(!clock.is_open(sun));
}

#[test]
fn market_session_holidays_rejected() {
    let clock = MarketSessionClock::default();

    // Good Friday: April 3, 2026 at 11:00 AM ET
    let good_friday = New_York
        .with_ymd_and_hms(2026, 4, 3, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(good_friday),
        MarketSessionState::MarketClosed(MarketClosedReason::Holiday("Good Friday".into()))
    );
    assert!(!clock.is_open(good_friday));

    // Memorial Day: May 25, 2026 at 11:00 AM ET
    let memorial_day = New_York
        .with_ymd_and_hms(2026, 5, 25, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(memorial_day),
        MarketSessionState::MarketClosed(MarketClosedReason::Holiday("Memorial Day".into()))
    );

    // Independence Day observed: July 3, 2026 (July 4 is Saturday)
    let independence_day = New_York
        .with_ymd_and_hms(2026, 7, 3, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(independence_day),
        MarketSessionState::MarketClosed(MarketClosedReason::Holiday("Independence Day".into()))
    );

    // Thanksgiving: November 26, 2026
    let thanksgiving = New_York
        .with_ymd_and_hms(2026, 11, 26, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(thanksgiving),
        MarketSessionState::MarketClosed(MarketClosedReason::Holiday("Thanksgiving Day".into()))
    );

    // Christmas: December 25, 2026
    let christmas = New_York
        .with_ymd_and_hms(2026, 12, 25, 11, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        clock.session_state_at(christmas),
        MarketSessionState::MarketClosed(MarketClosedReason::Holiday("Christmas Day".into()))
    );
}

#[test]
fn dst_transitions_correct_behavior() {
    let clock = MarketSessionClock::default();

    // 1. Winter EST (UTC-5): Friday, January 16, 2026
    // 9:30 AM EST = 14:30 UTC
    let winter_open = New_York
        .with_ymd_and_hms(2026, 1, 16, 9, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(winter_open.to_rfc3339(), "2026-01-16T14:30:00+00:00");
    assert!(clock.is_open(winter_open));

    // 4:00 PM EST = 21:00 UTC
    let winter_close = New_York
        .with_ymd_and_hms(2026, 1, 16, 16, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(winter_close.to_rfc3339(), "2026-01-16T21:00:00+00:00");
    assert!(!clock.is_open(winter_close));

    // 2. Summer EDT (UTC-4): Monday, June 15, 2026
    // 9:30 AM EDT = 13:30 UTC
    let summer_open = New_York
        .with_ymd_and_hms(2026, 6, 15, 9, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(summer_open.to_rfc3339(), "2026-06-15T13:30:00+00:00");
    assert!(clock.is_open(summer_open));

    // 4:00 PM EDT = 20:00 UTC
    let summer_close = New_York
        .with_ymd_and_hms(2026, 6, 15, 16, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(summer_close.to_rfc3339(), "2026-06-15T20:00:00+00:00");
    assert!(!clock.is_open(summer_close));

    // Spring forward in 2026: March 8, 2026 (Sunday). First trading day is Monday, March 9, 2026
    let post_spring = New_York
        .with_ymd_and_hms(2026, 3, 9, 9, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(post_spring.to_rfc3339(), "2026-03-09T13:30:00+00:00");
    assert!(clock.is_open(post_spring));

    // Fall back in 2026: November 1, 2026 (Sunday). First trading day is Monday, November 2, 2026
    let post_fall = New_York
        .with_ymd_and_hms(2026, 11, 2, 9, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(post_fall.to_rfc3339(), "2026-11-02T14:30:00+00:00");
    assert!(clock.is_open(post_fall));
}
