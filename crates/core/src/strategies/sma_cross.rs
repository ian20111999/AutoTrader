//! 均線交叉：快線在慢線之上就做多，回到之下就空手。

use super::{non_zero, StrategyParamError, Window};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// 均線交叉策略（趨勢跟隨）。
///
/// 兩條簡單移動平均（SMA）都用收盤價算：
///
/// - 快線 > 慢線 → 滿倉做多
/// - 其他情況（含兩線相等、暖機不足）→ 空手
///
/// ## 為什麼是「比大小」而不是偵測交叉事件
///
/// 策略回傳的是**目標部位**不是訂單，所以「偵測到向上交叉就買、向下交叉就賣」
/// 和「快線在上面時目標是滿倉、在下面時目標是空手」產生的部位序列完全一樣，
/// 但後者不必記住上一根的兩線關係，也不會因為第一根還沒有前一根而漏掉訊號。
///
/// ```
/// use at_core::strategies::SmaCross;
/// use at_core::Strategy;
///
/// let strategy = SmaCross::new(10, 50).unwrap();
/// assert_eq!(strategy.warmup_bars(), 50);
/// ```
#[derive(Debug)]
pub struct SmaCross {
    fast: Window,
    slow: Window,
}

impl SmaCross {
    /// 預設快線週期（根）。
    pub const DEFAULT_FAST: usize = 10;
    /// 預設慢線週期（根）。
    pub const DEFAULT_SLOW: usize = 50;

    /// 快線與慢線的週期都以「幾根 K 線」計。
    ///
    /// 快線必須短於慢線，否則「快線在上」就不再代表短期動能轉強。
    pub fn new(fast_period: usize, slow_period: usize) -> Result<SmaCross, StrategyParamError> {
        let fast_period = non_zero(fast_period)?;
        let slow_period = non_zero(slow_period)?;
        if fast_period >= slow_period {
            return Err(StrategyParamError::FastNotBelowSlow);
        }
        Ok(SmaCross {
            fast: Window::new(fast_period),
            slow: Window::new(slow_period),
        })
    }

    /// 目前的快線值。暖機不足時回傳 `None`。
    pub fn fast_sma(&self) -> Option<Fixed> {
        self.fast.mean()
    }

    /// 目前的慢線值。暖機不足時回傳 `None`。
    pub fn slow_sma(&self) -> Option<Fixed> {
        self.slow.mean()
    }
}

impl Default for SmaCross {
    /// 10 / 50，設計稿的範例參數。
    fn default() -> SmaCross {
        // 兩個常數寫死且合法，不是外部輸入
        SmaCross::new(SmaCross::DEFAULT_FAST, SmaCross::DEFAULT_SLOW).expect("內建預設參數必須合法")
    }
}

impl Strategy for SmaCross {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        self.fast.push(bar.close);
        self.slow.push(bar.close);
        match (self.fast.mean(), self.slow.mean()) {
            (Some(fast), Some(slow)) if fast > slow => TargetPosition::FULL_LONG,
            _ => TargetPosition::FLAT,
        }
    }

    /// 慢線的週期：慢線滿了，兩條線才都有值。
    fn warmup_bars(&self) -> usize {
        self.slow.cap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{closes, extreme_bars, fx, last_signal, signals};

    #[test]
    fn rejects_bad_periods() {
        assert_eq!(
            SmaCross::new(0, 10).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            SmaCross::new(10, 0).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            SmaCross::new(10, 10).unwrap_err(),
            StrategyParamError::FastNotBelowSlow
        );
        assert_eq!(
            SmaCross::new(50, 10).unwrap_err(),
            StrategyParamError::FastNotBelowSlow
        );
        assert!(SmaCross::new(1, 2).is_ok());
    }

    #[test]
    fn default_is_ten_over_fifty() {
        let s = SmaCross::default();
        assert_eq!(s.fast.cap(), 10);
        assert_eq!(s.slow.cap(), 50);
        assert_eq!(s.warmup_bars(), 50);
    }

    #[test]
    fn sma_values_match_hand_calculation() {
        let mut s = SmaCross::new(2, 3).unwrap();
        // 餵 10、20、30：快線 (20+30)/2 = 25，慢線 (10+20+30)/3 = 20
        last_signal(&mut s, &closes(&["10", "20", "30"]));
        assert_eq!(s.fast_sma(), Some(fx("25")));
        assert_eq!(s.slow_sma(), Some(fx("20")));
    }

    #[test]
    fn indicators_are_none_before_warmup() {
        let mut s = SmaCross::new(2, 3).unwrap();
        assert_eq!(s.fast_sma(), None);
        assert_eq!(s.slow_sma(), None);
        // 只餵 2 根：快線已經有值，慢線還沒有
        last_signal(&mut s, &closes(&["10", "20"]));
        assert_eq!(s.fast_sma(), Some(fx("15")));
        assert_eq!(s.slow_sma(), None);
    }

    #[test]
    fn stays_flat_until_the_slow_line_is_warmed_up() {
        let mut s = SmaCross::new(2, 4).unwrap();
        assert_eq!(s.warmup_bars(), 4);
        // 前 3 根慢線還沒有值，即使價格一路上漲也必須空手
        assert_eq!(signals(&mut s, &closes(&["10", "20", "30"])), "...");
    }

    #[test]
    fn goes_long_when_the_fast_line_crosses_above() {
        // 先跌再漲：10、9、8、7、6 之後拉到 12
        //   第 4 根：快線 (8+7)/2 = 7.5，慢線 (10+9+8+7)/4 = 8.5 → 快線在下，空手
        //   第 5 根：快線 (7+6)/2 = 6.5，慢線 (9+8+7+6)/4 = 7.5 → 還在下面，空手
        //   第 6 根：快線 (6+12)/2 = 9，慢線 (8+7+6+12)/4 = 8.25 → 交叉向上，做多
        let mut s = SmaCross::new(2, 4).unwrap();
        let bars = closes(&["10", "9", "8", "7", "6", "12"]);
        assert_eq!(signals(&mut s, &bars), ".....L");
        assert_eq!(s.fast_sma(), Some(fx("9")));
        assert_eq!(s.slow_sma(), Some(fx("8.25")));
    }

    #[test]
    fn goes_flat_again_when_the_fast_line_falls_back() {
        // 漲完立刻崩：快線比慢線先掉下來
        //   第 4 根：快線 (12+13)/2 = 12.5，慢線 46/4 = 11.5 → 做多
        //   第 5 根：快線 (13+14)/2 = 13.5，慢線 50/4 = 12.5 → 續抱
        //   第 6 根：快線 (14+9)/2 = 11.5，慢線 48/4 = 12   → 快線跌破，空手
        let mut s = SmaCross::new(2, 4).unwrap();
        let bars = closes(&["10", "11", "12", "13", "14", "9", "5"]);
        assert_eq!(signals(&mut s, &bars), "...LL..");
    }

    #[test]
    fn equal_lines_mean_flat_not_long() {
        // 價格完全不動時兩條線相等，寧可空手也不要開一個沒有理由的倉
        let mut s = SmaCross::new(2, 4).unwrap();
        assert_eq!(
            signals(&mut s, &closes(&["100", "100", "100", "100", "100"])),
            "....."
        );
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let mut s = SmaCross::new(2, 4).unwrap();
        let _ = signals(&mut s, &extreme_bars());
    }
}
