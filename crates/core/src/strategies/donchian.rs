//! 唐奇安突破：收盤突破前 N 根最高價買進、跌破前 M 根最低價出場。

use super::{non_zero, StrategyParamError, Window};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// 唐奇安通道突破策略（趨勢跟隨）。
///
/// - 收盤 > 前 `entry_period` 根的**最高價** → 滿倉做多
/// - 收盤 < 前 `exit_period` 根的**最低價** → 空手
/// - 兩者之間 → 維持上一根的部位（趨勢策略就是要抱得住）
///
/// ## 三個刻意的選擇
///
/// 1. **通道用「前幾根」算，不含當下這根。** 若把當下這根的最高價也算進通道，
///    收盤價永遠不可能高過含自己的最高價，訊號會永遠不出現。所以 `on_bar` 是
///    先用現有視窗判斷，**判斷完才把這根放進視窗**。
/// 2. **通道用最高／最低價，突破用收盤價。** 用盤中最高價判斷突破的話，
///    回測會假設一根 K 線內就成交，但引擎的規則是訊號在收盤產生、下一根開盤
///    成交（2.3），用收盤價判斷才對得上實際能成交的時點。
/// 3. **進出場用不同週期。** 經典的海龜系統就是「突破 N 根新高進場、跌破較短的
///    M 根新低出場」，出場比進場敏感，才不會把賺到的趨勢全部還回去。
///
/// 跌破下軌是**出場**而不是反手做空：現貨沒有做空，等 2.5 合約做完再決定。
#[derive(Debug)]
pub struct Donchian {
    highs: Window,
    lows: Window,
    holding: bool,
}

impl Donchian {
    /// 預設進場週期（根）。
    pub const DEFAULT_ENTRY: usize = 20;
    /// 預設出場週期（根）。
    pub const DEFAULT_EXIT: usize = 10;

    /// `entry_period` 是進場要突破的最高價視窗，`exit_period` 是出場要跌破的
    /// 最低價視窗。兩個週期可以自由設定，沒有「進場一定要比出場長」的限制——
    /// 反過來設定只是變成另一種（更敏感的）策略，不是錯誤。
    pub fn new(entry_period: usize, exit_period: usize) -> Result<Donchian, StrategyParamError> {
        Ok(Donchian {
            highs: Window::new(non_zero(entry_period)?),
            lows: Window::new(non_zero(exit_period)?),
            holding: false,
        })
    }

    /// 下一根 K 線的收盤價要**超過**這個價格才算突破（目前視窗內的最高價）。
    /// 暖機不足時回傳 `None`。
    pub fn entry_high(&self) -> Option<Fixed> {
        self.highs.highest()
    }

    /// 下一根 K 線的收盤價**跌破**這個價格就出場（目前視窗內的最低價）。
    /// 暖機不足時回傳 `None`。
    pub fn exit_low(&self) -> Option<Fixed> {
        self.lows.lowest()
    }
}

impl Default for Donchian {
    /// 20 / 10：經典海龜系統的第一套參數。
    /// 設計稿的範例畫的是 100 / 10，那是更慢、更長線的設定。
    fn default() -> Donchian {
        // 兩個常數寫死且合法，不是外部輸入
        Donchian::new(Donchian::DEFAULT_ENTRY, Donchian::DEFAULT_EXIT)
            .expect("內建預設參數必須合法")
    }
}

impl Strategy for Donchian {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        // 先取「這根之前」的通道，再把這根放進視窗——否則會拿自己跟自己比
        let channel = (self.entry_high(), self.exit_low());
        self.highs.push(bar.high);
        self.lows.push(bar.low);

