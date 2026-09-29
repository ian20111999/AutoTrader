//! 回測迴圈：只做多，訊號在收盤產生、下一根開盤成交，成交要付手續費與滑價。
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
//! ## 交易成本：滑價動價格、手續費動金額（2.4）
//!
//! 兩種成本進帳的方式不一樣，混在一起算會得到錯的數字：
//!
//! - **滑價改的是成交價**：買進成交在 `開盤價 × (1 + 滑價)`、賣出成交在
//!   `開盤價 × (1 − 滑價)`。兩邊都是買貴賣賤，因為滑價的方向不由你決定，
//!   回測要假設它永遠站在對你不利的那一邊。**評價用的收盤價不加滑價**——
//!   那是市價，不是你的成交價。
//! - **手續費改的是金額**：`成交金額 × 費率`，從現金扣。買進時要先算出
//!   「每一顆連手續費的總成本」才知道買得起多少，不然會買到手續費付不出來。
//!
//! 費率一律用 **taker（吃單）**：這一版的成交都是「下一根開盤直接成交」，
//! 也就是市價單。maker 費率要等有限價單與掛單簿模型才用得到。
//!
//! 成本一旦打開，「成交當根的權益 = 起始資金」這個恆等式就不再成立——買進的瞬間
//! 就先虧掉滑價與手續費。想確認記帳邏輯本身沒壞，用
//! [`BacktestConfig::frictionless`] 跑同一份資料：它應該完全複製 2.3 的數字。
//!
//! 還沒有的是：做空與槓桿（2.5）、績效指標（2.6）。
//!
//! ## 只做多、二元部位
//!
//! 目標部位大於 0 一律當成「全部資金做多」，小於等於 0 一律當成空手。
//! 半倉、兩倍槓桿、做空這一版都不處理（2.5 的事），所以帳上只有兩個狀態：
//! 滿倉或空手。

use crate::bar::Bar;
use crate::fees::FeeModel;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};
use crate::types::{Liquidity, Side};
use std::fmt;

/// 一次回測的設定：起始資金與交易成本。
///
/// 包成一個 struct 而不是一直加參數，是因為 2.5 的合約參數（槓桿、資金費）
/// 也要放進來，簽名不必再改一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BacktestConfig {
    /// 起始資金（報價幣，例如 USDT），必須大於 0。
    pub initial_capital: Fixed,
    /// 手續費模型。`None` 表示零費率，只當對照組用。
    pub fees: Option<FeeModel>,
    /// 滑價比例：`0.0005` 就是 0.05%。買貴賣賤，必須在 0（含）與 1（不含）之間。
    pub slippage: Fixed,
}

impl BacktestConfig {
    /// 零手續費、零滑價的理想化設定。
    ///
    /// 只有兩個用途：拿來和含成本的結果對照，以及當記帳邏輯的迴歸基準
    /// （它跑出來的數字必須和 2.3 完全一樣）。**不要拿它評估策略好不好**——
    /// 沒有成本的回測會把一堆高頻進出的爛策略美化成金雞母。
    pub fn frictionless(initial_capital: Fixed) -> BacktestConfig {
        BacktestConfig {
            initial_capital,
            fees: None,
            slippage: Fixed::ZERO,
        }
    }
}

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
    /// 滑價比例不在 0（含）與 1（不含）之間。
    InvalidSlippage,
    /// 吃單費率算不出來，或不在 0（含）與 1（不含）之間。
    InvalidFeeRate,
    /// 第 `index` 根 K 線的價格不是正數：成交用的開盤價或評價用的收盤價。
    NonPositivePrice { index: usize },
    /// 第 `index` 根 K 線的金額計算超出 `Fixed` 可表示的範圍。
    Overflow { index: usize },
}

