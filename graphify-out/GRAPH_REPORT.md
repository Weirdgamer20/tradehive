# Graph Report - TradingHive-Rust-V17  (2026-09-03)

## Corpus Check
- Large corpus: 80 files · ~1,376,543 words. Semantic extraction will be expensive (many Claude tokens). Consider running on a subfolder.

## Summary
- 960 nodes · 2495 edges · 43 communities (40 shown, 3 thin omitted)
- Extraction: 98% EXTRACTED · 2% INFERRED · 0% AMBIGUOUS · INFERRED: 48 edges (avg confidence: 0.85)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- Hive Rs Strategy
- Bot B Tradingruntime
- Market Data Rs
- Execution Rs Paperbroker
- Storage Store Rs
- Domain Rs Candlebuilder
- Strategy Rs Strategyregistry
- Risk Riskgovernor Rs
- History Storage Json
- Market Session Domain
- Horizon Momentum Strategy
- Deployment Botfleet Rs
- Research Intelligence Rs
- Backtest Rs Default
- Sizing Bot Dynamic
- Cli Certification Suite
- Pkg Th Hive
- Cli Main Rs
- Sentinel Rs Sentinelsnapshot
- State Rs Stateauthority
- Options Data Rs
- Memory Rs Experiencestore
- Operations Rs Health
- Reconcile Rs Externalorder
- Hive Is Q
- Multi Horizon Momentum
- Market Data Event
- Risk Order Binds
- Deploy Install Paper
- Pkg Th Sentinel

## God Nodes (most connected - your core abstractions)
1. `Bar` - 63 edges
2. `Store` - 45 edges
3. `StorageError` - 44 edges
4. `RuntimeError` - 30 edges
5. `TradingRuntime<B, P>` - 28 edges
6. `ExecutionError` - 25 edges
7. `TradingRuntime` - 24 edges
8. `run_analysis_with_q()` - 23 edges
9. `MultiHorizonMomentumStrategy` - 21 edges
10. `Broker` - 20 edges

## Surprising Connections (you probably didn't know these)
- `cert_bot_001_autonomous_sizing_formula_verification()` --calls--> `calculate_worker_quantity()`  [INFERRED]
  crates/cli/tests/certification_suite.rs → crates/bot/src/lib.rs
- `cert_hist_003_rl_history_transitions_and_q_tables()` --calls--> `persist_rl_history()`  [INFERRED]
  crates/cli/tests/certification_suite.rs → crates/hive/src/lib.rs
- `cert_exec_001_paper_broker_risk_governor_smoke()` --calls--> `order_hash()`  [INFERRED]
  crates/cli/tests/certification_suite.rs → crates/execution/src/lib.rs
- `cert_fail_004_reconciliation_mismatch_engages_kill_switch()` --calls--> `reconcile_positions()`  [INFERRED]
  crates/cli/tests/certification_suite.rs → crates/execution/src/lib.rs
- `run_option_model_backtest()` --calls--> `classify_regime()`  [INFERRED]
  crates/backtest/src/lib.rs → crates/strategy/src/lib.rs

## Import Cycles
- 2-file cycle: `crates/bot/src/lib.rs -> crates/bot/src/sizing.rs -> crates/bot/src/lib.rs`

## Communities (43 total, 3 thin omitted)

### Community 0 - "Hive Rs Strategy"
Cohesion: 0.07
Nodes (69): split(), cert_rl_001_002_two_session_reinforcement_learning_demonstration(), Bar, AnalysisBundle, AnalysisReport, confidence_allocate(), dataset_hash(), discover_variables() (+61 more)

### Community 1 - "Bot B Tradingruntime"
Cohesion: 0.10
Nodes (34): accepted_strat31_is_reconstructed_as_worker_strategy(), BotSizing, calculate_worker_quantity(), calculate_worker_quantity_with_strength(), current_analysis_start(), DataHealth, HealthSnapshot, OpenTrade (+26 more)

### Community 2 - "Market Data Rs"
Cohesion: 0.07
Nodes (48): EmptyBarsProvider, DateTime, OptionChain, Result, String, Utc, Vec, test_closed_loop_rl_knowledge_transfer() (+40 more)

