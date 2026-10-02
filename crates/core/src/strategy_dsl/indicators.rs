//! DSL 用到的指標狀態機。
//!
//! 這裡只放「餵一個值進來、問現在的指標值是多少」的小狀態機，不含任何
//! 判斷訊號的邏輯。SMA、布林通道、唐奇安、最高／最低價直接重用
//! [`crate::strategies::Window`]，所以這個檔案只需要處理平滑平均那一家：
//!
//! | 型別 | 用途 |
//! |---|---|
//! | [`Smoothed`] | 有理數係數的平滑平均。Wilder（α=1/n）與標準 EMA（α=2/(n+1)）共用 |
//! | [`RsiCore`] | RSI 的計算核心。內建 [`Rsi`](crate::strategies::Rsi) 策略用的就是這一份 |
//! | [`Macd`] | MACD 線／訊號線／柱狀體 |
//! | [`Atr`] | Wilder ATR |
//!
//! 全部用 `Fixed`（8 位小數、i64 raw）在 `i128` 中間值裡算，**除法往零的方向
//! 截尾**，理由同 [`crate::strategies`] 的模組註解：`f64` 的加總順序會讓同一份
//! 資料在回測與實盤落在門檻的兩邊。
//!
//! 所有 `push` 都不會 panic：算不出來就維持原值或回傳 `None`，由呼叫端對應成
//! 「這根空手」。

use crate::bar::Bar;
use crate::fixed::Fixed;
use std::collections::VecDeque;

/// 有理數係數（α = `num`/`den`）的平滑平均。
///
/// 兩個階段，和原本 `strategies::rsi` 裡的 `WilderAverage` 完全一樣：
///
/// 1. **種子期**：前 `period` 筆先算一個普通的算術平均。
/// 2. **之後每筆**：`新平均 = 舊平均 + (這筆 − 舊平均) × num ÷ den`。
///
/// 第 2 式的中間值一定落在「舊平均」與「這筆」之間（因為 `num ≤ den`），
/// 所以**不可能溢位**，不必在每根 K 線上處理一個永遠不會發生的錯誤。
/// 這個論證對帶正負號的輸入同樣成立——MACD 線可以是負的。
#[derive(Debug)]
pub(crate) struct Smoothed {
    /// 種子期的筆數，同時也是「指標週期」。
    period: usize,
    /// α 的分子與分母。只會是 `1/n`（Wilder）或 `2/(n+1)`（EMA），
    /// 所以 `delta × num` 最多放大兩倍，離 `i128` 的上限還很遠。
    num: i128,
    den: i128,
    seen: usize,
    seed_sum_raw: i128,
    average: Option<Fixed>,
}

impl Smoothed {
    /// Wilder 平滑：α = 1/period。RSI 的漲跌幅平均與 ATR 用這個。
    pub(crate) fn wilder(period: usize) -> Smoothed {
        let period = period.max(1);
        Smoothed::new(period, 1, period as i128)
    }

    /// 標準 EMA：α = 2/(period+1)。EMA 與 MACD 用這個。
    pub(crate) fn ema(period: usize) -> Smoothed {
        let period = period.max(1);
        Smoothed::new(period, 2, period as i128 + 1)
    }

    fn new(period: usize, num: i128, den: i128) -> Smoothed {
        Smoothed {
            period,
            num,
            den,
            seen: 0,
            seed_sum_raw: 0,
            average: None,
        }
    }

    /// 餵一筆新值。
    pub(crate) fn push(&mut self, value: Fixed) {
        if let Some(prev) = self.average {
            let prev_raw = prev.raw() as i128;
            let next_raw = prev_raw + (value.raw() as i128 - prev_raw) * self.num / self.den;
            // 結果一定在 prev 與 value 之間，轉回 i64 不可能失敗；
            // 萬一失敗就維持原值，不讓平均值退回種子模式而重複累加。
            if let Ok(raw) = i64::try_from(next_raw) {
                self.average = Some(Fixed::from_raw(raw));
            }
            return;
        }
        self.seen += 1;
        self.seed_sum_raw += value.raw() as i128;
        if self.seen >= self.period {
            self.average = i64::try_from(self.seed_sum_raw / self.period as i128)
                .ok()
                .map(Fixed::from_raw);
        }
    }

    /// 目前的平均值。種子期還沒湊滿時回傳 `None`。
    pub(crate) fn value(&self) -> Option<Fixed> {
        self.average
    }

    /// 種子期的筆數（也就是指標週期）。
    pub(crate) fn period(&self) -> usize {
        self.period
    }
}

