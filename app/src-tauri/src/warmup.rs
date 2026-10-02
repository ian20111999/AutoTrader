//! 啟動模擬／測試網交易之前，先把暖機要用的歷史 K 線抓回來。
//!
//! 引擎那一層（`at-paper-trading`、`at-testnet-trading`）只負責「把給它的歷史
//! K 線餵給策略」，刻意不碰網路；**抓不到要怎麼辦**是這裡的決定，因為只有這一層
//! 知道怎麼把話講給使用者聽。
//!
//! # 抓不到就不要開始：擋下啟動，不默默冷啟動
//!
//! 冷啟動不是「慢一點」，是**訊號和回測不一樣**（`at_core::warmup` 有算式）。
//! 使用者按下「開始」時相信跑的是他剛剛回測過的那套策略，所以寧可讓他看到
//! 一句清楚的錯誤訊息、自己決定要不要重試，也不要讓他以為暖機過了。
//!
//! 「降級成冷啟動但顯示警告」在這裡沒有好處：暖機用的是和即時行情同一個
//! 來源（Binance 正式環境的公開端點），**抓不到歷史的時候 WebSocket 幾乎一定
//! 也連不上**，讓它啟動只會換成一個更晚、更難懂的失敗。
//!
//! 兩個例外，都不是「默默降級」：
//!
//! - 策略宣告 `warmup_bars() == 0`（例如測試用的固定部位策略）：不需要暖機，
//!   連請求都不發。
//! - 抓到的根數比想要的少，但**還是到得了策略宣告的最低根數**
//!   （`WarmupBars::covers`）：新上市的交易對就是這樣。指標算得出數值、訊號有
//!   意義，只是安全餘裕變薄，所以放行。連最低根數都湊不到才擋下來。

use at_binance::market_data::recent_closed_bars;
use at_core::{warmup_fetch_count, Interval, Strategy, Symbol, WarmupBars};

/// 抓這個策略需要的暖機 K 線；抓不到或不夠用時回**給使用者看的**繁中錯誤訊息。
pub fn fetch(
    symbol: &Symbol,
    interval: Interval,
    strategy: &dyn Strategy,
) -> Result<WarmupBars, String> {
    let want = warmup_fetch_count(strategy);
    let warmup = recent_closed_bars(symbol, interval, want).map_err(|e| {
        format!(
            "抓不到暖機用的歷史 K 線，無法開始：{e}。\
                 沒有暖機的策略訊號會和回測不一樣，所以這裡不會用冷啟動代替。請確認網路後重試"
        )
    })?;

    if !warmup.covers(strategy) {
        return Err(format!(
            "{symbol} 的 {interval} 歷史 K 線只抓到 {got} 根，策略至少需要 {need} 根才算得出指標。\
             請改用較短的週期、較短參數的策略，或換一個上市較久的交易對",
            got = warmup.len(),
            need = strategy.warmup_bars(),
        ));
    }
    if strategy.needs_order_flow() && !warmup.has_order_flow() {
        return Err(
            "這支策略用到訂單流（成交筆數／主動買盤佔比），但暖機抓到的歷史 K 線\
             沒有這個資料。請確認交易所回應格式沒有變化後重試"
                .to_string(),
        );
    }
    Ok(warmup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{Bar, TargetPosition};

    struct NoWarmup;

    impl Strategy for NoWarmup {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FLAT
        }
    }

    #[test]
    fn a_strategy_that_needs_no_warmup_starts_without_touching_the_network() {
        // 這個測試不需要網路就會通過，這本身就是「沒發請求」的證據。
        let warmup = fetch(&Symbol::new("BTCUSDT").unwrap(), Interval::M1, &NoWarmup)
            .expect("不需要暖機的策略不該因為暖機而啟動失敗");
        assert_eq!(warmup, WarmupBars::none());
    }
}
