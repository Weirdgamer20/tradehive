use th_sentinel::*;
#[test]
fn unhealthy_never_authorizes() {
    let mut s = Sentinel::default();
    s.set("market_data", HealthState::Failed, "stale");
    assert!(s.authorize_trading().is_err());
}
#[test]
fn healthy_snapshot_authorizes() {
    let mut s = Sentinel::default();
    for n in ["market_data", "broker", "database", "risk", "state"] {
        s.set(n, HealthState::Healthy, "ok");
    }
    assert!(s.authorize_trading().is_ok());
    assert!(s.snapshot(true, true).healthy());
}
