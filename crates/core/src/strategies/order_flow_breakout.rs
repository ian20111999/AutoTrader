//! 訂單流確認突破：通道突破 + 主動買盤佔比同向，才算真訊號。

use super::{non_zero, taker_buy_ratio, taker_ratio_thresholds, StrategyParamError, Window};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// 訂單流確認突破（Order-flow Confirmed Breakout，高頻導向的趨勢突破）。
///
/// 骨架是唐奇安通道突破（收盤價創 N 根新高／新低），但多一道**訂單流確認**：
/// 突破的那根 K 線，主動買盤佔比也要偏向同一個方向。
///
/// - 收盤 > 前 `period` 根最高價，**且**主動買盤佔比 > `threshold` → 滿倉做多
/// - 收盤 < 前 `period` 根最低價，**且**主動買盤佔比 < `1 − threshold` → 空手
/// - 其他情形（含「突破了但訂單流不配合」）→ 維持上一根的部位
///
/// ## 為什麼要這道確認
///
/// 價格創新高有兩種成因，光看 K 線分不出來：有人**主動吃單**買上去（真的有買方
/// 在追價，趨勢比較站得住），或是賣方掛單撤走、薄薄的掛單簿被小量成交推上去
/// （一回頭就打回原點）。主動買盤佔比（`taker_buy_volume ÷ volume`）直接量的
/// 就是前者的比重：0.55 表示這根 K 線 55% 的成交量是主動買方促成的。
///
/// 在 1 秒 K 線這種高頻情境下差別特別大——單根的價格變化只有幾個跳動點，
/// 雜訊比例高，而訂單流是「成交量的結構」，不會因為週期短而失真。
///
/// ## 兩個刻意的選擇
///
/// 1. **沿用唐奇安的「通道用前幾根算、不含當下這根」**，理由完全一樣
///    （含自己的話收盤價永遠不可能高過含自己的最高價，訊號永遠不出現），
///    所以 `on_bar` 先用現有視窗判斷，判斷完才把這根放進視窗。
///    進出場共用同一個 `period`：進出場的敏感度差異交給訂單流門檻去調，
///    不再多一個使用者看不出差別的參數。
///
///    ponytail: 沒有把「突破前 N 根高低點」抽成和 [`Donchian`](super::Donchian)
///    共用的零件。真正會重複的只有「最近 N 根的最高／最低值」，而那就是
///    [`Window`] 本身，兩支策略都已經在重用它；剩下的 `highest()`／`lowest()`
///    比較各只有一行，再包一層抽象換不到任何東西。
/// 2. **跌破下軌是出場，不是反手做空。** 同其他內建策略，理由見
///    [`crate::strategies`] 的模組說明。
///
/// ## 資料不足（沒有訂單流）時怎麼辦：兩層，而且不是重複
///
/// [`needs_order_flow`](Strategy::needs_order_flow) 回 `true`，所以：
///
/// 1. **載入端硬擋**（和 DSL 自訂策略一致）：`app/src-tauri` 在跑回測／啟動
///    模擬之前就檢查「策略要訂單流、這份資料有沒有」，沒有就回一句繁中錯誤叫
///    使用者重新下載 K 線。因為「整份資料都是 6 欄舊格式」的結果會是**一場
///    靜悄悄、零交易的回測**——使用者會以為是策略不賺錢，而不是資料不對。
/// 2. **`on_bar` 保守不動作**：遇到 `order_flow` 是 `None`（或成交量為 0，佔比
///    無法判定）的那根 K 線，**不產生新訊號、維持原部位**，不 panic 也不報錯。
///
/// 第 2 層不是第 1 層的重複，因為兩者擋的是不同的事：第 1 層只看第一根
/// （同一個資料來源不會中途換格式），而「**某一根**成交量為 0」是真的會發生的
/// ——Binance 對完全沒成交的那一分鐘確實會發 K 線。而且 [`Strategy::on_bar`]
/// 的契約本來就是「算不出來寧可不動，也不亂猜」，策略不該假設呼叫端一定擋過。
///
/// 選「保守不動作」而不是在 `on_bar` 回報錯誤，是因為訊號層沒有錯誤通道
/// （`on_bar` 的回傳型別就是目標部位），而把缺一根資料升級成「中止整場回測」
/// 代價太大、也和 trait 契約相反。
#[derive(Debug)]
pub struct OrderFlowBreakout {
    highs: Window,
    lows: Window,
    /// 做多要求的主動買盤佔比下限。
    buy_above: Fixed,
    /// 出場要求的主動買盤佔比上限（`1 − buy_above`）。
    sell_below: Fixed,
    holding: bool,
}

