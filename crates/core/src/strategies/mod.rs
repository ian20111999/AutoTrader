//! 四個內建策略，以及它們共用的零件。
//!
//! 每個策略都照 [`Strategy`](crate::strategy::Strategy) 的契約寫：
//! K 線一根一根餵進來，策略自己維護 rolling window，
//! **暖機不足或算式溢位時回傳 [`TargetPosition::FLAT`](crate::strategy::TargetPosition::FLAT)**，
//! 絕不 panic、絕不拿不完整的視窗亂猜。
//!
//! | 策略 | 型別 | 進場 | 出場 | 預設參數 |
//! |---|---|---|---|---|
//! | 均線交叉 | [`SmaCross`] | 快線在慢線之上 | 快線回到慢線之下 | 10 / 50 |
//! | 布林通道 | [`Bollinger`] | 收盤跌破下軌 | 收盤回到中軌之上 | 20 根 / 2 倍 |
//! | 唐奇安突破 | [`Donchian`] | 收盤突破前 N 根最高價 | 收盤跌破前 M 根最低價 | 20 / 10 |
//! | RSI | [`Rsi`] | RSI ≤ 進場門檻 | RSI ≥ 出場門檻 | 14 / 30 / 70 |
//!
//! 四個都只**做多或空手**（現貨用得到的範圍）。
//! 介面本身支援做空（負數比例），等 2.5 合約做完再決定要不要加反手版本。
//!
//! ## 為什麼指標也全部用 `Fixed` 算，不用 `f64`
//!
//! 指標值不是下單金額，用 `f64` 算不會直接算錯錢。但訊號會決定要不要下單，
//! 而 `f64` 的加總順序會影響最低位的結果：同一份資料在回測與實盤可能因為
//! 浮點誤差落在門檻的兩邊，變成「回測有這筆交易、實盤沒有」。
//! 所以這裡連標準差都用整數開根號（[`isqrt`]）算，同一份輸入永遠得到同一個訊號。
//!
//! 代價是除法會截尾（往零的方向）：3 根 10 / 20 / 31 的均線是 `20.33333333`，
//! 不是無限多個 3。第 8 位小數以下的差異不影響訊號判斷。

mod bollinger;
mod donchian;
mod rsi;
mod sma_cross;

pub use bollinger::Bollinger;
pub use donchian::Donchian;
pub use rsi::Rsi;
pub use sma_cross::SmaCross;

use crate::fixed::{isqrt, Fixed};
use std::collections::VecDeque;
use std::fmt;

/// 建立策略時的參數錯誤。
///
/// 參數之後會從桌面介面的輸入框來，所以在建構子就擋掉不合理的組合，
/// 而不是等到 `on_bar` 才算出奇怪的結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyParamError {
    /// 週期是 0（0 根 K 線算不出任何指標）。
    ZeroPeriod,
    /// 均線交叉的快線週期沒有短於慢線。
    FastNotBelowSlow,
    /// 標準差倍數 ≤ 0（通道會變成一條線或上下顛倒）。
    NonPositiveMultiplier,
    /// RSI 門檻不在 0～100 之間。
    ThresholdOutOfRange,
    /// RSI 的進場門檻沒有低於出場門檻。
    ThresholdsOutOfOrder,
}

impl fmt::Display for StrategyParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StrategyParamError::ZeroPeriod => write!(f, "週期必須大於 0"),
            StrategyParamError::FastNotBelowSlow => write!(f, "快線週期必須短於慢線週期"),
            StrategyParamError::NonPositiveMultiplier => write!(f, "標準差倍數必須大於 0"),
            StrategyParamError::ThresholdOutOfRange => write!(f, "RSI 門檻必須在 0 到 100 之間"),
            StrategyParamError::ThresholdsOutOfOrder => write!(f, "RSI 進場門檻必須低於出場門檻"),
        }
    }
}

impl std::error::Error for StrategyParamError {}

/// 固定長度的滑動視窗，附帶移動總和。
///
/// 四個策略的指標都是「最近 N 根的某個統計量」，差別只在統計量是平均、
/// 標準差還是最高最低，所以共用這一個容器：
///
/// - 移動總和用 `i128` 累加，推進一根是 O(1)（減掉最舊的、加上最新的），
///   不必每根重掃整個視窗。用 `i128` 是因為 N 個 `i64` 相加一定放得下，
///   於是**加總本身永遠不會溢位**，`push` 不需要處理失敗。
/// - `stddev`、`highest`、`lowest` 是 O(N) 掃視窗。N 是使用者設定的週期
///   （通常 14～200），K 線根數才是資料量，所以整場回測是 O(根數 × N)，
///   不是 O(根數²)。
///   ponytail: 單調佇列可以把 `highest`/`lowest` 壓到 O(1)，等實測發現這裡是
///   瓶頸再換——200 根的掃描目前遠比讀一根 K 線本身便宜。
///
/// 所有統計量在**視窗還沒滿**時一律回傳 `None`，策略只要把 `None` 對應到
/// 「空手」，暖機期就自動安全。
#[derive(Debug)]
pub(crate) struct Window {
    cap: usize,
    values: VecDeque<Fixed>,
    sum_raw: i128,
}

