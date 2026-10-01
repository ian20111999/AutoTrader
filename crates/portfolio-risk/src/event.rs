//! 觸發紀錄的型別（ADR-002 第 8.4 節）。寫檔在 [`crate::store`]。
//!
//! # 存結構化資料，不存預先格式化好的字串
//!
//! 這裡**沒有** `detail_zh: String` 這種欄位。顯示文字由 [`RiskEventCause`] 的
//! `Display` 在讀取時產生，紀錄裡存的是「哪一條規則、什麼參數、做了什麼」。
//! 理由很實際：存結構化才能篩選（「只看對帳不一致」）、才能寫測試斷言
//! （比對 enum 而不是比對一句會改的中文）。
//!
//! 代價：之後改了 `Display` 的字句，舊紀錄讀起來會跟著變。對一個本機工具可以接受。
//!
//! # 時間來源一定要說清楚
//!
//! [`RiskEvent::clock`] 不是裝飾欄位。交易所時間和本機時間混在同一個列表裡而不標明
//! 哪個是哪個，之後查「為什麼這兩件事的順序看起來是反的」會被自己的紀錄誤導。

use crate::breaker::{BreakerAction, BreakerTrigger};
use crate::serde_at;
use at_core::Symbol;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 時間戳是誰給的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockSource {
    /// 交易所給的時間（K 線 `open_time`、訂單回報的時間）。
    Exchange,
    /// 本機系統時鐘。使用者按按鈕這類沒有交易所時間可用的事件。
    Local,
}

impl fmt::Display for ClockSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClockSource::Exchange => write!(f, "交易所時間"),
            ClockSource::Local => write!(f, "本機時間"),
        }
    }
}

/// 這一則紀錄在記什麼。
///
/// ADR 8.4 列了六種 cause，這裡只有**型別已經存在、今天真的會發生**的兩種。
/// 其餘四種（全域閘門擋單、session 閘門擋單、緊急處理、風控設定被修改）要等
/// `GlobalBlocked`、`EmergencyAction` 與風控設定頁落地才有東西可記；新增 variant
/// 時編譯器會逼每個消費端處理，所以刻意**不加** `#[non_exhaustive]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskEventCause {
    /// 熔斷觸發，以及套用的動作。
    BreakerTrip {
        trigger: BreakerTrigger,
        action: BreakerAction,
    },
    /// 一鍵停止被開啟／關閉。
    KillSwitch { on: bool },
}

impl fmt::Display for RiskEventCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskEventCause::BreakerTrip { trigger, action } => {
                write!(f, "熔斷觸發：{trigger} → {action}")
            }
            RiskEventCause::KillSwitch { on: true } => write!(f, "一鍵停止已開啟"),
            RiskEventCause::KillSwitch { on: false } => write!(f, "一鍵停止已關閉"),
        }
    }
}

/// 一則觸發紀錄。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskEvent {
    /// 單調遞增的序號，同一毫秒內有多筆也排得出先後。
    ///
    /// 由 [`crate::store::RiskEventLog::push`] 指派；自己 `new` 出來還沒 push 的
    /// 事件這個值沒有意義。
    pub seq: u64,
    /// 事件時間（毫秒）。這個 crate 不讀時鐘，由呼叫端給。
    pub at_ms: i64,
    /// `at_ms` 是哪個時鐘的時間。
    pub clock: ClockSource,
    /// 哪一個 session（策略）。`None` = 帳戶層的事件。
    ///
    /// 型別是 `String` 而不是 `SessionId`：session registry 還不存在，而 ADR 12.2
    /// 決定 `SessionId` 要由 registry 定義、本 crate 引用，所以不在這裡先發明一個
    /// 之後要被取代的型別。registry 落地時把這個欄位換掉。
    pub session: Option<String>,
    /// 哪一個交易對。`None` = 和特定交易對無關。
    #[serde(default, with = "serde_at::symbol_opt")]
    pub symbol: Option<Symbol>,
    pub cause: RiskEventCause,
}