impl OrderFlowBreakout {
    /// 預設突破回看根數。
    pub const DEFAULT_PERIOD: usize = 20;
    /// 預設主動買盤佔比門檻（0.55）。
    ///
    /// 不是 0.5：長期而言主動買賣大致各半，0.5 幾乎等於沒有篩選。0.55 代表
    /// 「這根明顯偏買方」，又不像 0.7 那樣罕見到幾乎不出訊號。
    pub const DEFAULT_TAKER_BUY_THRESHOLD: Fixed = Fixed::from_raw(55 * Fixed::SCALE / 100);

    /// `period` 是突破要回看的根數，`taker_buy_threshold` 是主動買盤佔比門檻
    /// （必須在 0.5～1 之間，理由見
    /// [`StrategyParamError::TakerRatioOutOfRange`]）。
    pub fn new(
        period: usize,
        taker_buy_threshold: Fixed,
    ) -> Result<OrderFlowBreakout, StrategyParamError> {
        let period = non_zero(period)?;
        let (buy_above, sell_below) = taker_ratio_thresholds(taker_buy_threshold)?;
        Ok(OrderFlowBreakout {
            highs: Window::new(period),
            lows: Window::new(period),
            buy_above,
            sell_below,
            holding: false,
        })
    }

    /// 下一根的收盤價要**超過**這個價格才算突破（目前視窗內的最高價）。
    /// 暖機不足時回傳 `None`。
    pub fn entry_high(&self) -> Option<Fixed> {
        self.highs.highest()
    }

    /// 下一根的收盤價**跌破**這個價格才算破底（目前視窗內的最低價）。
    /// 暖機不足時回傳 `None`。
    pub fn exit_low(&self) -> Option<Fixed> {
        self.lows.lowest()
    }
}

impl Default for OrderFlowBreakout {
    /// 20 根 / 0.55。
    fn default() -> OrderFlowBreakout {
        // 兩個常數寫死且合法，不是外部輸入
        OrderFlowBreakout::new(
            OrderFlowBreakout::DEFAULT_PERIOD,
            OrderFlowBreakout::DEFAULT_TAKER_BUY_THRESHOLD,
        )
        .expect("內建預設參數必須合法")
    }
}

impl Strategy for OrderFlowBreakout {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        // 先取「這根之前」的通道，再把這根放進視窗——否則會拿自己跟自己比
        let channel = (self.entry_high(), self.exit_low());
        self.highs.push(bar.high);
        self.lows.push(bar.low);

