use th_deployment::*;
#[test]
fn draft_can_enter_paper() {
    let mut f = BotFleet::default();
    let _ = f.create("b", "momentum", "1", 10000.0);
    assert!(f.promote_paper("b").is_ok());
}
