use chrono::Utc;
use th_domain::{OrderIntent, OrderSide};
use th_risk::{PortfolioRisk, RiskGovernor, RiskLimits};
use uuid::Uuid;
fn o(n: u32) -> OrderIntent {
    OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "OPT".into(),
        side: OrderSide::Buy,
        qty: n,
        limit_price: Some(1.0),
        reduce_only: false,
        strategy_id: "x".into(),
        created_at: Utc::now(),
        order_hash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
    }
}
#[test]
fn rejects_large_order() {
    let mut r = RiskGovernor::new(RiskLimits::default());
    let p = PortfolioRisk {
        cash: 10000.0,
        realized_today: 0.0,
        positions: vec![],
    };
    assert!(r.authorize(&o(100), 1.0, 1.0, &p).is_err())
}
#[test]
fn binds_token_to_order_hash() {
    let mut r = RiskGovernor::new(RiskLimits::default());
    let p = PortfolioRisk {
        cash: 10000.0,
        realized_today: 0.0,
        positions: vec![],
    };
    let x = o(1);
    let a = r.authorize(&x, 1.0, 1.0, &p).unwrap();
    assert!(r.validate_token(&a, x.client_order_id, "wrong").is_err());
}

#[test]
fn reduce_only_buy_is_rejected() {
    use th_domain::{OrderIntent, OrderSide};
    use uuid::Uuid;
    let mut g = th_risk::RiskGovernor::new(th_risk::RiskLimits::default());
    let o = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "X".into(),
        side: OrderSide::Buy,
        qty: 1,
        limit_price: Some(1.0),
        reduce_only: true,
        strategy_id: "s".into(),
        created_at: chrono::Utc::now(),
        order_hash: "x".into(),
    };
    let p = th_risk::PortfolioRisk {
        cash: 1000.0,
        realized_today: 0.0,
        positions: vec![],
    };
    assert!(g.authorize(&o, 1.0, 1.0, &p).is_err());
}
