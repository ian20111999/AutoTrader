//! `SessionRecord`：`sessions/index.json` 裡的一筆紀錄。
//!
//! 欄位對照 ADR-001 §6.1。`kind` 重用 `at_core::RunMode`、`market` 重用
//! `at_core::Market`（D4）——這兩個型別住在零依賴的 `at-core`，沒有
//! `Serialize`/`Deserialize`，所以用 `#[serde(with = "...")]` 接上小小的
//! 字串轉換模組，而不是在 `at-core` 加 serde 依賴。
//!
//! 金額（`Fixed`）一律用字串保存，理由同 `at-account-sync`：`Fixed` 同樣
//! 沒有 `Serialize`，而且字串是這個專案跨邊界傳 `Fixed` 的既有慣例
//! （`app/src-tauri/src/backtest.rs` 的 `BacktestSummary`）。

use at_core::{Market, RunMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 目前程式認得的索引格式版本。見 `super::store` 的格式演進規則（ADR §9.5）。
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// session 的生命週期狀態。`Interrupted` 是獨立的一個值，不是 `Stopped` 也
/// 不是 `Failed`：App 當掉／斷電時曲線被截斷、最終指標不可信，和「正常停止」
/// 「引擎回報失敗」是三件不同的事，UI 必須分得出來（ADR §9.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Stopped,
    Failed,
    /// 回測跑完整段資料（不是被中途停止）。
    Completed,
    /// App 當掉／斷電，啟動對帳時發現的孤兒 `running` 紀錄會被改成這個狀態。
    Interrupted,
}

/// 一條權益曲線的績效指標，欄位對應 `at_core::Metrics`，金額一律存字串。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetrics {
    pub total_return: Option<String>,
    pub annualized_return: Option<String>,
    pub max_drawdown: Option<String>,
    pub sharpe: Option<String>,
    pub span_years: Option<String>,
}

/// 種類專屬的計數器。欄位語意互不重疊，用全 `Option` 的結構讓「列表只讀
/// 共同欄位」的程式碼不必 match `kind`（ADR §6.1：「這是刻意選簡單」）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCounters {
    pub trades: Option<u64>,
    pub liquidations: Option<u64>,
    pub fills: Option<u64>,
    pub blocked: Option<u64>,
    pub fees_paid: Option<String>,
}

/// 「這個數字是用什麼假設算出來的」。缺了就無法解釋報酬（ADR §6.1）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostAssumptions {
    pub fee_model: Option<String>,
    pub slippage: Option<String>,
    pub funding_rate: Option<String>,
    pub maintenance_margin_rate: Option<String>,
}

/// 一個 session（回測／模擬／測試網）在索引裡的一筆摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub schema_version: u32,
    pub id: String,
    #[serde(with = "run_mode_json")]
    pub kind: RunMode,
    #[serde(with = "market_json")]
    pub market: Market,
    pub symbol: String,
    pub interval: String,
    pub strategy_id: String,
    pub strategy_name: String,
    /// 排序穩定的 map：寫出來的 JSON 逐次相同，`git diff`／人眼比對才有意義。
    pub params: BTreeMap<String, String>,

    pub started_at_ms: i64,
    /// `None` = 還在跑（或當機後沒收尾，見啟動對帳）。
    pub ended_at_ms: Option<i64>,
    pub status: SessionStatus,
    /// `Failed` 時放引擎的中文錯誤訊息。
    pub status_message: Option<String>,

    pub starting_capital: String,
    /// `None` = 一根都還沒收盤。
    pub final_equity: Option<String>,
    pub bars_seen: u64,

    /// `None` = 曲線太短算不出來。
    pub metrics: Option<SessionMetrics>,
    pub counters: SessionCounters,
    pub cost_assumptions: CostAssumptions,

    /// 使用者按過「儲存回測」才是 `true`；影響保留策略（ADR §6.3）。
    pub saved: bool,
    /// 只有 backtest 有；`None` 表示即時行情。
    pub data_source_path: Option<String>,
    pub notes: Option<String>,
}

impl SessionRecord {
    /// `kind.sends_orders() == false` 的紀錄不該帶 testnet 專屬計數器
    /// （ADR §6.1 提到的 debug_assert；這裡做成可在測試裡直接斷言的函式）。
    pub fn has_consistent_counters(&self) -> bool {
        if self.kind.sends_orders() {
            true
        } else {
            self.counters.fills.is_none()
                && self.counters.blocked.is_none()
                && self.counters.fees_paid.is_none()
        }
    }
}

