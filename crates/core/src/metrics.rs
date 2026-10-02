//! 績效指標：總報酬、年化報酬、最大回撤、夏普比率。
//!
//! 這一整個模組只吃 [`run_backtest`](crate::run_backtest) 輸出的權益曲線
//! （`&[EquityPoint]`），完全不知道策略、手續費、槓桿長什麼樣子。這是刻意的：
//! 指標是「一條權益曲線的統計量」，把它和回測邏輯綁在一起只會讓兩邊都難測。
//! 交易次數不在這裡，因為它**不在曲線裡**——那個數字在
//! [`BacktestResult`](crate::backtest::BacktestResult)。
//!
//! ## 四個指標的定義
//!
//! ```text
//! 總報酬   = 最後權益 ÷ 第一點權益 − 1
//! 年化報酬 = (最後權益 ÷ 第一點權益)^(1 ÷ 年數) − 1        // CAGR，幾何平均
//! 最大回撤 = max over t of (歷史最高權益 − 當下權益) ÷ 歷史最高權益
//! 夏普比率 = (每期報酬平均 ÷ 每期報酬標準差) × √(一年有幾期)
//! ```
//!
//! ## 年數怎麼算：用時間戳，不用根數
//!
//! ```text
//! 年數 = (最後一點的 open_time − 第一點的 open_time) ÷ 一年的毫秒數
//! ```
//!
//! 一年固定當 **365 天**（[`MS_PER_YEAR`]，幣圈全年無休，沒有「交易日」的概念）。
//! 絕不假設「一根 K 線 = 一天」：同一份程式要能同時吃 1 分線與日線，
//! 把根數當天數會讓 1 分線的年化差上 1440 倍。
//!
//! ## 夏普比率的三個假設（看報表的人一定要知道）
//!
//! 1. **取樣頻率 = K 線週期。**報酬序列是相鄰兩點的比值（`equity[i] ÷ equity[i−1] − 1`），
//!    所以 1 小時 K 線就是每小時一個樣本。沒有重新取樣成日頻或月頻——
//!    重新取樣要先決定「跨週末怎麼補點」之類的規則，規格還不清楚就不做。
//! 2. **一年的期數也是算出來的，不是寫死的常數。**
//!    `每年期數 = 報酬個數 ÷ 年數`，所以 1 小時 K 線自動得到約 8760、
//!    日線自動得到約 365。**寫死 365 去年化 1 小時的報酬會讓夏普少算約 4.9 倍**
//!    （√8760 ÷ √365 ≈ 4.9），這是這個指標最常見的錯。
//! 3. **無風險利率 = 0。**幣圈回測的慣例，而且加這個參數就得先決定它的計息頻率
//!    （年利率？每期利率？複利？），規格不清楚之前不無中生有。要算非 0 的版本，
//!    就在報酬序列上先減掉每期的無風險報酬再套同一條式子。
//!
//! 標準差用**母體**（除以 n），和 `strategies` 裡 `Window::stddev` 同一個慣例。
//! pandas 的 `.std()` 預設是**樣本**（除以 n−1），所以 2.8 拿 Python 對照時
//! 兩邊會差一個 `√(n ÷ (n−1))` 的倍數——n 大的時候幾乎看不出來，
//! n 小的時候很明顯，先知道就不會白花時間找錯。
//!
//! ## 權益是 0 的時候
//!
//! 2.5 的強制平倉模型保證**權益永遠不會是負的，但可以剛好是 0**（爆倉爆光）。
//! 所以「總報酬 −100%」是合法輸入，不是錯誤：
//!
//! - 總報酬、最大回撤照算（最大回撤就是 100%）。
//! - 年化報酬 = `0^(1÷年數) − 1` = −100%，不會除以 0。
//! - 報酬序列遇到「前一點是 0」時記 0，不是除以 0：帳戶歸零之後現金與倉位都是 0
//!   （見 `backtest::margin_call`），不可能再有任何波動。
//!
//! ## 算不出來就回 `None`，不回一個假數字
//!
//! [`Metrics`] 的欄位除了最大回撤都是 `Option<Fixed>`。回 `None` 的情況：
//! 空曲線、只有一點的曲線（沒有報酬序列可算）、第一點權益是 0（除不下去）、
//! 標準差是 0（一條完全沒波動的曲線，夏普是 `0 ÷ 0`）、或中間某一步溢位。
//! 報表上寫「無法計算」是誠實的，掛一個 0 上去會被當成「夏普 0」而誤導。
//!
//! ## 為什麼連開根號都用整數算
//!
//! 和 `strategies` 同一個理由：同一份輸入必須永遠得到同一個數字。
//! 夏普的標準差、年化的 `^(1÷年數)` 全部走
//! [`isqrt`](crate::fixed::isqrt)——年化的小數次方是用「連續開根號」
//! 展開的（`x^0.5 = √x`、`x^0.25 = √√x`…），不需要 `exp`/`ln`，也不碰 `f64`。
//! 代價是最後一兩位小數會有量級 10^-7 的誤差（見 [`pow`] 的說明）。

