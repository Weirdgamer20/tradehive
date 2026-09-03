use clap::{Parser, Subcommand};
use th_bot::{RuntimeConfig, TradingRuntime};
use th_execution::AlpacaBroker;
use th_market_data::{AlpacaConfig, AlpacaProvider, MarketDataProvider};
use th_strategy::StrategyRegistry;

#[derive(Parser)]
#[command(
    name = "trading-hive",
    version,
    about = "TradingHive 24/7 autonomous market-session system"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Strategies,
    ValidateData {
        #[arg(long)]
        symbol: String,
    },
    Run {
        #[arg(long)]
        max_ticks: Option<usize>,
    },
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    if let Err(e) = run().await {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn require_env(var: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(var).map_err(|_| format!("environment variable not found: {var}").into())
}

fn production_config() -> Result<RuntimeConfig, Box<dyn std::error::Error>> {
    let cfg = RuntimeConfig::from_env()?;

    if cfg.stop_loss_pct <= 0.0 {
        return Err("TRADING_STOP_LOSS_PCT must be > 0".into());
    }

    Ok(cfg)
}

async fn run_autonomous(max_ticks: Option<usize>) -> Result<(), Box<dyn std::error::Error>> {
    let key = require_env("APCA_API_KEY_ID")?;
    let secret = require_env("APCA_API_SECRET_KEY")?;
    let data_url = require_env("ALPACA_DATA_URL")?;
    let trading_url = require_env("ALPACA_TRADING_URL")?;

    let is_paper = trading_url.contains("paper-api.alpaca.markets");
    let is_live = !is_paper && trading_url.contains("api.alpaca.markets");

    if is_live && std::env::var("TRADING_HIVE_LIVE_CONFIRM").as_deref() != Ok("YES") {
        return Err(
            "LIVE trading requested but TRADING_HIVE_LIVE_CONFIRM=YES is not set; failing closed for safety"
                .into(),
        );
    }

    println!("HIVE_STARTED");
    println!("MODE={}", if is_live { "LIVE" } else { "PAPER" });
    println!("BROKER=ALPACA");
    println!("MARKET_DATA=ALPACA");
    println!("AUTONOMOUS=true");
    println!("UNIVERSE_SOURCE=ALPACA_SCREENER");

    let cfg = production_config()?;
    let _risk_limits = th_risk::RiskLimits::from_env()?;
    println!("CONFIG_VALIDATED version={}", cfg.config_version);

    let md = AlpacaConfig {
        key: key.clone(),
        secret: secret.clone(),
        data_url: data_url.clone(),
        news_url: std::env::var("ALPACA_NEWS_URL").unwrap_or_else(|_| data_url.clone()),
        options_feed: std::env::var("ALPACA_OPTIONS_FEED").ok(),
        stocks_feed: std::env::var("ALPACA_STOCKS_FEED")
            .ok()
            .or_else(|| std::env::var("ALPACA_FEED").ok())
            .or_else(|| Some("iex".into())),
    };
    let provider = AlpacaProvider::new(md)?;
    let broker = AlpacaBroker::new(trading_url, key, secret, is_live)?;

    use th_execution::Broker;
    let acct = broker
        .account()
        .await
        .map_err(|e| format!("BROKER_AUTH_FAILED: {e}"))?;
    println!(
        "BROKER_CONNECTED equity={:.2} cash={:.2} buying_power={:.2}",
        acct.equity, acct.cash, acct.buying_power
    );

    let clock = broker
        .clock()
        .await
        .map_err(|e| format!("MARKET_DATA_FAILED: {e}"))?;
    println!(
        "MARKET_DATA_CONNECTED is_open={} next_open={:?} next_close={:?}",
        clock.is_open, clock.next_open, clock.next_close
    );

    let mut runtime = TradingRuntime::new(cfg, broker, provider)?;

    let mut supervisor =
        th_bot::HiveSupervisor::new(&mut runtime, th_bot::SupervisorConfig::default());
    supervisor
        .initialize_and_recover()
        .await
        .map_err(|e| format!("SUPERVISOR_INIT_FAILED: {e}"))?;

    tokio::select! {
        res = supervisor.run_supervised(max_ticks) => {
            res.map_err(|e| format!("SUPERVISOR_ERROR: {e}"))?;
        }
        _ = tokio::signal::ctrl_c() => {
            println!("SHUTDOWN_SIGNAL_RECEIVED initiating_graceful_shutdown");
            supervisor.step_shutdown().await;
        }
    }
    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Strategies) => {
            for id in StrategyRegistry::new().ids() {
                println!("{id}");
            }
            Ok(())
        }
        Some(Commands::ValidateData { symbol }) => {
            let _ = require_env("APCA_API_KEY_ID")?;
            let _ = require_env("APCA_API_SECRET_KEY")?;
            let _ = require_env("ALPACA_DATA_URL")?;
            let symbol = symbol.trim().to_uppercase();
            let cfg = AlpacaConfig::from_env()?;
            let provider = AlpacaProvider::new(cfg)?;
            let end = chrono::Utc::now();
            let bars = provider
                .bars(&symbol, end - chrono::Duration::minutes(10), end)
                .await?;
            if bars.is_empty() {
                return Err("Alpaca returned no market bars".into());
            }
            println!("LIVE_DATA_OK symbol={symbol} bars={}", bars.len());
            Ok(())
        }
        Some(Commands::Run { max_ticks }) => run_autonomous(max_ticks).await,
        None => {
            // Default: running without subcommands starts 24/7 autonomous production runtime
            run_autonomous(None).await
        }
    }
}
