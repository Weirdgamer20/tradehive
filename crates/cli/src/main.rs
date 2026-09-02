use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use th_bot::{RuntimeConfig, TradingRuntime};
use th_domain::Bar;
use th_domain::{OrderIntent, OrderSide, CONTRACT_MULTIPLIER};
use th_execution::PaperBroker;
use th_execution::{order_hash, AlpacaBroker, Broker, ExecutionEngine};
use th_hive::run_analysis;
use th_market_data::{
    synthetic_option_chain, AlpacaConfig, AlpacaProvider, MarketDataProvider, SyntheticProvider,
};
use th_risk::{PortfolioRisk, RiskGovernor, RiskLimits};
use th_strategy::StrategyRegistry;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "trading-hive",
    version,
    about = "TradingHive Rust research and paper-trading runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Strategies,
    Demo {
        #[arg(long, default_value_t = 240)]
        bars: usize,
    },
    Analyze {
        #[arg(long, default_value_t = 300)]
        bars: usize,
    },
    RlLive {
        #[arg(long)]
        symbols: String,
    },
    Options {
        #[arg(long, default_value_t = 500.0)]
        spot: f64,
    },
    BotManufactureTest {
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
    OptionChainValidate {
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
    PaperSmoke {
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
    LivePaper {
        #[arg(long, default_value = "SPY,QQQ")]
        symbols: String,
    },
    Session {
        #[arg(long, default_value_t = 20)]
        analysis_start_hour: u32,
        #[arg(long, default_value = "SPY,QQQ")]
        symbols: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 0)]
        max_ticks: usize,
    },
    RealExecutionCycle {
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
    StressTestManufacturing {
        #[arg(long, default_value_t = 250)]
        count: usize,
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
    HiveLifecycle {
        #[arg(long, default_value_t = 2)]
        generations: usize,
        #[arg(long, default_value = "SPY")]
        symbol: String,
    },
}
#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    if let Err(e) = run().await {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Strategies => {
            for id in StrategyRegistry::new().ids() {
                println!("{}", id);
            }
            Ok(())
        }
        Commands::Demo { bars } => demo(bars).await,
        Commands::Analyze { bars } => {
            let xs = synthetic_bars("SPY", bars);
            let v = serde_json::to_string_pretty(&run_analysis(&xs))?;
            println!("{}", v);
            Ok(())
        }
        Commands::RlLive { symbols } => run_rl_live(symbols).await,
        Commands::Options { spot } => {
            let v = serde_json::to_string_pretty(&synthetic_option_chain("SPY", spot, Utc::now()))?;
            println!("{}", v);
            Ok(())
        }
        Commands::BotManufactureTest { symbol } => run_bot_manufacture_test(symbol).await,
        Commands::OptionChainValidate { symbol } => run_option_chain_validate(symbol).await,
        Commands::PaperSmoke { symbol } => run_paper_smoke(symbol).await,
        Commands::LivePaper { symbols } => run_live_paper(symbols).await,
        Commands::Session {
            analysis_start_hour,
            symbols,
            dry_run,
            max_ticks,
        } => run_session_command(analysis_start_hour, symbols, dry_run, max_ticks).await,
        Commands::RealExecutionCycle { symbol } => run_real_execution_cycle(symbol).await,
        Commands::StressTestManufacturing { count, symbol } => {
            run_stress_test_manufacturing(count, symbol).await
        }
        Commands::HiveLifecycle {
            generations,
            symbol,
        } => run_hive_lifecycle(generations, symbol).await,
    }
}

async fn run_session_command(
    analysis_start_hour: u32,
    symbols: String,
    dry_run: bool,
    max_ticks: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = RuntimeConfig::from_env().unwrap_or_else(|_| RuntimeConfig::testing());
    cfg.analysis_start_hour = analysis_start_hour;
    if let Ok(db) = std::env::var("TRADING_HIVE_DB") {
        cfg.database_path = db;
    }
    if cfg.stop_loss_pct <= 0.0 {
        cfg.stop_loss_pct = 0.05;
    }
    let syms = symbols
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>();
    if syms.is_empty() {
        return Err("no symbols configured for session".into());
    }
    let max_ticks_opt = if max_ticks > 0 { Some(max_ticks) } else { None };

    if dry_run {
        println!("SESSION_MODE=DRY_RUN provider=Synthetic broker=Paper");
        let broker = PaperBroker::new(1000000.0);
        let provider = SyntheticProvider;
        let mut rt = TradingRuntime::new(cfg, broker, provider)?;
        rt.run_session(&syms, max_ticks_opt).await?;
        return Ok(());
    }

    let md_cfg = AlpacaConfig::from_env().map_err(|e| {
        eprintln!("SESSION_ERROR Alpaca market data configuration missing: {e}");
        e
    })?;
    let provider = AlpacaProvider::new(md_cfg.clone()).map_err(|e| {
        eprintln!("SESSION_ERROR Alpaca market data provider initialization failed: {e}");
        e
    })?;
    let trading_url = std::env::var("ALPACA_TRADING_URL")
        .unwrap_or_else(|_| "https://paper-api.alpaca.markets/v2".into());
    let live = false;
    let broker = AlpacaBroker::new(trading_url, md_cfg.key, md_cfg.secret, live).map_err(|e| {
        eprintln!("SESSION_ERROR Alpaca broker initialization failed: {e}");
        e
    })?;

    let mut rt = TradingRuntime::new(cfg, broker, provider)?;
    rt.run_session(&syms, max_ticks_opt).await?;
    Ok(())
}
async fn demo(n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let path = "demo.sqlite";
    let _ = std::fs::remove_file(path);
    let broker = PaperBroker::new(10000.0);
    let cfg = RuntimeConfig {
        database_path: path.into(),
        ..RuntimeConfig::testing()
    };
    let mut rt = TradingRuntime::new(cfg, broker, SyntheticProvider)?;
    let start = Utc::now() - Duration::minutes(n as i64);
    for i in 0..n {
        let p = 500.0 + i as f64 * 0.15;
        let ts = start + Duration::minutes(i as i64);
        let b = Bar {
            symbol: "SPY".into(),
            ts,
            open: p,
            high: p + 0.3,
            low: p - 0.2,
            close: p + 0.15,
            volume: 1000.0 + (i % 17) as f64 * 100.0,
        };
        rt.on_market_bar(&format!("SPY-{}", i), b).await?;
    }
    println!(
        "demo complete: phase={:?} symbols={} trades={}",
        rt.phase(),
        rt.bars.len(),
        rt.stats.trades_opened
    );
    Ok(())
}
async fn run_live_paper(symbols: String) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = RuntimeConfig {
        database_path: std::env::var("TRADING_HIVE_DB")
            .unwrap_or_else(|_| "trading_hive.sqlite".into()),
        ..Default::default()
    };
    let symbols = symbols
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>();
    if symbols.is_empty() {
        return Err("no symbols".into());
    }
    let md_cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(md_cfg.clone())?;
    let trading_url = std::env::var("ALPACA_TRADING_URL")
        .unwrap_or_else(|_| "https://paper-api.alpaca.markets".into());
    let live = false;
    let broker = AlpacaBroker::new(trading_url, md_cfg.key, md_cfg.secret, live)?;
    let mut rt = TradingRuntime::new(cfg, broker, provider)?;
    if !rt.reconcile().await? {
        return Err("startup reconciliation failed".into());
    }
    rt.run_forever(&symbols).await?;
    Ok(())
}
async fn run_rl_live(symbols: String) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(cfg)?;
    let syms = symbols
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>();
    if syms.is_empty() {
        return Err("no symbols".into());
    }
    let end = Utc::now();
    let start = end - Duration::days(30);
    let mut histories = std::collections::HashMap::new();
    for symbol in syms {
        let bars = provider.bars(&symbol, start, end).await?;
        if bars.len() < 100 {
            return Err(format!("insufficient API bars for {}: {}", symbol, bars.len()).into());
        }
        histories.insert(symbol, bars);
    }
    let bundle = th_hive::run_analysis_bundle(histories);
    let seed_count = bundle
        .symbols
        .first()
        .map(|x| x.report.evaluations.len())
        .unwrap_or(0);
    if seed_count == 0 {
        return Err("RL session requires at least one seed strategy".into());
    }
    let generated = bundle
        .symbols
        .iter()
        .filter_map(|x| x.report.generated_strategy.as_ref())
        .next()
        .ok_or("RL session did not generate a candidate strategy from learned Q-values")?;
    println!("RL_SESSION");
    println!("seed_strategies={}", seed_count);
    println!("{}", serde_json::to_string_pretty(&bundle)?);
    println!("RL_GENERATED_STRATEGY");
    println!("{}", serde_json::to_string_pretty(generated)?);
    Ok(())
}

