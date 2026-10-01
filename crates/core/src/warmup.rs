//! 暖機回放：讓策略在接即時行情之前，先看過一段歷史 K 線。
//!
//! # 為什麼需要這個
//!
//! 回測是從一段歷史資料的第一根跑到最後一根，策略在統計績效的那一段之前
//! 已經看過前面的暖機期。模擬交易與測試網交易接的是「現在開始」的即時行情，
//! 策略物件剛建立時完全沒有歷史——[`Strategy::warmup_bars`] 宣告的根數形同虛設。
//!
//! 代價不只是「慢幾十分鐘才出第一個訊號」。像 RSI 這種 Wilder 平滑指標
//! （見 `strategies/rsi.rs` 的說明）理論上記得所有歷史，**從不同的起點開始餵
//! 會收斂到不同的值，也就是算出不同的訊號**。回測假設策略在有完整歷史時做判斷；
//! 冷啟動的實盤／模擬違反這個假設，兩邊的訊號會不一樣。
//!
//! # 這個模組的角色：只有「餵」，沒有「拿」
//!
//! 這裡只處理「把一段已經驗證過的歷史 K 線依序餵給策略」。歷史 K 線**從哪裡來**
//! （REST、本機檔案）、**抓不到要怎麼辦**，都是呼叫端的事，刻意不放進來：
//!
//! - [`replay`](WarmupBars::replay) 的參數只有 `&mut dyn Strategy`，沒有引擎、
//!   沒有下單窗口、沒有設定。**回放期間不可能送單，也不可能動到帳本**——
//!   這不是靠紀律，是型別上就拿不到那些東西。
//! - 回放產生的 [`TargetPosition`](crate::TargetPosition) 一律丟棄。暖機是讓
//!   指標內部狀態收斂，不是要在這段期間交易。
//!
//! # 用起來像這樣
//!
//! ```
//! use at_core::{warmup_fetch_count, Interval, SmaCross, Strategy, WarmupBars};
//!
//! let mut strategy = SmaCross::new(10, 30).unwrap();
//! // 要抓幾根：策略宣告的需求 × 安全係數。
//! assert_eq!(strategy.warmup_bars(), 30);
//! assert_eq!(warmup_fetch_count(&strategy), 150);
//!
//! // 呼叫端去把這麼多根已收盤的歷史 K 線抓回來（這裡用空的示意）。
//! let warmup = WarmupBars::new(Vec::new(), Interval::M1).unwrap();
//! let replayed_through = warmup.replay(&mut strategy);
//! assert_eq!(replayed_through, None); // 沒有暖機資料：維持冷啟動
//! ```

use crate::bar::{find_gaps, Bar, Interval, SeriesError};
use crate::strategy::Strategy;

/// 暖機回放的安全係數：實際要回放的根數 = `warmup_bars() ×` 這個係數。
///
/// # 為什麼是 5，不是架構文件建議的 2
///
/// [`Strategy::warmup_bars`] 回報的是「指標**算得出數值**的最低根數」，不是
/// 「指標的值**已經收斂**的根數」。對 Wilder 平滑（RSI、ATR，平滑係數
/// `α = 1/period`）來說，餵了 `n` 根之後，最初的種子值還留著
/// `(1 − 1/period)^n` 的權重：
///
/// | 回放根數 | 種子殘留權重 |
/// |---|---|
/// | `1 × period` | `e⁻¹` ≈ 37% |
/// | `2 × period` | `e⁻²` ≈ 14% |
/// | `5 × period` | `e⁻⁵` ≈ 0.7% |
///
/// ×2 還留著一成多的起點影響，等於「回放過了，但訊號還是跟回測不一樣」，
/// 剛好是這整件事要解決的問題。×5 把它壓到 1% 以下。
///
/// 成本上兩者沒有差別：內建策略最多宣告 50 根（均線交叉），×5 = 250 根，
/// Binance 的 `/api/v3/klines` 一次就能拿 1000 根，同樣是一個請求。
/// 對 SMA、通道這類「看固定視窗」的指標，多餵的幾根不影響結果，只多花頻寬。
///
/// 需要調的時候改這一個常數：它同時決定「抓幾根」與文件裡的承諾。
pub const WARMUP_SAFETY_FACTOR: usize = 5;

/// 要替這個策略抓幾根歷史 K 線來暖機。
///
/// `warmup_bars()` 回 0 的策略（例如 `AlwaysLong`）回 0：不需要暖機，
/// 呼叫端也就不必為它發任何網路請求。
pub fn warmup_fetch_count(strategy: &dyn Strategy) -> usize {
    strategy.warmup_bars().saturating_mul(WARMUP_SAFETY_FACTOR)
}