impl Window {
    /// `cap` 是視窗長度。策略的建構子已經擋掉 0，這裡再夾一次下限，
    /// 保證 `mean` 的除法不可能除以 0。
    pub(crate) fn new(cap: usize) -> Window {
        Window {
            cap: cap.max(1),
            values: VecDeque::new(),
            sum_raw: 0,
        }
    }

    /// 視窗長度，也就是策略宣告的週期。
    pub(crate) fn cap(&self) -> usize {
        self.cap
    }

    /// 放進一個新值；超過長度就把最舊的擠掉。
    pub(crate) fn push(&mut self, value: Fixed) {
        if self.values.len() == self.cap {
            if let Some(old) = self.values.pop_front() {
                self.sum_raw -= old.raw() as i128;
            }
        }
        self.sum_raw += value.raw() as i128;
        self.values.push_back(value);
    }

    pub(crate) fn is_full(&self) -> bool {
        self.values.len() >= self.cap
    }

    /// 視窗內數值的總和（內部整數單位）。視窗未滿時回傳 `None`。
    pub(crate) fn sum(&self) -> Option<i128> {
        if !self.is_full() {
            return None;
        }
        Some(self.sum_raw)
    }

    /// 簡單移動平均（SMA）。視窗未滿時回傳 `None`。除法往零的方向截尾。
    pub(crate) fn mean(&self) -> Option<Fixed> {
        let sum = self.sum()?;
        i64::try_from(sum / self.cap as i128)
            .ok()
            .map(Fixed::from_raw)
    }

    /// 母體標準差（除以 N，不是 N−1）——布林通道用的就是這個定義。
    /// 視窗未滿或平方和溢位時回傳 `None`。
    pub(crate) fn stddev(&self) -> Option<Fixed> {
        let mean = self.mean()?.raw() as i128;
        let mut sum_sq: i128 = 0;
        for value in &self.values {
            let dev = value.raw() as i128 - mean;
            sum_sq = sum_sq.checked_add(dev.checked_mul(dev)?)?;
        }
        // 離差平方是 10^16 倍，開根號後剛好回到 10^8 倍，不必再縮放
        let variance = sum_sq / self.cap as i128;
        i64::try_from(isqrt(variance as u128))
            .ok()
            .map(Fixed::from_raw)
    }

    /// 視窗內最大值。視窗未滿時回傳 `None`。
    pub(crate) fn highest(&self) -> Option<Fixed> {
        if !self.is_full() {
            return None;
        }
        self.values.iter().copied().max()
    }

    /// 視窗內最小值。視窗未滿時回傳 `None`。
    pub(crate) fn lowest(&self) -> Option<Fixed> {
        if !self.is_full() {
            return None;
        }
        self.values.iter().copied().min()
    }
}

