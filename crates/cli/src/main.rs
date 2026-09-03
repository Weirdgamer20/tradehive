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
        symbols: Option<String>,
        #[arg(long)]
        max_ticks: Option<usize>,
    },
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    dotenvy::from_filename(".env").ok();
    dotenvy::from_filename("config/.env").ok();
    dotenvy::from_filename("config/example.env").ok();
    if let Err(e) = run().await {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn require_env(var: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(var).map_err(|_| format!("environment variable not found: {var}").into())
}

fn production_config() -> Result<RuntimeConfig, Box<dyn std::error::Error>> {
    let mut cfg = RuntimeConfig {
        market_timezone: std::env::var("MARKET_TIMEZONE")
            .unwrap_or_else(|_| "America/New_York".into()),
        ..Default::default()
    };
    if let Ok(v) = std::env::var("TRADING_HIVE_DB") {
        cfg.database_path = v;
    }
    cfg.stop_loss_pct = std::env::var("TRADING_STOP_LOSS_PCT")
        .unwrap_or_else(|_| "0.05".into())
        .parse()
        .map_err(|_| "TRADING_STOP_LOSS_PCT invalid")?;
    cfg.take_profit_pct = std::env::var("TRADING_TAKE_PROFIT_PCT")
        .unwrap_or_else(|_| "0.10".into())
        .parse()
        .map_err(|_| "TRADING_TAKE_PROFIT_PCT invalid")?;
    cfg.validate()?;
    Ok(cfg)
}

fn symbols(value: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>();
    if out.is_empty() {
        return Err("no symbols configured".into());
    }
    Ok(out)
}

async fn run_autonomous(
    raw_symbols: Option<String>,
    max_ticks: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let key = require_env("APCA_API_KEY_ID")?;
    let secret = require_env("APCA_API_SECRET_KEY")?;
    let data_url = require_env("ALPACA_DATA_URL")?;
    let trading_url = require_env("ALPACA_TRADING_URL")?;

    let is_paper = trading_url.contains("paper-api.alpaca.markets");
    let is_live = !is_paper && trading_url.contains("api.alpaca.markets");

    println!("HIVE_STARTED");
    println!("MODE={}", if is_live { "LIVE" } else { "PAPER" });
    println!("AUTONOMOUS=true");
    println!("PRODUCTION_PROVIDER=ALPACA");
    println!("UNIVERSE_SOURCE=ALPACA_SCREENER");

    let cfg = production_config()?;
    let symbols = if let Some(raw) = raw_symbols {
        symbols(&raw)?
    } else {
        Vec::new()
    };

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
    let mut runtime = TradingRuntime::new(cfg, broker, provider)?;
    match max_ticks {
        Some(n) => {
            runtime.run_session(&symbols, Some(n)).await?;
        }
        None => {
            runtime.run_forever(&symbols).await?;
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
        Some(Commands::Run {
            symbols: raw_symbols,
            max_ticks,
        }) => run_autonomous(raw_symbols, max_ticks).await,
        None => {
            // Default: running without subcommands starts 24/7 autonomous production runtime
            run_autonomous(None, None).await
        }
    }
}