/// 一段**已經驗證過**可以安全餵給策略的歷史 K 線。
///
/// 存在的理由是把「資料有沒有問題」這件事釘在一個地方：K 線是從網路抓回來的
/// 外部輸入，而策略的契約是「按開盤時間遞增、每根只餵一次」
/// （見 [`Strategy::on_bar`]）。順序錯了還硬餵進去，指標的狀態就壞了，
/// 而且壞得看不出來——後面每一根訊號都是錯的，卻不會有任何錯誤訊息。
///
/// 所以驗證在 [`new`](Self::new) 一次做完：拿到 `WarmupBars` 的人不必再檢查，
/// 而 [`replay`](Self::replay) 也就不會失敗。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WarmupBars(Vec<Bar>);

impl WarmupBars {
    /// 不暖機（冷啟動）。
    ///
    /// 這是**明確寫出來的選擇**，不是忘記傳參數：策略會從第一根即時 K 線開始
    /// 看，指標在收斂之前照契約回傳空手。
    pub const fn none() -> WarmupBars {
        WarmupBars(Vec::new())
    }

    /// 驗證一段歷史 K 線可以拿來暖機。
    ///
    /// 沿用 [`find_gaps`] 做檢查（沒對齊週期、時間重複、時間倒退都回錯誤），
    /// 但**中間缺幾根不算錯誤**：交易所維護或冷門交易對真的會缺 K 線，為此
    /// 拒絕啟動整個 session 太過，而少幾根的暖機仍然遠勝於完全不暖機。
    /// 這和 [`find_gaps`] 自己的取捨一致。
    ///
    /// 空陣列是合法的，等於 [`none`](Self::none)。
    pub fn new(bars: Vec<Bar>, interval: Interval) -> Result<WarmupBars, SeriesError> {
        find_gaps(&bars, interval)?;
        Ok(WarmupBars(bars))
    }

    /// 回放的根數。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 這段資料夠不夠讓策略的指標**算得出數值**。
    ///
    /// 門檻是 [`Strategy::warmup_bars`] 宣告的最低根數，不是
    /// [`warmup_fetch_count`] 想要的根數：抓不到完整的安全餘裕（新上市的
    /// 交易對只有幾十根歷史）是「餘裕變薄」，指標還是算得出來、訊號還是有意義；
    /// 連宣告的最低根數都湊不到才是「訊號根本沒意義」。
    ///
    /// 呼叫端用它決定要不要擋下整個 session：差別很大，所以判斷寫在這裡一次，
    /// 不讓每個呼叫端各自抓一個數字來比。
    pub fn covers(&self, strategy: &dyn Strategy) -> bool {
        self.len() >= strategy.warmup_bars()
    }

    /// 依序餵給策略，**丟棄每一根產生的目標部位**，回傳最後一根的開盤時間。
    ///
    /// 回傳值就是呼叫端要用的分水嶺：即時行情裡開盤時間**小於或等於**它的
    /// K 線都已經算在策略的狀態裡了，再餵一次會違反「每根只餵一次」的契約
    /// （而且記帳引擎會因為時間沒有遞增而直接報錯）。沒有暖機資料時回 `None`。
    ///
    /// 這個函式拿不到引擎、帳本、下單窗口，所以**回放期間不可能有任何送單或
    /// 記帳**；它唯一的副作用是策略自己的內部狀態。
    pub fn replay(&self, strategy: &mut dyn Strategy) -> Option<i64> {
        for bar in &self.0 {
            // 刻意丟棄：暖機是讓指標收斂，不是交易。
            let _ = strategy.on_bar(bar);
        }
        self.0.last().map(|bar| bar.open_time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::Fixed;
    use crate::strategies::SmaCross;
    use crate::strategy::TargetPosition;

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;
    const MINUTE_MS: i64 = 60_000;

    fn bar(index: i64, close: &str) -> Bar {
        let close: Fixed = close.parse().unwrap();
        Bar {
            open_time: T0 + index * MINUTE_MS,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    /// 記下自己被餵過哪幾根，並且每一根都要求滿倉做多——如果有人沒把回放的
    /// 輸出丟掉，這個要求就會跑到帳本或交易所去。
    #[derive(Default)]
    struct Recorder {
        seen: Vec<i64>,
    }

    impl Strategy for Recorder {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.seen.push(bar.open_time);
            TargetPosition::FULL_LONG
        }
    }

    #[test]
    fn fetch_count_is_the_declared_need_times_the_safety_factor() {
        let strategy = SmaCross::new(10, 30).unwrap();
        assert_eq!(strategy.warmup_bars(), 30);
        assert_eq!(warmup_fetch_count(&strategy), 30 * WARMUP_SAFETY_FACTOR);
    }

    #[test]
    fn a_strategy_that_needs_no_warmup_needs_no_bars() {
        struct NoWarmup;
        impl Strategy for NoWarmup {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                TargetPosition::FLAT
            }
        }
        assert_eq!(
            warmup_fetch_count(&NoWarmup),
            0,
            "不需要暖機的策略不該讓呼叫端發任何網路請求"
        );
    }

    #[test]
    fn the_safety_factor_leaves_wilder_smoothing_under_one_percent() {
        // 這個常數的理由是算得出來的，不是喜好：種子殘留權重
        // (1 − 1/period)^(period × 係數) 必須小於 1%，否則「回放過了但訊號
        // 還是跟回測不一樣」。拿 RSI 常用的 14 週期驗一次。
        let period = 14.0_f64;
        let residual = (1.0 - 1.0 / period).powf(period * WARMUP_SAFETY_FACTOR as f64);
        assert!(
            residual < 0.01,
            "係數 {WARMUP_SAFETY_FACTOR} 的種子殘留權重 {residual} 太高，Wilder 平滑還沒收斂"
        );
    }

    #[test]
    fn covers_is_measured_against_the_declared_minimum_not_the_safety_margin() {
        let strategy = SmaCross::new(2, 3).unwrap();
        assert_eq!(strategy.warmup_bars(), 3);
        assert_eq!(warmup_fetch_count(&strategy), 15);

        let bars_for = |n: i64| {
            WarmupBars::new((0..n).map(|i| bar(i, "100")).collect(), Interval::M1).unwrap()
        };
        assert!(
            !bars_for(2).covers(&strategy),
            "連宣告的 3 根都湊不到：指標算不出數值，訊號沒有意義"
        );
        assert!(
            bars_for(3).covers(&strategy),
            "剛好湊到宣告的根數：餘裕變薄，但訊號有意義，不該擋下整個 session"
        );
        assert!(bars_for(15).covers(&strategy));
    }

    #[test]
    fn no_warmup_covers_a_strategy_that_needs_none() {
        struct NoWarmup;
        impl Strategy for NoWarmup {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                TargetPosition::FLAT
            }
        }
        assert!(
            WarmupBars::none().covers(&NoWarmup),
            "不需要暖機的策略不該因為沒有暖機資料而不能啟動"
        );
    }

    #[test]
    fn replay_feeds_every_bar_in_order_and_reports_the_last_open_time() {
        let bars = vec![bar(0, "100"), bar(1, "101"), bar(2, "102")];
        let warmup = WarmupBars::new(bars, Interval::M1).expect("時間遞增又對齊，應該合法");
        assert_eq!(warmup.len(), 3);

        let mut strategy = Recorder::default();
        let through = warmup.replay(&mut strategy);

        assert_eq!(
            strategy.seen,
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS],
            "回放要按時間順序把每一根都餵一次"
        );
        assert_eq!(
            through,
            Some(T0 + 2 * MINUTE_MS),
            "回傳值是最後一根的開盤時間：即時行情要從它之後才算新的"
        );
    }