impl RiskEvent {
    /// 一則還沒入帳的紀錄：`seq` 是 0（push 時才指派），`session` 與 `symbol` 是
    /// `None`，要填就用 struct update 語法補上。
    pub fn new(at_ms: i64, clock: ClockSource, cause: RiskEventCause) -> RiskEvent {
        RiskEvent {
            seq: 0,
            at_ms,
            clock,
            session: None,
            symbol: None,
            cause,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker::{BreakerRules, BreakerTrip, DEFAULT_CONSECUTIVE_LOSSES};

    fn trip_event() -> RiskEvent {
        RiskEvent {
            session: Some("session-1".to_string()),
            symbol: Some(Symbol::new("BTCUSDT").unwrap()),
            ..RiskEvent::new(
                1_790_000_000_000,
                ClockSource::Exchange,
                RiskEventCause::BreakerTrip {
                    trigger: DEFAULT_CONSECUTIVE_LOSSES,
                    action: BreakerAction::PauseStrategy,
                },
            )
        }
    }

    #[test]
    fn an_event_round_trips_through_json() {
        let event = trip_event();
        let json = serde_json::to_string_pretty(&event).unwrap();
        assert_eq!(serde_json::from_str::<RiskEvent>(&json).unwrap(), event);
    }

    #[test]
    fn the_record_stores_structure_not_a_formatted_sentence() {
        let json = serde_json::to_string(&trip_event()).unwrap();
        // 參數與動作都在檔案裡，可以用來篩選。
        assert!(json.contains(r#""count":5"#), "{json}");
        assert!(json.contains(r#""PauseStrategy""#), "{json}");
        assert!(json.contains(r#""Exchange""#), "{json}");
        // 顯示用的中文句子不在檔案裡。
        assert!(!json.contains("熔斷"), "{json}");
    }

    #[test]
    fn display_text_is_produced_when_reading() {
        assert_eq!(
            trip_event().cause.to_string(),
            "熔斷觸發：連續虧損 5 筆 → 暫停這個策略"
        );
        assert_eq!(
            RiskEventCause::KillSwitch { on: true }.to_string(),
            "一鍵停止已開啟"
        );
        assert_eq!(ClockSource::Local.to_string(), "本機時間");
    }

    #[test]
    fn a_trip_turns_straight_into_a_cause() {
        // 評估結果和紀錄之間不需要翻譯層：同一組 trigger + action。
        let BreakerTrip { trigger, action } = BreakerTrip {
            trigger: DEFAULT_CONSECUTIVE_LOSSES,
            action: BreakerAction::PauseStrategy,
        };
        let cause = RiskEventCause::BreakerTrip { trigger, action };
        assert_eq!(
            cause,
            RiskEventCause::BreakerTrip {
                trigger: DEFAULT_CONSECUTIVE_LOSSES,
                action: BreakerAction::PauseStrategy,
            }
        );
        // 規則集裡的強制規則也能原樣變成一則紀錄。
        let mandatory = BreakerRules::new(vec![]).rules()[0];
        let cause = RiskEventCause::BreakerTrip {
            trigger: mandatory.trigger,
            action: mandatory.action,
        };
        assert_eq!(
            cause.to_string(),
            "熔斷觸發：對帳不一致 → 暫停整個帳戶的下單"
        );
    }

    #[test]
    fn a_bad_symbol_in_the_file_is_rejected_not_silently_accepted() {
        let json = r#"{"seq":1,"at_ms":1,"clock":"Local",
                       "session":null,"symbol":"BTC/USDT",
                       "cause":{"KillSwitch":{"on":true}}}"#;
        assert!(serde_json::from_str::<RiskEvent>(json).is_err());
    }

    #[test]
    fn a_missing_symbol_field_reads_as_none() {
        let json = r#"{"seq":1,"at_ms":1,"clock":"Local","session":null,
                       "cause":{"KillSwitch":{"on":false}}}"#;
        let event: RiskEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.symbol, None);
    }
}
