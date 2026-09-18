# TradeHive

> **Autonomous quantitative trading infrastructure in Rust.**

TradeHive is a modular trading runtime built around a deliberate separation of concerns: **market intelligence proposes decisions; deterministic controls decide what may execute.**

The system combines tabular reinforcement learning, market-data ingestion, strategy evaluation, portfolio risk controls, broker execution, persistence, reconciliation, and session lifecycle management.

---

## System at a Glance

```text
                         TRADEHIVE
                            │
                ┌───────────┴───────────┐
                │                       │
          Market Intelligence       Session Runtime
                │                       │
        ┌───────┴────────┐       ┌──────┴──────┐
        │ Market Data    │       │ Bot Fleet   │
        │ RL / Strategy  │       │ Lifecycle   │
        │ Research       │       │ Scheduling  │
        └───────┬────────┘       └──────┬──────┘
                │                       │
                └───────────┬───────────┘
                            ▼
                  ┌──────────────────┐
                  │ Deterministic    │
                  │ Risk Governor    │
                  └────────┬─────────┘
                           │
                    Authorized Order
                           ▼
                  ┌──────────────────┐
                  │ Execution Layer  │
                  │ + Reconciliation │
                  └────────┬─────────┘
                           ▼
                  ┌──────────────────┐
                  │ SQLite / Audit   │
                  │ Session Memory   │
                  └──────────────────┘
```

## Core Architecture

TradeHive is organized as a Rust workspace with dedicated subsystems for market data, strategy and reinforcement learning, research, risk, execution, storage, bot lifecycle, reconciliation, and operations.

| Layer | Responsibility |
|---|---|
| **Market Data** | Market bars, option-chain data, asset discovery |
| **Strategy / RL** | State representation, Q-learning, policy selection |
| **Research** | Strategy evaluation and out-of-sample validation |
| **Risk** | Exposure, drawdown, position, buying-power and kill-switch controls |
| **Execution** | Broker abstraction, order submission and idempotency |
| **Bot Fleet** | Session-scoped worker creation, funding and retirement |
| **Storage** | SQLite persistence and event/audit history |
| **Reconciliation** | Broker-vs-local state verification |
| **Operations** | Runtime lifecycle and deployment-oriented tooling |

The workspace is deliberately split into bounded crates rather than concentrating trading logic in a single application module.

## Reinforcement Learning

The strategy layer implements **tabular Q-learning** over a discrete state representation.

```text
Market / Portfolio State
          ↓
      StateKey
          ↓
   Legal Actions
          ↓
   Q-value Selection
          ↓
      Action
          ↓
   Risk Authorization
          ↓
      Execution
          ↓
       Outcome
          ↓
     Bellman Update
```

The learning component does not bypass the risk subsystem. Risk authorization remains an independent deterministic boundary.

## Risk Architecture

The risk governor is intentionally separated from strategy intelligence.

Controls include:

- order-notional limits
- aggregate notional limits
- symbol exposure limits
- position-count limits
- position-quantity limits
- buying-power checks
- trade-risk limits
- portfolio-risk limits
- daily-loss controls
- kill-switch behaviour

> **A strategy can request an action; it cannot authorize its own execution.**

## Autonomous Session Lifecycle

```text
PRE-MARKET
   │
   ├─ Discover candidate instruments
   ├─ Restore learning state
   └─ Prepare session workers
   │
MARKET OPEN
   │
   ├─ Ingest market data
   ├─ Evaluate strategy state
   ├─ Apply risk authorization
   └─ Route orders
   │
MARKET CLOSE
   │
   └─ Flatten / stop new entries
   │
POST-MARKET
   │
   ├─ Reconcile broker state
   ├─ Persist session history
   └─ Retire workers
   │
LEARNING
   │
   ├─ Update Q-values
   ├─ Evaluate candidate strategies
   └─ Persist state for a future session
```

## Engineering Properties

**Deterministic controls** — safety-critical portfolio constraints are represented as explicit rules rather than learned behaviour.

**Idempotent execution** — orders carry client-side identifiers so duplicate submissions can be detected and controlled.

**Reconciliation** — broker state is treated as an external source of truth that must be reconciled with local runtime state.

**Session isolation** — worker bots are created and retired within explicit session boundaries to reduce unintended state leakage.

**Persistent auditability** — market events, orders, fills and session information are persisted for inspection and analysis.

## Repository Structure

```text
tradehive/
├── crates/
│   ├── domain/
│   ├── market-data/
│   ├── strategy/
│   ├── risk/
│   ├── execution/
│   ├── storage/
│   ├── backtest/
│   ├── hive/
│   ├── bot/
│   ├── intelligence/
│   ├── memory/
│   ├── state/
│   ├── operations/
│   ├── research/
│   ├── deployment/
│   ├── reconcile/
│   ├── sentinel/
│   └── options-data/
├── integration-tests/
├── deploy/
├── scripts/
├── Cargo.toml
└── README.md
```

## Verification

Run the workspace checks with:

```bash
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Dedicated lifecycle and certification suites are also included in the repository.

## Configuration

Create a local environment file from the supplied example:

```bash
cp .env.example .env
```

Credentials and deployment-specific configuration belong in the environment, never in source control.

For detailed installation and architectural notes, see [INSTALL.md](INSTALL.md) and [ONE_PAGE_WRITEUP.md](ONE_PAGE_WRITEUP.md).

---

### Project Status

**Active engineering project**

TradeHive is being developed as an experimental autonomous trading infrastructure stack, with emphasis on modular systems architecture, reinforcement learning, deterministic risk controls, and verifiable execution behaviour.

> TradeHive is a software and research system. Nothing in this repository should be interpreted as a claim of trading profitability or financial advice.