### Community 3 - "Execution Rs Paperbroker"
Cohesion: 0.08
Nodes (41): Arc, OrderIntent, OrderSide, OrderStatus, Position, AccountSnapshot, AlpacaBroker, AlpacaOrderResp (+33 more)

### Community 4 - "Storage Store Rs"
Cohesion: 0.10
Nodes (22): Connection, ExecutionFeedbackRecord, GenerationPerformanceRecord, hive_relational_models_round_trip(), HiveBotRecord, HiveGenerationRecord, OpenTradeRecord, DateTime (+14 more)

### Community 5 - "Domain Rs Candlebuilder"
Cohesion: 0.08
Nodes (31): CandleBuilder, cdf(), DomainError, Fill, Greeks, implied_volatility(), MarketEvent, MarketEventKind (+23 more)

### Community 6 - "Strategy Rs Strategyregistry"
Cohesion: 0.10
Nodes (35): SignalSide, atr(), choose_option(), create_extended(), ema(), extended_strategy_ids(), ExtendedStrategy, Kind (+27 more)

### Community 7 - "Risk Riskgovernor Rs"
Cohesion: 0.10
Nodes (18): CapitalAllocation, CapitalAuthority, PortfolioRisk, RiskApproval, RiskError, RiskGovernor, RiskLimits, DateTime (+10 more)

### Community 8 - "History Storage Json"
Cohesion: 0.16
Nodes (27): atomic_write(), BotHistoryRecord, BotsHistory, HiveManufacturingHistory, HiveManufacturingRun, JsonHistoryError, JsonHistoryStore, read_or_default() (+19 more)

### Community 9 - "Market Session Domain"
Cohesion: 0.10
Nodes (24): easter_sunday(), HolidayCalendar, last_weekday_of_month(), MarketClosedReason, MarketSessionClock, MarketSessionConfig, MarketSessionState, nth_weekday_of_month() (+16 more)

### Community 10 - "Horizon Momentum Strategy"
Cohesion: 0.10
Nodes (16): OptionExpiryPolicy, DateTime, Default, Option, Self, Utc, MultiHorizonMomentumConfig, MultiHorizonMomentumFeatures (+8 more)

### Community 11 - "Deployment Botfleet Rs"
Cohesion: 0.14
Nodes (19): BotCreationPlan, BotFleet, BotManufacturingRequest, BotSpec, BotStatus, default_generation_id(), DeploymentError, hive_manufactures_complete_plan() (+11 more)

### Community 12 - "Research Intelligence Rs"
Cohesion: 0.13
Nodes (25): analyze(), CausalEdge, IntelligenceError, IntelligenceReport, MarketState, DateTime, Result, String (+17 more)

### Community 13 - "Backtest Rs Default"
Cohesion: 0.19
Nodes (17): BacktestConfig, Backtester, BacktestError, BacktestReport, ChronologicalSplit, cost(), option_mark(), OptionBacktestReport (+9 more)

### Community 14 - "Sizing Bot Dynamic"
Cohesion: 0.20
Nodes (18): calculate_dynamic_risk_quantity(), CeilingAction, DynamicSizingInputs, DynamicSizingResult, Result, Self, String, SizingAction (+10 more)

### Community 15 - "Cli Certification Suite"
Cohesion: 0.12
Nodes (12): cert_arch_001_zero_directional_authority_in_hive(), cert_bot_001_autonomous_sizing_formula_verification(), cert_bot_002_003_signal_routing_and_flat_behavior(), cert_exec_001_paper_broker_risk_governor_smoke(), cert_fail_002_stale_and_future_quotes_rejected(), cert_fail_003_inverted_and_non_positive_quotes_rejected(), cert_fail_004_reconciliation_mismatch_engages_kill_switch(), cert_hist_003_rl_history_transitions_and_q_tables() (+4 more)

### Community 16 - "Pkg Th Hive"
Cohesion: 0.31
Nodes (18): th-backtest, th-bot, th-deployment, th-domain, th-execution, th-hive, th-intelligence, th-market-data (+10 more)

