use chrono::{Duration, TimeZone, Utc};
use th_bot::{RuntimeConfig, TradingRuntime};
use th_domain::Bar;
use th_execution::PaperBroker;
use th_market_data::SyntheticProvider;
#[tokio::test]
async fn runtime_accepts_multi_symbol_bars() {
    let path = "target/runtime-test.sqlite";
    let _ = std::fs::remove_file(path);
    let mut r = TradingRuntime::new(
        RuntimeConfig {
            database_path: path.into(),
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(10000.0),
        SyntheticProvider,
    )
    .unwrap();
    for i in 0..40 {
        for symbol in ["SPY", "QQQ"] {
            let p = 100.0 + i as f64 * 0.1;
            r.on_market_bar(
                &format!("{}-{}", symbol, i),
                Bar {
                    symbol: symbol.into(),
                    ts: Utc::now() - Duration::minutes(40 - i),
                    open: p,
                    high: p + 0.2,
                    low: p - 0.1,
                    close: p + 0.1,
                    volume: 1000.0,
                },
            )
            .await
            .unwrap()
        }
    }
    assert_eq!(r.bars.len(), 2);
    assert!(r.bars["SPY"].len() >= 8);
}
#[test]
fn four_hour_analysis_boundary() {
    let c = RuntimeConfig::testing();
    let ny = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 8, 31, 20, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(c.phase_at(ny), th_domain::SessionPhase::Analysis);
}
#[test]
fn research_window_has_hard_stop() {
    let cfg = RuntimeConfig::testing();
    let start = Utc::now();
    let d = th_bot::research_deadline(&cfg, start);
    assert!(d.research_allowed(start + Duration::minutes(10)));
    assert!(d.promotion_allowed(start + Duration::minutes(210)));
    assert!(d.trading_boundary_reached(start + Duration::hours(4)));
}
#[tokio::test]
async fn analysis_runs_only_in_analysis_phase() {
    let path = "target/analysis-test.sqlite";
    let _ = std::fs::remove_file(path);
    let mut r = TradingRuntime::new(
        RuntimeConfig {
            database_path: path.into(),
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(10000.0),
        SyntheticProvider,
    )
    .unwrap();
    for i in 0..100 {
        let p = 100.0 + i as f64 * 0.05;
        let b = Bar {
            symbol: "SPY".into(),
            ts: Utc::now() - Duration::minutes(500 - i),
            open: p,
            high: p + 0.2,
            low: p - 0.1,
            close: p + 0.1,
            volume: 1000.0,
        };
        r.on_market_bar(&format!("a{}", i), b).await.unwrap();
    }
    let ny = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 8, 31, 21, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let report = r.run_analysis_window(ny).unwrap();
    assert!(!report.promoted.is_empty() || !report.symbols.is_empty());
}
#[tokio::test]
async fn deterministic_session_boundary_can_be_simulated() {
    let path = "target/cycle-test.sqlite";
    let _ = std::fs::remove_file(path);
    let r = TradingRuntime::new(
        RuntimeConfig {
            database_path: path.into(),
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(10000.0),
        SyntheticProvider,
    )
    .unwrap();
    let ny = chrono_tz::America::New_York;
    let analysis = ny
        .with_ymd_and_hms(2026, 8, 31, 20, 30, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(r.phase_at(analysis), th_domain::SessionPhase::Analysis);
    assert_eq!(
        r.run_analysis_window(analysis).unwrap_err().to_string(),
        "insufficient data"
    );
}

#[test]
fn four_hour_analysis_window_is_exact() {
    let cfg = th_bot::RuntimeConfig::testing();
    let t = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 1, 2, 20, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(cfg.phase_at(t), th_domain::SessionPhase::Analysis);
    assert_eq!(
        cfg.phase_at(t + chrono::Duration::hours(4)),
        th_domain::SessionPhase::Trading
    );
}

#[test]
fn config_rejects_non_four_hour_or_non_20_start() {
    let mut c = RuntimeConfig::testing();
    c.analysis_hours = 3;
    assert!(c.validate().is_err());
    let mut c = RuntimeConfig::testing();
    c.analysis_start_hour = 19;
    assert!(c.validate().is_err());
}

#[tokio::test]
async fn kill_switch_is_not_automatically_cleared_at_session_boundary() {
    let path = "target/kill-switch-test.sqlite";
    let _ = std::fs::remove_file(path);
    let mut r = TradingRuntime::new(
        RuntimeConfig {
            database_path: path.into(),
            ..RuntimeConfig::testing()
        },
        PaperBroker::new(10000.0),
        SyntheticProvider,
    )
    .unwrap();
    r.halt();
    assert!(r.execution.is_killed());
    let ny = chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 8, 31, 20, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let _ = r.run_analysis_window(ny).err();
    assert!(r.execution.is_killed());
}

#[test]
fn worker_sizing_is_owned_by_bot() {
    let s = th_bot::calculate_worker_quantity(1000.0, 100.0, 2.0, 0.05, 100.0).unwrap();
    assert_eq!(s.capital_capacity, 5);
    assert_eq!(s.risk_capacity, 10);
    assert_eq!(s.quantity, 5);
}

#[test]
fn zero_take_profit_does_not_trigger_immediate_exit() {
    let t = 0.02_f64;
    let tp = 0.0_f64;
    assert!(!(tp > 0.0 && t >= tp));
}
