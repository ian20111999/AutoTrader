//! 內建策略清單：把 `at_core::strategies` 既有的預設參數包成前端能用的
//! JSON 形狀。這裡只組裝資料，不重新定義策略邏輯或驗證規則——
//! 參數合法性檢查（`StrategyParamError`）留給 3.5 調參頁面時重用。

use at_core::{
    Bollinger, Donchian, OrderFlowBreakout, Rsi, SmaCross, TakerBuyMomentum, VegasTunnel,
};
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

/// 內建策略與參數 schema，直接讀 `at_core::strategies` 現有的
/// `DEFAULT_*` 常數（`Default` 實作用的就是同一組常數），不在這裡寫死一份。
///
/// `label` 是前端「調整參數」表單唯一的說明文字（schema 沒有獨立的 description
/// 欄位），所以它要寫得讓使用者看得懂這個參數在策略裡扮演什麼角色，而不是
/// 把程式裡的英文代號翻成中文就算了。策略卡片上的參數摘要是
/// `label + default` 直接相接，所以 label 裡不重複寫預設值。
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
        StrategyInfo {
            id: "vegas_tunnel".to_string(),
            name: "維加斯通道".to_string(),
            params: vec![
                integer(
                    "tunnelFastPeriod",
                    "通道快線 EMA 週期",
                    VegasTunnel::DEFAULT_TUNNEL_FAST,
                ),
                integer(
                    "tunnelSlowPeriod",
                    "通道慢線 EMA 週期",
                    VegasTunnel::DEFAULT_TUNNEL_SLOW,
                ),
                integer(
                    "filterPeriod",
                    "確認突破的過濾 EMA 週期",
                    VegasTunnel::DEFAULT_FILTER,
                ),
            ],
        },
        StrategyInfo {
            id: "order_flow_breakout".to_string(),
            name: "訂單流確認突破".to_string(),
            params: vec![
                integer(
                    "period",
                    "突破要回看幾根 K 線",
                    OrderFlowBreakout::DEFAULT_PERIOD,
                ),
                decimal(
                    "takerBuyThreshold",
                    "確認用的主動買盤佔比門檻",
                    OrderFlowBreakout::DEFAULT_TAKER_BUY_THRESHOLD,
                ),
            ],
        },
        StrategyInfo {
            id: "taker_buy_momentum".to_string(),
            name: "主動買盤動能".to_string(),
            params: vec![
                integer(
                    "streak",
                    "要連續幾根同方向",
                    TakerBuyMomentum::DEFAULT_STREAK,
                ),
                decimal(
                    "takerBuyThreshold",
                    "做多要求的主動買盤佔比門檻",
                    TakerBuyMomentum::DEFAULT_TAKER_BUY_THRESHOLD,
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_builtin_strategy() {
        let strategies = builtin_strategies();
        let ids: Vec<&str> = strategies.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "sma_cross",
                "bollinger",
                "donchian",
                "rsi",
                "vegas_tunnel",
                "order_flow_breakout",
                "taker_buy_momentum",
            ]
        );
    }

    #[test]
    fn every_listed_strategy_can_actually_be_built_with_its_own_defaults() {
        // 策略庫列得出來、按下去卻跑不起來（參數 key 打錯、少一個欄位）是
        // 使用者看得到的壞掉。這個測試把「清單」和 `build_strategy` 的查表
        // 綁在一起，兩邊只要對不上就紅。
        for info in builtin_strategies() {
            let params: std::collections::HashMap<String, String> = info
                .params
                .iter()
                .map(|p| (p.key.clone(), p.default.clone()))
                .collect();
            assert!(
                crate::backtest::build_strategy(&info.id, &params).is_ok(),
                "{} 的預設參數建不出策略",
                info.id
            );
        }
    }

    #[test]
    fn vegas_tunnel_params_match_at_core_defaults() {
        let vegas = &builtin_strategies()[4];
        assert_eq!(vegas.name, "維加斯通道");
        assert_eq!(vegas.params[0].key, "tunnelFastPeriod");
        assert_eq!(vegas.params[0].default, "144");
        assert_eq!(vegas.params[1].key, "tunnelSlowPeriod");
        assert_eq!(vegas.params[1].default, "169");
        assert_eq!(vegas.params[2].key, "filterPeriod");
        assert_eq!(vegas.params[2].default, "12");
    }

    #[test]
    fn order_flow_strategy_params_match_at_core_defaults() {
        let breakout = &builtin_strategies()[5];
        assert_eq!(breakout.name, "訂單流確認突破");
        assert_eq!(breakout.params[0].default, "20");
        assert_eq!(breakout.params[0].kind, ParamKind::Integer);
        assert_eq!(breakout.params[1].key, "takerBuyThreshold");
        assert_eq!(breakout.params[1].default, "0.55");
        assert_eq!(breakout.params[1].kind, ParamKind::Decimal);

        let momentum = &builtin_strategies()[6];
        assert_eq!(momentum.name, "主動買盤動能");
        assert_eq!(momentum.params[0].key, "streak");
        assert_eq!(momentum.params[0].default, "3");
        assert_eq!(momentum.params[1].default, "0.6");
    }

    #[test]
    fn every_param_label_is_chinese_not_just_the_english_key() {
        // 「調整參數」表單只有 label 這一個說明文字，所以它不能是英文代號
        for info in builtin_strategies() {
            for param in &info.params {
                assert!(
                    !param.label.is_ascii(),
                    "{}／{} 的 label 沒有中文說明：{}",
                    info.id,
                    param.key,
                    param.label
                );
            }
        }
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
