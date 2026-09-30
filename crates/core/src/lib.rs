//! 自動交易台的共用型別。
//!
//! 回測、模擬、測試網、實盤四種模式都使用這裡的同一套型別，
//! 確保策略在各模式間的行為一致。

pub mod backtest;
pub mod bar;
pub mod bar_store;
pub mod fees;
pub mod fixed;
pub mod kline_csv;
pub mod metrics;
pub mod rules;
pub mod strategies;
pub mod strategy;
pub mod types;

pub use backtest::{
    run_backtest, BacktestConfig, BacktestError, BacktestResult, EquityPoint, PaperEngine,
};
pub use bar::{find_gaps, Bar, BarError, Gap, Interval, ParseIntervalError, SeriesError};
pub use bar_store::{format_bars, parse_bars, read_bars_file, write_bars_file, BarStoreError};
pub use fees::{CommissionRates, FeeError, FeeModel, FeeSchedule, FuturesFees, SpotFees};
pub use fixed::{Fixed, ParseFixedError};
pub use kline_csv::{parse_klines_csv, read_klines_csv, KlineCsvError};
pub use metrics::{Metrics, MS_PER_YEAR};
pub use rules::{RuleViolation, RulesError, SymbolRules};
pub use strategies::{Bollinger, Donchian, Rsi, SmaCross, StrategyParamError};
pub use strategy::{Strategy, TargetPosition};
pub use types::{Liquidity, Market, RunMode, Side, Symbol, SymbolError};

/// 目前版本（來自 Cargo.toml）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
