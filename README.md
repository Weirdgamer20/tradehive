# TradingHive v0.9.0

> **Submission Document**: See [**ONE_PAGE_WRITEUP.md**](ONE_PAGE_WRITEUP.md) for the complete one-page architectural write-up detailing our **AI Logic**, **Deterministic Risk Gates**, and **Alpaca Infrastructure Implementation**.

A production-grade, 24/7 autonomous quantitative trading runtime and reinforcement learning system written entirely in Rust.

---

## Autonomous 24/7 Architecture

TradingHive operates continuously without requiring human operators to supply daily symbols, trading signals, or session commands. The Hive autonomously manages its entire lifecycle synchronized to the authoritative **NYSE / US Eastern Market Clock**:

```
PRE-MARKET (08:30 - 09:30 ET)
  ↓ Dynamic Alpaca screener asset discovery (top volume US equities)
  ↓ Load prior RL session Q-table & state transfer
  ↓ Manufacture & fund dedicated worker bots per instrument
MARKET OPEN (09:30 - 15:55 ET)
  ↓ Ingest 1-minute bars & OCC options chains from Alpaca
  ↓ Live Q-policy decision gating (regime-aware signal filtering)
  ↓ Worker options sizing & deterministic portfolio risk governor
  ↓ Idempotent order execution via Alpaca REST/WebSocket
MARKET CLOSING (15:55 - 16:00 ET)
  ↓ Stop new entries
  ↓ Mandatory position flattening via reduce_only market orders (zero overnight gap risk)
POST-MARKET (16:00 - 16:30 ET)
  ↓ Reconcile broker positions & retire bot fleet
  ↓ Package complete session dataset into SQLite & JSON audit history
LEARNING PIPELINE (16:30 - 17:30 ET)
  ↓ Session-scoped tabular Q-learning on realized outcomes
  ↓ Synthesize & out-of-sample validate new candidate strategy blueprints
  ↓ Persist learned state for next day's Pre-Market cycle
WAITING FOR NEXT SESSION (17:30 - 08:30 ET)
  ↓ Low-power heartbeat daemon; awakens automatically at 08:30 ET
```

---

## Quick Start

### 1. Prerequisites
- Rust 1.70+ (stable toolchain)
- Alpaca paper or live trading account

### 2. Configure Environment
Copy `.env.example` to `.env` and fill in your Alpaca API credentials:
```bash
cp .env.example .env
```

```env
# Alpaca Credentials
APCA_API_KEY_ID=your_alpaca_key_id
APCA_API_SECRET_KEY=your_alpaca_secret_key
ALPACA_DATA_URL=https://data.alpaca.markets

# Safety & Mode Configuration
TRADING_HIVE_LIVE_CONFIRM=NO    # NO = Paper trading (safe), YES = Live trading
TRADING_STOP_LOSS_PCT=0.05      # 5% maximum stop loss per contract
TRADING_TAKE_PROFIT_PCT=0.10     # 10% take profit target
HIVE_DISCOVERY_LIMIT=200        # Candidates to scan via Alpaca screener
HIVE_UNIVERSE_SIZE=10           # Maximum active symbols traded per session
```

### 3. Launch Autonomous Runtime
Run the autonomous executable with **zero arguments**:

```bash
# Release binary execution:
.\target\release\trading-hive.exe

# Or via Cargo in development:
cargo run --release
```

The process remains alive indefinitely, monitoring market hours and running the full cycle autonomously.

---

## Key Subsystems

| Subsystem | Responsibility |
|---|---|
| **AI & RL Engine** (`crates/hive`) | Tabular Q-learning with discrete regime state representation, live supervisory decision gating, online Bellman updates, and evolutionary strategy blueprint synthesis. |
| **Deterministic Risk Governor** (`crates/risk`) | Multi-layer safety controls: gross/net portfolio limits, symbol concentration caps, daily drawdown circuit breaker, consecutive loss limiters, and kill switch. Strategy AI has zero authority to bypass risk rules. |
| **Alpaca Infrastructure** (`crates/market-data`, `crates/execution`) | Real-time 1m candle aggregation, OCC option chain fetching, dynamic screener asset discovery (`/v1beta1/screener/stocks/most-actives`), idempotent order routing with UUID tracking, and broker position reconciliation. |
| **Storage & Audit Trail** (`crates/storage`, `crates/memory`) | Write-ahead logged SQLite event store capturing all market events, orders, fills, and session-tagged `TRADE_AUTOPSY` records. JSON audit trail for model reproducibility. |
| **Bot Fleet Manager** (`crates/bot`, `crates/deployment`) | Dynamically manufactures, funds, activates, and retires specialized worker bots each session without global state leakage. |

---

## Safety Features

- 🛡️ **Fail-Closed by Design**: Missing credentials, unavailable market data, or clock drift halts trading immediately.
- 🛡️ **Triple-Layer Live Confirmation**: Production execution requires valid keys, production endpoint, and explicit `TRADING_HIVE_LIVE_CONFIRM=YES`.
- 🛡️ **Zero Overnight Risk**: Every position is flattened before 16:00 ET.
- 🛡️ **Pre- & Post-Trade Reconciliation**: Local state is reconciled with Alpaca broker positions before and after every session.
- 🛡️ **Strict Idempotency**: All orders use deterministic client order IDs preventing duplicate order submission.

---

## Verification & Testing

Run the full certification and lifecycle test suite:

```bash
# Run the 24/7 autonomous lifecycle state machine test suite:
cargo test -p th-bot --test autonomous_lifecycle

# Run the 16-point production certification suite:
cargo test -p trading-hive --test certification_suite

# Run the entire workspace test suite across all 19 crates:
cargo test --workspace --all-targets --all-features

# Verify formatting and lints:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

---

## System Write-Up & Documentation
- [**ONE_PAGE_WRITEUP.md**](ONE_PAGE_WRITEUP.md): One-page submission write-up covering AI logic, risk gates, and Alpaca infrastructure.
- [**INSTALL.md**](INSTALL.md): Comprehensive platform installation instructions.