/// 擋掉 0 週期。
fn non_zero(period: usize) -> Result<usize, StrategyParamError> {
    if period == 0 {
        return Err(StrategyParamError::ZeroPeriod);
    }
    Ok(period)
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::bar::Bar;
    use crate::fixed::Fixed;
    use crate::strategy::{Strategy, TargetPosition};

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;

    pub(crate) fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 一串 1 小時 K 線，開高低收都等於收盤價（只看收盤價的策略用這個）。
    pub(crate) fn closes(values: &[&str]) -> Vec<Bar> {
        values
            .iter()
            .enumerate()
            .map(|(i, c)| ohlc_bar(i as i64, c, c, c))
            .collect()
    }

    /// 一串自訂最高／最低／收盤價的 K 線（唐奇安要看高低價）。
    pub(crate) fn highs_lows_closes(rows: &[(&str, &str, &str)]) -> Vec<Bar> {
        rows.iter()
            .enumerate()
            .map(|(i, (h, l, c))| ohlc_bar(i as i64, h, l, c))
            .collect()
    }

    fn ohlc_bar(index: i64, high: &str, low: &str, close: &str) -> Bar {
        let bar = Bar {
            open_time: T0 + index * 3_600_000,
            open: fx(close),
            high: fx(high),
            low: fx(low),
            close: fx(close),
            volume: 1.0,
            order_flow: None,
        };
        assert_eq!(bar.validate(), Ok(()), "測試資料本身要是合理的 K 線");
        bar
    }

    /// 把 K 線餵進策略，把每根的訊號縮寫成一個字元：
    /// `.` 空手、`L` 做多、`S` 做空。這樣預期值可以一眼看完。
    pub(crate) fn signals(strategy: &mut dyn Strategy, bars: &[Bar]) -> String {
        bars.iter()
            .map(|bar| {
                let target = strategy.on_bar(bar);
                if target.is_flat() {
                    '.'
                } else if target.ratio().is_negative() {
                    'S'
                } else {
                    'L'
                }
            })
            .collect()
    }

    /// 餵完一串 K 線後，最後一根的訊號。
    pub(crate) fn last_signal(strategy: &mut dyn Strategy, bars: &[Bar]) -> TargetPosition {
        let mut last = TargetPosition::FLAT;
        for bar in bars {
            last = strategy.on_bar(bar);
        }
        last
    }

    /// 一串極端價格的 K 線，用來確認策略不會 panic（`Strategy` 契約的一部分）。
    pub(crate) fn extreme_bars() -> Vec<Bar> {
        let huge = Fixed::from_raw(i64::MAX);
        let tiny = Fixed::from_raw(1);
        (0..8)
            .map(|i| {
                let close = if i % 2 == 0 { huge } else { tiny };
                Bar {
                    open_time: T0 + i * 3_600_000,
                    open: close,
                    high: huge,
                    low: tiny,
                    close,
                    volume: 1.0,
                    order_flow: None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::fx;
    use super::*;

    fn filled(values: &[&str], cap: usize) -> Window {
        let mut w = Window::new(cap);
        for v in values {
            w.push(fx(v));
        }
        w
    }

    #[test]
    fn statistics_are_none_until_the_window_is_full() {
        let mut w = Window::new(3);
        w.push(fx("10"));
        assert!(!w.is_full());
        assert_eq!(w.mean(), None);
        assert_eq!(w.stddev(), None);
        w.push(fx("20"));
        assert_eq!(w.mean(), None);
        w.push(fx("30"));
        assert!(w.is_full());
        assert_eq!(w.mean(), Some(fx("20")));
    }

    #[test]
    fn mean_is_the_moving_average_of_the_last_cap_values() {
        // 手算：(10+20+30)/3 = 20；推進一根後 (20+30+40)/3 = 30
        let mut w = filled(&["10", "20", "30"], 3);
        assert_eq!(w.mean(), Some(fx("20")));
        w.push(fx("40"));
        assert_eq!(w.mean(), Some(fx("30")));
        assert_eq!(w.cap(), 3);
    }

    #[test]
    fn mean_truncates_instead_of_rounding() {
        // (10+20+31)/3 = 20.333…，留 8 位小數後截尾
        let w = filled(&["10", "20", "31"], 3);
        assert_eq!(w.mean(), Some(fx("20.33333333")));
    }

    #[test]
    fn stddev_is_the_population_standard_deviation() {
        // 手算：10、30、30、10 的平均是 20，離差 ±10，
        // 平方和 400，除以 4 得變異數 100，標準差 10
        let w = filled(&["10", "30", "30", "10"], 4);
        assert_eq!(w.mean(), Some(fx("20")));
        assert_eq!(w.stddev(), Some(fx("10")));
    }

    #[test]
    fn stddev_of_an_irrational_case_truncates_to_eight_decimals() {
        // 1、3、5、7 的平均是 4，平方和 20，變異數 5，標準差 √5 = 2.2360679774…
        let w = filled(&["1", "3", "5", "7"], 4);
        assert_eq!(w.stddev(), Some(fx("2.23606797")));
    }

    #[test]
    fn stddev_of_a_flat_window_is_zero() {
        let w = filled(&["100", "100", "100"], 3);
        assert_eq!(w.stddev(), Some(Fixed::ZERO));
    }

    #[test]
    fn highest_and_lowest_follow_the_window() {
        // 30 是最舊的那根，所以下一次 push 會把它擠掉
        let mut w = filled(&["30", "10", "20"], 3);
        assert_eq!(w.highest(), Some(fx("30")));
        assert_eq!(w.lowest(), Some(fx("10")));
        // 最高價離開視窗後，通道上緣要跟著下來
        w.push(fx("15"));
        assert_eq!(w.highest(), Some(fx("20")));
        assert_eq!(w.lowest(), Some(fx("10")));
    }

    #[test]
    fn zero_cap_cannot_divide_by_zero() {
        let mut w = Window::new(0);
        w.push(fx("5"));
        assert_eq!(w.cap(), 1);
        assert_eq!(w.mean(), Some(fx("5")));
    }

    #[test]
    fn param_errors_have_chinese_messages() {
        assert_eq!(non_zero(0), Err(StrategyParamError::ZeroPeriod));
        assert_eq!(non_zero(5), Ok(5));
        assert_eq!(StrategyParamError::ZeroPeriod.to_string(), "週期必須大於 0");
    }
}