        let (Some(entry_high), Some(exit_low)) = channel else {
            self.holding = false;
            return TargetPosition::FLAT;
        };
        if bar.close > entry_high {
            self.holding = true;
        } else if bar.close < exit_low {
            self.holding = false;
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    /// 較長的那個視窗要填滿，**再加 1 根**才能出第一個訊號：
    /// 通道是「前幾根」算的，所以判斷用的那根本身不算在視窗裡。
    fn warmup_bars(&self) -> usize {
        self.highs.cap().max(self.lows.cap()) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{extreme_bars, fx, highs_lows_closes, last_signal, signals};

    /// 測試用的一段行情：(最高價, 最低價, 收盤價)
    fn rising_then_breaking() -> Vec<Bar> {
        highs_lows_closes(&[
            ("10", "9", "10"),
            ("11", "9", "11"),
            ("12", "10", "12"),
            ("13", "11", "13"),
            ("14", "12", "13"),
            ("14", "9", "9"),
        ])
    }

    #[test]
    fn rejects_zero_periods() {
        assert_eq!(
            Donchian::new(0, 10).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            Donchian::new(20, 0).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert!(Donchian::new(1, 1).is_ok());
    }

    #[test]
    fn default_is_twenty_over_ten() {
        let s = Donchian::default();
        assert_eq!(s.highs.cap(), 20);
        assert_eq!(s.lows.cap(), 10);
        // 20 根填滿視窗，第 21 根才是第一根能判斷突破的 K 線
        assert_eq!(s.warmup_bars(), 21);
    }

    #[test]
    fn channel_matches_hand_calculation() {
        // 進場 3 根、出場 2 根；餵完前 4 根後，
        // 視窗裡是第 2～4 根的最高價 11/12/13 與第 3～4 根的最低價 10/11
        let mut s = Donchian::new(3, 2).unwrap();
        last_signal(&mut s, &rising_then_breaking()[..4]);
        assert_eq!(s.entry_high(), Some(fx("13")));
        assert_eq!(s.exit_low(), Some(fx("10")));
    }

    #[test]
    fn channel_is_none_before_warmup() {
        let mut s = Donchian::new(3, 2).unwrap();
        assert_eq!(s.warmup_bars(), 4);
        assert_eq!(s.entry_high(), None);
        last_signal(&mut s, &rising_then_breaking()[..2]);
        assert_eq!(s.entry_high(), None);
        assert_eq!(s.exit_low(), Some(fx("9")));
    }

    #[test]
    fn stays_flat_until_warmed_up_even_while_making_new_highs() {
        // 前 3 根每根都是新高，但通道還沒成形，不能進場
        let mut s = Donchian::new(3, 2).unwrap();
        assert_eq!(signals(&mut s, &rising_then_breaking()[..3]), "...");
    }

    #[test]
    fn breaks_out_holds_then_exits_on_a_new_low() {
        // 進場 3 根、出場 2 根，手算每一根：
        //   第 4 根：通道上緣 = max(10,11,12) = 12，收盤 13 > 12 → 進場
        //   第 5 根：上緣 = max(11,12,13) = 13，收盤 13 不大於 13；
        //           下緣 = min(10,11) = 10，收盤也沒跌破 → 續抱
        //   第 6 根：下緣 = min(11,12) = 11，收盤 9 < 11 → 出場
        let mut s = Donchian::new(3, 2).unwrap();
        assert_eq!(signals(&mut s, &rising_then_breaking()), "...LL.");
    }

    #[test]
    fn equalling_the_old_high_is_not_a_breakout() {
        // 收盤等於前 N 根最高價不算突破（要「創新高」，所以是嚴格大於）
        let mut s = Donchian::new(2, 2).unwrap();
        let bars = highs_lows_closes(&[
            ("10", "9", "10"),
            ("12", "9", "12"),
            ("12", "10", "12"),
            ("12", "10", "12"),
        ]);
        assert_eq!(signals(&mut s, &bars), "....");
    }

    #[test]
    fn a_flat_market_never_opens_a_position() {
        let mut s = Donchian::new(2, 2).unwrap();
        let bars = highs_lows_closes(&[
            ("50", "50", "50"),
            ("50", "50", "50"),
            ("50", "50", "50"),
            ("50", "50", "50"),
        ]);
        assert_eq!(signals(&mut s, &bars), "....");
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let mut s = Donchian::new(3, 2).unwrap();
        let _ = signals(&mut s, &extreme_bars());
    }
}