/// RSI 的計算核心：把收盤價序列變成 0～100 的 RSI。
///
/// `RSI = 100 × 平均漲幅 ÷ (平均漲幅 + 平均跌幅)`，平均用 Wilder 平滑
/// （TradingView、ta-lib、Binance 介面上的「RSI」指的都是它）。
///
/// 內建的 [`Rsi`](crate::strategies::Rsi) 策略與 DSL 的 `rsi` 指標節點共用這一份，
/// 所以兩邊**不可能**算出不同的值——這正是這份 DSL 設計最想避免的事。
#[derive(Debug)]
pub(crate) struct RsiCore {
    avg_gain: Smoothed,
    avg_loss: Smoothed,
    prev_close: Option<Fixed>,
}

impl RsiCore {
    /// 漲跌幅都是 0 時採用的中性值（RSI 在數學上無定義）。
    pub(crate) const NEUTRAL: Fixed = Fixed::from_raw(50 * Fixed::SCALE);
    /// RSI 的上限。
    pub(crate) const FULL: Fixed = Fixed::from_raw(100 * Fixed::SCALE);

    pub(crate) fn new(period: usize) -> RsiCore {
        RsiCore {
            avg_gain: Smoothed::wilder(period),
            avg_loss: Smoothed::wilder(period),
            prev_close: None,
        }
    }

    /// 餵一根收盤價。
    ///
    /// 回傳 `false` 表示這根**算不出漲跌幅**（第一根沒有前一根可比，或相減溢位），
    /// 這時什麼都沒有餵進平均值裡——餵 0 會汙染種子，算出一條假的線。
    pub(crate) fn push_close(&mut self, close: Fixed) -> bool {
        let Some(prev_close) = self.prev_close.replace(close) else {
            return false;
        };
        let Some(change) = close.checked_sub(prev_close) else {
            return false;
        };
        if change.is_negative() {
            self.avg_gain.push(Fixed::ZERO);
            self.avg_loss.push(abs(change));
        } else {
            self.avg_gain.push(change);
            self.avg_loss.push(Fixed::ZERO);
        }
        true
    }

    /// 目前的 RSI（0～100）。暖機不足時回傳 `None`。
    pub(crate) fn value(&self) -> Option<Fixed> {
        let gain = self.avg_gain.value()?.raw() as i128;
        let loss = self.avg_loss.value()?.raw() as i128;
        let total = gain + loss;
        if total == 0 {
            return Some(RsiCore::NEUTRAL);
        }
        // gain 最多是 i64 的上限（約 9.2×10^18），乘上 10^10 仍遠小於 i128 的上限
        let raw = 100 * (Fixed::SCALE as i128) * gain / total;
        i64::try_from(raw).ok().map(Fixed::from_raw)
    }

    /// 平均漲跌幅的週期。
    pub(crate) fn period(&self) -> usize {
        self.avg_gain.period()
    }
}

/// MACD：快慢 EMA 之差、它的 EMA（訊號線）、以及兩者之差（柱狀體）。
///
/// ```text
/// macd_line = EMA(close, fast) − EMA(close, slow)
/// signal    = EMA(macd_line, signal)
/// histogram = macd_line − signal
/// ```
///
/// 關鍵的細節：**`macd_line` 還沒有值的那段期間，什麼都不餵給訊號線**。
/// 餵 0 會把訊號線的種子汙染成一堆零，算出一條不存在的線。
#[derive(Debug)]
pub(crate) struct Macd {
    fast: Smoothed,
    slow: Smoothed,
    signal: Smoothed,
    line: Option<Fixed>,
    signal_line: Option<Fixed>,
    histogram: Option<Fixed>,
}

/// MACD 的三路輸出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacdOutput {
    Line,
    Signal,
    Histogram,
}

impl Macd {
    /// 呼叫端（`compile()`）已經驗證 `fast < slow`、三者皆 ≥ 1。
    pub(crate) fn new(fast: usize, slow: usize, signal: usize) -> Macd {
        Macd {
            fast: Smoothed::ema(fast),
            slow: Smoothed::ema(slow),
            signal: Smoothed::ema(signal),
            line: None,
            signal_line: None,
            histogram: None,
        }
    }

