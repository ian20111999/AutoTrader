//! 布林通道：跌破下軌買進、回到中軌賣出（均值回歸）。

use super::{non_zero, StrategyParamError, Window};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// 布林通道策略（均值回歸）。
///
/// 三條線都用收盤價算：中軌是 N 根的簡單移動平均，上下軌是中軌 ± 標準差倍數
/// × 母體標準差。
///
/// - 收盤 ≤ 下軌 → 滿倉做多（假設價格會拉回平均）
/// - 收盤 ≥ 中軌 → 空手（回歸完成就獲利出場）
/// - 兩者之間 → **維持上一根的部位**
///
/// 「維持上一根」是這個策略必須記狀態的原因：進場條件（碰下軌）只成立一瞬間，
/// 若不記著，下一根價格離開下軌就會立刻平倉，等於完全沒有參與均值回歸那一段。
///
/// 規格裡的「觸及上軌也空手」不必另外寫：倍數大於 0 時上軌一定高於中軌，
/// 所以碰到上軌的收盤價早就滿足「≥ 中軌」了。
///
/// 標準差用**母體**定義（除以 N，不是 N−1），和 TradingView、Binance 的
/// 布林通道一致，這樣對照驗證（2.8）時數字才會對得上。
#[derive(Debug)]
pub struct Bollinger {
    window: Window,
    multiplier: Fixed,
    holding: bool,
}

impl Bollinger {
    /// 預設週期（根）。
    pub const DEFAULT_PERIOD: usize = 20;
    /// 預設標準差倍數（2 倍）。
    pub const DEFAULT_MULTIPLIER: Fixed = Fixed::from_raw(2 * Fixed::SCALE);

    /// `period` 是中軌的平均根數，`multiplier` 是上下軌離中軌幾個標準差。
    pub fn new(period: usize, multiplier: Fixed) -> Result<Bollinger, StrategyParamError> {
        let period = non_zero(period)?;
        if multiplier <= Fixed::ZERO {
            return Err(StrategyParamError::NonPositiveMultiplier);
        }
        Ok(Bollinger {
            window: Window::new(period),
            multiplier,
            holding: false,
        })
    }

    /// 中軌（N 根收盤價的 SMA）。暖機不足時回傳 `None`。
    pub fn middle(&self) -> Option<Fixed> {
        self.window.mean()
    }

    /// 上軌。暖機不足或算式溢位時回傳 `None`。
    pub fn upper(&self) -> Option<Fixed> {
        self.middle()?.checked_add(self.band_width()?)
    }

    /// 下軌。暖機不足或算式溢位時回傳 `None`。
    pub fn lower(&self) -> Option<Fixed> {
        self.middle()?.checked_sub(self.band_width()?)
    }

    /// 中軌到單邊軌道的距離（倍數 × 標準差）。
    fn band_width(&self) -> Option<Fixed> {
        self.window.stddev()?.checked_mul(self.multiplier)
    }
}

impl Default for Bollinger {
    /// 20 根 / 2 倍標準差（教科書上的經典設定）。
    fn default() -> Bollinger {
        // 兩個常數寫死且合法，不是外部輸入
        Bollinger::new(Bollinger::DEFAULT_PERIOD, Bollinger::DEFAULT_MULTIPLIER)
            .expect("內建預設參數必須合法")
    }
}

