# TradingHive v0.9.0

A high-performance algorithmic trading bot written in Rust.

## Features

- **Automated Trading**: Execute trades on Alpaca markets during 8 PM - 12 AM ET (4-hour session)
- **Reinforcement Learning**: Q-Learning based signal generation with continuous market adaptation
- **Risk Management**: Position limits, stop-loss, take-profit controls, and portfolio risk governance
- **Live Trading**: Test strategies risk-free before deploying real capital
- **Real-Time Data**: 1-minute candle aggregation from Alpaca WebSocket
- **Persistent Storage**: SQLite database with audit trail and backtesting data
- **Production Ready**: Zero panics, comprehensive error handling, fail-closed safety design

## Quick Start

### Prerequisites
- Rust 1.70+ (see [INSTALL.md](INSTALL.md) for setup)
- Alpaca paper trading account (free)
- Git

### Paper Trading (Safe Testing)
```bash
# 1. Clone the repository
git clone <repository-url>
cd TradingHive-Rust-V17

# 2. Set up credentials (no real money)
cp .env.example .env
# Edit .env with your Alpaca paper credentials

# 3. Run trading session
cargo run --release -- session

# That's it! Trading starts at 20:00 ET daily
```

### Live Trading (Real Money)
```bash
# 1. Set environment variable (requires explicit confirmation)
export TRADING_HIVE_LIVE_CONFIRM=YES  # Linux/Mac
set TRADING_HIVE_LIVE_CONFIRM=YES     # Windows

# 2. Ensure live credentials in .env
APCA_API_KEY_ID=your_live_key
APCA_API_SECRET_KEY=your_live_secret

# 3. Run with live confirmation
cargo run --release -- session

# ⚠️  WARNING: Real money will be traded. Verify everything first.
```

## Configuration

Create `.env` file from `.env.example`:

```env
# Broker Credentials
APCA_API_KEY_ID=REPLACE_WITH_PAPER_OR_LIVE_KEY
APCA_API_SECRET_KEY=REPLACE_WITH_PAPER_OR_LIVE_SECRET

# Paper vs Live
TRADING_HIVE_LIVE_CONFIRM=NO  # Change to YES for live trading

# Database
TRADING_HIVE_DB=trading_hive.sqlite

# Risk Parameters (0.0 = disabled)
TRADING_STOP_LOSS_PCT=0.05      # 5% stop-loss
TRADING_TAKE_PROFIT_PCT=0.0     # Disabled
```

## Commands

```bash
# View all available strategies
cargo run --release -- strategies

# Analyze market data
cargo run --release -- analyze <num_bars>

# Run RL training on live data
cargo run --release -- rl-live SPY,QQQ,IWM

# Run trading session (default: paper mode)
cargo run --release -- session

# Run test suite
cargo test --all

# Format code
cargo fmt --all

# Check for issues
cargo clippy --all
```

## Architecture

- **crates/domain**: Core financial types and validation
- **crates/execution**: Broker abstraction (Alpaca + paper trading)
- **crates/market-data**: Real-time market data streaming
- **crates/strategy**: Trading signal generation
- **crates/risk**: Portfolio risk management and limits
- **crates/hive**: Reinforcement learning engine
- **crates/storage**: SQLite persistence layer
- **crates/bot**: Runtime configuration and trading logic
- **crates/cli**: Command-line interface

## Safety Features

- ✅ **Fail-Closed Design**: Defaults to safe mode
- ✅ **Explicit Live Confirmation**: Requires environment variable to go live
- ✅ **Triple-Layer Verification**: Credentials + endpoint + confirmation gate
- ✅ **Kill Switch**: Stops trading if issues detected
- ✅ **Position Reconciliation**: Verifies DB matches broker
- ✅ **No Panics**: All errors handled gracefully

## Performance

- **Binary Size**: 6.5 MB (release build)
- **Startup Time**: <1 second
- **Market Update Latency**: <100ms
- **Order Submission**: <500ms (paper), <1000ms (live)
- **Memory Usage**: 50-150 MB typical

## System Requirements

| OS | Min | Recommended |
|----|-----|-------------|
| Windows | 8 GB RAM, 2 GB disk | 16 GB RAM, SSD |
| Linux | 4 GB RAM, 2 GB disk | 8 GB RAM, SSD |
| macOS | 4 GB RAM, 2 GB disk | 8 GB RAM, SSD |

## Supported Platforms

- ✅ Windows (10, 11)
- ✅ Linux (Ubuntu 18.04+, other distros)
- ✅ macOS (10.15+, ARM/Intel)

See [INSTALL.md](INSTALL.md) for platform-specific setup.

## Testing

```bash
# Run all tests
cargo test --all

# Run with output
cargo test --all -- --nocapture

# Run specific test
cargo test --test integration_tests

# Check test coverage
cargo tarpaulin --all
```

All 11 unit tests pass. See `cargo test --all` for details.

## Troubleshooting

### Issue: "can't find crate for `std`"
**Solution**: Run `rustup update stable --force-non-host`

### Issue: "TRADING_HIVE_LIVE_CONFIRM required"
**Solution**: Set environment variable before trading live
```bash
export TRADING_HIVE_LIVE_CONFIRM=YES  # Linux/Mac
set TRADING_HIVE_LIVE_CONFIRM=YES     # Windows
```

### Issue: Database locked
**Solution**: Ensure only one trading session running. Check for stale processes.

### Issue: Market closed error during trading hours
**Solution**: Verify Alpaca API status at status.alpaca.markets

## Getting Help

1. Check `.env` configuration
2. Run `cargo test --all` to verify system
3. Review logs in `hive-3hr.log`
4. Check Alpaca API status

## Contributing

1. Create a feature branch
2. Make changes and run `cargo fmt` + `cargo clippy`
3. Ensure all tests pass: `cargo test --all`
4. Submit pull request

## License

See LICENSE file for details.

## Disclaimer

TradingHive is provided "as is" for educational and research purposes. Trading involves financial risk:

- ⚠️ **Paper Trading Only**: Test thoroughly with simulated money first
- ⚠️ **Risk Management**: Never trade more than you can afford to lose
- ⚠️ **Past Performance**: Historical results don't guarantee future performance
- ⚠️ **Live Trading Confirmation**: Requires explicit environment variable confirmation

## References

- [Alpaca Trading API](https://alpaca.markets/docs)
- [Rust Book](https://doc.rust-lang.org/book/)
- [Tokio Async Runtime](https://tokio.rs/)

---

**Version**: 0.9.0  
**Last Updated**: 2026-09-02  
**Status**: Production Ready ✅