        let (Some(entry_high), Some(exit_low)) = channel else {
            self.holding = false;
            return TargetPosition::FLAT;
        };
        // 訂單流算不出來（來源沒這個欄位、或這根成交量為 0）→ 這根不產生新訊號，
        // 維持原部位（型別說明的第 2 層）。
        if let Some(ratio) = taker_buy_ratio(bar) {
            // 門檻保證 sell_below ≤ 0.5 ≤ buy_above，兩個條件互斥，順序不影響結果
            if bar.close > entry_high && ratio > self.buy_above {
                self.holding = true;
            } else if bar.close < exit_low && ratio < self.sell_below {
                self.holding = false;
            }
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    /// 視窗要填滿，**再加 1 根**才能出第一個訊號：通道是「前幾根」算的，
    /// 判斷用的那根本身不算在視窗裡。
    fn warmup_bars(&self) -> usize {
        self.highs.cap() + 1
    }

    /// 核心邏輯依賴 `Bar::order_flow`，所以載入端該把這支策略納入
    /// 「資料有沒有訂單流」的檢查（型別說明的第 1 層）。
    fn needs_order_flow(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::OrderFlow;
    use crate::strategies::test_util::{
        extreme_bars, fx, highs_lows_closes, signals, with_taker_ratios,
    };

    /// 測試用：回看 2 根、門檻 0.625（出場門檻因此是 0.375）。
    ///
    /// 用 0.625 而不是預設的 0.55，是為了讓邊界斷言站得住：0.625 = 5/8 在二進位
    /// 下精確，佔比從兩個 `f64` 成交量算回來不會差一個最小跳動點
    /// （見 `test_util::with_taker_ratios` 的說明）。預設值 0.55 本身由
    /// `Fixed::from_raw` 寫死，不經過 `f64`，由 `default_is_*` 那個測試驗。
    fn breakout() -> OrderFlowBreakout {
        OrderFlowBreakout::new(2, fx("0.625")).unwrap()
    }

    /// 一段「盤整兩根後突破、再破底」的行情：(最高, 最低, 收盤)
    ///
    ///   第 1、2 根：建立通道（最高 10、最低 9）
    ///   第 3 根   ：收 12 > 前兩根最高 10 → 價格面突破成立
    ///   第 4 根   ：收 8 < 前兩根最低 9  → 價格面破底成立
    fn breakout_then_breakdown() -> Vec<Bar> {
        highs_lows_closes(&[
            ("10", "9", "10"),
            ("10", "9", "10"),
            ("12", "9", "12"),
            ("12", "8", "8"),
        ])
    }

    #[test]
    fn rejects_bad_params() {
        assert_eq!(
            OrderFlowBreakout::new(0, fx("0.55")).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        // 門檻低於 0.5：做多與出場條件會重疊，擋掉
        assert_eq!(
            OrderFlowBreakout::new(20, fx("0.49999999")).unwrap_err(),
            StrategyParamError::TakerRatioOutOfRange
        );
        assert_eq!(
            OrderFlowBreakout::new(20, fx("1.00000001")).unwrap_err(),
            StrategyParamError::TakerRatioOutOfRange
        );
        assert_eq!(
            OrderFlowBreakout::new(20, fx("-0.55")).unwrap_err(),
            StrategyParamError::TakerRatioOutOfRange
        );
        // 邊界值兩端都合法
        assert!(OrderFlowBreakout::new(1, fx("0.5")).is_ok());
        assert!(OrderFlowBreakout::new(1, fx("1")).is_ok());
    }

    #[test]
    fn default_is_twenty_bars_and_zero_point_five_five() {
        let s = OrderFlowBreakout::default();
        assert_eq!(s.highs.cap(), 20);
        assert_eq!(s.buy_above, fx("0.55"));
        assert_eq!(s.sell_below, fx("0.45"));
        // 20 根填滿視窗，第 21 根才是第一根能判斷突破的 K 線
        assert_eq!(s.warmup_bars(), 21);
        assert!(s.needs_order_flow());
    }

    #[test]
    fn a_breakout_backed_by_taker_buying_enters_and_a_breakdown_exits() {
        // 第 3 根突破且佔比 0.75 > 0.625 → 進場
        // 第 4 根破底且佔比 0.25 < 0.375 → 出場
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.75), Some(0.25)],
        );
        assert_eq!(signals(&mut s, &bars), "..L.");
    }