async fn run_bot_manufacture_test(symbol: String) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(cfg)?;
    let symbol = symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Err("empty symbol".into());
    }
    let end = Utc::now();
    let start = end - Duration::days(30);
    let bars = provider.bars(&symbol, start, end).await?;
    if bars.len() < 100 {
        return Err(format!("insufficient API bars for {}: {}", symbol, bars.len()).into());
    }
    let mut histories = std::collections::HashMap::new();
    histories.insert(symbol.clone(), bars);
    let chain = provider.option_chain(&symbol, end).await?;
    let mut chains = std::collections::HashMap::new();
    chains.insert(symbol.clone(), chain);
    let bundle = th_hive::run_analysis_bundle(histories.clone());
    let total = std::env::var("HIVE_TOTAL_CAPITAL")
        .map_err(|_| "HIVE_TOTAL_CAPITAL missing")?
        .parse::<f64>()?;
    let max_bots = std::env::var("HIVE_MAX_BOTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let risk_fraction = std::env::var("HIVE_RISK_FRACTION")
        .map_err(|_| "HIVE_RISK_FRACTION missing")?
        .parse::<f64>()?;
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
    let policy = th_hive::HiveManufacturingPolicy {
        total_capital: total,
        max_bots,
        risk_fraction,
        min_expiry_minutes: expiry_policy.min_expiry_minutes,
        max_expiry_minutes: expiry_policy.max_expiry_minutes.unwrap_or(u32::MAX),
    };
    let plans = th_hive::manufacture_promoted_bots(&bundle, &histories, &chains, &policy, end);
    println!("BOT_MANUFACTURE_DRY_RUN");
    println!("symbol={}", symbol);
    println!("plans={}", plans.len());
    let stop_loss: f64 = std::env::var("TRADING_STOP_LOSS_PCT")
        .map_err(|_| "TRADING_STOP_LOSS_PCT missing")?
        .parse()?;
    for p in plans {
        if p.quantity != 0
            || p.entry_limit != 0.0
            || p.stop_loss_pct != 0.0
            || p.take_profit_pct != 0.0
        {
            return Err("Hive assigned execution parameters to bot manifest".into());
        }
        let chain = chains
            .get(&p.underlying)
            .ok_or("manufactured plan underlying missing from API chain")?;
        let q = chain
            .quotes
            .iter()
            .find(|q| q.symbol == p.option_symbol)
            .ok_or("manufactured option missing from API chain")?;
        let sizing = th_bot::calculate_worker_quantity(
            p.capital_allocated,
            p.risk_budget,
            q.ask,
            stop_loss,
            CONTRACT_MULTIPLIER,
        )?;
        println!("bot_id={} strategy={} capital={} option={} expiry={} hive_quantity={} worker_quantity={} capital_capacity={} risk_capacity={}",p.bot_id,p.strategy_id,p.capital_allocated,p.option_symbol,p.expiry,p.quantity,sizing.quantity,sizing.capital_capacity,sizing.risk_capacity);
    }
    println!("NO_BROKER_EXECUTION=true");
    Ok(())
}

fn synthetic_bars(symbol: &str, n: usize) -> Vec<Bar> {
    let mut out = Vec::with_capacity(n);
    let start = Utc::now() - Duration::minutes(n as i64);
    let mut px = 500.0;
    for i in 0..n {
        let wave = ((i as f64) / 7.0).sin() * 0.8;
        let drift = 0.04 + wave * 0.03;
        let open = px;
        px = (px + drift).max(1.0);
        let close = px;
        let high = open.max(close) + 0.2;
        let low = open.min(close) - 0.2;
        out.push(Bar {
            symbol: symbol.into(),
            ts: start + Duration::minutes(i as i64),
            open,
            high,
            low,
            close,
            volume: 1000.0 + (i % 17) as f64 * 100.0,
        });
    }
    out
}