mod run_mode_json {
    use at_core::RunMode;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &RunMode, s: S) -> Result<S::Ok, S::Error> {
        let text = match value {
            RunMode::Backtest => "backtest",
            RunMode::Paper => "paper",
            RunMode::Testnet => "testnet",
            RunMode::Live => "live",
        };
        s.serialize_str(text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RunMode, D::Error> {
        let text = String::deserialize(d)?;
        match text.as_str() {
            "backtest" => Ok(RunMode::Backtest),
            "paper" => Ok(RunMode::Paper),
            "testnet" => Ok(RunMode::Testnet),
            "live" => Ok(RunMode::Live),
            other => Err(D::Error::custom(format!("不認得的 kind：{other}"))),
        }
    }
}

mod market_json {
    use at_core::Market;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Market, s: S) -> Result<S::Ok, S::Error> {
        let text = match value {
            Market::Spot => "spot",
            Market::UsdmPerp => "usdmPerp",
        };
        s.serialize_str(text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Market, D::Error> {
        let text = String::deserialize(d)?;
        match text.as_str() {
            "spot" => Ok(Market::Spot),
            "usdmPerp" => Ok(Market::UsdmPerp),
            other => Err(D::Error::custom(format!("不認得的 market：{other}"))),
        }
    }
}

/// `list_sessions` 的查詢條件（ADR §7.2 `list_sessions(filter)`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFilter {
    pub kinds: Option<Vec<RunMode>>,
    pub strategy_id: Option<String>,
    pub symbol: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub saved_only: bool,
    /// `None` = 用預設值（見 `store::DEFAULT_LIST_LIMIT`）。
    pub limit: Option<usize>,
}

impl SessionFilter {
    pub fn matches(&self, record: &SessionRecord) -> bool {
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&record.kind) {
                return false;
            }
        }
        if let Some(id) = &self.strategy_id {
            if &record.strategy_id != id {
                return false;
            }
        }
        if let Some(symbol) = &self.symbol {
            if &record.symbol != symbol {
                return false;
            }
        }
        if let Some(since) = self.since_ms {
            if record.started_at_ms < since {
                return false;
            }
        }
        if let Some(until) = self.until_ms {
            if record.started_at_ms > until {
                return false;
            }
        }
        if self.saved_only && !record.saved {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_record(id: &str, kind: RunMode) -> SessionRecord {
        SessionRecord {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: id.to_string(),
            kind,
            market: Market::Spot,
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy_id: "sma_cross".to_string(),
            strategy_name: "均線交叉".to_string(),
            params: BTreeMap::new(),
            started_at_ms: 0,
            ended_at_ms: None,
            status: SessionStatus::Running,
            status_message: None,
            starting_capital: "10000".to_string(),
            final_equity: None,
            bars_seen: 0,
            metrics: None,
            counters: SessionCounters::default(),
            cost_assumptions: CostAssumptions::default(),
            saved: false,
            data_source_path: None,
            notes: None,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut record = minimal_record("paper-1-001", RunMode::Paper);
        record
            .params
            .insert("fastPeriod".to_string(), "10".to_string());
        record.metrics = Some(SessionMetrics {
            total_return: Some("0.0184".to_string()),
            ..Default::default()
        });

        let json = serde_json::to_string(&record).unwrap();
        let back: SessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn kind_and_market_serialize_to_expected_strings() {
        let record = minimal_record("testnet-1-001", RunMode::Testnet);
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["kind"], "testnet");
        assert_eq!(json["market"], "spot");
    }

    #[test]
    fn unknown_kind_string_is_rejected_not_defaulted() {
        let bad = r#"{"schemaVersion":1,"id":"x","kind":"live_forever","market":"spot",
            "symbol":"BTCUSDT","interval":"1m","strategyId":"s","strategyName":"s",
            "params":{},"startedAtMs":0,"endedAtMs":null,"status":"running",
            "statusMessage":null,"startingCapital":"0","finalEquity":null,"barsSeen":0,
            "metrics":null,"counters":{"trades":null,"liquidations":null,"fills":null,
            "blocked":null,"feesPaid":null},"costAssumptions":{"feeModel":null,
            "slippage":null,"fundingRate":null,"maintenanceMarginRate":null},
            "saved":false,"dataSourcePath":null,"notes":null}"#;
        assert!(serde_json::from_str::<SessionRecord>(bad).is_err());
    }

    #[test]
    fn counter_consistency_check_flags_testnet_counters_on_non_testnet_record() {
        let mut record = minimal_record("paper-1-001", RunMode::Paper);
        assert!(record.has_consistent_counters());
        record.counters.fills = Some(3);
        assert!(!record.has_consistent_counters());
    }

    #[test]
    fn filter_matches_strategy_and_time_range() {
        let mut record = minimal_record("backtest-1-001", RunMode::Backtest);
        record.strategy_id = "rsi".to_string();
        record.started_at_ms = 1000;

        let filter = SessionFilter {
            strategy_id: Some("rsi".to_string()),
            since_ms: Some(500),
            until_ms: Some(1500),
            ..Default::default()
        };
        assert!(filter.matches(&record));

        let filter_wrong_strategy = SessionFilter {
            strategy_id: Some("sma_cross".to_string()),
            ..Default::default()
        };
        assert!(!filter_wrong_strategy.matches(&record));
    }
}
