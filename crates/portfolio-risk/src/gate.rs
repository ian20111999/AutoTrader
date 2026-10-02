//! 送單路徑上的全域閘門（ADR-002 第 4 節）。
//!
//! # 這個檔案裡沒有任何判斷邏輯，只有契約
//!
//! 熔斷規則的評估在 [`crate::breaker`]，累加器在 App 層（ADR 5.4）。這裡定義的是
//! 「`at-testnet-trading` 的送單路徑要呼叫誰、擋下時長什麼樣子」——**兩道並列的
//! 閘門**裡的第二道（第一道是 6.3 的 `at_risk_control::RiskLimits::check`）。
//!
//! # 為什麼是 trait 而不是具體型別
//!
//! 比照 `at_testnet_trading::OrderGateway` 的既有慣例：累加器要吃 App 層的事件流
//! （`TestnetUpdate`），而送單路徑在這個 crate 的下游。用 trait 注入之後
//! `at-testnet-trading` 只認得這個契約，測試自己寫三行的 fake，production 的唯一
//! 接線點在 App 層。
//!
//! **刻意不是 `Option<gate>`。** `Option` 等於在型別上開一條「這條路沒有全域風控」
//! 的合法路徑，正是 ADR 第 11 節第 10 項要防的東西。
//!
//! # 為什麼 trait 上有「行情心跳」
//!
//! 「行情中斷超過 N 毫秒」這條熔斷規則要的輸入是**每一則行情事件的時間**，而那條
//! 事件流只存在於交易迴圈裡：App 層只看得到「收盤 K 線處理完的快照」
//! （`TestnetUpdate::Bar`），看不到未收盤的 K 線與 ticker。1 分鐘 K 線的間隔本來就
//! 遠大於 3 秒門檻，只拿收盤 K 線當心跳的話這條規則只有兩種下場：永遠觸發，或者
//! 把門檻調到沒有意義。所以心跳由迴圈主動餵進來，一行呼叫。
//!
//! 代價：trait 有兩個方法而不是一個，而且「有沒有餵心跳」是呼叫端的紀律。緩解是
//! 沒有餵心跳的實作會**永遠擋單**（`last_market_event_ms` 一直是 `None` →
//! 行情中斷規則觸發），失效方向是安全的那一邊。
//!
//! # 這裡沒有 `GlobalOrder`
//!
//! ADR 4.2 的草案簽名是 `check(&GlobalOrder, now_ms)`，因為全域上限
//! （單一幣種／總部位）需要這筆單的 symbol 與數量。那一半（`GlobalLimits`）還沒有
//! 落地，而熔斷規則的判斷**完全不看這筆單長什麼樣**（連續虧損、行情中斷、拒絕率、
//! 對帳、回撤都是 session 狀態的函式）。先加一個今天沒有人讀的參數，是把成本付在
//! 錯的地方；真的做全域上限時再加，編譯器會指出每一個呼叫點。

use crate::breaker::BreakerTrip;
use std::fmt;

/// 送單前的全域檢查。實作者持有熔斷累加器與觸發紀錄（App 層）。
///
/// `Send + Sync` 是 bound 而不是呼叫端的 where 條件：閘門會被交易執行緒持有，
/// 同時 App 層的轉發執行緒也在餵它事件流。
pub trait PortfolioGate: Send + Sync {
    /// 收到**任何**一則行情事件時呼叫（含未收盤的 K 線與 ticker），`at_ms` 是
    /// 交易所給的事件時間。
    ///
    /// 這是「行情中斷」熔斷規則唯一的輸入。實作者可以在這裡就評估規則並讓觸發
    /// 黏著（ADR 5.6），不必等到下一次 [`PortfolioGate::check`]。
    fn observe_market_event(&self, at_ms: i64);

    /// 這一筆單可不可以送出去。`now_ms` 由呼叫端提供，必須和
    /// [`PortfolioGate::observe_market_event`] 同一個時鐘來源（交易所時間）。
    ///
    /// 回 `Err` 就是**不送**。實作者負責在擋下的當下記一筆觸發紀錄——擋單理由
    /// 只會出現在這一根 K 線的下單結果裡，紀錄才是之後查得到的證據。
    fn check(&self, now_ms: i64) -> Result<(), GlobalBlocked>;
}

/// 全域閘門擋下一筆單的原因。
///
/// 和 6.3 的 `Blocked` 是**兩個獨立的 enum**（ADR 2.2）：那一層的理由都是
/// session 內的語氣，而且把兩層混成一個 enum 之後「是哪一道閘門擋的」就消失了，
/// 而觸發紀錄要這個資訊。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalBlocked {
    /// 熔斷已觸發。帶著觸發當下的條件與動作，所以訊息裡有門檻值。
    BreakerTripped(BreakerTrip),
    /// 風控狀態本身讀不到（鎖被 panic 毒化、累加器不存在……）。
    ///
    /// 判不出來就擋：熔斷器自己壞掉的時候放行，等於風控從來沒存在過。
    RiskStateUnavailable,
}

impl fmt::Display for GlobalBlocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlobalBlocked::BreakerTripped(trip) => write!(
                f,
                "熔斷已觸發（{trip}），這筆單沒有送出。確認部位之後請停止這場交易再重新啟動"
            ),
            GlobalBlocked::RiskStateUnavailable => write!(
                f,
                "風控狀態讀不到，判不出來一律擋單，這筆單沒有送出。請停止這場交易並查看觸發紀錄"
            ),
        }
    }
}

impl std::error::Error for GlobalBlocked {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker::{BreakerAction, DEFAULT_CONSECUTIVE_LOSSES};

    #[test]
    fn a_blocked_order_says_which_rule_and_what_the_threshold_was() {
        let blocked = GlobalBlocked::BreakerTripped(BreakerTrip {
            trigger: DEFAULT_CONSECUTIVE_LOSSES,
            action: BreakerAction::PauseStrategy,
        });
        let message = blocked.to_string();
        // 門檻值要在訊息裡：使用者看到的不能只是「熔斷了」。
        assert!(message.contains("連續虧損 5 筆"), "{message}");
        assert!(message.contains("沒有送出"), "{message}");
    }

    #[test]
    fn an_unreadable_risk_state_says_it_blocked_rather_than_passed() {
        let message = GlobalBlocked::RiskStateUnavailable.to_string();
        assert!(message.contains("擋單"), "{message}");
        assert!(message.contains("沒有送出"), "{message}");
    }
}