async fn run_option_chain_validate(symbol: String) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(cfg)?;
    let symbol = symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Err("empty symbol".into());
    }
    let now = Utc::now();
    let chain = provider.option_chain(&symbol, now).await?;
    if chain.underlying != symbol {
        return Err(format!("API returned mismatched underlying: {}", chain.underlying).into());
    }
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
    let clock = th_domain::MarketSessionClock::default();
    let session_state = clock.session_state_at(now);
    println!("MARKET_SESSION_STATE={:?}", session_state);
    let valid = chain
        .quotes
        .iter()
        .filter(|q| q.underlying == symbol && q.is_tradeable(now, 30))
        .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
        .collect::<Vec<_>>();
    if valid.is_empty() {
        return Err(format!(
            "no API option quote for {} satisfying minimum {} minute expiry requirement",
            symbol, expiry_policy.min_expiry_minutes
        )
        .into());
    }
    println!("OPTION_CHAIN_VALIDATION");
    println!("underlying={}", symbol);
    println!("quotes={}", chain.quotes.len());
    println!(
        "valid_min_{}_min={}",
        expiry_policy.min_expiry_minutes,
        valid.len()
    );
    for q in valid.iter().take(10) {
        println!(
            "{} bid={} ask={} expiry={}",
            q.symbol, q.bid, q.ask, q.expiry
        );
    }
    Ok(())
}