    #[test]
    fn a_breakout_without_taker_buying_is_not_a_signal() {
        // 價格面完全一樣，只把突破那根的主動買盤佔比壓到 0.5（< 0.625）→ 不進場。
        // 這是這支策略和純唐奇安突破的唯一差別，所以單獨釘一個測試。
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.5), Some(0.25)],
        );
        assert_eq!(signals(&mut s, &bars), "....");
        // 確認「價格確實突破了」，不進場純粹是訂單流擋下來的
        assert_eq!(bars[2].close, fx("12"));
        assert!(bars[2].close > fx("10"));
    }

    #[test]
    fn the_threshold_is_strictly_greater_not_greater_or_equal() {
        // 佔比剛好等於門檻 0.625 不算確認（和「創新高」用嚴格大於一致）
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.625), Some(0.25)],
        );
        // 先證明這根的佔比真的**剛好**落在門檻上，不是差了一點點
        assert_eq!(taker_buy_ratio(&bars[2]), Some(fx("0.625")));
        assert_eq!(signals(&mut s, &bars), "....");

        // 只要高過門檻就過關
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.75), Some(0.25)],
        );
        assert_eq!(signals(&mut s, &bars), "..L.");
    }

    #[test]
    fn the_exit_also_needs_taker_selling() {
        // 破底但主動賣盤不夠（佔比剛好 0.375，不低於出場門檻 0.375）→ 不出場，續抱
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.75), Some(0.375)],
        );
        assert_eq!(taker_buy_ratio(&bars[3]), Some(fx("0.375")));
        assert_eq!(signals(&mut s, &bars), "..LL");
    }

    #[test]
    fn stays_flat_until_the_channel_is_warmed_up() {
        // 前 2 根不管訂單流多漂亮都不能進場：通道還沒成形
        let mut s = breakout();
        let bars = with_taker_ratios(
            &highs_lows_closes(&[("10", "9", "10"), ("12", "9", "12")]),
            &[Some(1.0), Some(1.0)],
        );
        assert_eq!(signals(&mut s, &bars), "..");
        assert_eq!(s.warmup_bars(), 3);
    }

    #[test]
    fn a_bar_without_order_flow_produces_no_new_signal_and_keeps_the_position() {
        // 先用好資料進場，再餵一根「來源沒有訂單流」的 K 線：維持做多，
        // 不因為缺資料就被動出場，也不 panic。
        let mut s = breakout();
        let bars = with_taker_ratios(
            &highs_lows_closes(&[
                ("10", "9", "10"),
                ("10", "9", "10"),
                ("12", "9", "12"), // 突破 + 佔比 0.75 → 進場
                ("12", "8", "8"),  // 破底，但這根沒有訂單流 → 忽略，續抱
            ]),
            &[Some(0.5), Some(0.5), Some(0.75), None],
        );
        assert_eq!(bars[3].order_flow, None);
        assert_eq!(signals(&mut s, &bars), "..LL");
    }

    #[test]
    fn a_bar_without_order_flow_cannot_enter_either() {
        // 反向確認：缺訂單流時「維持原部位」是雙向的，空手也不會變成做多
        let mut s = breakout();
        let bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), None, Some(0.25)],
        );
        assert_eq!(signals(&mut s, &bars), "....");
    }

    #[test]
    fn a_zero_volume_bar_is_undecidable_not_all_selling() {
        // 成交量 0 時佔比的分母是 0。把它當成 0（＝全是賣壓）會誤觸出場，
        // 所以當成「算不出來」→ 維持原部位。
        let mut s = breakout();
        let mut bars = with_taker_ratios(
            &breakout_then_breakdown(),
            &[Some(0.5), Some(0.5), Some(0.75), Some(0.0)],
        );
        bars[3].volume = 0.0;
        bars[3].order_flow = Some(OrderFlow {
            trades: 0,
            taker_buy_volume: 0.0,
        });
        assert_eq!(bars[3].validate(), Ok(()), "沒成交的 K 線本身是合理的");
        assert_eq!(signals(&mut s, &bars), "..LL");
    }

    #[test]
    fn equalling_the_old_high_is_not_a_breakout() {
        // 價格條件仍然是嚴格大於：佔比 1.0（全是主動買盤）也不能把「平高」變成突破
        let mut s = breakout();
        let bars = with_taker_ratios(
            &highs_lows_closes(&[("10", "9", "10"), ("10", "9", "10"), ("10", "9", "10")]),
            &[Some(1.0), Some(1.0), Some(1.0)],
        );
        assert_eq!(signals(&mut s, &bars), "...");
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        // extreme_bars 的 order_flow 是 None，所以同時也是「缺訂單流不會炸」的檢查
        let mut s = breakout();
        let _ = signals(&mut s, &extreme_bars());
    }
}
