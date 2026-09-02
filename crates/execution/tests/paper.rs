use chrono::Utc;
use th_domain::{OrderIntent, OrderSide, OrderStatus};
use th_execution::{order_hash, Broker, PaperBroker};
use uuid::Uuid;
fn order(side: OrderSide, reduce_only: bool) -> OrderIntent {
    let mut o = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "SPY260901C00500000".into(),
        side,
        qty: 1,
        limit_price: Some(1.0),
        reduce_only,
        strategy_id: "test".into(),
        created_at: Utc::now(),
        order_hash: String::new(),
    };
    o.order_hash = order_hash(&o);
    o
}
#[tokio::test]
async fn paper_fills_and_deduplicates() {
    let b = PaperBroker::new(10000.0);
    let o = order(OrderSide::Buy, false);
    let x = b.submit(&o).await.unwrap();
    assert_eq!(x.status, OrderStatus::Filled);
    assert!(b.submit(&o).await.is_err());
    assert_eq!(b.positions().await.unwrap().len(), 1);
    let close = OrderIntent {
        client_order_id: Uuid::new_v4(),
        ..order(OrderSide::Sell, true)
    };
    let _ = b.submit(&close).await.unwrap();
    assert!(b.positions().await.unwrap().is_empty());
}

#[test]
fn order_hash_changes_with_material_fields() {
    use chrono::Utc;
    use th_domain::{OrderIntent, OrderSide};
    use uuid::Uuid;
    let mut a = OrderIntent {
        client_order_id: Uuid::new_v4(),
        symbol: "X".into(),
        side: OrderSide::Buy,
        qty: 1,
        limit_price: Some(1.0),
        reduce_only: false,
        strategy_id: "s".into(),
        created_at: Utc::now(),
        order_hash: String::new(),
    };
    let mut b = a.clone();
    b.qty = 2;
    assert_ne!(th_execution::order_hash(&a), th_execution::order_hash(&b));
    a.order_hash = th_execution::order_hash(&a);
    assert!(!a.order_hash.is_empty());
}

#[tokio::test]
async fn execution_engine_blocks_out_of_session_orders() {
    use th_execution::{ExecutionEngine, ExecutionError, PortfolioRisk};
    use th_risk::{RiskGovernor, RiskLimits};

    let broker = PaperBroker::new(10000.0);
    let mut engine = ExecutionEngine::new(broker.clone(), RiskGovernor::new(RiskLimits::default()));

    let portfolio = PortfolioRisk {
        cash: 10000.0,
        realized_today: 0.0,
        positions: vec![],
    };

    // 1. Explicitly mock market closed:
    broker.set_mock_clock(false);
    let o_closed = order(OrderSide::Buy, false);
    let res_closed = engine.execute(o_closed, 1.0, 10.0, &portfolio).await;
    assert!(matches!(res_closed, Err(ExecutionError::MarketClosed(_))));

    // 2. Explicitly mock market open:
    broker.set_mock_clock(true);
    let o_open = order(OrderSide::Buy, false);
    let res_open = engine.execute(o_open, 1.0, 10.0, &portfolio).await;
    assert!(res_open.is_ok(), "In-session order must be executed");
}