use crate::backtest::EquityPoint;
use crate::fixed::{isqrt, Fixed};

/// 一年的毫秒數，固定用 365 天。
///
/// 幣圈全年無休，沒有「一年 252 個交易日」的問題。沒用 365.25 天（儒略年）是為了
/// 讓年化的數字可以手算對照；差別是 0.07%，比回測本身的假設誤差小兩個數量級。
pub const MS_PER_YEAR: i64 = 365 * 24 * 60 * 60 * 1000;

/// 一條權益曲線的績效指標。全部是比例，`0.25` 就是 25%。
///
/// 怎麼讀：`total_return` 看賺多少、`max_drawdown` 看途中最痛的時候有多痛、
/// `sharpe` 看那些報酬是穩定拿到的還是靠幾根大陽線。三個一起看才有意義——
/// 年化 30% 配 60% 回撤和年化 15% 配 10% 回撤是完全不同的兩個產品。
///
/// 還要再看一眼 [`BacktestResult`](crate::backtest::BacktestResult) 的
/// `liquidations`：爆倉過的曲線，上面每一個數字都只是「殘值的統計」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// 總報酬：`最後 ÷ 第一點 − 1`。`-1` 就是虧光。
    pub total_return: Option<Fixed>,
    /// 年化報酬（CAGR）：把總報酬換算成「平均每年複利多少」。
    ///
    /// **回測跨的時間很短時這個數字沒有意義**：3 根 1 分線賺 1% 會被年化成
    /// 天文數字，甚至直接溢位變成 `None`。看它之前先看 `span_years`。
    pub annualized_return: Option<Fixed>,
    /// 最大回撤：從歷史最高點往下跌最深的那一次，`0.19` 就是 −19%。
    ///
    /// 永遠算得出來（沒有曲線就是 0），所以不是 `Option`。
    pub max_drawdown: Fixed,
    /// 年化夏普比率。假設寫在模組說明裡，用之前先讀。
    pub sharpe: Option<Fixed>,
    /// 曲線實際跨了幾年（用第一點與最後一點的 `open_time` 算）。
    ///
    /// 放在結果裡是為了讓年化與夏普可以被驗算：這兩個指標都是拿它當分母，
    /// 對不上 Python 的時候第一個要比的就是這個數字。
    pub span_years: Option<Fixed>,
}

impl Metrics {
    /// 從權益曲線算出四個指標。空曲線不是錯誤，回傳全部 `None` + 回撤 0。
    ///
    /// `curve` 要是時間遞增的（`run_backtest` 已經保證）。時間倒退或所有時間戳
    /// 相同時，年化與夏普回 `None`——不會拿一個負的或 0 的年數去除。
    pub fn from_curve(curve: &[EquityPoint]) -> Metrics {
        Metrics {
            total_return: total_return(curve),
            annualized_return: annualized_return(curve),
            max_drawdown: max_drawdown(curve),
            sharpe: sharpe(curve),
            span_years: span_years(curve),
        }
    }
}

/// `最後 ÷ 第一點 − 1`。只有一點時是 0（起點就是終點）。
fn total_return(curve: &[EquityPoint]) -> Option<Fixed> {
    let first = curve.first()?.equity;
    let last = curve.last()?.equity;
    last.checked_div(first)?.checked_sub(Fixed::ONE)
}

/// 曲線跨的年數。少於兩點、時間沒有前進、或大到放不進 `Fixed` 時回 `None`。
fn span_years(curve: &[EquityPoint]) -> Option<Fixed> {
    if curve.len() < 2 {
        return None;
    }
    let span = curve
        .last()?
        .open_time
        .checked_sub(curve.first()?.open_time)?;
    if span <= 0 {
        return None;
    }
    // 毫秒數乘 10^8 會超過 i64（4 年就是 1.3×10^19），所以在 i128 裡換算完才收回 Fixed
    let raw = (span as i128) * (Fixed::SCALE as i128) / (MS_PER_YEAR as i128);
    i64::try_from(raw).ok().map(Fixed::from_raw)
}

