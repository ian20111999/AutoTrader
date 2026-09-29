//! RSI：超賣買進、超買出場（均值回歸）。

use super::{non_zero, StrategyParamError};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};

/// Wilder 平滑平均：RSI 的漲幅／跌幅平均用的就是這個。
///
/// 兩個階段：
///
/// 1. **種子期**：前 `period` 筆先算一個普通的算術平均。
/// 2. **之後每筆**：`新平均 = 舊平均 + (這筆 − 舊平均) ÷ period`。
///
/// 第 2 式和常見的寫法 `(舊平均 × (period−1) + 這筆) ÷ period` 在數學上相同，
/// 但這個寫法的中間值一定落在「舊平均」與「這筆」之間，所以**不可能溢位**，
/// 不必在每根 K 線上處理一個永遠不會發生的錯誤。
#[derive(Debug)]
struct WilderAverage {
    period: usize,
    seen: usize,
    seed_sum_raw: i128,
    average: Option<Fixed>,
}

impl WilderAverage {
    fn new(period: usize) -> WilderAverage {
        WilderAverage {
            period: period.max(1),
            seen: 0,
            seed_sum_raw: 0,
            average: None,
        }
    }

    fn push(&mut self, value: Fixed) {
        let n = self.period as i128;
        if let Some(prev) = self.average {
            let prev_raw = prev.raw() as i128;
            let next_raw = prev_raw + (value.raw() as i128 - prev_raw) / n;
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
            self.average = i64::try_from(self.seed_sum_raw / n)
                .ok()
                .map(Fixed::from_raw);
        }
    }

    fn value(&self) -> Option<Fixed> {
        self.average
    }
}

/// RSI 策略（均值回歸）。
///
/// RSI（相對強弱指標）把最近的漲幅與跌幅平均起來，換算成 0～100 的數字：
/// `RSI = 100 × 平均漲幅 ÷ (平均漲幅 + 平均跌幅)`。
/// 100 表示這段期間只漲沒跌，0 表示只跌沒漲，50 表示漲跌相當。
///
/// - RSI ≤ `buy_below`（預設 30，超賣）→ 滿倉做多
/// - RSI ≥ `exit_above`（預設 70，超買）→ 空手
/// - 兩者之間 → 維持上一根的部位
///
/// ## 用 Wilder 平滑，不是單純的移動平均
///
/// 平均漲跌幅有兩種算法：Wilder 原始定義的平滑平均（見 [`WilderAverage`]），
/// 和單純取最近 N 根的算術平均（一般叫 Cutler's RSI）。兩者的數值不一樣。
///
/// 這裡用 **Wilder**，因為 TradingView、ta-lib、Binance 介面上寫「RSI」指的
/// 都是它。2.8 要和之前的 Python 回測對照，用同一個定義才對得上。
///
/// 代價是 Wilder 平滑理論上會記得所有歷史（每根只衰減 1/N），所以
/// [`warmup_bars`](Strategy::warmup_bars) 回報的 `period + 1` 只是「算得出數值」
/// 的最低根數；數值要收斂到和「從更早開始餵」幾乎一致，通常還要再幾個週期。
/// 對照驗證時兩邊要從同一根 K 線開始餵，才不會被這個差異絆倒。
///
/// 完全沒有波動時（平均漲幅與跌幅都是 0）RSI 在數學上無定義，這裡取中性值 50。
#[derive(Debug)]
pub struct Rsi {
    avg_gain: WilderAverage,
    avg_loss: WilderAverage,
    prev_close: Option<Fixed>,
    buy_below: Fixed,
    exit_above: Fixed,
    holding: bool,
}

impl Rsi {
    /// 預設週期（根）。
    pub const DEFAULT_PERIOD: usize = 14;
    /// 預設進場門檻（超賣）。
    pub const DEFAULT_BUY_BELOW: Fixed = Fixed::from_raw(30 * Fixed::SCALE);
    /// 預設出場門檻（超買）。
    pub const DEFAULT_EXIT_ABOVE: Fixed = Fixed::from_raw(70 * Fixed::SCALE);
    /// 漲跌幅都是 0 時採用的中性值。
    pub const NEUTRAL: Fixed = Fixed::from_raw(50 * Fixed::SCALE);
    /// RSI 的上限。
    const FULL: Fixed = Fixed::from_raw(100 * Fixed::SCALE);

