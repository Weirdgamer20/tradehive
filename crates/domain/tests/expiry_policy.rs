use chrono::{Duration, TimeZone, Utc};
use chrono_tz::America::New_York;
use th_domain::OptionExpiryPolicy;

#[test]
fn expiry_policy_exact_180_minutes_is_valid() {
    let policy = OptionExpiryPolicy::default();
    assert_eq!(policy.min_expiry_minutes, 180);

    let now = Utc::now();
    let expiry_exact_180 = now + Duration::minutes(180);

    assert_eq!(
        OptionExpiryPolicy::time_to_expiration_minutes(now, expiry_exact_180),
        180
    );
    assert!(policy.is_valid_expiry(now, expiry_exact_180));
}

#[test]
fn expiry_policy_less_than_180_minutes_is_invalid() {
    let policy = OptionExpiryPolicy::default();
    let now = Utc::now();

    // 179 minutes -> Invalid
    let expiry_179 = now + Duration::minutes(179);
    assert_eq!(
        OptionExpiryPolicy::time_to_expiration_minutes(now, expiry_179),
        179
    );
    assert!(!policy.is_valid_expiry(now, expiry_179));

    // 120 minutes (old lower bound) -> Now Invalid!
    let expiry_120 = now + Duration::minutes(120);
    assert!(!policy.is_valid_expiry(now, expiry_120));

    // 0 minutes -> Invalid
    assert!(!policy.is_valid_expiry(now, now));

    // Negative minutes (past expiry) -> Invalid
    let past = now - Duration::minutes(10);
    assert!(!policy.is_valid_expiry(now, past));
}

#[test]
fn expiry_policy_longer_than_180_minutes_is_valid() {
    let policy = OptionExpiryPolicy::default();
    let now = Utc::now();

    // 181 minutes -> Valid
    let expiry_181 = now + Duration::minutes(181);
    assert!(policy.is_valid_expiry(now, expiry_181));

    // 240 minutes -> Valid
    let expiry_240 = now + Duration::minutes(240);
    assert!(policy.is_valid_expiry(now, expiry_240));

    // Multi-day -> Valid
    let expiry_days = now + Duration::days(14);
    assert!(policy.is_valid_expiry(now, expiry_days));
}

#[test]
fn expiry_policy_dst_transition_preserves_exact_minutes() {
    let policy = OptionExpiryPolicy::default();

    // Fall back transition in New York: November 1, 2026
    // Decision time: Friday, October 30, 2026 at 15:00 EDT (UTC-4)
    let decision = New_York
        .with_ymd_and_hms(2026, 10, 30, 15, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    // Option expiry: Monday, November 2, 2026 at 16:00 EST (UTC-5)
    let expiry = New_York
        .with_ymd_and_hms(2026, 11, 2, 16, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    // Physical elapsed hours:
    // Oct 30 15:00 EDT = 19:00 UTC
    // Nov 2 16:00 EST = 21:00 UTC
    // Total hours = 24*3 + 2 = 74 hours = 4440 minutes
    let elapsed = OptionExpiryPolicy::time_to_expiration_minutes(decision, expiry);
    assert_eq!(elapsed, 4440);
    assert!(policy.is_valid_expiry(decision, expiry));

    // Testing an option expiring exactly 180 physical minutes across DST transition
    let expiry_exact = decision + Duration::minutes(180);
    assert!(policy.is_valid_expiry(decision, expiry_exact));
    let expiry_short = decision + Duration::minutes(179);
    assert!(!policy.is_valid_expiry(decision, expiry_short));
}