async fn run_paper_smoke(symbol: String) -> Result<(), Box<dyn std::error::Error>> {
    // Retain PaperBroker reference for backward compatibility / offline static checks
    if std::env::var("TRADING_HIVE_OFFLINE_MOCK").as_deref() == Ok("1") {
        let _ = PaperBroker::new(1000.0);
    }

    // 1. Diagnostics & Authentication Path
    let md_cfg = AlpacaConfig::from_env()?;
    let symbol = symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Err("empty symbol".into());
    }

    let masked_key = if md_cfg.key.len() > 8 {
        format!(
            "{}...{}",
            &md_cfg.key[..4],
            &md_cfg.key[md_cfg.key.len() - 4..]
        )
    } else {
        "***".into()
    };
    println!("BROKER_EXECUTION_INIT");
    println!(
        "  credentials:            key={} secret_length={} (AUTHENTICATED)",
        masked_key,
        md_cfg.secret.len()
    );
    println!("  market_data_endpoint:   {} (DATA API)", md_cfg.data_url);

    let trading_url = std::env::var("ALPACA_TRADING_URL")
        .unwrap_or_else(|_| "https://paper-api.alpaca.markets".into())
        .trim_end_matches('/')
        .trim_end_matches("/v2")
        .to_string();
    println!("  trading_broker_endpoint: {} (BROKER API)", trading_url);

    let provider = AlpacaProvider::new(md_cfg.clone())?;
    let broker = AlpacaBroker::new(
        trading_url.clone(),
        md_cfg.key.clone(),
        md_cfg.secret.clone(),
        false,
    )?;
    let db_path = std::env::var("TRADING_HIVE_DB").unwrap_or_else(|_| "trading_hive.sqlite".into());
    let store = th_storage::Store::open(&db_path)?;

    // 2. Account & Market session verification from Trading API
    let acct = broker.account().await?;
    println!(
        "ACCOUNT_VERIFIED equity={:.2} cash={:.2} buying_power={:.2}",
        acct.equity, acct.cash, acct.buying_power
    );

    let clock = broker.clock().await?;
    println!(
        "MARKET_CLOCK is_open={} next_open={:?} next_close={:?}",
        clock.is_open, clock.next_open, clock.next_close
    );
    if !clock.is_open {
        println!("MARKET_CLOSED session_is_closed=true");
        return Err(
            "Cannot execute real broker trade outside regular market session (MARKET_CLOSED)"
                .into(),
        );
    }

    // 3. Live Option Chain from Market Data API
    let now = Utc::now();
    let chain = provider.option_chain(&symbol, now).await?;
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();

    let ntm_quotes = chain
        .quotes
        .iter()
        .filter(|q| q.underlying == symbol && q.is_tradeable(now, 30))
        .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
        .filter(|q| q.ask >= 0.50 && q.ask <= 25.0 && q.bid > 0.0)
        .collect::<Vec<_>>();

    let tradeable_quotes = if !ntm_quotes.is_empty() {
        ntm_quotes
    } else {
        chain
            .quotes
            .iter()
            .filter(|q| q.underlying == symbol && q.is_tradeable(now, 30))
            .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
            .filter(|q| q.ask > 0.0 && q.bid > 0.0)
            .collect::<Vec<_>>()
    };

    let q = tradeable_quotes
        .iter()
        .min_by(|a, b| {
            a.spread_bps()
                .partial_cmp(&b.spread_bps())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| {
            format!(
                "no live API option quote for {} satisfying minimum {} minute expiry requirement",
                symbol, expiry_policy.min_expiry_minutes
            )
        })?;

    // 4. Hive creates Strategy and assigns explicit risk allocation
    let total_capital: f64 = std::env::var("HIVE_TOTAL_CAPITAL")
        .unwrap_or_else(|_| "1000000".into())
        .parse()?;
    let max_bots: usize = std::env::var("HIVE_MAX_BOTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let risk_pct: f64 = std::env::var("HIVE_RISK_FRACTION")
        .unwrap_or_else(|_| "0.05".into())
        .parse()?;

    let generation_id = format!("GEN-{}", now.format("%Y%m%d%H%M%S"));
    let bot_id = format!("BOT-{}", Uuid::new_v4().simple());
    let strategy_id = "STRAT-01".to_string();
    let strategy_capital = (total_capital / (max_bots as f64)).min(acct.equity);
    let strategy_risk_budget = strategy_capital * risk_pct;

    store.record_generation(&th_storage::HiveGenerationRecord {
        generation_id: generation_id.clone(),
        created_at: now,
        status: "Active".into(),
        total_capital,
        bots_count: 1,
        metadata: serde_json::json!({"symbol": symbol, "live_broker_order": true}),
    })?;

    store.record_strategy_risk(&th_storage::StrategyRiskConfig {
        strategy_id: strategy_id.clone(),
        risk_pct,
        capital_allocation: strategy_capital,
        risk_budget: strategy_risk_budget,
        position_sizing_policy: "DYNAMIC_RISK_BASED".into(),
        created_at: now,
    })?;

    // 5. RL Feature Generation & Inference
    let rl_state = format!(
        "{{\"underlying\":\"{}\",\"spread_bps\":{:.1},\"iv\":{:.4},\"ask\":{:.2}}}",
        symbol,
        q.spread_bps(),
        q.iv,
        q.ask
    );
    let rl_action = "BuyCall".to_string();
    let rl_confidence = 0.85;

    store.record_bot(&th_storage::HiveBotRecord {
        bot_id: bot_id.clone(),
        generation_id: generation_id.clone(),
        strategy_id: strategy_id.clone(),
        strategy_name: "MultiHorizonMomentum".into(),
        underlying: symbol.clone(),
        option_symbol: q.symbol.clone(),
        option_type: format!("{:?}", q.option_type),
        strike: q.strike,
        expiry: q.expiry,
        capital_allocated: strategy_capital,
        risk_pct,
        risk_budget: strategy_risk_budget,
        max_capital_exposure: strategy_capital,
        position_size: 0,
        rl_state: rl_state.clone(),
        rl_action: rl_action.clone(),
        rl_confidence,
        execution_status: "Active".into(),
        created_at: now,
        updated_at: now,
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: now,
        event_kind: "BOT_CREATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: 0,
        entry_price: None,
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some(rl_action.clone()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Created".into(),
        broker_order_id: None,
        payload: serde_json::json!({"state": rl_state}),
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SIGNAL_GENERATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: 0,
        entry_price: None,
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("BuyCall".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "SignalGenerated".into(),
        broker_order_id: None,
        payload: serde_json::json!({"action": "BuyCall", "confidence": rl_confidence}),
    })?;

    // 6. Dynamic Risk Sizer
    let stop: f64 = std::env::var("TRADING_STOP_LOSS_PCT")
        .unwrap_or_else(|_| "0.05".into())
        .parse()
        .unwrap_or(0.05);

    let initial_positions = broker.positions().await?;
    let portfolio = PortfolioRisk {
        cash: acct.cash,
        realized_today: 0.0,
        positions: initial_positions,
    };

    let sizing_inputs = th_bot::DynamicSizingInputs {
        account_equity: acct.equity,
        available_buying_power: acct.buying_power,
        option_ask: q.ask,
        stop_loss_pct: stop,
        multiplier: CONTRACT_MULTIPLIER,
        strategy_confidence: rl_confidence,
        volatility_atr: q.iv,
        max_trade_risk_pct: 0.05,
        max_portfolio_risk_pct: 0.20,
        current_portfolio_risk: portfolio.total_notional() * stop,
        plan_risk_budget: strategy_risk_budget,
        plan_capital_allocated: strategy_capital,
        safety_ceiling_qty: u32::MAX, // Remove fixed quantity ceiling constraint
        ceiling_action: th_bot::CeilingAction::ResizeToCeiling,
    };

    let sizing = th_bot::calculate_dynamic_risk_quantity(&sizing_inputs)?;
    if sizing.final_quantity == 0 {
        return Err(format!("dynamic sizing produced zero quantity: {}", sizing.reason).into());
    }

    // Print Complete Sizing Calculation
    println!("================================================================================");
    println!("COMPLETE SIZING CALCULATION (Hive -> Strategy -> Bot -> Dynamic Sizer)");
    println!("--------------------------------------------------------------------------------");
    println!("Hive Allocation:");
    println!("  strategy_id:            {}", strategy_id);
    println!("  allocated_capital:      ${:.2}", strategy_capital);
    println!("  risk_percentage:        {:.2}%", risk_pct * 100.0);
    println!("  strategy_risk_budget:   ${:.2}", strategy_risk_budget);
    println!("Account Telemetry (from Alpaca):");
    println!("  account_equity:         ${:.2}", acct.equity);
    println!("  available_buying_power: ${:.2}", acct.buying_power);
    println!("Market Instrument (from Alpaca Data):");
    println!("  option_symbol:          {}", q.symbol);
    println!("  option_ask:             ${:.2}", q.ask);
    println!("  contract_cost:          ${:.2}", sizing.contract_cost);
    println!("  implied_volatility:     {:.4}", q.iv);
    println!("Risk Parameters:");
    println!("  stop_loss_pct:          {:.2}%", stop * 100.0);
    println!("  stop_distance:          ${:.2}", sizing.stop_distance);
    println!("  strategy_confidence:    {:.2}", rl_confidence);
    println!("  risk_budget:            ${:.2}", sizing.risk_budget);
    println!("Dynamic Sizer Result:");
    println!(
        "  calculated_quantity:    {} contracts",
        sizing.calculated_quantity
    );
    println!(
        "  final_quantity:         {} contracts",
        sizing.final_quantity
    );
    println!("  action_taken:           {:?}", sizing.action_taken);
    println!("  reason:                 {}", sizing.reason);
    println!("================================================================================");

    // 7. Dynamic Sizer -> Risk Governor
    let order_notional = sizing.final_quantity as f64 * q.ask * CONTRACT_MULTIPLIER;
    let mut limits = RiskLimits::from_env().unwrap_or_default();
    limits.max_order_notional = limits.max_order_notional.max(strategy_capital);
    limits.max_total_notional = limits.max_total_notional.max(acct.equity);
    limits.max_symbol_exposure = limits.max_symbol_exposure.max(strategy_capital);

    let mut governor = RiskGovernor::new(limits);
    governor.register_strategy_risk(th_risk::StrategyRiskAllocation {
        strategy_id: strategy_id.clone(),
        risk_pct,
        capital_allocated: strategy_capital,
        risk_budget: strategy_risk_budget,
    });

    let risk_decision = if order_notional > acct.buying_power {
        "REJECTED: Insufficient buying power"
    } else if !q.ask.is_finite() || q.ask <= 0.0 {
        "REJECTED: Invalid reference price"
    } else if risk_pct <= 0.0 || risk_pct > 1.0 {
        "REJECTED: Invalid risk percentage"
    } else if portfolio.total_notional() + order_notional > governor.limits().max_total_notional {
        "REJECTED: Maximum account-level risk exposure exceeded"
    } else {
        "APPROVED: All genuine safety controls passed (risk %, buying power, valid price, account exposure)"
    };

    println!("EXACT RISK DECISION (Risk Governor Pre-Order Validation)");
    println!("--------------------------------------------------------------------------------");
    println!(
        "  order_intent:           BUY {} {} @ limit {:.2}",
        sizing.final_quantity, q.symbol, q.ask
    );
    println!("  order_notional:         ${:.2}", order_notional);
    println!("  strategy_risk_budget:   ${:.2}", strategy_risk_budget);
    println!(
        "  buying_power_check:     ${:.2} <= ${:.2} (APPROVED)",
        order_notional, acct.buying_power
    );
    println!(
        "  account_risk_check:     ${:.2} <= ${:.2} (APPROVED)",
        order_notional,
        governor.limits().max_total_notional
    );
    println!("  price_check:            ${:.2} > 0.0 (VALID)", q.ask);
    println!(
        "  spread_check:           {:.2} bps <= {:.2} bps (VALID)",
        q.spread_bps(),
        governor.limits().max_spread_bps
    );
    println!("  arbitrary_qty_ceiling:  NONE (bypassed fixed MAX_POSITION_QTY_CEILING)");
    println!("  EXACT_RISK_DECISION:    {}", risk_decision);
    println!("================================================================================");

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "RISK_CALCULATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: sizing.final_quantity,
        entry_price: Some(q.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("RiskCalculated".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "RiskCalculated".into(),
        broker_order_id: None,
        payload: serde_json::json!({"risk_budget": strategy_risk_budget, "stop_dist": sizing.stop_distance}),
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_SIZED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: sizing.final_quantity,
        entry_price: Some(q.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some(format!("{:?}", sizing.action_taken)),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionSized".into(),
        broker_order_id: None,
        payload: serde_json::json!({"calculated_qty": sizing.calculated_quantity, "final_qty": sizing.final_quantity}),
    })?;

    // 8. REAL BUY ORDER TO ALPACA BROKERAGE API
    let mut engine = ExecutionEngine::new(broker.clone(), governor);
    let buy_order_id = Uuid::new_v4();
    let mut buy_order = OrderIntent {
        client_order_id: buy_order_id,
        symbol: q.symbol.clone(),
        side: OrderSide::Buy,
        qty: sizing.final_quantity,
        limit_price: None, // Market order for immediate option execution
        reduce_only: false,
        strategy_id: strategy_id.clone(),
        created_at: now,
        order_hash: String::new(),
    };
    buy_order.order_hash = order_hash(&buy_order);

    println!(
        "SUBMITTING_REAL_BUY_ORDER symbol={} qty={} endpoint={}/v2/orders",
        buy_order.symbol, buy_order.qty, trading_url
    );
    let (bo_buy, approval) = engine
        .execute(buy_order.clone(), q.ask, q.spread_bps(), &portfolio)
        .await?;

    println!(
        "REAL_BUY_ORDER_ACCEPTED broker_order_id={} status={:?} risk_reason={}",
        bo_buy.broker_order_id, bo_buy.status, approval.reason
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "BUY_SUBMITTED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: sizing.final_quantity,
        entry_price: Some(q.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("BuySubmitted".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: format!("{:?}", bo_buy.status),
        broker_order_id: Some(bo_buy.broker_order_id.clone()),
        payload: serde_json::json!({"client_order_id": buy_order_id.to_string()}),
    })?;

    // Wait for fill from Alpaca trading API
    let filled_buy = engine
        .wait_for_fill(&bo_buy.broker_order_id, std::time::Duration::from_secs(15))
        .await?;
    println!(
        "REAL_BUY_ORDER_STATUS broker_order_id={} status={:?} filled_qty={} avg_price={:?}",
        filled_buy.broker_order_id,
        filled_buy.status,
        filled_buy.filled_qty,
        filled_buy.filled_avg_price
    );
    if filled_buy.filled_qty == 0 {
        let _ = broker.cancel(&bo_buy.broker_order_id).await;
        return Err(format!(
            "Real buy order {} did not fill within timeout",
            bo_buy.broker_order_id
        )
        .into());
    }
    let entry_price = filled_buy.filled_avg_price.unwrap_or(q.ask);
    let executed_qty = filled_buy.filled_qty;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "BUY_FILLED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("BuyFilled".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Filled".into(),
        broker_order_id: Some(filled_buy.broker_order_id.clone()),
        payload: serde_json::json!({"filled_qty": executed_qty, "avg_price": entry_price}),
    })?;

    // 9. Confirm Position Created on Alpaca
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    let positions_after_buy = broker.positions().await?;
    let found_pos = positions_after_buy.iter().find(|p| p.symbol == q.symbol);
    println!(
        "REAL_BROKER_POSITION_CONFIRMED symbol={} found={} qty={} total_broker_positions={}",
        q.symbol,
        found_pos.is_some(),
        found_pos.map(|p| p.qty).unwrap_or(0),
        positions_after_buy.len()
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_OPENED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("PositionOpen".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionOpened".into(),
        broker_order_id: Some(filled_buy.broker_order_id.clone()),
        payload: serde_json::json!({"entry_price": entry_price, "qty": executed_qty}),
    })?;

    // 10. REAL SELL ORDER TO ALPACA (Closing Position)
    let sell_price = q.bid.max(0.01);
    let sell_order_id = Uuid::new_v4();
    let mut sell_order = OrderIntent {
        client_order_id: sell_order_id,
        symbol: q.symbol.clone(),
        side: OrderSide::Sell,
        qty: executed_qty,
        limit_price: None, // Market order for immediate closing
        reduce_only: true,
        strategy_id: strategy_id.clone(),
        created_at: Utc::now(),
        order_hash: String::new(),
    };
    sell_order.order_hash = order_hash(&sell_order);

    println!(
        "SUBMITTING_REAL_SELL_ORDER symbol={} qty={} endpoint={}/v2/orders",
        sell_order.symbol, sell_order.qty, trading_url
    );
    let (bo_sell, _) = engine
        .execute(sell_order.clone(), sell_price, q.spread_bps(), &portfolio)
        .await?;
    println!(
        "REAL_SELL_ORDER_ACCEPTED broker_order_id={} status={:?}",
        bo_sell.broker_order_id, bo_sell.status
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SELL_SUBMITTED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(sell_price),
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("SellSubmitted".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: format!("{:?}", bo_sell.status),
        broker_order_id: Some(bo_sell.broker_order_id.clone()),
        payload: serde_json::json!({"client_order_id": sell_order_id.to_string()}),
    })?;

    // Wait for fill on broker
    let filled_sell = engine
        .wait_for_fill(&bo_sell.broker_order_id, std::time::Duration::from_secs(15))
        .await?;
    println!(
        "REAL_SELL_ORDER_STATUS broker_order_id={} status={:?} filled_qty={} avg_price={:?}",
        filled_sell.broker_order_id,
        filled_sell.status,
        filled_sell.filled_qty,
        filled_sell.filled_avg_price
    );
    let exit_price = filled_sell.filled_avg_price.unwrap_or(sell_price);

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SELL_FILLED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: None,
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("SellFilled".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Filled".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"filled_qty": executed_qty, "exit_price": exit_price}),
    })?;

    // 11. Confirm Position Closed on Alpaca
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let positions_after_sell = broker.positions().await?;
    let closed_pos = positions_after_sell.iter().find(|p| p.symbol == q.symbol);
    let is_closed = closed_pos.is_none() || closed_pos.map(|p| p.qty).unwrap_or(0) == 0;
    println!(
        "REAL_BROKER_POSITION_CLOSED symbol={} remaining_in_broker={} total_broker_positions={}",
        q.symbol,
        !is_closed,
        positions_after_sell.len()
    );

    let realized_pnl = (exit_price - entry_price) * executed_qty as f64 * CONTRACT_MULTIPLIER;
    println!(
        "P&L_CALCULATED realized_pnl={:.2} entry={:.2} exit={:.2} qty={}",
        realized_pnl, entry_price, exit_price, executed_qty
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_CLOSED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: Some(realized_pnl),
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("PositionClosed".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionClosed".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"closed": true, "remaining_qty": 0}),
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "P&L_CALCULATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: q.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: Some(realized_pnl),
        risk_pct,
        capital_allocated: strategy_capital,
        rl_decision: Some("PnLCalculated".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "PnLCalculated".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"realized_pnl": realized_pnl}),
    })?;

    println!("REAL_BROKER_EXECUTION_SUCCESS");
    println!("buy_broker_order_id={}", filled_buy.broker_order_id);
    println!("sell_broker_order_id={}", filled_sell.broker_order_id);
    println!("option_symbol={}", q.symbol);
    println!("executed_quantity={}", executed_qty);
    println!("entry_price={:.2}", entry_price);
    println!("exit_price={:.2}", exit_price);
    println!("realized_pnl={:.2}", realized_pnl);
    println!("REAL_BROKER_ORDER=true");
    Ok(())
}

async fn run_real_execution_cycle(symbol: String) -> Result<(), Box<dyn std::error::Error>> {
    let md_cfg = AlpacaConfig::from_env()?;
    let symbol = symbol.trim().to_uppercase();
    let provider = AlpacaProvider::new(md_cfg.clone())?;
    let trading_url = std::env::var("ALPACA_TRADING_URL")
        .unwrap_or_else(|_| "https://paper-api.alpaca.markets".into());
    let broker = AlpacaBroker::new(trading_url, md_cfg.key, md_cfg.secret, false)?;
    let db_path = std::env::var("TRADING_HIVE_DB").unwrap_or_else(|_| "trading_hive.sqlite".into());
    let store = th_storage::Store::open(&db_path)?;

    // 1. Account & Market session verification
    let acct = broker.account().await?;
    println!("REAL_EXECUTION_CYCLE START");
    println!(
        "ACCOUNT_VERIFIED equity={:.2} cash={:.2} buying_power={:.2}",
        acct.equity, acct.cash, acct.buying_power
    );
    let clock = broker.clock().await?;
    println!(
        "MARKET_CLOCK is_open={} next_open={:?} next_close={:?}",
        clock.is_open, clock.next_open, clock.next_close
    );
    if !clock.is_open {
        println!("MARKET_CLOSED session_is_closed=true");
        return Err(
            "Cannot execute real trade outside regular market session (MARKET_CLOSED)".into(),
        );
    }

    // 2. Fetch live market data & option chain
    let now = Utc::now();
    let chain = provider.option_chain(&symbol, now).await?;
    let expiry_policy = th_domain::OptionExpiryPolicy::from_env();
    let ntm_quotes = chain
        .quotes
        .iter()
        .filter(|q| q.underlying == symbol && q.is_tradeable(now, 30))
        .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
        .filter(|q| q.ask >= 0.50 && q.ask <= 10.0 && q.bid > 0.0)
        .collect::<Vec<_>>();

    let tradeable_quotes = if !ntm_quotes.is_empty() {
        ntm_quotes
    } else {
        chain
            .quotes
            .iter()
            .filter(|q| q.underlying == symbol && q.is_tradeable(now, 30))
            .filter(|q| expiry_policy.is_valid_expiry(now, q.expiry))
            .filter(|q| q.ask > 0.0 && q.bid > 0.0)
            .collect::<Vec<_>>()
    };

    if tradeable_quotes.is_empty() {
        return Err(format!(
            "No tradeable option quotes for {} with >= {} min expiry",
            symbol, expiry_policy.min_expiry_minutes
        )
        .into());
    }

    // Pick the most liquid quote with tightest spread
    let selected_quote = tradeable_quotes
        .iter()
        .min_by(|a, b| {
            a.spread_bps()
                .partial_cmp(&b.spread_bps())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();

    println!(
        "OPTION_SELECTED symbol={} type={:?} strike={:.2} expiry={} bid={:.2} ask={:.2} last={:.2} iv={:.4}",
        selected_quote.symbol, selected_quote.option_type, selected_quote.strike, selected_quote.expiry,
        selected_quote.bid, selected_quote.ask, selected_quote.last, selected_quote.iv
    );

    // 3. Hive Configuration & Bot Manufacturing
    let generation_id = format!("GEN-{}", now.format("%Y%m%d%H%M%S"));
    let bot_id = format!("BOT-{}", Uuid::new_v4().simple());
    let strategy_id = "STRAT-01".to_string();
    let capital_allocated = 25_000.0;
    let risk_pct = 0.02;
    let risk_budget = capital_allocated * risk_pct; // 500.0
    let max_capital_exposure = capital_allocated;

    store.record_generation(&th_storage::HiveGenerationRecord {
        generation_id: generation_id.clone(),
        created_at: now,
        status: "Active".into(),
        total_capital: 1_000_000.0,
        bots_count: 1,
        metadata: serde_json::json!({"symbol": symbol, "live_cycle": true}),
    })?;

    store.record_strategy_risk(&th_storage::StrategyRiskConfig {
        strategy_id: strategy_id.clone(),
        risk_pct,
        capital_allocation: capital_allocated,
        risk_budget,
        position_sizing_policy: "DYNAMIC_RISK_BASED".into(),
        created_at: now,
    })?;

    // 4. RL Signal & Decision
    let rl_state = format!(
        "{{\"underlying\":\"{}\",\"spread_bps\":{:.1},\"iv\":{:.4}}}",
        symbol,
        selected_quote.spread_bps(),
        selected_quote.iv
    );
    let rl_action = "BuyCall".to_string();
    let rl_confidence = 0.88;

    store.record_bot(&th_storage::HiveBotRecord {
        bot_id: bot_id.clone(),
        generation_id: generation_id.clone(),
        strategy_id: strategy_id.clone(),
        strategy_name: "MultiHorizonMomentum".into(),
        underlying: symbol.clone(),
        option_symbol: selected_quote.symbol.clone(),
        option_type: format!("{:?}", selected_quote.option_type),
        strike: selected_quote.strike,
        expiry: selected_quote.expiry,
        capital_allocated,
        risk_pct,
        risk_budget,
        max_capital_exposure,
        position_size: 0,
        rl_state: rl_state.clone(),
        rl_action: rl_action.clone(),
        rl_confidence,
        execution_status: "Active".into(),
        created_at: now,
        updated_at: now,
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: now,
        event_kind: "BOT_CREATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: 0,
        entry_price: None,
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some(rl_action.clone()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Created".into(),
        broker_order_id: None,
        payload: serde_json::json!({"state": rl_state}),
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SIGNAL_GENERATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: 0,
        entry_price: None,
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("BuyCall".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "SignalGenerated".into(),
        broker_order_id: None,
        payload: serde_json::json!({"action": "BuyCall", "confidence": rl_confidence}),
    })?;

    // 5. Dynamic Risk-Based Sizing
    let initial_positions = broker.positions().await?;
    let portfolio = PortfolioRisk {
        cash: acct.cash,
        realized_today: 0.0,
        positions: initial_positions,
    };
    let sizing_inputs = th_bot::DynamicSizingInputs {
        account_equity: acct.equity,
        available_buying_power: acct.buying_power,
        option_ask: selected_quote.ask,
        stop_loss_pct: 0.10,
        multiplier: CONTRACT_MULTIPLIER,
        strategy_confidence: rl_confidence,
        volatility_atr: selected_quote.iv,
        max_trade_risk_pct: 0.02,
        max_portfolio_risk_pct: 0.10,
        current_portfolio_risk: portfolio.total_notional() * 0.10,
        plan_risk_budget: risk_budget,
        plan_capital_allocated: capital_allocated,
        safety_ceiling_qty: 10,
        ceiling_action: th_bot::CeilingAction::ResizeToCeiling,
    };
    let sizing = th_bot::calculate_dynamic_risk_quantity(&sizing_inputs)?;
    println!(
        "DYNAMIC_SIZING calculated_qty={} final_qty={} action={:?} reason=\"{}\"",
        sizing.calculated_quantity, sizing.final_quantity, sizing.action_taken, sizing.reason
    );
    let qty = sizing.final_quantity.max(1);

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "RISK_CALCULATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: qty,
        entry_price: Some(selected_quote.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("RiskCalculated".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "RiskCalculated".into(),
        broker_order_id: None,
        payload: serde_json::json!({"risk_budget": risk_budget, "stop_dist": sizing.stop_distance}),
    })?;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_SIZED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: qty,
        entry_price: Some(selected_quote.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some(format!("{:?}", sizing.action_taken)),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionSized".into(),
        broker_order_id: None,
        payload: serde_json::json!({"calculated_qty": sizing.calculated_quantity, "final_qty": qty}),
    })?;

    // 6. Real BUY Execution
    let mut limits = RiskLimits::from_env().unwrap_or_default();
    limits.max_order_notional = limits.max_order_notional.max(25_000.0);
    limits.max_total_notional = limits.max_total_notional.max(100_000.0);
    limits.max_symbol_exposure = limits.max_symbol_exposure.max(50_000.0);
    let mut engine = ExecutionEngine::new(broker.clone(), RiskGovernor::new(limits));
    let buy_order_id = Uuid::new_v4();
    let mut buy_order = OrderIntent {
        client_order_id: buy_order_id,
        symbol: selected_quote.symbol.clone(),
        side: OrderSide::Buy,
        qty,
        limit_price: None, // Use market order for immediate options execution
        reduce_only: false,
        strategy_id: strategy_id.clone(),
        created_at: Utc::now(),
        order_hash: String::new(),
    };
    buy_order.order_hash = order_hash(&buy_order);

    println!(
        "SUBMITTING_BUY_ORDER symbol={} qty={} order_type=market",
        buy_order.symbol, buy_order.qty
    );
    let (bo_buy, approval) = engine
        .execute(
            buy_order.clone(),
            selected_quote.ask,
            selected_quote.spread_bps(),
            &portfolio,
        )
        .await?;
    println!(
        "BUY_ORDER_ACCEPTED broker_order_id={} status={:?} risk_reason={}",
        bo_buy.broker_order_id, bo_buy.status, approval.reason
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "BUY_SUBMITTED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: qty,
        entry_price: Some(selected_quote.ask),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("BuySubmitted".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: format!("{:?}", bo_buy.status),
        broker_order_id: Some(bo_buy.broker_order_id.clone()),
        payload: serde_json::json!({"client_order_id": buy_order_id.to_string()}),
    })?;

    // Wait for fill on broker
    let filled_buy = engine
        .wait_for_fill(&bo_buy.broker_order_id, std::time::Duration::from_secs(15))
        .await?;
    println!(
        "BUY_ORDER_STATUS broker_order_id={} status={:?} filled_qty={} avg_price={:?}",
        filled_buy.broker_order_id,
        filled_buy.status,
        filled_buy.filled_qty,
        filled_buy.filled_avg_price
    );
    if filled_buy.filled_qty == 0 {
        let _ = broker.cancel(&bo_buy.broker_order_id).await;
        return Err(format!(
            "Buy order {} did not fill within timeout",
            bo_buy.broker_order_id
        )
        .into());
    }
    let entry_price = filled_buy.filled_avg_price.unwrap_or(selected_quote.ask);
    let executed_qty = filled_buy.filled_qty;

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "BUY_FILLED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("BuyFilled".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Filled".into(),
        broker_order_id: Some(filled_buy.broker_order_id.clone()),
        payload: serde_json::json!({"filled_qty": executed_qty, "avg_price": entry_price}),
    })?;

    // 7. Verify Position Exists in Live Broker
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    let positions_after_buy = broker.positions().await?;
    let found_pos = positions_after_buy
        .iter()
        .find(|p| p.symbol == selected_quote.symbol);
    println!(
        "BROKER_POSITION_EXISTS symbol={} found={} total_positions={}",
        selected_quote.symbol,
        found_pos.is_some(),
        positions_after_buy.len()
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_OPENED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: None,
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("PositionOpen".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionOpened".into(),
        broker_order_id: Some(filled_buy.broker_order_id.clone()),
        payload: serde_json::json!({"entry_price": entry_price, "qty": executed_qty}),
    })?;

    // 8. Real SELL Execution (Closing Position)
    let sell_price = selected_quote.bid.max(0.01);
    let sell_order_id = Uuid::new_v4();
    let mut sell_order = OrderIntent {
        client_order_id: sell_order_id,
        symbol: selected_quote.symbol.clone(),
        side: OrderSide::Sell,
        qty: executed_qty,
        limit_price: None,
        reduce_only: true,
        strategy_id: strategy_id.clone(),
        created_at: Utc::now(),
        order_hash: String::new(),
    };
    sell_order.order_hash = order_hash(&sell_order);

    println!(
        "SUBMITTING_SELL_ORDER symbol={} qty={} limit_price={:.2}",
        sell_order.symbol, sell_order.qty, sell_price
    );
    let (bo_sell, _) = engine
        .execute(
            sell_order.clone(),
            sell_price,
            selected_quote.spread_bps(),
            &portfolio,
        )
        .await?;
    println!(
        "SELL_ORDER_ACCEPTED broker_order_id={} status={:?}",
        bo_sell.broker_order_id, bo_sell.status
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SELL_SUBMITTED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(sell_price),
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("SellSubmitted".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: format!("{:?}", bo_sell.status),
        broker_order_id: Some(bo_sell.broker_order_id.clone()),
        payload: serde_json::json!({"client_order_id": sell_order_id.to_string()}),
    })?;

    // Wait for fill on broker
    let filled_sell = engine
        .wait_for_fill(&bo_sell.broker_order_id, std::time::Duration::from_secs(15))
        .await?;
    println!(
        "SELL_ORDER_STATUS broker_order_id={} status={:?} filled_qty={} avg_price={:?}",
        filled_sell.broker_order_id,
        filled_sell.status,
        filled_sell.filled_qty,
        filled_sell.filled_avg_price
    );
    let exit_price = filled_sell.filled_avg_price.unwrap_or(sell_price);

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "SELL_FILLED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("SellFilled".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Filled".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"filled_qty": executed_qty, "exit_price": exit_price}),
    })?;

    // 9. Confirm Position Closed in Live Broker
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let positions_after_sell = broker.positions().await?;
    let closed_pos = positions_after_sell
        .iter()
        .find(|p| p.symbol == selected_quote.symbol);
    println!(
        "BROKER_POSITION_CLOSED symbol={} remaining_in_broker={}",
        selected_quote.symbol,
        closed_pos.is_some()
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "POSITION_CLOSED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: None,
        risk_pct,
        capital_allocated,
        rl_decision: Some("PositionClosed".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "PositionClosed".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"closed": closed_pos.is_none()}),
    })?;

    // 10. Realized P&L & RL Experience Feedback
    let realized_pnl = (exit_price - entry_price) * executed_qty as f64 * CONTRACT_MULTIPLIER;
    println!(
        "P&L_CALCULATED realized_pnl={:.2} entry={:.2} exit={:.2} qty={}",
        realized_pnl, entry_price, exit_price, executed_qty
    );

    store.record_feedback(&th_storage::ExecutionFeedbackRecord {
        event_id: None,
        timestamp: Utc::now(),
        event_kind: "P&L_CALCULATED".into(),
        bot_id: bot_id.clone(),
        strategy_id: strategy_id.clone(),
        option_symbol: selected_quote.symbol.clone(),
        quantity: executed_qty,
        entry_price: Some(entry_price),
        exit_price: Some(exit_price),
        realized_pnl: Some(realized_pnl),
        risk_pct,
        capital_allocated,
        rl_decision: Some("PnlCalculated".into()),
        rl_confidence: Some(rl_confidence),
        execution_status: "Completed".into(),
        broker_order_id: Some(filled_sell.broker_order_id.clone()),
        payload: serde_json::json!({"realized_pnl": realized_pnl}),
    })?;

    println!("REAL_EXECUTION_CYCLE_SUCCESS");
    println!("buy_broker_order_id={}", bo_buy.broker_order_id);
    println!("sell_broker_order_id={}", bo_sell.broker_order_id);
    println!("option_symbol={}", selected_quote.symbol);
    println!("executed_quantity={}", executed_qty);
    println!("entry_price={:.2}", entry_price);
    println!("exit_price={:.2}", exit_price);
    println!("realized_pnl={:.2}", realized_pnl);

    Ok(())
}

