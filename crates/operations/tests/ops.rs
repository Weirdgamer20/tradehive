use th_operations::*;
#[test]
fn resume_requires_health() {
    let h = Health {
        at: chrono::Utc::now(),
        process: true,
        market_data: true,
        broker: true,
        database: true,
        gate3a_open: true,
    };
    assert!(authorize(ControlCommand::Resume, &h).accepted);
}
