# TradingHive V12 Production Validation Matrix

## Build gates

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --release`

## Bot manufacturing (no broker execution)

`trading-hive bot-manufacture-test --symbol SPY`

Requirements:
- live Alpaca market-data credentials
- at least 100 returned bars
- live option chain
- contract expiry between 120 and 180 minutes
- Hive supplies strategy/capital/underlying/contract/risk budget only
- worker quantity remains zero in the Hive manifest
- no broker order is submitted

## Option-chain validation

`trading-hive option-chain-validate --symbol SPY`

The command rejects missing, stale, malformed, mismatched, or out-of-window contracts.

## Paper execution smoke test

`trading-hive paper-smoke --symbol SPY`

Uses a live API option quote but sends the order only to the local PaperBroker. It verifies worker sizing, risk authorization, and filled paper execution.

## RL session

`trading-hive rl-live --symbols SPY,QQQ`

Requirements:
- exactly 30 seed strategies evaluated
- actual API bars
- Q-learning updates from observed next-bar returns
- STRAT-31 generated from learned Q-values
- STRAT-31 evaluated on chronological train/validation/OOS partitions
- no invented prices, option quotes, fills, or returns

## Cross-platform

The GitHub Actions matrix runs the build/test/lint/release gates on Ubuntu, Windows, and macOS.