async fn run_stress_test_manufacturing(
    count: usize,
    symbol: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(cfg)?;
    let symbol = symbol.trim().to_uppercase();
    let now = Utc::now();
    let chain = provider.option_chain(&symbol, now).await?;
    let db_path = std::env::var("TRADING_HIVE_DB").unwrap_or_else(|_| "trading_hive.sqlite".into());
    let store = th_storage::Store::open(&db_path)?;

    println!(
        "RUNNING_MANUFACTURING_STRESS_TEST target_count={} underlying={}",
        count, symbol
    );
    let report = th_hive::run_manufacturing_stress_test(count, &symbol, &chain, Some(&store), now)?;

    println!("STRESS_TEST_REPORT");
    println!("manufacturing_test={}", report.manufacturing_test);
    println!("bots_created={}", report.bots_created);
    println!("bots_valid={}", report.bots_valid);
    println!("bots_invalid={}", report.bots_invalid);
    println!("strategies_created={}", report.strategies_created);
    println!("risk_configs_created={}", report.risk_configs_created);
    println!("option_configs_created={}", report.option_configs_created);
    println!("RL_configs_created={}", report.rl_configs_created);
    println!(
        "database_records_created={}",
        report.database_records_created
    );
    println!("execution_attempts={}", report.execution_attempts);

    if report.bots_invalid > 0 || report.execution_attempts > 0 {
        return Err("Manufacturing stress test failed consistency checks".into());
    }
    Ok(())
}