    /// `period` 是平均漲跌幅的週期，`buy_below` / `exit_above` 是 0～100 的門檻。
    pub fn new(
        period: usize,
        buy_below: Fixed,
        exit_above: Fixed,
    ) -> Result<Rsi, StrategyParamError> {
        let period = non_zero(period)?;
        for threshold in [buy_below, exit_above] {
            if threshold < Fixed::ZERO || threshold > Rsi::FULL {
                return Err(StrategyParamError::ThresholdOutOfRange);
            }
        }
        if buy_below >= exit_above {
            return Err(StrategyParamError::ThresholdsOutOfOrder);
        }
        Ok(Rsi {
            avg_gain: WilderAverage::new(period),
            avg_loss: WilderAverage::new(period),
            prev_close: None,
            buy_below,
            exit_above,
            holding: false,
        })
    }

    /// 目前的 RSI（0～100）。暖機不足時回傳 `None`。
    pub fn value(&self) -> Option<Fixed> {
        let gain = self.avg_gain.value()?.raw() as i128;
        let loss = self.avg_loss.value()?.raw() as i128;
        let total = gain + loss;
        if total == 0 {
            return Some(Rsi::NEUTRAL);
        }
        // gain 最多是 i64 的上限（約 9.2×10^18），乘上 10^10 仍遠小於 i128 的上限
        let raw = 100 * (Fixed::SCALE as i128) * gain / total;
        i64::try_from(raw).ok().map(Fixed::from_raw)
    }
}

impl Default for Rsi {
    /// 14 / 30 / 70（教科書上的經典設定）。
    /// 設計稿的範例畫的是 14 / 20 / 50，那是更保守進場、更早出場的設定。
    fn default() -> Rsi {
        // 三個常數寫死且合法，不是外部輸入
        Rsi::new(
            Rsi::DEFAULT_PERIOD,
            Rsi::DEFAULT_BUY_BELOW,
            Rsi::DEFAULT_EXIT_ABOVE,
        )
        .expect("內建預設參數必須合法")
    }
}

impl Strategy for Rsi {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        let Some(prev_close) = self.prev_close.replace(bar.close) else {
            // 第一根沒有前一根可比，還算不出漲跌幅
            return TargetPosition::FLAT;
        };
        let Some(change) = bar.close.checked_sub(prev_close) else {
            // 兩個正數相減不可能溢位；真的發生就空手，不要餵壞平均值
            return TargetPosition::FLAT;
        };
        if change.is_negative() {
            self.avg_gain.push(Fixed::ZERO);
            self.avg_loss.push(change.abs());
        } else {
            self.avg_gain.push(change);
            self.avg_loss.push(Fixed::ZERO);
        }

