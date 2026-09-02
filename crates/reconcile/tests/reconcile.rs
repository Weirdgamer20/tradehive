use th_reconcile::*;
#[test]
fn duplicate_external_ids_fail() {
    let t = chrono::Utc::now();
    let x = ExternalOrder {
        id: "1".into(),
        symbol: "SPY".into(),
        qty: 1.0,
        status: "open".into(),
        updated_at: t,
    };
    assert!(reconcile(&[], &[x.clone(), x]).is_err());
}
