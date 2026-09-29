//! 回測迴圈：只做多、不計成本，訊號在收盤產生、下一根開盤成交。
//!
//! 做的事只有三件：把 K 線一根一根餵給策略、策略說做多就把現金全部換成部位
//! （說空手就全部換回現金）、每根收盤記一次帳戶總值。那串總值就是**權益曲線**，
//! 也是 2.6 績效指標唯一需要的輸入。
//!
//! ## 成交時點：決定與成交永遠隔一根（2.3）
//!
//! 每根 K 線的順序固定是三步：**先成交上一根留下的目標（用這根的開盤價）
//! → 用這根的收盤價評價、記一點權益 → 才問策略這根收盤想要什麼部位**。
//!
//! 這樣寫是因為現實就是這樣：收盤價要等收盤才知道，看到它才決定的單子，
//! 最快也只能送到下一根開盤。2.2 用同一根的收盤價成交，等於「看完收盤價才決定、
//! 卻還拿得到那個價格」——回測最典型的偷看未來（look-ahead bias）。
//!
//! 代價是決定與成交之間的跳空要自己承受：第 N 根收盤看到的價格和第 N+1 根
//! 開盤真正成交的價格可以差很多，而且不保證對自己有利。這正是回測要算進去的東西。
//!
//! 最後一根收盤產生的目標沒有下一根可以成交，直接作廢——它只是「還沒來得及執行」，
//! 不影響已經記完的權益曲線。
//!
//! 還沒有的是：手續費與滑價（2.4）、做空與槓桿（2.5）、績效指標（2.6）。
//!
//! ## 只做多、二元部位
//!
//! 目標部位大於 0 一律當成「全部資金做多」，小於等於 0 一律當成空手。
//! 半倉、兩倍槓桿、做空這一版都不處理（2.5 的事），所以帳上只有兩個狀態：
//! 滿倉或空手。

use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};
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
    /// 第 `index` 根 K 線的價格不是正數：成交用的開盤價或評價用的收盤價。
    NonPositivePrice { index: usize },
    /// 第 `index` 根 K 線的金額計算超出 `Fixed` 可表示的範圍。
    Overflow { index: usize },
}

