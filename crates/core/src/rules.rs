//! 交易規則：交易所對每個交易對的下單限制。
//!
//! 對應 Binance `exchangeInfo` 裡的幾個 filter：
//!
//! | 欄位 | Binance filter | 意思 |
//! |---|---|---|
//! | `tick_size` | `PRICE_FILTER.tickSize` | 價格必須是它的倍數 |
//! | `step_size` | `LOT_SIZE.stepSize` | 數量必須是它的倍數 |
//! | `min_qty` / `max_qty` | `LOT_SIZE` | 數量上下限 |
//! | `min_notional` | `NOTIONAL` / `MIN_NOTIONAL` | 價格 × 數量的最低金額 |
//!
//! 不合規的單會被交易所直接拒絕，所以回測和模擬也要套用同一套規則，
//! 否則回測裡會出現「真實世界下不了的單」。

use crate::fixed::Fixed;
use crate::types::Side;
use std::fmt;

/// 一個交易對的下單規則。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolRules {
    pub tick_size: Fixed,
    pub step_size: Fixed,
    pub min_qty: Fixed,
    pub max_qty: Fixed,
    pub min_notional: Fixed,
}

/// 建立規則時的設定錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesError {
    /// 價格跳動或數量級距 ≤ 0。
    NonPositiveStep,
    /// 最小數量大於最大數量。
    MinAboveMax,
}

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RulesError::NonPositiveStep => write!(f, "價格跳動與數量級距必須大於 0"),
            RulesError::MinAboveMax => write!(f, "最小數量不可大於最大數量"),
        }
    }
}

impl std::error::Error for RulesError {}

/// 一筆單不合規的原因，附上實際數值方便除錯。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleViolation {
    NonPositivePrice {
        price: Fixed,
    },
    PriceNotOnTick {
        price: Fixed,
        tick: Fixed,
    },
    QtyNotOnStep {
        qty: Fixed,
        step: Fixed,
    },
    QtyTooSmall {
        qty: Fixed,
        min: Fixed,
    },
    QtyTooLarge {
        qty: Fixed,
        max: Fixed,
    },
    NotionalTooSmall {
        notional: Fixed,
        min: Fixed,
    },
    /// 價格 × 數量超出可表示範圍。
    Overflow,
}

impl fmt::Display for RuleViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleViolation::NonPositivePrice { price } => write!(f, "價格 {price} 必須大於 0"),
            RuleViolation::PriceNotOnTick { price, tick } => {
                write!(f, "價格 {price} 不是價格跳動 {tick} 的倍數")
            }
            RuleViolation::QtyNotOnStep { qty, step } => {
                write!(f, "數量 {qty} 不是數量級距 {step} 的倍數")
            }
            RuleViolation::QtyTooSmall { qty, min } => write!(f, "數量 {qty} 小於最小數量 {min}"),
            RuleViolation::QtyTooLarge { qty, max } => write!(f, "數量 {qty} 大於最大數量 {max}"),
            RuleViolation::NotionalTooSmall { notional, min } => {
                write!(f, "金額 {notional} 小於最小金額 {min}")
            }
            RuleViolation::Overflow => write!(f, "價格 × 數量超出可表示範圍"),
        }
    }
}

impl std::error::Error for RuleViolation {}

impl SymbolRules {
    /// 建立規則，並檢查設定本身是否合理。
    pub fn new(
        tick_size: Fixed,
        step_size: Fixed,
        min_qty: Fixed,
        max_qty: Fixed,
        min_notional: Fixed,
    ) -> Result<SymbolRules, RulesError> {
        if tick_size <= Fixed::ZERO || step_size <= Fixed::ZERO {
            return Err(RulesError::NonPositiveStep);
        }
        if min_qty > max_qty {
            return Err(RulesError::MinAboveMax);
        }
        Ok(SymbolRules {
            tick_size,
            step_size,
            min_qty,
            max_qty,
            min_notional,
        })
    }

    /// 數量往下取整到級距。往下是為了不超過想投入的金額或帳戶餘額。
    pub fn round_qty(&self, qty: Fixed) -> Option<Fixed> {
        qty.floor_to_step(self.step_size)
    }

    /// 限價單價格取整到價格跳動：買單往下、賣單往上。
    ///
    /// 這樣取整後的價格只會和原本一樣，或對自己更有利
    /// （買得更便宜、賣得更貴），不會因為取整而多付錢。
    pub fn round_price(&self, side: Side, price: Fixed) -> Option<Fixed> {
        match side {
            Side::Buy => price.floor_to_step(self.tick_size),
            Side::Sell => price.ceil_to_step(self.tick_size),
        }
    }

    /// 用 `quote` 金額（例如 1000 USDT）在 `price` 最多能買多少，往下取整到級距。
    pub fn qty_for_quote(&self, quote: Fixed, price: Fixed) -> Option<Fixed> {
        if price <= Fixed::ZERO || quote.is_negative() {
            return None;
        }
        // quote / price，中間用 i128 避免溢位，無條件捨去
        let raw = quote.raw() as i128 * Fixed::SCALE as i128 / price.raw() as i128;
        let qty = Fixed::from_raw(i64::try_from(raw).ok()?);
        self.round_qty(qty)
    }