impl Strategy for Bollinger {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        self.window.push(bar.close);
        let (Some(middle), Some(lower)) = (self.middle(), self.lower()) else {
            // 暖機不足或算不出通道：空手，也不要留著上一根的部位
            self.holding = false;
            return TargetPosition::FLAT;
        };
        // 出場條件先判斷：標準差為 0 時三條線重疊，收盤同時「≤ 下軌」與「≥ 中軌」，
        // 這時正確的答案是空手，不是憑零波動開一個倉。
        // 標準差 > 0 時下軌一定低於中軌，兩個條件互斥，順序不影響結果。
        if bar.close >= middle {
            self.holding = false;
        } else if bar.close <= lower {
            self.holding = true;
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    fn warmup_bars(&self) -> usize {
        self.window.cap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{closes, extreme_bars, fx, last_signal, signals};

    #[test]
    fn rejects_bad_params() {
        assert_eq!(
            Bollinger::new(0, fx("2")).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            Bollinger::new(20, Fixed::ZERO).unwrap_err(),
            StrategyParamError::NonPositiveMultiplier
        );
        assert_eq!(
            Bollinger::new(20, fx("-1.5")).unwrap_err(),
            StrategyParamError::NonPositiveMultiplier
        );
        assert!(Bollinger::new(1, fx("0.00000001")).is_ok());
    }

    #[test]
    fn default_is_twenty_bars_and_two_sigma() {
        let s = Bollinger::default();
        assert_eq!(s.warmup_bars(), 20);
        assert_eq!(s.multiplier, fx("2"));
    }

    #[test]
    fn bands_match_hand_calculation() {
        // 手算 10、30、30、10：平均 20，離差 ±10，平方和 400，
        // 變異數 400/4 = 100，標準差 10
        let mut s = Bollinger::new(4, fx("1.5")).unwrap();
        last_signal(&mut s, &closes(&["10", "30", "30", "10"]));
        assert_eq!(s.middle(), Some(fx("20")));
        // 1.5 倍標準差 = 15 → 下軌 5、上軌 35
        assert_eq!(s.lower(), Some(fx("5")));
        assert_eq!(s.upper(), Some(fx("35")));
    }

    #[test]
    fn bands_are_none_before_warmup() {
        let mut s = Bollinger::new(4, fx("2")).unwrap();
        assert_eq!(s.warmup_bars(), 4);
        assert_eq!(s.middle(), None);
        last_signal(&mut s, &closes(&["10", "30", "30"]));
        assert_eq!(s.middle(), None);
        assert_eq!(s.lower(), None);
        assert_eq!(s.upper(), None);
    }

    #[test]
    fn stays_flat_until_warmed_up_even_when_price_collapses() {
        // 前 3 根還沒有通道，就算價格腰斬也不能進場
        let mut s = Bollinger::new(4, fx("2")).unwrap();
        assert_eq!(signals(&mut s, &closes(&["100", "80", "40"])), "...");
    }

    #[test]
    fn buys_the_lower_band_holds_then_exits_at_the_middle() {
        // 週期 4、1 倍標準差，手算每一根：
        //   第 4 根：視窗 10/30/30/10 → 中軌 20、σ 10、下軌 10，收盤 10 ≤ 10 → 進場
        //   第 5 根：視窗 30/30/10/12 → 中軌 20.5，收盤 12 在下軌與中軌之間 → 續抱
        //   第 6 根：視窗 30/10/12/25 → 中軌 19.25，收盤 25 ≥ 19.25 → 出場
        let mut s = Bollinger::new(4, fx("1")).unwrap();
        let bars = closes(&["10", "30", "30", "10", "12", "25"]);
        assert_eq!(signals(&mut s, &bars), "...LL.");
        assert_eq!(s.middle(), Some(fx("19.25")));
    }

    #[test]
    fn touching_the_upper_band_is_already_above_the_middle() {
        // 上軌一定高於中軌，所以「碰上軌」不需要另外寫一條出場規則
        let mut s = Bollinger::new(4, fx("1")).unwrap();
        last_signal(&mut s, &closes(&["10", "30", "30", "10"]));
        let middle = s.middle().unwrap();
        assert!(s.upper().unwrap() > middle);
        assert!(s.lower().unwrap() < middle);
    }

    #[test]
    fn a_flat_market_has_no_band_width_and_stays_flat() {
        // 價格完全不動：σ = 0，三條線重疊，收盤同時「≤ 下軌」也「≥ 中軌」。
        // 出場條件先判斷，所以結果是空手——不會憑零波動開倉。
        let mut s = Bollinger::new(3, fx("2")).unwrap();
        let bars = closes(&["50", "50", "50", "50"]);
        assert_eq!(signals(&mut s, &bars), "....");
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let mut s = Bollinger::new(4, fx("2")).unwrap();
        let _ = signals(&mut s, &extreme_bars());
    }
}