    pub(crate) fn push(&mut self, value: Fixed) {
        self.fast.push(value);
        self.slow.push(value);
        // 慢線的週期比較長，所以 macd 線從第 slow 根才開始存在
        self.line = match (self.fast.value(), self.slow.value()) {
            (Some(fast), Some(slow)) => fast.checked_sub(slow),
            _ => None,
        };
        if let Some(line) = self.line {
            self.signal.push(line);
        }
        self.signal_line = self.signal.value();
        self.histogram = match (self.line, self.signal_line) {
            (Some(line), Some(signal)) => line.checked_sub(signal),
            _ => None,
        };
    }

    pub(crate) fn value(&self, output: MacdOutput) -> Option<Fixed> {
        match output {
            MacdOutput::Line => self.line,
            MacdOutput::Signal => self.signal_line,
            MacdOutput::Histogram => self.histogram,
        }
    }
}

/// ATR（Average True Range，Wilder）。
///
/// ```text
/// TR_t = max( high_t − low_t, |high_t − close_{t−1}|, |low_t − close_{t−1}| )
/// ATR  = Wilder 平滑(TR, period)
/// ```
///
/// **第一根沒有 `close_{t−1}`，所以沒有 TR**（和 ta-lib 一致，也和內建 `Rsi`
/// 第一根直接空手的風格一致）。所以第一個 ATR 值在第 `period + 1` 根。
#[derive(Debug)]
pub(crate) struct Atr {
    tr: Smoothed,
    prev_close: Option<Fixed>,
}

impl Atr {
    pub(crate) fn new(period: usize) -> Atr {
        Atr {
            tr: Smoothed::wilder(period),
            prev_close: None,
        }
    }

    /// ATR 吃的是整根 K 線，不是單一數列。
    pub(crate) fn push(&mut self, bar: &Bar) {
        let Some(prev_close) = self.prev_close.replace(bar.close) else {
            return;
        };
        // 三個相減理論上都不會溢位（價格都是正數），但契約要求用 checked_*；
        // 算不出來就這根不餵 TR，不要拿壞值汙染平均。
        let Some(high_low) = bar.high.checked_sub(bar.low) else {
            return;
        };
        let Some(high_close) = bar.high.checked_sub(prev_close).map(abs) else {
            return;
        };
        let Some(low_close) = bar.low.checked_sub(prev_close).map(abs) else {
            return;
        };
        self.tr.push(high_low.max(high_close).max(low_close));
    }

    pub(crate) fn value(&self) -> Option<Fixed> {
        self.tr.value()
    }
}

/// 絕對值。`Fixed::abs` 在 `i64::MIN` 會 panic，這裡飽和到 `i64::MAX`：
/// 價格都是正數所以不會走到，但 `on_bar` 絕不 panic 是 `Strategy` 的契約。
fn abs(value: Fixed) -> Fixed {
    Fixed::from_raw(value.raw().checked_abs().unwrap_or(i64::MAX))
}

/// `swing_high`／`swing_low` 的兩路輸出（ADR-004 §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwingOutput {
    /// 最近一個已確認的擺動點價位。
    Last,
    /// 上一個已確認的擺動點價位（`swing_high(last) > swing_high(previous)`
    /// 這種「結構在創更高高點」寫法要用這個）。
    Previous,
}

/// 擺動高低點（局部極值）的狀態機（ADR-004 §4）。
///
/// **語意是「最近一個已確認的擺動點價位」，不是「這一根是不是擺動點」。**
/// 一根K線是不是擺動點，天生要等看到右側 `right` 根之後才能確定，所以這個
/// 狀態機對最近 `right` 根不作任何宣稱——複用既有的 `Option::None`
/// （和 ADR-003 訂單流欄位的「這個指標在某些情況下沒有意義」同一個模式），
/// 不另外發明「待確認」狀態。
///
/// 兩側都是**嚴格**不等式（`>`／`<`，不是 `>=`/`<=`）：平盤序列永遠不會產生
/// 擺動點（安全方向，跟既有「訊號矛盾／不明確時空手」的立場一致）。
#[derive(Debug)]
pub(crate) struct PivotTracker {
    left: usize,
    right: usize,
    /// `true` = 找局部最大值（swing high），`false` = 找局部最小值（swing low）。
    higher: bool,
    /// 有界緩衝區，長度固定在 `left + right + 1`；候選點永遠是 `buf[left]`。
    buf: VecDeque<Fixed>,
    last: Option<Fixed>,
    previous: Option<Fixed>,
}

impl PivotTracker {
    pub(crate) fn new(left: usize, right: usize, higher: bool) -> PivotTracker {
        PivotTracker {
            left,
            right,
            higher,
            buf: VecDeque::with_capacity(left + right + 1),
            last: None,
            previous: None,
        }
    }