    /// 檢查一筆單是否合規，回傳第一個不合規的地方。
    pub fn check(&self, price: Fixed, qty: Fixed) -> Result<(), RuleViolation> {
        if price <= Fixed::ZERO {
            return Err(RuleViolation::NonPositivePrice { price });
        }
        if !price.is_multiple_of(self.tick_size) {
            return Err(RuleViolation::PriceNotOnTick {
                price,
                tick: self.tick_size,
            });
        }
        if !qty.is_multiple_of(self.step_size) {
            return Err(RuleViolation::QtyNotOnStep {
                qty,
                step: self.step_size,
            });
        }
        if qty < self.min_qty {
            return Err(RuleViolation::QtyTooSmall {
                qty,
                min: self.min_qty,
            });
        }
        if qty > self.max_qty {
            return Err(RuleViolation::QtyTooLarge {
                qty,
                max: self.max_qty,
            });
        }
        let notional = price.checked_mul(qty).ok_or(RuleViolation::Overflow)?;
        if notional < self.min_notional {
            return Err(RuleViolation::NotionalTooSmall {
                notional,
                min: self.min_notional,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 和 Binance BTCUSDT 現貨相近的規則。
    fn btc() -> SymbolRules {
        SymbolRules::new(
            fx("0.01"),
            fx("0.00001"),
            fx("0.00001"),
            fx("9000"),
            fx("5"),
        )
        .unwrap()
    }

    #[test]
    fn new_rejects_bad_settings() {
        let (one, z) = (Fixed::ONE, Fixed::ZERO);
        assert_eq!(
            SymbolRules::new(z, one, one, one, one),
            Err(RulesError::NonPositiveStep)
        );
        assert_eq!(
            SymbolRules::new(one, z, one, one, one),
            Err(RulesError::NonPositiveStep)
        );
        assert_eq!(
            SymbolRules::new(one, one, fx("2"), one, one),
            Err(RulesError::MinAboveMax)
        );
    }

    #[test]
    fn qty_rounds_down_to_step() {
        assert_eq!(btc().round_qty(fx("0.0156789")), Some(fx("0.01567")));
        assert_eq!(btc().round_qty(fx("0.01567")), Some(fx("0.01567")));
    }

    #[test]
    fn buy_price_rounds_down_sell_price_rounds_up() {
        let r = btc();
        assert_eq!(
            r.round_price(Side::Buy, fx("63880.129")),
            Some(fx("63880.12"))
        );
        assert_eq!(
            r.round_price(Side::Sell, fx("63880.121")),
            Some(fx("63880.13"))
        );
        // 已經對齊的價格不動
        assert_eq!(
            r.round_price(Side::Sell, fx("63880.12")),
            Some(fx("63880.12"))
        );
    }

    #[test]
    fn qty_for_quote_never_exceeds_budget() {
        let r = btc();
        let price = fx("63880.1");
        let qty = r.qty_for_quote(fx("1000"), price).unwrap();
        assert_eq!(qty, fx("0.01565"));
        assert!(price.checked_mul(qty).unwrap() <= fx("1000"));
        // 再多一個級距就會超過預算
        let more = qty.checked_add(r.step_size).unwrap();
        assert!(price.checked_mul(more).unwrap() > fx("1000"));
        assert_eq!(r.qty_for_quote(fx("1000"), Fixed::ZERO), None);
    }

    #[test]
    fn valid_order_passes() {
        assert_eq!(btc().check(fx("63880.12"), fx("0.01565")), Ok(()));
    }

    #[test]
    fn price_off_tick_is_rejected() {
        assert_eq!(
            btc().check(fx("63880.123"), fx("0.01")),
            Err(RuleViolation::PriceNotOnTick {
                price: fx("63880.123"),
                tick: fx("0.01")
            })
        );
        assert_eq!(
            btc().check(Fixed::ZERO, fx("0.01")),
            Err(RuleViolation::NonPositivePrice { price: Fixed::ZERO })
        );
    }

    #[test]
    fn qty_off_step_is_rejected() {
        assert_eq!(
            btc().check(fx("63880.12"), fx("0.012345")),
            Err(RuleViolation::QtyNotOnStep {
                qty: fx("0.012345"),
                step: fx("0.00001")
            })
        );
    }

    #[test]
    fn qty_outside_limits_is_rejected() {
        let r = btc();
        assert_eq!(
            r.check(fx("63880.12"), Fixed::ZERO),
            Err(RuleViolation::QtyTooSmall {
                qty: Fixed::ZERO,
                min: fx("0.00001")
            })
        );
        assert_eq!(
            r.check(fx("1"), fx("9000.00001")),
            Err(RuleViolation::QtyTooLarge {
                qty: fx("9000.00001"),
                max: fx("9000")
            })
        );
    }

    #[test]
    fn small_notional_is_rejected_with_values() {
        let err = btc().check(fx("63880.12"), fx("0.00007")).unwrap_err();
        assert_eq!(
            err,
            RuleViolation::NotionalTooSmall {
                notional: fx("4.4716084"),
                min: fx("5")
            }
        );
        assert_eq!(err.to_string(), "金額 4.4716084 小於最小金額 5");
    }

    #[test]
    fn rounded_order_passes_check() {
        let r = btc();
        let price = r.round_price(Side::Buy, fx("63880.12999")).unwrap();
        let qty = r.qty_for_quote(fx("250"), price).unwrap();
        assert_eq!(r.check(price, qty), Ok(()));
    }
}