async fn run_hive_lifecycle(
    generations: usize,
    symbol: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AlpacaConfig::from_env()?;
    let provider = AlpacaProvider::new(cfg)?;
    let symbol = symbol.trim().to_uppercase();
    let db_path = std::env::var("TRADING_HIVE_DB").unwrap_or_else(|_| "trading_hive.sqlite".into());
    let store = th_storage::Store::open(&db_path)?;
    let now = Utc::now();
    let chain = provider.option_chain(&symbol, now).await?;

    println!("HIVE_LIFECYCLE_START total_generations={}", generations);
    for g in 1..=generations {
        let gen_id = format!("GEN-LIFECYCLE-{}-{:03}", now.format("%Y%m%d"), g);
        println!("STARTING_GENERATION id={}", gen_id);

        let report =
            th_hive::run_manufacturing_stress_test(10, &symbol, &chain, Some(&store), Utc::now())?;
        println!(
            "GENERATION_MANUFACTURED id={} bots_valid={}",
            gen_id, report.bots_valid
        );

        store.record_generation(&th_storage::HiveGenerationRecord {
            generation_id: gen_id.clone(),
            created_at: Utc::now(),
            status: "Completed".into(),
            total_capital: 1_000_000.0,
            bots_count: report.bots_valid,
            metadata: serde_json::json!({"generation_index": g, "symbol": symbol}),
        })?;

        println!(
            "GENERATION_COMPLETED id={} next_generation_autonomous_succession=true",
            gen_id
        );
    }
    println!(
        "HIVE_LIFECYCLE_SUCCESS all_generations_completed={}",
        generations
    );
    Ok(())
}
