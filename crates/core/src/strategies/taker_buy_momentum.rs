//! 主動買盤動能：連續幾根都是買方主導就做多，連續幾根賣方主導就出場。

use super::{non_zero, taker_buy_ratio, taker_ratio_thresholds, StrategyParamError};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// 主動買盤動能（Taker Buy Momentum，純訂單流動能）。
///
/// 完全不看價格，只看**成交量的結構**：主動買盤佔比
/// （`taker_buy_volume ÷ volume`）連續 `streak` 根都高過門檻就做多，
/// 連續 `streak` 根都低於 `1 − 門檻` 就出場。
///
/// - 連續 `streak` 根佔比 > `threshold` → 滿倉做多
/// - 連續 `streak` 根佔比 < `1 − threshold` → 空手
/// - 其他情形 → 維持上一根的部位
///
/// ## 和[訂單流確認突破](super::OrderFlowBreakout)的差別
///
/// 那一支是「價格突破」為主、訂單流當確認；這一支**沒有價格條件**，訊號完全
/// 來自訂單流自己。價格創新高通常已經是結果，而主動買盤連續偏向一方是「還在
/// 發生中」的事，在 1 秒 K 線這種單根價格只動幾個跳動點的情境下，它往往比價格
/// 先表態。兩支策略會在同一段行情給出不同的進出點，這就是它們各自存在的理由。
///
/// ## 為什麼要「連續」而不是單根
///
/// 單根的主動買盤佔比雜訊太大：一筆大單吃掉掛單就能讓某一秒的佔比衝到 0.9，
/// 下一秒就回到 0.4。要求連續幾根都同向，等於要求「買方的壓力持續存在」，
/// 這才是動能。`streak` 就是這個策略唯一的平滑手段，所以它開放調整。
///
/// ## 兩個刻意的選擇
///
/// 1. **缺資料會打斷連續，但不會改變部位。** 某根 K 線算不出佔比
///    （來源沒有 `order_flow`，或成交量為 0）時，連續計數**歸零**，但目標部位
///    維持原狀。歸零是因為「連續 N 根都滿足」這句話在缺了一根之後就不再成立
///    ——把缺的那根當成「滿足」是在替資料編故事，當成「不滿足」又會莫名其妙
///    推進反向的計數，所以兩邊都歸零、重新數。
/// 2. **跌破門檻是出場，不是反手做空。** 同其他內建策略，理由見
///    [`crate::strategies`] 的模組說明。
///
/// 「載入端硬擋 + `on_bar` 保守不動作」這兩層的分工和
/// [`OrderFlowBreakout`](super::OrderFlowBreakout) 完全一樣，那邊有完整說明。
#[derive(Debug)]
pub struct TakerBuyMomentum {
    /// 要連續幾根同向才算動能成立。
    streak: usize,
    /// 做多要求的主動買盤佔比下限。
    buy_above: Fixed,
    /// 出場要求的主動買盤佔比上限（`1 − buy_above`）。
    sell_below: Fixed,
    /// 目前連續幾根佔比高過 `buy_above`。
    buy_run: usize,
    /// 目前連續幾根佔比低於 `sell_below`。
    sell_run: usize,
    holding: bool,
}

impl TakerBuyMomentum {
    /// 預設連續根數。
    pub const DEFAULT_STREAK: usize = 3;
    /// 預設主動買盤佔比門檻（0.6）。
    ///
    /// 比[訂單流確認突破](super::OrderFlowBreakout)的 0.55 嚴格，因為這支策略
    /// 沒有價格條件當第二道關卡，門檻鬆一點就會變成一直在進出場。
    pub const DEFAULT_TAKER_BUY_THRESHOLD: Fixed = Fixed::from_raw(60 * Fixed::SCALE / 100);