    /// 餵一根新值（`swing_high` 餵 `bar.high`，`swing_low` 餵 `bar.low`）。
    pub(crate) fn push(&mut self, value: Fixed) {
        let cap = self.left + self.right + 1;
        self.buf.push_back(value);
        if self.buf.len() > cap {
            self.buf.pop_front();
        }
        if self.buf.len() < cap {
            // 緩衝還沒滿，連第一個候選點都還看不到右側的根數
            return;
        }
        let candidate = self.buf[self.left];
        let is_pivot = self.buf.iter().enumerate().all(|(i, &v)| {
            i == self.left
                || if self.higher {
                    candidate > v
                } else {
                    candidate < v
                }
        });
        if is_pivot {
            self.previous = self.last;
            self.last = Some(candidate);
        }
    }

    /// `output: last`／`output: previous` 共用的讀取入口。
    pub(crate) fn value(&self, output: SwingOutput) -> Option<Fixed> {
        match output {
            SwingOutput::Last => self.last,
            SwingOutput::Previous => self.previous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::fx;

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;

    fn bar(index: i64, high: &str, low: &str, close: &str) -> Bar {
        Bar {
            open_time: T0 + index * 3_600_000,
            open: fx(close),
            high: fx(high),
            low: fx(low),
            close: fx(close),
            volume: 1.0,
            order_flow: None,
        }
    }

    #[test]
    fn wilder_matches_the_original_formula() {
        // 週期 4，餵 4、0、0、0 → 種子 = 1；再餵 4 → 1 + (4−1)/4 = 1.75
        let mut s = Smoothed::wilder(4);
        for v in ["4", "0", "0", "0"] {
            assert_eq!(s.value(), None, "種子期還沒湊滿就不該有值");
            s.push(fx(v));
        }
        assert_eq!(s.value(), Some(fx("1")));
        s.push(fx("4"));
        assert_eq!(s.value(), Some(fx("1.75")));
        assert_eq!(s.period(), 4);
    }

    #[test]
    fn ema_alpha_is_two_over_period_plus_one() {
        // 週期 2 → α = 2/3；種子 (10+12)/2 = 11；11 + (14−11)×2/3 = 13
        let mut s = Smoothed::ema(2);
        s.push(fx("10"));
        s.push(fx("12"));
        assert_eq!(s.value(), Some(fx("11")));
        s.push(fx("14"));
        assert_eq!(s.value(), Some(fx("13")));
    }

    #[test]
    fn smoothed_handles_negative_input() {
        // MACD 線可以是負的，平滑平均要能往負的方向走
        let mut s = Smoothed::ema(2);
        s.push(fx("-1"));
        s.push(fx("-3"));
        assert_eq!(s.value(), Some(fx("-2")));
        s.push(fx("-8"));
        // −2 + (−8 − (−2)) × 2/3 = −2 − 4 = −6
        assert_eq!(s.value(), Some(fx("-6")));
    }

    #[test]
    fn smoothed_truncates_towards_zero() {
        // 種子 (1+2)/2 = 1.5；1.5 + (0 − 1.5) × 2/3 = 0.5
        // raw 核對：(0 − 150000000) × 2 = −300000000；÷ 3 = −100000000（整除）
        let mut s = Smoothed::ema(2);
        s.push(fx("1"));
        s.push(fx("2"));
        assert_eq!(s.value(), Some(fx("1.5")));
        s.push(Fixed::ZERO);
        assert_eq!(s.value(), Some(fx("0.5")));
    }

    #[test]
    fn period_one_is_the_raw_value() {
        let mut s = Smoothed::ema(1);
        s.push(fx("10"));
        assert_eq!(s.value(), Some(fx("10")));
        s.push(fx("7"));
        assert_eq!(s.value(), Some(fx("7")));
    }

    #[test]
    fn zero_period_cannot_divide_by_zero() {
        let mut s = Smoothed::wilder(0);
        s.push(fx("5"));
        assert_eq!(s.value(), Some(fx("5")));
    }

    #[test]
    fn rsi_core_matches_the_builtin_strategy_formula() {
        // 和 strategies::rsi 的既有測試同一組資料：100 → 104 → 100 → 100 → 100
        let mut core = RsiCore::new(4);
        assert!(!core.push_close(fx("100")), "第一根沒有前一根可比");
        for v in ["104", "100", "100", "100"] {
            assert!(core.push_close(fx(v)));
        }
        assert_eq!(core.value(), Some(fx("50")));
        assert!(core.push_close(fx("104")));
        assert_eq!(core.value(), Some(fx("70")));
        assert_eq!(core.period(), 4);
    }

    #[test]
    fn rsi_core_is_neutral_when_nothing_moves() {
        let mut core = RsiCore::new(2);
        for v in ["100", "100", "100"] {
            core.push_close(fx(v));
        }
        assert_eq!(core.value(), Some(RsiCore::NEUTRAL));
    }

    /// ADR 第 5.2 節的測試向量：`fast=2, slow=3, signal=2`，收盤 10、12、14、13、15。
    /// 一根的期望值：收盤價、MACD 線、訊號線、柱狀體。
    type MacdRow = (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );

    /// 一根的期望值：最高、最低、收盤、ATR。
    type AtrRow = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    );

    #[test]
    fn macd_matches_the_adr_test_vector() {
        let mut macd = Macd::new(2, 3, 2);
        let expected: [MacdRow; 5] = [
            ("10", None, None, None),
            ("12", None, None, None),
            ("14", Some("1"), None, None),
            ("13", Some("0.5"), Some("0.75"), Some("-0.25")),
            (
                "15",
                Some("0.58333333"),
                Some("0.63888889"),
                Some("-0.05555556"),
            ),
        ];
        for (index, (close, line, signal, histogram)) in expected.iter().enumerate() {
            macd.push(fx(close));
            let got = (
                macd.value(MacdOutput::Line),
                macd.value(MacdOutput::Signal),
                macd.value(MacdOutput::Histogram),
            );
            let want = (line.map(fx), signal.map(fx), histogram.map(fx));
            assert_eq!(got, want, "第 {} 根", index + 1);
        }
    }

    #[test]
    fn macd_signal_first_value_is_at_slow_plus_signal_minus_one() {
        // slow + signal − 1 = 3 + 2 − 1 = 4，和 warmup_bars 的公式一致
        let mut macd = Macd::new(2, 3, 2);
        for (index, close) in ["10", "12", "14", "13"].iter().enumerate() {
            macd.push(fx(close));
            let has_signal = macd.value(MacdOutput::Signal).is_some();
            assert_eq!(has_signal, index + 1 == 4, "第 {} 根", index + 1);
        }
    }

    /// ADR 第 5.3 節的測試向量：`period = 3`。
    #[test]
    fn atr_matches_the_adr_test_vector() {
        let mut atr = Atr::new(3);
        let rows: [AtrRow; 5] = [
            ("10", "8", "9", None),
            ("11", "9", "10.5", None),
            ("12", "10.5", "11", None),
            ("11", "9", "9.5", Some("1.83333333")),
            ("10", "9.5", "10", Some("1.38888889")),
        ];
        for (index, (high, low, close, expected)) in rows.iter().enumerate() {
            atr.push(&bar(index as i64, high, low, close));
            assert_eq!(atr.value(), expected.map(fx), "第 {} 根", index + 1);
        }
    }

    #[test]
    fn atr_has_no_true_range_on_the_first_bar() {
        // 第一根沒有 prev_close，所以週期 1 的 ATR 要到第 2 根才有值
        let mut atr = Atr::new(1);
        atr.push(&bar(0, "10", "8", "9"));
        assert_eq!(atr.value(), None);
        atr.push(&bar(1, "11", "9", "10"));
        assert_eq!(atr.value(), Some(fx("2")));
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let huge = Fixed::from_raw(i64::MAX);
        let tiny = Fixed::from_raw(1);
        let mut atr = Atr::new(2);
        let mut macd = Macd::new(2, 3, 2);
        let mut core = RsiCore::new(2);
        for i in 0..8 {
            let close = if i % 2 == 0 { huge } else { tiny };
            let extreme = Bar {
                open_time: T0 + i * 3_600_000,
                open: close,
                high: huge,
                low: tiny,
                close,
                volume: 1.0,
                order_flow: None,
            };
            atr.push(&extreme);
            macd.push(close);
            core.push_close(close);
        }
        // 不 panic 就算通過；值本身在這種輸入下沒有意義
        let _ = (atr.value(), macd.value(MacdOutput::Line), core.value());
    }

    // -----------------------------------------------------------------------
    // PivotTracker（擺動高低點，ADR-004 §4／§9.4）
    // -----------------------------------------------------------------------

    fn push_all(tracker: &mut PivotTracker, values: &[&str]) {
        for v in values {
            tracker.push(fx(v));
        }
    }

    #[test]
    fn a_clear_local_maximum_is_detected_and_confirmed() {
        // left=right=2：10,10,20,10,10 → 候選點 20 在左右各 2 根都比它低
        let mut tracker = PivotTracker::new(2, 2, true);
        push_all(&mut tracker, &["10", "10", "20", "10", "10"]);
        assert_eq!(tracker.value(SwingOutput::Last), Some(fx("20")));
    }

    #[test]
    fn a_pivot_is_none_until_the_right_side_confirms_it() {
        // 同一組資料，逐根檢查：確認前（右側根數不足）必須是 None
        let mut tracker = PivotTracker::new(2, 2, true);
        let values = ["10", "10", "20", "10", "10"];
        for (i, v) in values.iter().enumerate() {
            tracker.push(fx(v));
            let confirmed = i + 1 == values.len(); // 緩衝滿了才可能確認
            if confirmed {
                assert_eq!(
                    tracker.value(SwingOutput::Last),
                    Some(fx("20")),
                    "第 {} 根",
                    i + 1
                );
            } else {
                assert_eq!(tracker.value(SwingOutput::Last), None, "第 {} 根", i + 1);
            }
        }
    }

    #[test]
    fn a_flat_sequence_never_produces_a_pivot() {
        let mut tracker = PivotTracker::new(2, 2, true);
        push_all(
            &mut tracker,
            &["10", "10", "10", "10", "10", "10", "10", "10"],
        );
        assert_eq!(tracker.value(SwingOutput::Last), None);

        let mut low_tracker = PivotTracker::new(2, 2, false);
        push_all(
            &mut low_tracker,
            &["10", "10", "10", "10", "10", "10", "10", "10"],
        );
        assert_eq!(low_tracker.value(SwingOutput::Last), None);
    }

    #[test]
    fn last_and_previous_differ_once_two_pivots_are_confirmed() {
        // 延續「暖機下界」那組測資：20 先確認，15 後確認
        let mut tracker = PivotTracker::new(2, 2, true);
        push_all(
            &mut tracker,
            &["10", "10", "20", "10", "10", "15", "10", "10"],
        );
        assert_eq!(tracker.value(SwingOutput::Last), Some(fx("15")));
        assert_eq!(tracker.value(SwingOutput::Previous), Some(fx("20")));
    }

    #[test]
    fn swing_low_uses_the_strictly_less_than_direction() {
        let mut tracker = PivotTracker::new(1, 1, false);
        push_all(&mut tracker, &["10", "5", "10"]);
        assert_eq!(tracker.value(SwingOutput::Last), Some(fx("5")));
    }

    /// ADR-004 §4.6：`L=R=2` 時 `output: previous` 的暖機下界是 8 根——
    /// 第 7 根之後還是 `None`，第 8 根之後才第一次有值（不是提早、也不是延後）。
    #[test]
    fn previous_output_warmup_lower_bound_is_tight_for_l_eq_r_eq_2() {
        let mut tracker = PivotTracker::new(2, 2, true);
        let values = ["10", "10", "20", "10", "10", "15", "10", "10"];
        for (i, v) in values.iter().enumerate() {
            tracker.push(fx(v));
            if i + 1 < 8 {
                assert_eq!(
                    tracker.value(SwingOutput::Previous),
                    None,
                    "第 {} 根不該提早確認第二個擺動點",
                    i + 1
                );
            }
        }
        assert_eq!(tracker.value(SwingOutput::Previous), Some(fx("20")));
    }

    /// ADR-004 §4.6：`L=3, R=1` 驗證暖機下界公式是 `min(left,right)`
    /// 不是 `max(left,right)`——`min` 下的下界是 7 根，`max` 會錯誤地算成 9 根。
    /// 這裡用「最緊」測資實測：第 6 根還是 `None`，第 7 根之後才第一次有值，
    /// 證明 7 是真的下界、9 太保守。
    #[test]
    fn previous_output_warmup_lower_bound_uses_min_not_max_for_l3_r1() {
        let mut tracker = PivotTracker::new(3, 1, true);
        let values = ["10", "10", "10", "20", "10", "25", "10"];
        for (i, v) in values.iter().enumerate() {
            tracker.push(fx(v));
            if i + 1 < 7 {
                assert_eq!(
                    tracker.value(SwingOutput::Previous),
                    None,
                    "第 {} 根不該提早確認第二個擺動點",
                    i + 1
                );
            }
        }
        assert_eq!(tracker.value(SwingOutput::Previous), Some(fx("20")));
    }
}
