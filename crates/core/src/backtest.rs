//! 最簡回測迴圈：只做多、不計成本。
//!
//! 做的事只有三件：把 K 線一根一根餵給策略、策略說做多就把現金全部換成部位
//! （說空手就全部換回現金）、每根收盤記一次帳戶總值。那串總值就是**權益曲線**，
//! 也是 2.6 績效指標唯一需要的輸入。
//!
//! ## 這一版的成交價刻意是樂觀的，不可當成真實績效
//!
//! 訊號在收盤產生，就用**同一根的收盤價**成交——等於「看完收盤價才決定，
//! 卻還拿得到這個價格」，這是回測最典型的偷看未來（look-ahead bias）。
//! 2.3 會改成「下一根開盤成交」，數字一定會變差，那才是可信的版本。
//!
//! 先寫這一版，是為了把「餵 K 線 → 記帳 → 權益曲線」這條骨架單獨釘住：
//! 一步只改一件事，之後 2.3 改成交時點、2.4 加手續費與滑價、2.5 加做空與槓桿，
//! 每一步造成的數字變化才能各自解釋。
//!
//! ## 只做多、二元部位
//!
//! 目標部位大於 0 一律當成「全部資金做多」，小於等於 0 一律當成空手。
//! 半倉、兩倍槓桿、做空這一版都不處理（2.5 的事），所以帳上只有兩個狀態：
//! 滿倉或空手。

use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::Strategy;
use std::fmt;

/// 權益曲線上的一點。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquityPoint {
    /// 對應 K 線的開盤時間（UTC 毫秒），和 [`Bar::open_time`] 一致。
    pub open_time: i64,
    /// 這根 K 線**收盤時**的帳戶總值 = 現金 + 持倉市值（以收盤價計）。
    pub equity: Fixed,
}

/// 回測跑不下去的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacktestError {
    /// 起始資金必須大於 0。
    NonPositiveCapital,
    /// 第 `index` 根 K 線的收盤價不是正數，不能當成成交價。
    NonPositivePrice { index: usize },
    /// 第 `index` 根 K 線的金額計算超出 `Fixed` 可表示的範圍。
    Overflow { index: usize },
}

impl fmt::Display for BacktestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BacktestError::NonPositiveCapital => write!(f, "起始資金必須大於 0"),
            BacktestError::NonPositivePrice { index } => {
                write!(f, "第 {index} 根 K 線的收盤價不是正數，不能當成成交價")
            }
            BacktestError::Overflow { index } => {
                write!(f, "第 {index} 根 K 線的金額計算超出可表示範圍")
            }
        }
    }
}

impl std::error::Error for BacktestError {}

