use th_research::*;
#[test]
fn promotion_needs_multiple_windows() {
    let w = vec![WalkForwardWindow {
        train_start: 0,
        train_end: 10,
        test_start: 10,
        test_end: 20,
        score: 0.1,
    }];
    assert!(!promotion_allowed(
        &w,
        &PromotionGate {
            min_oos_score: 0.0,
            max_drawdown: 0.2,
            min_windows: 2
        }
    ));
}
