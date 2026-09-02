use th_state::*;
#[test]
fn stale_transition_is_rejected() {
    let mut a = StateAuthority::new();
    let r = a.snapshot().revision;
    assert!(a.transition(r, Gate3A::Open, true, "open".into()).is_ok());
    assert!(a
        .transition(r, Gate3A::Closed, false, "stale".into())
        .is_err());
}