impl fmt::Display for BacktestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BacktestError::NonPositiveCapital => write!(f, "起始資金必須大於 0"),
            BacktestError::NonPositivePrice { index } => {
                write!(f, "第 {index} 根 K 線的價格不是正數，不能拿來成交或評價")
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
    // 上一根收盤算出、還沒成交的目標部位。第一根之前沒有目標，所以是 None。
    let mut pending: Option<TargetPosition> = None;
    let mut curve = Vec::with_capacity(bars.len());

    for (index, bar) in bars.iter().enumerate() {
        // 先成交上一根留下的目標：用**這根的開盤價**，不是上一根的收盤價。
        if let Some(target) = pending.take() {
            let fill = bar.open;
            if fill <= Fixed::ZERO {
                return Err(BacktestError::NonPositivePrice { index });
            }
            let want_long = target.ratio() > Fixed::ZERO;
            if want_long && qty.is_zero() {
                let bought = cash
                    .checked_div(fill)
                    .ok_or(BacktestError::Overflow { index })?;
                let spent = bought
                    .checked_mul(fill)
                    .ok_or(BacktestError::Overflow { index })?;
                // 數量四捨五入到 8 位後換不掉的零頭留在現金裡，
                // 權益才不會在成交當根因為進位憑空多出或少掉一點。
                cash = cash
                    .checked_sub(spent)
                    .ok_or(BacktestError::Overflow { index })?;
                qty = bought;
            } else if !want_long && !qty.is_zero() {
                let proceeds = qty
                    .checked_mul(fill)
                    .ok_or(BacktestError::Overflow { index })?;
                cash = cash
                    .checked_add(proceeds)
                    .ok_or(BacktestError::Overflow { index })?;
                qty = Fixed::ZERO;
            }
        }

        let price = bar.close;
        if price <= Fixed::ZERO {
            return Err(BacktestError::NonPositivePrice { index });
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

        // 這根收盤才問策略，答案留到下一根開盤成交。
        // 最後一根的目標沒有下一根可以成交，迴圈結束時直接連同 `pending` 作廢。
        pending = Some(strategy.on_bar(bar));
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

    /// 一串 `open != close` 的 K 線：每個元素是 `(開盤價, 收盤價)`。
    ///
    /// 成交價來自**下一根的開盤**、評價價來自**這根的收盤**，兩者不同才分得出
    /// 成交時點有沒有做對，所以這一步的行為測試都用這個輔助函式，不用 `bars()`。
    fn oc_bars(pairs: &[(&str, &str)]) -> Vec<Bar> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (o, c))| {
                let open = fx(o);
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * 3_600_000,
                    open,
                    high: open.max(close),
                    low: open.min(close),
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
    fn entry_fills_at_the_next_open_not_at_this_close() {
        // 第 0 根：開 100 收 125，策略在收盤說做多 → 不成交，只記帳。
        // 第 1 根：開 100 → 用 100 把 10000 換成 100 顆；收 200 → 權益 100 × 200 = 20000。
        let curve = run_backtest(
            &oc_bars(&[("100", "125"), ("100", "200")]),
            &mut AlwaysLong,
            fx("10000"),
        )
        .unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"), fx("20000")]);
        // 舊模型（本根收盤 125 成交）會買到 10000 ÷ 125 = 80 顆，最後是 80 × 200 = 16000。
        // 兩個答案不同，這條測試才有意義。
        assert_ne!(curve[1].equity, fx("16000"));
        // 時間戳要跟著 K 線走，2.6 算年化要用
        assert_eq!(curve[1].open_time, T0 + 3_600_000);
    }

    #[test]
    fn exit_fills_at_the_next_open_so_the_gap_counts() {
        // 第 0 根（開 100 收 100）：說做多 → 待成交。
        // 第 1 根（開 100 收 200）：開盤 100 買 100 顆，收盤權益 20000；收盤說空手 → 待成交。
        // 第 2 根（開 150 收 300）：開盤 150 賣掉 → 現金 15000，之後漲到 300 都與我無關。
        //
        // 賣在 150 不是賣在第 1 根的收盤 200：決定與成交之間隔著一次跳空，
        // 這 5000 的價差就是「訊號價不等於成交價」的實測。
        let mut s = LongThenFlat {
            long_bars: 1,
            seen: 0,
        };
        let curve = run_backtest(
            &oc_bars(&[("100", "100"), ("100", "200"), ("150", "300")]),
            &mut s,
            fx("10000"),
        )
        .unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("20000"), fx("15000")]
        );
    }

    #[test]
    fn a_single_bar_never_trades() {
        // 只有一根：收盤算出的目標沒有下一根可以成交，直接作廢。
        // 所以即使策略喊做多、價格從 100 衝到 500，權益仍然是起始資金（而且不 panic）。
        let curve =
            run_backtest(&oc_bars(&[("100", "500")]), &mut AlwaysLong, fx("10000")).unwrap();
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
        // 第 0 根只觀察，第 1 根開盤 100 買進，之後每根漲 10%：
        // 10000 → 10000 → 11000 → 12100
        let curve = run_backtest(
            &bars(&["100", "100", "110", "121"]),
            &mut AlwaysLong,
            fx("10000"),
        )
        .unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("11000"), fx("12100")]
        );
        // 手算對照：10000 × 1.1 × 1.1
        let by_hand = fx("10000")
            .checked_mul(fx("1.1"))
            .unwrap()
            .checked_mul(fx("1.1"))
            .unwrap();
        assert_eq!(curve.last().unwrap().equity, by_hand);
    }

    #[test]
    fn selling_locks_in_the_gain() {
        // 第 1 根開盤 100 買進、第 3 根開盤 110 賣出（決定是在第 2 根收盤下的），
        // 之後崩到 50 也不再影響權益
        let mut s = LongThenFlat {
            long_bars: 2,
            seen: 0,
        };
        let curve = run_backtest(
            &bars(&["100", "100", "110", "110", "50"]),
            &mut s,
            fx("10000"),
        )
        .unwrap();
        assert_eq!(
            equities(&curve),
            vec![
                fx("10000"),
                fx("10000"),
                fx("11000"),
                fx("11000"),
                fx("11000")
            ]
        );
    }

    #[test]
    fn short_target_is_treated_as_flat_for_now() {
        // 這一版只做多：做空訊號當成空手，不是反向獲利（2.5 才做）
        let curve = run_backtest(&bars(&["100", "50"]), &mut AlwaysShort, fx("10000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000")]);
    }

    #[test]
    fn rounding_leftover_stays_in_cash() {
        // 第 1 根開盤 3 買進：10000 ÷ 3 = 3333.33333333（8 位），
        // 買不完的 0.00000001 留在現金，所以成交當根的權益仍然剛好是起始資金
        let curve = run_backtest(&bars(&["3", "3", "6"]), &mut AlwaysLong, fx("10000")).unwrap();
        assert_eq!(curve[1].equity, fx("10000"));
        // 價格翻倍：3333.33333333 × 6 + 0.00000001 = 19999.99999999
        assert_eq!(curve[2].equity, fx("19999.99999999"));
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
        // 評價用的收盤價
        let mut bad_close = bars(&["100", "110"]);
        bad_close[1].close = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_close, &mut AlwaysLong, fx("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 成交用的開盤價（第 0 根收盤的目標要在這裡成交）
        let mut bad_open = bars(&["100", "110"]);
        bad_open[1].open = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_open, &mut AlwaysLong, fx("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 第一根永遠不成交，它的開盤價用不到，所以不檢查也不會拿它算出錯誤的成交
        let mut zero_first_open = bars(&["100", "110"]);
        zero_first_open[0].open = Fixed::ZERO;
        assert!(run_backtest(&zero_first_open, &mut AlwaysLong, fx("10000")).is_ok());
    }

    #[test]
    fn extreme_numbers_return_an_error_instead_of_panicking() {
        // 天價資金 ÷ 極小價格 = 數量爆掉，要回錯誤而不是溢位 panic。
        // 成交發生在第 1 根的開盤，所以出錯的是 index 1（2.2 是 index 0）。
        assert_eq!(
            run_backtest(
                &bars(&["0.00000001", "0.00000001"]),
                &mut AlwaysLong,
                Fixed::from_raw(i64::MAX)
            ),
            Err(BacktestError::Overflow { index: 1 })
        );
    }

    #[test]
    fn works_through_a_boxed_strategy() {
        // 之後的策略庫會拿 Box<dyn Strategy> 跑同一份資料
        let mut boxed: Box<dyn Strategy> = Box::new(AlwaysLong);
        let curve =
            run_backtest(&bars(&["100", "100", "200"]), boxed.as_mut(), fx("1000")).unwrap();
        assert_eq!(equities(&curve), vec![fx("1000"), fx("1000"), fx("2000")]);
    }
}
