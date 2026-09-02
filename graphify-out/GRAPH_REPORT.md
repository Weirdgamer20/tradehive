# Graph Report - TradingHive-Rust-V17  (2026-08-31)

## Corpus Check
- 104 files · ~83,111 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 738 nodes · 1941 edges · 36 communities (33 shown, 3 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 22 edges (avg confidence: 0.85)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- Quantitative Strategy Engine
- Quantitative Strategy Engine
- Quantitative Strategy Engine
- Execution Engine & Broker
- Quantitative Strategy Engine
- Risk Governance & Limits
- Quantitative Strategy Engine
- Hive RL & Research
- Risk Governance & Limits
- Hive RL & Research
- Hive RL & Research
- Chronological Backtest
- Quantitative Strategy Engine
- Bot Runtime & Lifecycle
- Sentinel & Health Operations
- Market Data & Quotes
- Storage & Audit Ledger
- Sentinel & Health Operations
- Component Cluster 18
- Execution Engine & Broker
- Quantitative Strategy Engine
- Market Data & Quotes
- Component Cluster 31
- Component Cluster 32
- Sentinel & Health Operations

## God Nodes (most connected - your core abstractions)
1. `Bar` - 58 edges
2. `StorageError` - 37 edges
3. `Store` - 37 edges
4. `ExecutionError` - 24 edges
5. `run_analysis_with_q()` - 22 edges
6. `TradingRuntime` - 20 edges
7. `Broker` - 19 edges
8. `TradingRuntime<B,P>` - 18 edges
9. `RuntimeError` - 18 edges
10. `OrderIntent` - 18 edges

## Surprising Connections (you probably didn't know these)
- `run_option_model_backtest()` --calls--> `classify_regime()`  [INFERRED]
  crates/backtest/src/lib.rs → crates/strategy/src/lib.rs
- `main()` --calls--> `run_analysis()`  [INFERRED]
  crates/cli/src/main.rs → crates/hive/src/lib.rs
- `main()` --calls--> `synthetic_option_chain()`  [INFERRED]
  crates/cli/src/main.rs → crates/market-data/src/lib.rs
- `run_paper_smoke()` --calls--> `order_hash()`  [INFERRED]
  crates/cli/src/main.rs → crates/execution/src/lib.rs
- `b()` --references--> `Bar`  [EXTRACTED]
  crates/domain/tests/domain.rs → crates/domain/src/lib.rs

## Import Cycles
- None detected.

## Communities (36 total, 3 thin omitted)

### Community 0 - "Quantitative Strategy Engine"
Cohesion: 0.08
Nodes (57): split(), Bar, AnalysisBundle, AnalysisReport, confidence_allocate(), dataset_hash(), discover_variables(), evaluate_strategies() (+49 more)

### Community 1 - "Quantitative Strategy Engine"
Cohesion: 0.06
Nodes (38): CandleBuilder, cdf(), DomainError, Fill, Greeks, implied_volatility(), MarketEvent, MarketEventKind (+30 more)

### Community 2 - "Quantitative Strategy Engine"
Cohesion: 0.11
Nodes (33): accepted_strat31_is_reconstructed_as_worker_strategy(), BotSizing, calculate_worker_quantity(), current_analysis_start(), DataHealth, HealthSnapshot, OpenTrade, research_deadline() (+25 more)

### Community 3 - "Execution Engine & Broker"
Cohesion: 0.10
Nodes (34): Arc, OrderStatus, AccountSnapshot, AlpacaBroker, AlpacaOrderResp, Broker, BrokerOrder, ExecutionEngine (+26 more)

### Community 4 - "Quantitative Strategy Engine"
Cohesion: 0.09
Nodes (38): SignalSide, Box, strategy_population(), atr(), choose_option(), classify_regime(), create_extended(), ema() (+30 more)

### Community 5 - "Risk Governance & Limits"
Cohesion: 0.12
Nodes (38): aggregate_5m(), AlpacaBar, AlpacaBars, AlpacaConfig, AlpacaGreeks, AlpacaNews, AlpacaOptionPage, AlpacaOptionSnapshot (+30 more)

### Community 6 - "Quantitative Strategy Engine"
Cohesion: 0.13
Nodes (12): Connection, Error, Option, Result, Self, String, T, Uuid (+4 more)

### Community 7 - "Hive RL & Research"
Cohesion: 0.16
Nodes (27): atomic_write(), BotHistoryRecord, BotsHistory, HiveManufacturingHistory, HiveManufacturingRun, JsonHistoryError, JsonHistoryStore, read_or_default() (+19 more)

### Community 8 - "Risk Governance & Limits"
Cohesion: 0.10
Nodes (17): CapitalAllocation, CapitalAuthority, PortfolioRisk, RiskApproval, RiskError, RiskGovernor, RiskLimits, DateTime (+9 more)

### Community 9 - "Hive RL & Research"
Cohesion: 0.13
Nodes (25): analyze(), CausalEdge, IntelligenceError, IntelligenceReport, MarketState, DateTime, Result, String (+17 more)

### Community 10 - "Hive RL & Research"
Cohesion: 0.18
Nodes (17): BotCreationPlan, BotFleet, BotSpec, BotStatus, DeploymentError, hive_manufactures_complete_plan(), invalid_expiry_rejected(), manufacture_bot_plan() (+9 more)

### Community 11 - "Chronological Backtest"
Cohesion: 0.19
Nodes (17): BacktestConfig, Backtester, BacktestError, BacktestReport, ChronologicalSplit, cost(), option_mark(), OptionBacktestReport (+9 more)

### Community 12 - "Quantitative Strategy Engine"
Cohesion: 0.29
Nodes (18): th-backtest, th-bot, th-deployment, th-domain, th-execution, th-hive, th-intelligence, th-market-data (+10 more)

### Community 13 - "Bot Runtime & Lifecycle"
Cohesion: 0.36
Nodes (15): Cli, Commands, demo(), main(), Box, Error, Result, String (+7 more)

### Community 14 - "Sentinel & Health Operations"
Cohesion: 0.22
Nodes (10): ComponentHealth, HealthState, DateTime, Result, String, Utc, Vec, Sentinel (+2 more)

### Community 15 - "Market Data & Quotes"
Cohesion: 0.31
Nodes (9): OptionChain, OptionDataError, OptionQuote, DateTime, Option, Result, String, Utc (+1 more)

### Community 16 - "Storage & Audit Ledger"
Cohesion: 0.32
Nodes (9): ExperienceStore, MemoryEvent, DateTime, Option, String, Utc, Vec, TradeAutopsy (+1 more)

### Community 17 - "Sentinel & Health Operations"
Cohesion: 0.29
Nodes (10): authorize(), ControlCommand, ControlResult, Health, Metric, Operations, DateTime, String (+2 more)

### Community 18 - "Component Cluster 18"
Cohesion: 0.24
Nodes (9): Gate3A, DateTime, Result, Self, String, Utc, StateAuthority, StateError (+1 more)

### Community 20 - "Execution Engine & Broker"
Cohesion: 0.33
Nodes (9): ExternalOrder, reconcile(), ReconcileError, Reconciliation, DateTime, Result, String, Utc (+1 more)

### Community 21 - "Quantitative Strategy Engine"
Cohesion: 0.29
Nodes (3): bs(), research_pipeline_produces_candidates(), Vec

### Community 22 - "Market Data & Quotes"
Cohesion: 0.38
Nodes (5): bar(), event_cache_evicts_oldest_deterministically(), DateTime, Utc, symbols_have_independent_builders()

## Knowledge Gaps
- **9 isolated node(s):** `HiveError`, `th-operations`, `th-options-data`, `th-reconcile`, `th-sentinel` (+4 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **3 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Bar` connect `Quantitative Strategy Engine` to `Quantitative Strategy Engine`, `Quantitative Strategy Engine`, `Quantitative Strategy Engine`, `Risk Governance & Limits`, `Quantitative Strategy Engine`, `Hive RL & Research`, `Chronological Backtest`, `Bot Runtime & Lifecycle`, `Quantitative Strategy Engine`?**
  _High betweenness centrality (0.329) - this node is a cross-community bridge._
- **Why does `TradingRuntime` connect `Quantitative Strategy Engine` to `Quantitative Strategy Engine`, `Execution Engine & Broker`, `Quantitative Strategy Engine`, `Risk Governance & Limits`, `Quantitative Strategy Engine`, `Hive RL & Research`, `Hive RL & Research`, `Storage & Audit Ledger`?**
  _High betweenness centrality (0.226) - this node is a cross-community bridge._
- **Why does `ExecutionEngine` connect `Execution Engine & Broker` to `Risk Governance & Limits`, `Quantitative Strategy Engine`?**
  _High betweenness centrality (0.078) - this node is a cross-community bridge._
- **What connects `HiveError`, `th-operations`, `th-options-data` to the rest of the system?**
  _9 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Quantitative Strategy Engine` be split into smaller, more focused modules?**
  _Cohesion score 0.08373205741626795 - nodes in this community are weakly interconnected._
- **Should `Quantitative Strategy Engine` be split into smaller, more focused modules?**
  _Cohesion score 0.06351236146632566 - nodes in this community are weakly interconnected._
- **Should `Quantitative Strategy Engine` be split into smaller, more focused modules?**
  _Cohesion score 0.10752688172043011 - nodes in this community are weakly interconnected._