        let Some(rsi) = self.value() else {
            self.holding = false;
            return TargetPosition::FLAT;
        };
        // 門檻已經保證 buy_below < exit_above，兩個條件互斥，順序不影響結果
        if rsi >= self.exit_above {
            self.holding = false;
        } else if rsi <= self.buy_below {
            self.holding = true;
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    /// 週期 + 1：N 筆漲跌幅需要 N+1 根 K 線。
    fn warmup_bars(&self) -> usize {
        self.avg_gain.period + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{closes, extreme_bars, fx, last_signal, signals};

    fn rsi4() -> Rsi {
        Rsi::new(4, fx("30"), fx("70")).unwrap()
    }

    #[test]
    fn rejects_bad_params() {
        assert_eq!(
            Rsi::new(0, fx("30"), fx("70")).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            Rsi::new(14, fx("-1"), fx("70")).unwrap_err(),
            StrategyParamError::ThresholdOutOfRange
        );
        assert_eq!(
            Rsi::new(14, fx("30"), fx("101")).unwrap_err(),
            StrategyParamError::ThresholdOutOfRange
        );
        assert_eq!(
            Rsi::new(14, fx("70"), fx("30")).unwrap_err(),
            StrategyParamError::ThresholdsOutOfOrder
        );
        assert_eq!(
            Rsi::new(14, fx("50"), fx("50")).unwrap_err(),
            StrategyParamError::ThresholdsOutOfOrder
        );
        assert!(Rsi::new(1, Fixed::ZERO, fx("100")).is_ok());
    }

    #[test]
    fn default_is_fourteen_thirty_seventy() {
        let s = Rsi::default();
        assert_eq!(s.warmup_bars(), 15);
        assert_eq!(s.buy_below, fx("30"));
        assert_eq!(s.exit_above, fx("70"));
    }

    #[test]
    fn rsi_value_matches_hand_calculation() {
        // 週期 4，收盤 100 → 104 → 100 → 100 → 100 → 104，漲跌幅 +4 −4 0 0 +4
        let mut s = rsi4();
        let bars = closes(&["100", "104", "100", "100", "100", "104"]);

        // 種子期：前 4 筆漲幅總和 4、跌幅總和 4，平均都是 1
        //   → RSI = 100 × 1 ÷ (1+1) = 50
        last_signal(&mut s, &bars[..5]);
        assert_eq!(s.value(), Some(fx("50")));

        // 第 5 筆是 +4，Wilder 平滑：
        //   平均漲幅 = 1 + (4−1)/4 = 1.75
        //   平均跌幅 = 1 + (0−1)/4 = 0.75
        //   → RSI = 100 × 1.75 ÷ 2.5 = 70
        last_signal(&mut s, &bars[5..]);
        assert_eq!(s.value(), Some(fx("70")));
    }

    #[test]
    fn rsi_is_none_before_warmup() {
        let mut s = rsi4();
        assert_eq!(s.warmup_bars(), 5);
        assert_eq!(s.value(), None);
        // 4 根 K 線只有 3 筆漲跌幅，種子還沒湊滿
        last_signal(&mut s, &closes(&["100", "101", "102", "103"]));
        assert_eq!(s.value(), None);
    }

    #[test]
    fn only_rising_gives_one_hundred() {
        let mut s = rsi4();
        last_signal(&mut s, &closes(&["10", "11", "12", "13", "14"]));
        assert_eq!(s.value(), Some(fx("100")));
    }

    #[test]
    fn only_falling_gives_zero() {
        let mut s = rsi4();
        last_signal(&mut s, &closes(&["14", "13", "12", "11", "10"]));
        assert_eq!(s.value(), Some(Fixed::ZERO));
    }

    #[test]
    fn a_flat_market_is_neutral_fifty_and_stays_flat() {
        // 漲跌幅全是 0，RSI 在數學上無定義，取 50 → 落在兩個門檻之間 → 空手
        let mut s = rsi4();
        let bars = closes(&["100", "100", "100", "100", "100"]);
        assert_eq!(signals(&mut s, &bars), ".....");
        assert_eq!(s.value(), Some(fx("50")));
    }

    #[test]
    fn stays_flat_until_warmed_up_even_while_crashing() {
        // 前 4 根一路下跌（RSI 若算得出來一定是 0，早就該進場），
        // 但種子還沒湊滿，必須空手
        let mut s = rsi4();
        assert_eq!(signals(&mut s, &closes(&["100", "99", "98", "97"])), "....");
    }

    #[test]
    fn buys_oversold_holds_then_exits_overbought() {
        // 週期 4、門檻 30 / 70，手算每一根：
        //   第 5 根：漲跌幅 −1×4 → 平均漲幅 0、跌幅 1 → RSI 0 ≤ 30 → 進場
        //   第 6 根：+2 → 漲幅 0+(2−0)/4 = 0.5、跌幅 1+(0−1)/4 = 0.75
        //           → RSI = 100 × 0.5 ÷ 1.25 = 40，在兩個門檻之間 → 續抱
        //   第 7 根：+12 → RSI 約 85.7 ≥ 70 → 出場
        let mut s = rsi4();
        let bars = closes(&["100", "99", "98", "97", "96", "98", "110"]);
        assert_eq!(signals(&mut s, &bars), "....LL.");
    }

    #[test]
    fn the_middle_rsi_of_the_hold_case_is_exactly_forty() {
        let mut s = rsi4();
        last_signal(&mut s, &closes(&["100", "99", "98", "97", "96", "98"]));
        assert_eq!(s.value(), Some(fx("40")));
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let mut s = rsi4();
        let _ = signals(&mut s, &extreme_bars());
    }
}