    /// `streak` 是要連續幾根同向，`taker_buy_threshold` 是主動買盤佔比門檻
    /// （必須在 0.5～1 之間，理由見
    /// [`StrategyParamError::TakerRatioOutOfRange`]）。
    pub fn new(
        streak: usize,
        taker_buy_threshold: Fixed,
    ) -> Result<TakerBuyMomentum, StrategyParamError> {
        let streak = non_zero(streak)?;
        let (buy_above, sell_below) = taker_ratio_thresholds(taker_buy_threshold)?;
        Ok(TakerBuyMomentum {
            streak,
            buy_above,
            sell_below,
            buy_run: 0,
            sell_run: 0,
            holding: false,
        })
    }

    /// 目前連續幾根是買方主導（用來看策略內部狀態、也給測試斷言）。
    pub fn buy_run(&self) -> usize {
        self.buy_run
    }

    /// 目前連續幾根是賣方主導。
    pub fn sell_run(&self) -> usize {
        self.sell_run
    }
}

impl Default for TakerBuyMomentum {
    /// 3 根 / 0.6。
    fn default() -> TakerBuyMomentum {
        // 兩個常數寫死且合法，不是外部輸入
        TakerBuyMomentum::new(
            TakerBuyMomentum::DEFAULT_STREAK,
            TakerBuyMomentum::DEFAULT_TAKER_BUY_THRESHOLD,
        )
        .expect("內建預設參數必須合法")
    }
}

impl Strategy for TakerBuyMomentum {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        // 三個分支互斥（門檻保證 sell_below ≤ 0.5 ≤ buy_above）：
        // 買方主導推進買方計數、賣方主導推進賣方計數，其餘（含算不出佔比）
        // 兩邊歸零重新數。
        match taker_buy_ratio(bar) {
            Some(ratio) if ratio > self.buy_above => {
                self.buy_run = self.buy_run.saturating_add(1);
                self.sell_run = 0;
            }
            Some(ratio) if ratio < self.sell_below => {
                self.sell_run = self.sell_run.saturating_add(1);
                self.buy_run = 0;
            }
            _ => {
                self.buy_run = 0;
                self.sell_run = 0;
            }
        }