/// CAGR：`(最後 ÷ 第一點)^(1 ÷ 年數) − 1`。
fn annualized_return(curve: &[EquityPoint]) -> Option<Fixed> {
    let years = span_years(curve)?;
    let first = curve.first()?.equity;
    let last = curve.last()?.equity;
    let growth = last.checked_div(first)?;
    let exponent = Fixed::ONE.checked_div(years)?;
    pow(growth, exponent)?.checked_sub(Fixed::ONE)
}

/// 從歷史最高點算「現在比最高點跌了多少」，取整條曲線的最大值。
///
/// 兩個邊界：**還沒創過新高**（第一點就是目前的高點）時回撤是 0；
/// **創新高後又跌**才開始累積。所以一路向上的曲線回撤是 0，
/// 先漲到頂再跌回起點的曲線回撤是「從頂點算的跌幅」，不是「從起點算的」。
fn max_drawdown(curve: &[EquityPoint]) -> Fixed {
    let mut peak = Fixed::ZERO;
    let mut worst = Fixed::ZERO;
    for point in curve {
        if point.equity > peak {
            peak = point.equity;
        }
        // 還沒出現正的高點（曲線從 0 開始）時，回撤無從定義
        if peak <= Fixed::ZERO {
            continue;
        }
        if let Some(drawdown) = peak
            .checked_sub(point.equity)
            .and_then(|fall| fall.checked_div(peak))
        {
            if drawdown > worst {
                worst = drawdown;
            }
        }
    }
    worst
}

/// 相鄰兩點的報酬率序列。長度是曲線長度 − 1。
fn returns(curve: &[EquityPoint]) -> Option<Vec<Fixed>> {
    let mut out = Vec::with_capacity(curve.len().saturating_sub(1));
    for pair in curve.windows(2) {
        // ponytail: 前一點是 0 就記 0 報酬。爆倉之後現金與倉位都是 0，權益再也不會動，
        // 所以這不是近似而是事實；真正要避免的是「0 當分母」變成 NaN 或 panic。
        let ret = if pair[0].equity.is_zero() {
            Fixed::ZERO
        } else {
            pair[1]
                .equity
                .checked_div(pair[0].equity)?
                .checked_sub(Fixed::ONE)?
        };
        out.push(ret);
    }
    Some(out)
}

/// 年化夏普比率：`(平均 ÷ 母體標準差) × √(每年期數)`，無風險利率 0。
fn sharpe(curve: &[EquityPoint]) -> Option<Fixed> {
    let returns = returns(curve)?;
    let count = returns.len();
    if count == 0 {
        return None;
    }
    // 平均與離差平方都在 i128 裡用 raw 累加（和 Window::stddev 同一套做法）：
    // n 個 i64 相加、n 個 10^16 量級的平方相加都放得下，所以加總本身不會溢位
    let sum: i128 = returns.iter().map(|ret| ret.raw() as i128).sum();
    let mean_raw = sum / count as i128;
    let mut sum_sq: i128 = 0;
    for ret in &returns {
        let deviation = ret.raw() as i128 - mean_raw;
        sum_sq = sum_sq.checked_add(deviation.checked_mul(deviation)?)?;
    }
    // 離差平方是 10^16 倍，開根號後剛好回到 10^8 倍，不必再縮放
    let variance = u128::try_from(sum_sq / count as i128).ok()?;
    let stddev = Fixed::from_raw(i64::try_from(isqrt(variance)).ok()?);
    if stddev.is_zero() {
        // 完全沒波動的曲線，夏普是 0 ÷ 0：寫「無法計算」而不是掛一個 0 上去
        return None;
    }
    let mean = Fixed::from_raw(i64::try_from(mean_raw).ok()?);
    let per_period = mean.checked_div(stddev)?;
    // 每年期數從時間戳算，所以 1 分線與日線都自動對；報酬個數剛好等於曲線跨過的期數
    let periods_per_year = Fixed::from_int(count as i64)?.checked_div(span_years(curve)?)?;
    per_period.checked_mul(sqrt(periods_per_year)?)
}