### Community 17 - "Cli Main Rs"
Cohesion: 0.36
Nodes (14): Cli, Commands, main(), production_config(), require_env(), Box, Error, Option (+6 more)

### Community 18 - "Sentinel Rs Sentinelsnapshot"
Cohesion: 0.22
Nodes (10): ComponentHealth, HealthState, DateTime, Result, String, Utc, Vec, Sentinel (+2 more)

### Community 19 - "State Rs Stateauthority"
Cohesion: 0.22
Nodes (10): Gate3A, DateTime, Default, Result, Self, String, Utc, StateAuthority (+2 more)

### Community 20 - "Options Data Rs"
Cohesion: 0.31
Nodes (9): OptionChain, OptionDataError, OptionQuote, DateTime, Option, Result, String, Utc (+1 more)

### Community 21 - "Memory Rs Experiencestore"
Cohesion: 0.32
Nodes (9): ExperienceStore, MemoryEvent, DateTime, Option, String, Utc, Vec, TradeAutopsy (+1 more)

### Community 22 - "Operations Rs Health"
Cohesion: 0.29
Nodes (10): authorize(), ControlCommand, ControlResult, Health, Metric, Operations, DateTime, String (+2 more)

### Community 24 - "Reconcile Rs Externalorder"
Cohesion: 0.33
Nodes (9): ExternalOrder, reconcile(), ReconcileError, Reconciliation, DateTime, Result, String, Utc (+1 more)

### Community 25 - "Hive Is Q"
Cohesion: 0.25
Nodes (3): bs(), research_pipeline_produces_candidates(), Vec

### Community 26 - "Multi Horizon Momentum"
Cohesion: 0.31
Nodes (7): generate_market_bars(), multi_horizon_momentum_blocks_outside_market_hours(), multi_horizon_momentum_generates_bearish_consensus(), multi_horizon_momentum_generates_bullish_consensus(), DateTime, Utc, Vec

### Community 27 - "Market Data Event"
Cohesion: 0.38
Nodes (5): bar(), event_cache_evicts_oldest_deterministically(), DateTime, Utc, symbols_have_independent_builders()

## Knowledge Gaps
- **7 isolated node(s):** `th-operations`, `th-options-data`, `th-reconcile`, `th-sentinel`, `th-state` (+2 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **3 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Bar` connect `Hive Rs Strategy` to `Bot B Tradingruntime`, `Market Data Rs`, `Storage Store Rs`, `Domain Rs Candlebuilder`, `Strategy Rs Strategyregistry`, `Horizon Momentum Strategy`, `Research Intelligence Rs`, `Backtest Rs Default`, `Cli Certification Suite`, `Hive Is Q`, `Multi Horizon Momentum`?**
  _High betweenness centrality (0.312) - this node is a cross-community bridge._
- **Why does `TradingRuntime` connect `Bot B Tradingruntime` to `Hive Rs Strategy`, `Market Data Rs`, `Execution Rs Paperbroker`, `Storage Store Rs`, `Strategy Rs Strategyregistry`, `History Storage Json`, `Market Session Domain`, `Deployment Botfleet Rs`, `Memory Rs Experiencestore`?**
  _High betweenness centrality (0.253) - this node is a cross-community bridge._
- **Why does `ExecutionEngine` connect `Execution Rs Paperbroker` to `Bot B Tradingruntime`, `Risk Riskgovernor Rs`?**
  _High betweenness centrality (0.074) - this node is a cross-community bridge._
- **What connects `th-operations`, `th-options-data`, `th-reconcile` to the rest of the system?**
  _7 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Hive Rs Strategy` be split into smaller, more focused modules?**
  _Cohesion score 0.06903965599617773 - nodes in this community are weakly interconnected._
- **Should `Bot B Tradingruntime` be split into smaller, more focused modules?**
  _Cohesion score 0.0969030969030969 - nodes in this community are weakly interconnected._
- **Should `Market Data Rs` be split into smaller, more focused modules?**
  _Cohesion score 0.07298245614035087 - nodes in this community are weakly interconnected._