    #[test]
    fn replaying_nothing_leaves_the_strategy_untouched() {
        let mut strategy = Recorder::default();
        assert_eq!(WarmupBars::none().replay(&mut strategy), None);
        assert!(WarmupBars::none().is_empty());
        assert!(
            strategy.seen.is_empty(),
            "沒有暖機資料時不該假造任何一根 K 線"
        );
    }

    #[test]
    fn an_empty_vec_is_the_same_as_no_warmup() {
        let warmup = WarmupBars::new(Vec::new(), Interval::M1).expect("空陣列合法");
        assert_eq!(warmup, WarmupBars::none());
    }

    #[test]
    fn out_of_order_bars_are_rejected_before_they_reach_the_strategy() {
        let bars = vec![bar(2, "100"), bar(1, "101")];
        assert_eq!(
            WarmupBars::new(bars, Interval::M1),
            Err(SeriesError::OutOfOrder {
                index: 1,
                open_time: T0 + MINUTE_MS,
            }),
            "時間倒退的資料餵進去會讓指標狀態壞掉，而且壞得沒有任何錯誤訊息"
        );
    }

    #[test]
    fn duplicate_bars_are_rejected() {
        let bars = vec![bar(1, "100"), bar(1, "101")];
        assert_eq!(
            WarmupBars::new(bars, Interval::M1),
            Err(SeriesError::Duplicate {
                index: 1,
                open_time: T0 + MINUTE_MS,
            }),
            "同一根餵兩次違反 Strategy::on_bar 的契約"
        );
    }

    #[test]
    fn bars_not_aligned_to_the_interval_are_rejected() {
        let mut odd = bar(0, "100");
        odd.open_time += 1;
        assert!(matches!(
            WarmupBars::new(vec![odd], Interval::M1),
            Err(SeriesError::Misaligned { index: 0, .. })
        ));
    }

    #[test]
    fn a_hole_in_the_middle_is_accepted() {
        // 交易所維護、冷門交易對真的會缺 K 線。少幾根的暖機仍然遠勝於
        // 完全不暖機，為此拒絕啟動 session 太過。
        let bars = vec![bar(0, "100"), bar(5, "101")];
        let warmup = WarmupBars::new(bars, Interval::M1).expect("中間缺幾根不算錯誤");
        let mut strategy = Recorder::default();
        assert_eq!(warmup.replay(&mut strategy), Some(T0 + 5 * MINUTE_MS));
        assert_eq!(strategy.seen, vec![T0, T0 + 5 * MINUTE_MS]);
    }
}