/// 定點數開根號，四捨五入到第 8 位小數。負數回 `None`。
fn sqrt(x: Fixed) -> Option<Fixed> {
    if x.is_negative() {
        return None;
    }
    // raw 已經是 10^8 倍，再乘一次 10^8 之後開根號才回到 10^8 倍
    let scaled = (x.raw() as u128) * (Fixed::SCALE as u128);
    let root = isqrt(scaled);
    // 四捨五入：root+1 比較近的條件是 scaled − root² > (root+1)² − scaled，化簡成這一行
    let root = if scaled > root * root + root {
        root + 1
    } else {
        root
    };
    i64::try_from(root).ok().map(Fixed::from_raw)
}

/// `base^exponent`，兩者都必須 ≥ 0。算不出來或溢位回 `None`。
///
/// 只有年化報酬用得到，而它的指數是 `1 ÷ 年數`——幾乎永遠是小數，所以不能只寫
/// 整數次方。做法是把指數拆成整數部分與小數部分：
///
/// - **整數部分**用平方累乘（`x^13 = x^8 × x^4 × x^1`）。
/// - **小數部分**展開成二進位，每一位對應一次開根號：
///   `x^0.5 = √x`、`x^0.25 = √√x`、`x^0.75 = √x × √√x`。
///
/// 這樣整條路徑只用到整數開根號與定點數乘法，不需要 `exp`/`ln`，也不碰 `f64`。
/// 代價是誤差：最多 32 次開根號各自四捨五入一次，累積下來量級約 10^-7，
/// 也就是年化報酬的最後一兩位小數是雜訊。報表上顯示到小數第 4 位（0.01%）沒有問題。
fn pow(base: Fixed, exponent: Fixed) -> Option<Fixed> {
    if base.is_negative() || exponent.is_negative() {
        return None;
    }
    if exponent.is_zero() {
        return Some(Fixed::ONE);
    }
    if base.is_zero() {
        // 0 的正數次方是 0：爆倉爆光的曲線，年化報酬就是 −100%
        return Some(Fixed::ZERO);
    }

    let mut result = Fixed::ONE;

    let mut whole = exponent.raw() / Fixed::SCALE;
    let mut squared = base;
    while whole > 0 {
        if whole & 1 == 1 {
            result = result.checked_mul(squared)?;
        }
        whole >>= 1;
        if whole > 0 {
            squared = squared.checked_mul(squared)?;
        }
    }

    // 小數部分轉成 32 位二進位小數（最高位的權重是 1/2）。
    // 用 2 的冪當分母才不會有截尾誤差：直接把 10^8 一直除以 2 會越除越偏。
    let fraction = (exponent.raw() % Fixed::SCALE) as u128;
    let mut bits = (fraction << 32) / Fixed::SCALE as u128;
    let mut root = base;
    let mut weight = 1u128 << 31;
    while bits != 0 {
        root = sqrt(root)?;
        if bits & weight != 0 {
            result = result.checked_mul(root)?;
            bits ^= weight;
        }
        weight >>= 1;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;
    const HOUR: i64 = 3_600_000;

    /// 一條每 `step` 毫秒一點的權益曲線。
    fn curve_every(step: i64, equities: &[&str]) -> Vec<EquityPoint> {
        equities
            .iter()
            .enumerate()
            .map(|(i, e)| EquityPoint {
                open_time: T0 + i as i64 * step,
                equity: fx(e),
            })
            .collect()
    }

    /// 一條「一點 = 一年」的權益曲線：年化與夏普的手算case用它，
    /// 因為每年剛好一期，`√(每年期數)` 等於 1，年化的倍數不會插進手算裡。
    fn yearly(equities: &[&str]) -> Vec<EquityPoint> {
        curve_every(MS_PER_YEAR, equities)
    }

    /// 這一步的手算基準：5 個點、4 期，漲漲跌跌。
    ///
    /// | 期 | 權益 | 報酬 |
    /// |---|---|---|
    /// | 0 | 10000 | — |
    /// | 1 | 12000 | +20% |
    /// | 2 | 14400 | +20% |
    /// | 3 | 12960 | −10% |
    /// | 4 | 11664 | −10% |
    ///
    /// - **總報酬** = 11664 ÷ 10000 − 1 = **0.1664**
    /// - **最大回撤**：歷史最高是 14400，最低點是 11664
    ///   → (14400 − 11664) ÷ 14400 = 2736 ÷ 14400 = **0.19**
    /// - **夏普**：平均 = (0.2 + 0.2 − 0.1 − 0.1) ÷ 4 = 0.05；
    ///   離差都是 ±0.15 → 母體變異數 = 0.0225 → 標準差 = 0.15（剛好整數，所以手算得完）；
    ///   每年 1 期 → √1 = 1 → 夏普 = 0.05 ÷ 0.15 = **0.33333333**
    /// - **年化**：4 年、總成長 1.1664 = 1.08²
    ///   → CAGR = 1.1664^(1÷4) − 1 = √1.08 − 1 = **0.03923048**
    ///   （驗算：1.03923048² = 1.0799999990…，四捨五入回 1.08）
    fn hand_curve() -> Vec<EquityPoint> {
        yearly(&["10000", "12000", "14400", "12960", "11664"])
    }

    #[test]
    fn the_hand_computed_curve_matches_all_four_metrics() {
        let m = Metrics::from_curve(&hand_curve());
        assert_eq!(m.total_return, Some(fx("0.1664")));
        assert_eq!(m.max_drawdown, fx("0.19"));
        assert_eq!(m.sharpe, Some(fx("0.33333333")));
        assert_eq!(m.annualized_return, Some(fx("0.03923048")));
        assert_eq!(m.span_years, Some(fx("4")));
    }

    #[test]
    fn annualized_return_compounds_back_to_the_total_return() {
        // 年化是幾何平均，所以「年化連乘年數次」要回到總成長。
        // 0.03923048 是 √1.08 − 1，四次方回去應該非常接近 1.1664。
        let m = Metrics::from_curve(&hand_curve());
        let annual = m.annualized_return.unwrap();
        let growth = Fixed::ONE.checked_add(annual).unwrap();
        let compounded = pow(growth, fx("4")).unwrap();
        let error = compounded.checked_sub(fx("1.1664")).unwrap().abs();
        assert!(error < fx("0.0000001"), "誤差 {error} 太大");
    }

    /// 年化必須用時間戳算，不能用根數。
    ///
    /// 同一條權益曲線，一次當 1 小時 K 線、一次當日線：總報酬與最大回撤完全一樣
    /// （它們跟時間無關），年化與夏普必須不一樣，而且 1 小時的倍數要大得多。
    #[test]
    fn the_same_shape_annualizes_differently_at_different_intervals() {
        let shape = ["10000", "11000", "12100", "13310"];
        let hourly = Metrics::from_curve(&curve_every(HOUR, &shape));
        let daily = Metrics::from_curve(&curve_every(24 * HOUR, &shape));

        assert_eq!(hourly.total_return, daily.total_return);
        assert_eq!(hourly.max_drawdown, daily.max_drawdown);

        // 3 小時 vs 3 天：年數差 24 倍
        assert_eq!(hourly.span_years, Some(fx("0.00034246")));
        assert_eq!(daily.span_years, Some(fx("0.00821917")));

        // 每根都漲 10%，標準差是 0 → 夏普算不出來（分母 0），兩邊都是 None
        assert_eq!(hourly.sharpe, None);
        assert_eq!(daily.sharpe, None);

        // 一路漲、沒有回撤
        assert_eq!(hourly.max_drawdown, Fixed::ZERO);

        // 3 小時賺 33% 年化成天文數字 → 直接溢位回 None，這比印一個假數字誠實
        assert_eq!(hourly.annualized_return, None);
        assert_eq!(daily.annualized_return, None);
    }

    /// 夏普的年化倍數要跟著取樣頻率走。
    ///
    /// 同一組報酬（手算那一組，平均 0.05、標準差 0.15），一次當 1 小時 K 線、
    /// 一次當日線：**每期夏普一樣，年化夏普要差 √24 ≈ 4.8989795 倍**。
    /// 一天有 24 小時，所以 1 小時的取樣一年有 24 倍多的期數。
    ///
    /// 這一條就是在釘住「年化常數不能寫死」：寫死 365 的話兩邊會一模一樣，
    /// 1 小時 K 線的夏普就被少算了將近 5 倍。
    #[test]
    fn sharpe_annualizes_with_the_actual_sampling_frequency() {
        let shape = ["10000", "12000", "14400", "12960", "11664"];
        let hourly = Metrics::from_curve(&curve_every(HOUR, &shape))
            .sharpe
            .unwrap();
        let daily = Metrics::from_curve(&curve_every(24 * HOUR, &shape))
            .sharpe
            .unwrap();
        assert_ne!(hourly, daily);
        let ratio = hourly.checked_div(daily).unwrap();
        let expected = sqrt(fx("24")).unwrap(); // 4.89897949
        let error = ratio.checked_sub(expected).unwrap().abs();
        assert!(error < fx("0.0001"), "倍數 {ratio} 應該接近 {expected}");
    }

    #[test]
    fn an_empty_curve_has_no_metrics_and_does_not_panic() {
        let m = Metrics::from_curve(&[]);
        assert_eq!(m.total_return, None);
        assert_eq!(m.annualized_return, None);
        assert_eq!(m.sharpe, None);
        assert_eq!(m.span_years, None);
        assert_eq!(m.max_drawdown, Fixed::ZERO);
    }

    #[test]
    fn a_single_point_has_a_zero_return_and_nothing_else() {
        let m = Metrics::from_curve(&yearly(&["10000"]));
        // 起點就是終點：沒賺沒賠、沒回撤
        assert_eq!(m.total_return, Some(Fixed::ZERO));
        assert_eq!(m.max_drawdown, Fixed::ZERO);
        // 一個點沒有時間長度也沒有報酬序列，年化與夏普不是 0 而是「算不出來」
        assert_eq!(m.annualized_return, None);
        assert_eq!(m.sharpe, None);
        assert_eq!(m.span_years, None);
    }

    #[test]
    fn a_flat_curve_has_no_sharpe_because_the_stddev_is_zero() {
        let m = Metrics::from_curve(&yearly(&["10000", "10000", "10000"]));
        assert_eq!(m.total_return, Some(Fixed::ZERO));
        assert_eq!(m.annualized_return, Some(Fixed::ZERO));
        assert_eq!(m.max_drawdown, Fixed::ZERO);
        // 完全不動的曲線，夏普是 0 ÷ 0
        assert_eq!(m.sharpe, None);
    }

    /// 曾經觸及 0 的曲線：不可以 panic、不可以出現 NaN 式的假數字。
    ///
    /// 10000 → 5000 → 0 → 0 → 0（爆倉爆光，之後躺平）：
    /// 報酬序列是 −50%、−100%、0、0（後兩期的前一點是 0，記 0 而不是除以 0）。
    #[test]
    fn a_curve_that_hits_zero_is_a_hundred_percent_loss_not_a_division_by_zero() {
        let m = Metrics::from_curve(&yearly(&["10000", "5000", "0", "0", "0"]));
        assert_eq!(m.total_return, Some(fx("-1")));
        assert_eq!(m.max_drawdown, Fixed::ONE);
        // 0^(1/4) = 0 → 年化也是 −100%
        assert_eq!(m.annualized_return, Some(fx("-1")));
        // 報酬序列 [−0.5, −1, 0, 0]：平均 −0.375，算得出來的負夏普
        let sharpe = m.sharpe.unwrap();
        assert!(sharpe.is_negative(), "夏普 {sharpe} 應該是負的");
    }

    #[test]
    fn a_curve_that_starts_at_zero_returns_none_instead_of_dividing_by_zero() {
        // 第一點是 0：總報酬與年化都除不下去，但不准 panic
        let m = Metrics::from_curve(&yearly(&["0", "0", "5000"]));
        assert_eq!(m.total_return, None);
        assert_eq!(m.annualized_return, None);
        // 0 → 0 → 5000：歷史最高點出現在最後一點，全程沒有回撤
        assert_eq!(m.max_drawdown, Fixed::ZERO);
    }

    /// 最大回撤的兩個邊界：一路創新高 vs 創新高後又跌。
    #[test]
    fn drawdown_only_counts_falls_from_a_previous_peak() {
        // 一路向上：每一點都是新高，回撤永遠是 0
        let rising = Metrics::from_curve(&yearly(&["10000", "12000", "15000", "20000"]));
        assert_eq!(rising.max_drawdown, Fixed::ZERO);

        // 同樣的高點，但最後跌回 10000：從 20000 算跌了 50%
        let falls = Metrics::from_curve(&yearly(&["10000", "12000", "15000", "20000", "10000"]));
        assert_eq!(falls.max_drawdown, fx("0.5"));
        // 兩條曲線的最高點一樣，總報酬不同也不影響上面的結論
        assert_eq!(rising.total_return, Some(fx("1")));
        assert_eq!(falls.total_return, Some(Fixed::ZERO));
    }

    /// 回撤算的是「離歷史最高點多遠」，不是「離起點多遠」。
    ///
    /// 10000 → 20000 → 15000 → 30000：中間那段從 20000 跌到 15000 是 −25%，
    /// 但權益從頭到尾沒有低於起始的 10000。只看「有沒有虧錢」會漏掉這 25%。
    #[test]
    fn drawdown_is_measured_from_the_peak_not_from_the_start() {
        let m = Metrics::from_curve(&yearly(&["10000", "20000", "15000", "30000"]));
        assert_eq!(m.max_drawdown, fx("0.25"));
        assert_eq!(m.total_return, Some(fx("2")));
    }

    /// 最深的那一次才算，不是最後一次。
    #[test]
    fn the_worst_drawdown_wins_even_if_a_later_one_is_shallower() {
        // 先從 10000 跌到 5000（−50%），再創新高 12000 後跌到 10800（−10%）
        let m = Metrics::from_curve(&yearly(&["10000", "5000", "12000", "10800"]));
        assert_eq!(m.max_drawdown, fx("0.5"));
    }

    #[test]
    fn total_return_can_be_negative() {
        let m = Metrics::from_curve(&yearly(&["10000", "7500"]));
        assert_eq!(m.total_return, Some(fx("-0.25")));
        assert_eq!(m.max_drawdown, fx("0.25"));
        // 一年 −25%，年化就是 −25%
        assert_eq!(m.annualized_return, Some(fx("-0.25")));
    }

    #[test]
    fn a_backwards_curve_has_no_annualized_return() {
        // 時間倒退（`run_backtest` 不會產生，但外部組出來的曲線可能）：
        // 用負的年數去除會得到一個反過來的假年化，所以回 None
        let mut curve = yearly(&["10000", "12000"]);
        curve[1].open_time = T0 - MS_PER_YEAR;
        let m = Metrics::from_curve(&curve);
        assert_eq!(m.span_years, None);
        assert_eq!(m.annualized_return, None);
        assert_eq!(m.sharpe, None);
        // 跟時間無關的兩個指標照算
        assert_eq!(m.total_return, Some(fx("0.2")));
    }

    #[test]
    fn identical_timestamps_have_no_annualized_return() {
        let mut curve = yearly(&["10000", "12000"]);
        curve[1].open_time = T0;
        let m = Metrics::from_curve(&curve);
        assert_eq!(m.span_years, None);
        assert_eq!(m.annualized_return, None);
    }

    /// 兩年翻倍的年化是 √2 − 1，不是 50%。
    ///
    /// 這是「年化 ≠ 總報酬 ÷ 年數」最短的反例：算成 50% 會高估 8.6 個百分點。
    #[test]
    fn annualized_return_is_geometric_not_linear() {
        let m = Metrics::from_curve(&yearly(&["10000", "10000", "20000"]));
        assert_eq!(m.annualized_return, Some(fx("0.41421356")));
        assert_ne!(m.annualized_return, Some(fx("0.5")));
        assert_eq!(m.total_return, Some(fx("1")));
    }

    /// 真的跑一次回測再算指標：這是「回測」與「指標」唯一接在一起的地方。
    ///
    /// 4 根 K 線、每根間隔剛好三分之一年，所以**整段回測剛好跨 1 年**——
    /// 這時候年化報酬必須等於總報酬（一年的複利就是它自己），對不上就是
    /// 年數或 `pow` 算錯了。
    ///
    /// 價格 100 → 100 → 110 → 121，1 倍滿倉做多：
    /// 權益 10000 → 10000（第 1 根開盤才成交）→ 11000 → 12100，
    /// 總報酬 21%、一路創新高所以沒有回撤、只送出一筆單（1 倍滿倉不需要再平衡）。
    #[test]
    fn metrics_read_a_real_backtest_curve() {
        use crate::backtest::{run_backtest, BacktestConfig};
        use crate::bar::Bar;
        use crate::strategy::{Strategy, TargetPosition};

        struct AlwaysLong;
        impl Strategy for AlwaysLong {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                TargetPosition::FULL_LONG
            }
        }

        let third_of_a_year = MS_PER_YEAR / 3;
        let bars: Vec<Bar> = ["100", "100", "110", "121"]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * third_of_a_year,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                    order_flow: None,
                }
            })
            .collect();

        let result = run_backtest(
            &bars,
            &mut AlwaysLong,
            &BacktestConfig::frictionless(fx("10000")),
        )
        .unwrap();
        assert_eq!(result.trades, 1);
        assert_eq!(result.liquidations, 0);

        let m = Metrics::from_curve(&result.curve);
        assert_eq!(m.span_years, Some(Fixed::ONE));
        assert_eq!(m.total_return, Some(fx("0.21")));
        // 剛好一年 → 年化 = 總報酬
        assert_eq!(m.annualized_return, m.total_return);
        assert_eq!(m.max_drawdown, Fixed::ZERO);
        // 報酬序列是 [0, +10%, +10%]（第一段還沒進場），所以夏普算得出來且為正
        assert!(m.sharpe.unwrap() > Fixed::ZERO);
    }

    // ── 底層的兩個數學函式 ────────────────────────────────────────────

    #[test]
    fn sqrt_rounds_to_eight_decimals() {
        assert_eq!(sqrt(fx("4")), Some(fx("2")));
        assert_eq!(sqrt(fx("1.1664")), Some(fx("1.08")));
        assert_eq!(sqrt(Fixed::ZERO), Some(Fixed::ZERO));
        assert_eq!(sqrt(Fixed::ONE), Some(Fixed::ONE));
        // √2 = 1.41421356237…、√1.08 = 1.03923048454…
        assert_eq!(sqrt(fx("2")), Some(fx("1.41421356")));
        assert_eq!(sqrt(fx("1.08")), Some(fx("1.03923048")));
        // 0.49 的根是 0.7，小數也要對
        assert_eq!(sqrt(fx("0.49")), Some(fx("0.7")));
        assert_eq!(sqrt(fx("-1")), None);
    }

    #[test]
    fn pow_handles_whole_and_fractional_exponents() {
        // 整數次方：平方累乘
        assert_eq!(pow(fx("2"), fx("10")), Some(fx("1024")));
        assert_eq!(pow(fx("1.1"), fx("2")), Some(fx("1.21")));
        // 小數次方：連續開根號
        assert_eq!(pow(fx("4"), fx("0.5")), Some(fx("2")));
        assert_eq!(pow(fx("1.21"), fx("0.5")), Some(fx("1.1")));
        assert_eq!(pow(fx("16"), fx("0.25")), Some(fx("2")));
        // 混合：2^2.5 = 4√2 = 5.65685424…
        let mixed = pow(fx("2"), fx("2.5")).unwrap();
        let error = mixed.checked_sub(fx("5.65685425")).unwrap().abs();
        assert!(error < fx("0.0000001"), "2^2.5 = {mixed}");
        // 邊界
        assert_eq!(pow(fx("7"), Fixed::ZERO), Some(Fixed::ONE));
        assert_eq!(pow(Fixed::ZERO, fx("0.5")), Some(Fixed::ZERO));
        assert_eq!(pow(fx("-2"), fx("2")), None);
        assert_eq!(pow(fx("2"), fx("-1")), None);
        // 溢位不 panic：Fixed 只到約 922 億
        assert_eq!(pow(fx("10"), fx("20")), None);
    }

    /// 不是 2 的冪的小數指數也要夠準：0.3 次方走 32 位二進位展開。
    ///
    /// 0.3 的二進位是循環小數（0.0100110011…），所以這一條才測得到展開的精度。
    /// 真值 `1.5^0.3 = exp(0.3 × ln 1.5) = exp(0.1216395324) = 1.129346968…`，
    /// 這裡算出 1.12934689，差 8×10^-8——就是 `pow` 說明裡講的誤差量級。
    #[test]
    fn pow_is_accurate_for_awkward_fractional_exponents() {
        let value = pow(fx("1.5"), fx("0.3")).unwrap();
        let error = value.checked_sub(fx("1.12934697")).unwrap().abs();
        assert!(error < fx("0.000001"), "1.5^0.3 = {value}");
        // 反過來驗：結果的 (1/0.3) 次方要回到 1.5 附近
        let back = pow(value, fx("3.33333333")).unwrap();
        let error = back.checked_sub(fx("1.5")).unwrap().abs();
        assert!(error < fx("0.00001"), "回推得到 {back}");
    }
}