impl fmt::Display for BacktestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BacktestError::NonPositiveCapital => write!(f, "起始資金必須大於 0"),
            BacktestError::InvalidSlippage => {
                write!(f, "滑價比例必須大於等於 0 且小於 1")
            }
            BacktestError::InvalidFeeRate => {
                write!(f, "吃單費率必須大於等於 0 且小於 1")
            }
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
    config: &BacktestConfig,
) -> Result<Vec<EquityPoint>, BacktestError> {
    if config.initial_capital <= Fixed::ZERO {
        return Err(BacktestError::NonPositiveCapital);
    }
    if config.slippage.is_negative() || config.slippage >= Fixed::ONE {
        return Err(BacktestError::InvalidSlippage);
    }
    // 成交價的滑價倍數：買貴（>1）、賣賤（<1）。滑價在 [0, 1) 之間，兩個乘數都算得出來。
    let buy_price_mult = Fixed::ONE
        .checked_add(config.slippage)
        .ok_or(BacktestError::InvalidSlippage)?;
    let sell_price_mult = Fixed::ONE
        .checked_sub(config.slippage)
        .ok_or(BacktestError::InvalidSlippage)?;

    // 這一版的成交都是市價單，所以只用 taker 費率；買方、賣方費率可能不同，各取一次。
    // 費率整場不變，先算出來也順便讓不合理的費率在第一筆成交之前就被擋掉。
    let (buy_rate, sell_rate) = match &config.fees {
        Some(model) => (
            model
                .rate(Liquidity::Taker, Side::Buy)
                .ok_or(BacktestError::InvalidFeeRate)?,
            model
                .rate(Liquidity::Taker, Side::Sell)
                .ok_or(BacktestError::InvalidFeeRate)?,
        ),
        None => (Fixed::ZERO, Fixed::ZERO),
    };
    if buy_rate.is_negative()
        || buy_rate >= Fixed::ONE
        || sell_rate.is_negative()
        || sell_rate >= Fixed::ONE
    {
        return Err(BacktestError::InvalidFeeRate);
    }
    // 買進時每一顆的總成本倍數：成交價要再加上這一顆的手續費。
    let buy_cost_mult = Fixed::ONE
        .checked_add(buy_rate)
        .ok_or(BacktestError::InvalidFeeRate)?;

    let mut cash = config.initial_capital;
    // 持倉數量（基礎幣，例如 BTC）。只做多，所以永遠 >= 0。
    let mut qty = Fixed::ZERO;
    // 上一根收盤算出、還沒成交的目標部位。第一根之前沒有目標，所以是 None。
    let mut pending: Option<TargetPosition> = None;
    let mut curve = Vec::with_capacity(bars.len());

    for (index, bar) in bars.iter().enumerate() {
        // 先成交上一根留下的目標：用**這根的開盤價**，不是上一根的收盤價。
        if let Some(target) = pending.take() {
            let want_long = target.ratio() > Fixed::ZERO;
            // 成交價 = 這根的開盤價加上滑價。開盤價 ≤ 0 時滑價後仍然 ≤ 0，一起擋掉。
            let fill = bar
                .open
                .checked_mul(if want_long {
                    buy_price_mult
                } else {
                    sell_price_mult
                })
                .ok_or(BacktestError::Overflow { index })?;
            if fill <= Fixed::ZERO {
                return Err(BacktestError::NonPositivePrice { index });
            }
            if want_long && qty.is_zero() {
                // 先用「成交價 + 手續費」當單價，算出這些現金真正買得起多少，
                // 再回頭用成交價算貨款、用貨款算手續費。順序反了會買到付不出手續費。
                let cost_per_unit = fill
                    .checked_mul(buy_cost_mult)
                    .ok_or(BacktestError::Overflow { index })?;
                let bought = cash
                    .checked_div(cost_per_unit)
                    .ok_or(BacktestError::Overflow { index })?;
                let spent = bought
                    .checked_mul(fill)
                    .ok_or(BacktestError::Overflow { index })?;
                let fee = spent
                    .checked_mul(buy_rate)
                    .ok_or(BacktestError::Overflow { index })?;
                // 數量四捨五入到 8 位後換不掉的零頭留在現金裡，
                // 權益才不會在成交當根因為進位憑空多出或少掉一點。
                cash = cash
                    .checked_sub(spent)
                    .and_then(|c| c.checked_sub(fee))
                    .ok_or(BacktestError::Overflow { index })?;
                qty = bought;
            } else if !want_long && !qty.is_zero() {
                let proceeds = qty
                    .checked_mul(fill)
                    .ok_or(BacktestError::Overflow { index })?;
                let fee = proceeds
                    .checked_mul(sell_rate)
                    .ok_or(BacktestError::Overflow { index })?;
                cash = cash
                    .checked_add(proceeds)
                    .and_then(|c| c.checked_sub(fee))
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
    use crate::fees::FuturesFees;
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

    /// 零費率、零滑價：2.3 留下來的行為測試全部用這個設定，數字才對得回 2.3。
    fn frictionless(capital: &str) -> BacktestConfig {
        BacktestConfig::frictionless(fx(capital))
    }

    /// Binance 現貨 VIP 0（吃單 0.1%）+ 指定滑價。
    fn with_costs(capital: &str, slippage: &str) -> BacktestConfig {
        BacktestConfig {
            initial_capital: fx(capital),
            fees: Some(FeeModel::spot_vip0()),
            slippage: fx(slippage),
        }
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
        assert_eq!(
            run_backtest(&[], &mut AlwaysLong, &frictionless("10000")),
            Ok(vec![])
        );
    }

    #[test]
    fn flat_strategy_keeps_equity_flat() {
        let curve = run_backtest(
            &bars(&["100", "110", "121"]),
            &mut AlwaysFlat,
            &frictionless("10000"),
        )
        .unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"); 3]);
    }

    #[test]
    fn entry_fills_at_the_next_open_not_at_this_close() {
        // 第 0 根：開 100 收 125，策略在收盤說做多 → 不成交，只記帳。
        // 第 1 根：開 100 → 用 100 把 10000 換成 100 顆；收 200 → 權益 100 × 200 = 20000。
        let curve = run_backtest(
            &oc_bars(&[("100", "125"), ("100", "200")]),
            &mut AlwaysLong,
            &frictionless("10000"),
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
            &frictionless("10000"),
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
        let curve = run_backtest(
            &oc_bars(&[("100", "500")]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap();
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
            &frictionless("10000"),
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
            &frictionless("10000"),
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
        let curve = run_backtest(
            &bars(&["100", "50"]),
            &mut AlwaysShort,
            &frictionless("10000"),
        )
        .unwrap();
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000")]);
    }

    #[test]
    fn rounding_leftover_stays_in_cash() {
        // 第 1 根開盤 3 買進：10000 ÷ 3 = 3333.33333333（8 位），
        // 買不完的 0.00000001 留在現金，所以成交當根的權益仍然剛好是起始資金
        let curve = run_backtest(
            &bars(&["3", "3", "6"]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap();
        assert_eq!(curve[1].equity, fx("10000"));
        // 價格翻倍：3333.33333333 × 6 + 0.00000001 = 19999.99999999
        assert_eq!(curve[2].equity, fx("19999.99999999"));
    }

    #[test]
    fn non_positive_capital_is_rejected() {
        assert_eq!(
            run_backtest(
                &bars(&["100"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(Fixed::ZERO)
            ),
            Err(BacktestError::NonPositiveCapital)
        );
        assert_eq!(
            run_backtest(
                &bars(&["100"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(fx("-1"))
            ),
            Err(BacktestError::NonPositiveCapital)
        );
    }

    #[test]
    fn non_positive_price_is_rejected() {
        // 評價用的收盤價
        let mut bad_close = bars(&["100", "110"]);
        bad_close[1].close = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_close, &mut AlwaysLong, &frictionless("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 成交用的開盤價（第 0 根收盤的目標要在這裡成交）
        let mut bad_open = bars(&["100", "110"]);
        bad_open[1].open = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_open, &mut AlwaysLong, &frictionless("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 第一根永遠不成交，它的開盤價用不到，所以不檢查也不會拿它算出錯誤的成交
        let mut zero_first_open = bars(&["100", "110"]);
        zero_first_open[0].open = Fixed::ZERO;
        assert!(run_backtest(&zero_first_open, &mut AlwaysLong, &frictionless("10000")).is_ok());
    }

    #[test]
    fn extreme_numbers_return_an_error_instead_of_panicking() {
        // 天價資金 ÷ 極小價格 = 數量爆掉，要回錯誤而不是溢位 panic。
        // 成交發生在第 1 根的開盤，所以出錯的是 index 1（2.2 是 index 0）。
        assert_eq!(
            run_backtest(
                &bars(&["0.00000001", "0.00000001"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(Fixed::from_raw(i64::MAX))
            ),
            Err(BacktestError::Overflow { index: 1 })
        );
    }

    /// 迴歸基準：零費率 + 零滑價必須完整複製 2.3 的數字。
    ///
    /// 成本一打開，「成交當根的權益 = 起始資金」就不再成立，光看數字變了沒辦法
    /// 分辨是真的扣了成本、還是記帳邏輯壞了。所以這條測試要一直留著：它固定住
    /// 「成本歸零時的行為」，任何讓它變色的改動都是記帳邏輯本身出了問題。
    #[test]
    fn zero_cost_reproduces_the_2_3_numbers() {
        let data = oc_bars(&[("100", "100"), ("100", "200"), ("150", "300")]);
        let mut s = LongThenFlat {
            long_bars: 1,
            seen: 0,
        };
        // 和 exit_fills_at_the_next_open_so_the_gap_counts 同一組資料、同一組答案
        let curve = run_backtest(&data, &mut s, &frictionless("10000")).unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("20000"), fx("15000")]
        );

        // 同一份資料加上成本，答案一定變差；兩者相同就表示成本根本沒被算進去
        let mut s = LongThenFlat {
            long_bars: 1,
            seen: 0,
        };
        let with_cost = run_backtest(&data, &mut s, &with_costs("10000", "0.0005")).unwrap();
        assert!(
            with_cost.last().unwrap().equity < fx("15000"),
            "含成本的權益 {} 應該比無成本的 15000 低",
            with_cost.last().unwrap().equity
        );
    }

    #[test]
    fn slippage_alone_makes_you_buy_dearer() {
        // 滑價 0.05%：開盤價 100 的買單成交在 100 × 1.0005 = 100.05。
        // 起始資金剛好 10005 = 100.05 × 100，所以買到 100 顆、現金歸零。
        // 收盤市價還是 100（評價不加滑價），權益 100 × 100 = 10000，
        // 也就是一買進就先少了 5 = 10000 × 0.05%。
        let cfg = BacktestConfig {
            initial_capital: fx("10005"),
            fees: None,
            slippage: fx("0.0005"),
        };
        let curve = run_backtest(&bars(&["100", "100"]), &mut AlwaysLong, &cfg).unwrap();
        assert_eq!(equities(&curve), vec![fx("10005"), fx("10000")]);
    }

    #[test]
    fn fee_alone_is_charged_on_the_notional() {
        // 吃單 0.1%、無滑價：每一顆的總成本是 100 × 1.001 = 100.1，
        // 起始資金 10010 剛好買 100 顆 → 貨款 10000、手續費 10、現金歸零。
        // 收盤權益 10000，少掉的 10 就是 10000 × 0.1%。
        let cfg = BacktestConfig {
            initial_capital: fx("10010"),
            fees: Some(FeeModel::spot_vip0()),
            slippage: Fixed::ZERO,
        };
        let curve = run_backtest(&bars(&["100", "100"]), &mut AlwaysLong, &cfg).unwrap();
        assert_eq!(equities(&curve), vec![fx("10010"), fx("10000")]);
    }

    /// 手算一趟完整的來回：價格完全不動，成本自己就能把帳戶吃掉 30。
    ///
    /// 吃單 0.1%、滑價 0.05%、起始資金 10015.005（挑這個數字是為了讓數量剛好 100 顆，
    /// 不用跟四捨五入的零頭糾纏），四根 K 線的開盤與收盤都是 100。
    ///
    /// | K 線 | 這根做的事 | 手算 | 權益 |
    /// |---|---|---|---|
    /// | 0 | 收盤說做多 → 記著 | — | 10015.005 |
    /// | 1 | 買：成交價 100 × 1.0005 = 100.05；每顆總成本 100.05 × 1.001 = 100.15005；數量 10015.005 ÷ 100.15005 = 100；貨款 10005、手續費 10.005 → 現金 0 | 0 + 100 × 100 | 10000 |
    /// | 2 | 賣：成交價 100 × 0.9995 = 99.95；收入 9995、手續費 9.995 → 現金 9985.005 | — | 9985.005 |
    /// | 3 | 空手 | — | 9985.005 |
    ///
    /// 來回總成本 10015.005 − 9985.005 = 30，四筆拆開剛好對得起來：
    /// 買滑價 5 + 買手續費 10.005 + 賣滑價 5 + 賣手續費 9.995 = 30。
    #[test]
    fn a_round_trip_on_a_flat_market_costs_exactly_the_four_charges() {
        let mut s = LongThenFlat {
            long_bars: 1,
            seen: 0,
        };
        let curve = run_backtest(
            &bars(&["100", "100", "100", "100"]),
            &mut s,
            &with_costs("10015.005", "0.0005"),
        )
        .unwrap();
        assert_eq!(
            equities(&curve),
            vec![fx("10015.005"), fx("10000"), fx("9985.005"), fx("9985.005")]
        );
        // 四筆費用各自對得起來
        let buy_slip = fx("10000").checked_mul(fx("0.0005")).unwrap(); // 5
        let buy_fee = fx("10005").checked_mul(fx("0.001")).unwrap(); // 10.005
        let sell_slip = fx("10000").checked_mul(fx("0.0005")).unwrap(); // 5
        let sell_fee = fx("9995").checked_mul(fx("0.001")).unwrap(); // 9.995
        let total = [buy_slip, buy_fee, sell_slip, sell_fee]
            .into_iter()
            .fold(Fixed::ZERO, |a, b| a.checked_add(b).unwrap());
        assert_eq!(total, fx("30"));
        assert_eq!(fx("10015.005").checked_sub(curve[2].equity), Some(fx("30")));
    }

    #[test]
    fn awkward_prices_still_cost_only_slippage_and_fee() {
        // 價格難整除（3）、費率與滑價都不是好數字：現金不可以被扣成負數，
        // 否則就是「買了付不出手續費」，帳上會多出一筆不存在的錢。
        let cfg = BacktestConfig {
            initial_capital: fx("10000"),
            fees: Some(FeeModel::spot_vip0()),
            slippage: fx("0.00037"),
        };
        let curve = run_backtest(&bars(&["3", "3", "3"]), &mut AlwaysLong, &cfg).unwrap();
        // 買在 3 × 1.00037 = 3.00111，收盤評價回 3，所以權益一定比起始資金低
        assert!(curve[1].equity < fx("10000"));
        // 低的幅度不該超過滑價 + 手續費（10000 × (0.00037 + 0.001) 約 13.7）
        assert!(curve[1].equity > fx("9986"));
        assert_eq!(curve[1].equity, curve[2].equity);
    }

    #[test]
    fn invalid_slippage_is_rejected() {
        let bad = |s: &str| BacktestConfig {
            initial_capital: fx("10000"),
            fees: None,
            slippage: fx(s),
        };
        // 負滑價等於「每筆都買便宜賣貴」，回測會憑空生錢
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("-0.0001")),
            Err(BacktestError::InvalidSlippage)
        );
        // 滑價 100% 會讓賣出價變成 0
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("1")),
            Err(BacktestError::InvalidSlippage)
        );
    }

    #[test]
    fn invalid_taker_rate_is_rejected() {
        let with_taker = |taker: &str| BacktestConfig {
            initial_capital: fx("10000"),
            fees: Some(FeeModel::Futures(FuturesFees {
                maker: fx("-0.00005"), // 掛單返佣是真的存在，不該被這個檢查擋到
                taker: fx(taker),
                bnb_discount: None,
            })),
            slippage: Fixed::ZERO,
        };
        // 吃單返佣不存在：負費率會讓回測以為交易本身就能賺錢
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("-0.0001")),
            Err(BacktestError::InvalidFeeRate)
        );
        // 費率 100% 表示一筆成交把本金全部吃掉，一定是填錯（例如把 0.1% 寫成 100）
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("1")),
            Err(BacktestError::InvalidFeeRate)
        );
        // 正常的吃單費率照跑
        assert!(run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("0.0005")).is_ok());
    }

    #[test]
    fn works_through_a_boxed_strategy() {
        // 之後的策略庫會拿 Box<dyn Strategy> 跑同一份資料
        let mut boxed: Box<dyn Strategy> = Box::new(AlwaysLong);
        let curve = run_backtest(
            &bars(&["100", "100", "200"]),
            boxed.as_mut(),
            &frictionless("1000"),
        )
        .unwrap();
        assert_eq!(equities(&curve), vec![fx("1000"), fx("1000"), fx("2000")]);
    }
}
