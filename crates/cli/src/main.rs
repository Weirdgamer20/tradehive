use clap::{Parser, Subcommand};
use th_bot::{RuntimeConfig, TradingRuntime};
use th_execution::AlpacaBroker;
use th_market_data::{AlpacaConfig, AlpacaProvider, MarketDataProvider};
use th_strategy::StrategyRegistry;

#[derive(Parser)]
#[command(name = "trading-hive", version, about = "TradingHive real-market runtime")]
struct Cli { #[command(subcommand)] command: Commands }

#[derive(Subcommand)]
enum Commands {
    Strategies,
    ValidateData { #[arg(long)] symbol: String },
    Run { #[arg(long)] symbols: String, #[arg(long)] max_ticks: Option<usize> },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await { eprintln!("ERROR: {e}"); std::process::exit(1); }
}

fn production_config() -> Result<RuntimeConfig, Box<dyn std::error::Error>> {
    let mut cfg = RuntimeConfig::default();
    cfg.market_timezone = "UTC".into();
    if let Ok(v) = std::env::var("TRADING_HIVE_DB") { cfg.database_path = v; }
    cfg.stop_loss_pct = std::env::var("TRADING_STOP_LOSS_PCT")?.parse()?;
    cfg.take_profit_pct = std::env::var("TRADING_TAKE_PROFIT_PCT").unwrap_or_else(|_| "0".into()).parse()?;
    cfg.validate()?;
    Ok(cfg)
}

fn symbols(value: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_uppercase).collect::<Vec<_>>();
    if out.is_empty() { return Err("no symbols configured".into()); }
    Ok(out)
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Commands::Strategies => { for id in StrategyRegistry::new().ids() { println!("{id}"); } }
        Commands::ValidateData { symbol } => {
            let symbol = symbol.trim().to_uppercase();
            let cfg = AlpacaConfig::from_env()?;
            let provider = AlpacaProvider::new(cfg)?;
            let end = chrono::Utc::now();
            let bars = provider.bars(&symbol, end - chrono::Duration::minutes(10), end).await?;
            if bars.is_empty() { return Err("Alpaca returned no market bars".into()); }
            println!("LIVE_DATA_OK symbol={symbol} bars={}", bars.len());
        }
        Commands::Run { symbols: raw_symbols, max_ticks } => {
            let cfg = production_config()?;
            let symbols = symbols(&raw_symbols)?;
            let md = AlpacaConfig::from_env()?;
            let provider = AlpacaProvider::new(md.clone())?;
            let live = std::env::var("TRADING_HIVE_LIVE").ok().as_deref() == Some("YES");
            let url = std::env::var("ALPACA_TRADING_URL").unwrap_or_else(|_| if live { "https://api.alpaca.markets/v2".into() } else { "https://paper-api.alpaca.markets/v2".into() });
            let broker = AlpacaBroker::new(url, md.key, md.secret, live)?;
            let mut runtime = TradingRuntime::new(cfg, broker, provider)?;
            match max_ticks { Some(n) => { runtime.run_session(&symbols, Some(n)).await?; }, None => runtime.run_forever(&symbols).await? }
        }
    }
    Ok(())
}