/// 跑一次回測，回傳每根 K 線收盤時的權益。
///
/// `bars` 要是同一個交易對與週期、時間遞增的連續 K 線（用 [`crate::find_gaps`] 先檢查）。
/// 空的 `bars` 不是錯誤，回傳空曲線。
pub fn run_backtest(
    bars: &[Bar],
    strategy: &mut dyn Strategy,
    initial_capital: Fixed,
) -> Result<Vec<EquityPoint>, BacktestError> {
    if initial_capital <= Fixed::ZERO {
        return Err(BacktestError::NonPositiveCapital);
    }

    let mut cash = initial_capital;
    // 持倉數量（基礎幣，例如 BTC）。只做多，所以永遠 >= 0。
    let mut qty = Fixed::ZERO;
    let mut curve = Vec::with_capacity(bars.len());

    for (index, bar) in bars.iter().enumerate() {
        let price = bar.close;
        if price <= Fixed::ZERO {
            return Err(BacktestError::NonPositivePrice { index });
        }

        let want_long = strategy.on_bar(bar).ratio() > Fixed::ZERO;
        if want_long && qty.is_zero() {
            let bought = cash
                .checked_div(price)
                .ok_or(BacktestError::Overflow { index })?;
            let spent = bought
                .checked_mul(price)
                .ok_or(BacktestError::Overflow { index })?;
            // 數量四捨五入到 8 位後換不掉的零頭留在現金裡，
            // 權益才不會在成交當根因為進位憑空多出或少掉一點。
            cash = cash
                .checked_sub(spent)
                .ok_or(BacktestError::Overflow { index })?;
            qty = bought;
        } else if !want_long && !qty.is_zero() {
            let proceeds = qty
                .checked_mul(price)
                .ok_or(BacktestError::Overflow { index })?;
            cash = cash
                .checked_add(proceeds)
                .ok_or(BacktestError::Overflow { index })?;
            qty = Fixed::ZERO;
        }

        let position_value = qty
            .checked_mul(price)
            .ok_or(BacktestError::Overflow { index })?;
        let equity = cash
            .checked_add(position_value)
            .ok_or(BacktestError::Overflow { index })?;
        curve.push(EquityPoint {
            open_time: bar.open_time,
            equity,
        });
    }

    Ok(curve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::TargetPosition;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;

    /// 一串只有收盤價有意義的 K 線（這一版只用得到收盤價）。
    fn bars(closes: &[&str]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * 3_600_000,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                }
            })
            .collect()
    }

    fn equities(curve: &[EquityPoint]) -> Vec<Fixed> {
        curve.iter().map(|p| p.equity).collect()
    }

    struct AlwaysLong;
    impl Strategy for AlwaysLong {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FULL_LONG
        }
    }

    struct AlwaysFlat;
    impl Strategy for AlwaysFlat {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FLAT
        }
    }

    struct AlwaysShort;
    impl Strategy for AlwaysShort {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::short(Fixed::ONE)
        }
    }

    /// 前 `long_bars` 根做多，之後空手。
    struct LongThenFlat {
        long_bars: usize,
        seen: usize,
    }
    impl Strategy for LongThenFlat {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            self.seen += 1;
            if self.seen <= self.long_bars {
                TargetPosition::FULL_LONG
            } else {
                TargetPosition::FLAT
            }
        }
    }

    #[test]
    fn empty_bars_give_an_empty_curve() {
        // 沒資料不是錯誤，也不能 panic
        assert_eq!(run_backtest(&[], &mut AlwaysLong, fx("10000")), Ok(vec![]));
    }

    #[test]
    fn flat_strategy_keeps_equity_flat() {
        let curve =
            run_backtest(&bars(&["100", "110", "121"]), &mut AlwaysFlat, fx("10000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"); 3]);
    }

    #[test]
    fn single_bar_ends_with_the_starting_capital() {
        // 只有一根：在收盤買進，當根還來不及有損益
        let curve = run_backtest(&bars(&["100"]), &mut AlwaysLong, fx("10000")).unwrap();
        assert_eq!(
            curve,
            vec![EquityPoint {
                open_time: T0,
                equity: fx("10000"),
            }]
        );
    }

    #[test]
    fn all_in_long_compounds_price_moves() {
        // 100 → 110 → 121，各漲 10%：10000 → 10000 → 11000 → 12100
        // 第一根是買進當根，所以還是 10000
        let curve =
            run_backtest(&bars(&["100", "110", "121"]), &mut AlwaysLong, fx("10000")).unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("11000"), fx("12100")]
        );
        // 手算對照：10000 × 1.1 × 1.1
        let by_hand = fx("10000")
            .checked_mul(fx("1.1"))
            .unwrap()
            .checked_mul(fx("1.1"))
            .unwrap();
        assert_eq!(curve.last().unwrap().equity, by_hand);
        // 時間戳要跟著 K 線走，2.6 算年化要用
        assert_eq!(curve[2].open_time, T0 + 2 * 3_600_000);
    }

    #[test]
    fn selling_locks_in_the_gain() {
        // 前兩根做多（100 → 110 賺 10%），第三根收盤賣掉（價格還在 110），
        // 之後崩到 50 也不再影響權益
        let mut s = LongThenFlat {
            long_bars: 2,
            seen: 0,
        };
        let curve = run_backtest(&bars(&["100", "110", "110", "50"]), &mut s, fx("10000")).unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("11000"), fx("11000"), fx("11000")]
        );
    }

    #[test]
    fn the_exit_bar_own_move_still_counts() {
        // 部位一直持有到「決定空手那根的收盤」才賣掉，所以那根自己的跌幅照算：
        // 110 → 50 權益從 11000 掉到 5000。這是這一版成交模型的直接後果；
        // 2.3 改成下一根開盤成交後，這裡會變成用下一根開盤價賣的結果。
        let mut s = LongThenFlat {
            long_bars: 2,
            seen: 0,
        };
        let curve = run_backtest(&bars(&["100", "110", "50"]), &mut s, fx("10000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"), fx("11000"), fx("5000")]);
    }

    #[test]
    fn short_target_is_treated_as_flat_for_now() {
        // 這一版只做多：做空訊號當成空手，不是反向獲利（2.5 才做）
        let curve = run_backtest(&bars(&["100", "50"]), &mut AlwaysShort, fx("10000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000")]);
    }

    #[test]
    fn rounding_leftover_stays_in_cash() {
        // 10000 ÷ 3 = 3333.33333333（8 位），買不完的 0.00000001 留在現金，
        // 所以成交當根的權益仍然剛好是起始資金
        let curve = run_backtest(&bars(&["3", "6"]), &mut AlwaysLong, fx("10000")).unwrap();
        assert_eq!(curve[0].equity, fx("10000"));
        // 價格翻倍：3333.33333333 × 6 + 0.00000001 = 19999.99999999
        assert_eq!(curve[1].equity, fx("19999.99999999"));
    }

    #[test]
    fn non_positive_capital_is_rejected() {
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, Fixed::ZERO),
            Err(BacktestError::NonPositiveCapital)
        );
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, fx("-1")),
            Err(BacktestError::NonPositiveCapital)
        );
    }

    #[test]
    fn non_positive_price_is_rejected() {
        let mut bad = bars(&["100", "110"]);
        bad[1].close = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad, &mut AlwaysLong, fx("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
    }

    #[test]
    fn extreme_numbers_return_an_error_instead_of_panicking() {
        // 天價資金 ÷ 極小價格 = 數量爆掉，要回錯誤而不是溢位 panic
        assert_eq!(
            run_backtest(
                &bars(&["0.00000001"]),
                &mut AlwaysLong,
                Fixed::from_raw(i64::MAX)
            ),
            Err(BacktestError::Overflow { index: 0 })
        );
    }

    #[test]
    fn works_through_a_boxed_strategy() {
        // 之後的策略庫會拿 Box<dyn Strategy> 跑同一份資料
        let mut boxed: Box<dyn Strategy> = Box::new(AlwaysLong);
        let curve = run_backtest(&bars(&["100", "200"]), boxed.as_mut(), fx("1000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("1000"), fx("2000")]);
    }
}
