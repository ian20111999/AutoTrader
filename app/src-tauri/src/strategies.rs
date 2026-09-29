//! 內建策略清單：把 `at_core::strategies` 既有的預設參數包成前端能用的
//! JSON 形狀。這裡只組裝資料，不重新定義策略邏輯或驗證規則——
//! 參數合法性檢查（`StrategyParamError`）留給 3.5 調參頁面時重用。

use at_core::{Bollinger, Donchian, Rsi, SmaCross};
use serde::Serialize;

/// 單一參數的型別：決定前端輸入框要用整數還是小數規則（3.5 會用到）。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Integer,
    Decimal,
}

/// 一個可調參數。`default` 用字串保留 `Fixed`/整數的精確表示，不用 `f64`。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StrategyParam {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: String,
}

/// 一個內建策略：名稱＋參數 schema。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StrategyInfo {
    pub id: String,
    pub name: String,
    pub params: Vec<StrategyParam>,
}

fn integer(key: &str, label: &str, default: usize) -> StrategyParam {
    StrategyParam {
        key: key.to_string(),
        label: label.to_string(),
        kind: ParamKind::Integer,
        default: default.to_string(),
    }
}

fn decimal(key: &str, label: &str, default: at_core::Fixed) -> StrategyParam {
    StrategyParam {
        key: key.to_string(),
        label: label.to_string(),
        kind: ParamKind::Decimal,
        default: default.to_string(),
    }
}

/// 四個內建策略與參數 schema，直接讀 `at_core::strategies` 現有的
/// `DEFAULT_*` 常數（`Default` 實作用的就是同一組常數），不在這裡寫死一份。
pub fn builtin_strategies() -> Vec<StrategyInfo> {
    vec![
        StrategyInfo {
            id: "sma_cross".to_string(),
            name: "均線交叉".to_string(),
            params: vec![
                integer("fastPeriod", "快線週期", SmaCross::DEFAULT_FAST),
                integer("slowPeriod", "慢線週期", SmaCross::DEFAULT_SLOW),
            ],
        },
        StrategyInfo {
            id: "bollinger".to_string(),
            name: "布林通道".to_string(),
            params: vec![
                integer("period", "週期", Bollinger::DEFAULT_PERIOD),
                decimal("multiplier", "標準差倍數", Bollinger::DEFAULT_MULTIPLIER),
            ],
        },
        StrategyInfo {
            id: "donchian".to_string(),
            name: "唐奇安突破".to_string(),
            params: vec![
                integer("entryPeriod", "進場週期", Donchian::DEFAULT_ENTRY),
                integer("exitPeriod", "出場週期", Donchian::DEFAULT_EXIT),
            ],
        },
        StrategyInfo {
            id: "rsi".to_string(),
            name: "RSI".to_string(),
            params: vec![
                integer("period", "週期", Rsi::DEFAULT_PERIOD),
                decimal("buyBelow", "進場門檻", Rsi::DEFAULT_BUY_BELOW),
                decimal("exitAbove", "出場門檻", Rsi::DEFAULT_EXIT_ABOVE),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_all_four_builtin_strategies() {
        let strategies = builtin_strategies();
        let ids: Vec<&str> = strategies.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["sma_cross", "bollinger", "donchian", "rsi"]);
    }

    #[test]
    fn sma_cross_params_match_at_core_defaults() {
        let sma = &builtin_strategies()[0];
        assert_eq!(sma.name, "均線交叉");
        assert_eq!(sma.params[0].key, "fastPeriod");
        assert_eq!(sma.params[0].default, "10");
        assert_eq!(sma.params[1].key, "slowPeriod");
        assert_eq!(sma.params[1].default, "50");
    }

    #[test]
    fn bollinger_params_match_at_core_defaults() {
        let bollinger = &builtin_strategies()[1];
        assert_eq!(bollinger.params[0].default, "20");
        assert_eq!(bollinger.params[0].kind, ParamKind::Integer);
        assert_eq!(bollinger.params[1].default, "2");
        assert_eq!(bollinger.params[1].kind, ParamKind::Decimal);
    }

    #[test]
    fn donchian_params_match_at_core_defaults() {
        let donchian = &builtin_strategies()[2];
        assert_eq!(donchian.params[0].default, "20");
        assert_eq!(donchian.params[1].default, "10");
    }

    #[test]
    fn rsi_params_match_at_core_defaults() {
        let rsi = &builtin_strategies()[3];
        assert_eq!(rsi.params[0].default, "14");
        assert_eq!(rsi.params[1].default, "30");
        assert_eq!(rsi.params[2].default, "70");
    }

    #[test]
    fn serializes_to_camel_case_json() {
        let json = serde_json::to_value(&builtin_strategies()[0]).unwrap();
        assert_eq!(json["id"], "sma_cross");
        assert_eq!(json["params"][0]["key"], "fastPeriod");
        assert_eq!(json["params"][0]["kind"], "integer");
    }
}