        if self.buy_run >= self.streak {
            self.holding = true;
        } else if self.sell_run >= self.streak {
            self.holding = false;
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    /// 連續根數本身就是暖機需求：`streak` 根之前不可能數到 `streak` 根連續。
    /// 沒有任何需要收斂的平滑指標，所以不多也不少。
    fn warmup_bars(&self) -> usize {
        self.streak
    }

    /// 核心邏輯就是 `Bar::order_flow`，沒有這個欄位這支策略完全不會出訊號。
    fn needs_order_flow(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{closes, extreme_bars, fx, signals, with_taker_ratios};

    /// 測試用：連續 3 根、門檻 0.625（出場門檻因此是 0.375）。
    ///
    /// 用 0.625 而不是預設的 0.6，是為了讓邊界斷言站得住：0.625 = 5/8 在二進位
    /// 下精確，佔比從兩個 `f64` 成交量算回來不會差一個最小跳動點
    /// （見 `test_util::with_taker_ratios` 的說明）。預設值 0.6 本身由
    /// `Fixed::from_raw` 寫死，不經過 `f64`，由 `default_is_*` 那個測試驗。
    fn momentum() -> TakerBuyMomentum {
        TakerBuyMomentum::new(3, fx("0.625")).unwrap()
    }

    /// 價格全部固定在 100——這支策略不看價格，用固定價才看得出訊號只來自訂單流。
    fn flow(ratios: &[Option<f64>]) -> Vec<Bar> {
        let prices = vec!["100"; ratios.len()];
        with_taker_ratios(&closes(&prices), ratios)
    }

    #[test]
    fn rejects_bad_params() {
        assert_eq!(
            TakerBuyMomentum::new(0, fx("0.6")).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            TakerBuyMomentum::new(3, fx("0.4")).unwrap_err(),
            StrategyParamError::TakerRatioOutOfRange
        );
        assert_eq!(
            TakerBuyMomentum::new(3, fx("1.5")).unwrap_err(),
            StrategyParamError::TakerRatioOutOfRange
        );
        assert!(TakerBuyMomentum::new(1, fx("0.5")).is_ok());
    }

    #[test]
    fn default_is_three_bars_and_zero_point_six() {
        let s = TakerBuyMomentum::default();
        assert_eq!(s.streak, 3);
        assert_eq!(s.buy_above, fx("0.6"));
        assert_eq!(s.sell_below, fx("0.4"));
        assert_eq!(s.warmup_bars(), 3);
        assert!(s.needs_order_flow());
    }

    #[test]
    fn three_buyer_led_bars_enter_and_three_seller_led_bars_exit() {
        // 前兩根買方主導還不夠（連續 2 < 3），第三根才進場；
        // 之後連續三根賣方主導才出場，同樣是第三根才動作。
        let mut s = momentum();
        let bars = flow(&[
            Some(0.75),
            Some(0.875),
            Some(1.0), // 連續 3 根 → 進場
            Some(0.25),
            Some(0.25),  // 連續 2 根賣方，還不夠 → 續抱
            Some(0.125), // 連續 3 根 → 出場
        ]);
        assert_eq!(signals(&mut s, &bars), "..LLL.");
    }

    #[test]
    fn a_middle_bar_breaks_the_run() {
        // 第 3 根佔比 0.5 落在兩個門檻之間（既不是買方主導也不是賣方主導）→
        // 連續中斷，要重新數 3 根。
        let mut s = momentum();
        let bars = flow(&[
            Some(0.75),
            Some(0.875),
            Some(0.5), // 中斷
            Some(0.75),
            Some(0.875), // 重新數到 2
            Some(1.0),   // 重新數到 3 → 進場
        ]);
        assert_eq!(signals(&mut s, &bars), ".....L");
        assert_eq!(s.buy_run(), 3);
        assert_eq!(s.sell_run(), 0);
    }

    #[test]
    fn the_thresholds_are_strict_so_sitting_on_them_breaks_the_run() {
        // 佔比剛好等於門檻不算同向（和另外兩支策略的嚴格比較一致）
        let mut s = momentum();
        let bars = flow(&[Some(0.75), Some(0.625), Some(0.75), Some(0.875)]);
        assert_eq!(taker_buy_ratio(&bars[1]), Some(fx("0.625")));
        // 第 2 根剛好 0.625 → 中斷，所以到第 4 根只數到 2 根，還不進場
        assert_eq!(signals(&mut s, &bars), "....");
        assert_eq!(s.buy_run(), 2);
    }

    #[test]
    fn stays_flat_until_warmed_up_even_when_every_bar_is_buyer_led() {
        // 前 2 根就算佔比 1.0（成交全是主動買盤）也不能進場
        let mut s = momentum();
        assert_eq!(signals(&mut s, &flow(&[Some(1.0), Some(1.0)])), "..");
    }

    #[test]
    fn a_bar_without_order_flow_breaks_the_run_but_keeps_the_position() {
        // 進場後遇到一根沒有訂單流的 K 線：部位不變（不被動出場），
        // 但連續計數歸零——缺了一根就不能再聲稱「連續 N 根都滿足」。
        let mut s = momentum();
        let bars = flow(&[
            Some(0.75),
            Some(0.875),
            Some(1.0), // 進場
            None,      // 來源沒有訂單流 → 續抱、計數歸零
            Some(0.125),
            Some(0.125), // 賣方只數到 2，還不夠出場
        ]);
        assert_eq!(signals(&mut s, &bars), "..LLLL");
        assert_eq!((s.buy_run(), s.sell_run()), (0, 2));
    }

    #[test]
    fn a_missing_bar_also_delays_the_entry() {
        // 反向確認：缺資料是雙向的，空手時也不會因為缺資料而進場
        let mut s = momentum();
        let bars = flow(&[Some(0.75), Some(0.875), None, Some(1.0), Some(1.0)]);
        assert_eq!(signals(&mut s, &bars), ".....");
        assert_eq!(s.buy_run(), 2);
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        // extreme_bars 的 order_flow 是 None，所以這支策略在上面永遠空手
        let mut s = momentum();
        assert_eq!(signals(&mut s, &extreme_bars()), "........");
    }
}
