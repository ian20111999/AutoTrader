//! 手續費模型：算出每一筆成交要付多少手續費。
//!
//! 回測、模擬、測試網、實盤共用這一份模型，所以四種模式的成本算法完全一樣。
//! 費率之後（第 4 步）會從你的 Binance 帳戶抓：
//!
//! - 現貨：`GET /api/v3/account/commission`，回傳三組費率
//!   `standardCommission`（標準）、`taxCommission`（稅費）、`specialCommission`（特殊），
//!   每組都有 `maker`、`taker`、`buyer`、`seller` 四個欄位；
//!   `discount` 說明有沒有開 BNB 抵扣、折扣多少。
//! - 合約：`GET /fapi/v1/commissionRate?symbol=` 回傳該交易對的掛單、吃單費率；
//!   `GET /fapi/v2/account` 的 `feeBurn` 說明有沒有開 BNB 抵扣。
//!
//! 在抓到帳戶費率之前，先用 Binance 公開的最低等級（VIP 0）當預設值。
//!
//! 手續費一律以報價幣（例如 USDT）計。開 BNB 抵扣時實際扣的是 BNB，
//! 這裡記的是它等值多少 USDT，方便和損益放在一起看。

use crate::fixed::Fixed;
use crate::types::{Liquidity, Side, Symbol};
use std::collections::HashMap;
use std::fmt;

/// 一組費率，欄位和 Binance 現貨 API 相同。
///
/// 一筆成交的費率 = 掛單或吃單費率 + 買方或賣方費率。
/// 大部分帳戶的 `buyer`、`seller` 是 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommissionRates {
    pub maker: Fixed,
    pub taker: Fixed,
    pub buyer: Fixed,
    pub seller: Fixed,
}

impl CommissionRates {
    /// 只有掛單、吃單費率，買賣方費率為 0。
    pub fn maker_taker(maker: Fixed, taker: Fixed) -> CommissionRates {
        CommissionRates {
            maker,
            taker,
            ..CommissionRates::default()
        }
    }

    /// 某一筆成交適用的費率。
    pub fn rate(&self, liquidity: Liquidity, side: Side) -> Option<Fixed> {
        let base = match liquidity {
            Liquidity::Maker => self.maker,
            Liquidity::Taker => self.taker,
        };
        let by_side = match side {
            Side::Buy => self.buyer,
            Side::Sell => self.seller,
        };
        base.checked_add(by_side)
    }
}

/// 現貨的費率：三組相加才是實際付的錢。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotFees {
    pub standard: CommissionRates,
    pub tax: CommissionRates,
    pub special: CommissionRates,
    /// 開 BNB 抵扣時，標準費率要乘上的倍數（Binance 回傳 `0.75` 表示打 75 折）。
    /// `None` 表示沒開。只折標準費率，稅費與特殊費率不折。
    pub bnb_discount: Option<Fixed>,
}

/// U 本位合約的費率：每個交易對分開。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuturesFees {
    pub maker: Fixed,
    pub taker: Fixed,
    /// 開 BNB 抵扣時，整個手續費要乘上的倍數。`None` 表示沒開。
    /// 合約的 API 只告訴你有沒有開，不給折扣比例，所以由呼叫端依 Binance 公告填入。
    pub bnb_discount: Option<Fixed>,
}

/// 一個交易對的費率模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeModel {
    Spot(SpotFees),
    Futures(FuturesFees),
}

/// 算手續費失敗的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeError {
    /// 費率表裡沒有這個交易對。
    UnknownSymbol(Symbol),
    /// 價格 ≤ 0 或數量 < 0。
    InvalidFill { price: Fixed, qty: Fixed },
    /// 計算超出可表示範圍。
    Overflow,
}

impl fmt::Display for FeeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeeError::UnknownSymbol(s) => write!(f, "費率表裡沒有 {s}"),
            FeeError::InvalidFill { price, qty } => {
                write!(f, "成交價格 {price} 必須大於 0，數量 {qty} 不可為負")
            }
            FeeError::Overflow => write!(f, "手續費計算超出可表示範圍"),
        }
    }
}

impl std::error::Error for FeeError {}

fn fx(s: &str) -> Fixed {
    // 只用在下面寫死的常數，字串一定合法
    s.parse().expect("內建費率常數格式錯誤")
}

impl FeeModel {
    /// Binance 現貨 VIP 0：掛單、吃單都是 0.1%，沒開 BNB 抵扣。
    pub fn spot_vip0() -> FeeModel {
        let r = fx("0.001");
        FeeModel::Spot(SpotFees {
            standard: CommissionRates::maker_taker(r, r),
            tax: CommissionRates::default(),
            special: CommissionRates::default(),
            bnb_discount: None,
        })
    }

    /// Binance U 本位合約 VIP 0：掛單 0.02%、吃單 0.05%，沒開 BNB 抵扣。
    pub fn futures_vip0() -> FeeModel {
        FeeModel::Futures(FuturesFees {
            maker: fx("0.0002"),
            taker: fx("0.0005"),
            bnb_discount: None,
        })
    }

    /// 某一筆成交實際適用的費率（已含 BNB 折扣）。
    ///
    /// 可能是負數：部分高等級帳戶的合約掛單是返佣。
    pub fn rate(&self, liquidity: Liquidity, side: Side) -> Option<Fixed> {
        match self {
            FeeModel::Spot(s) => {
                let mut standard = s.standard.rate(liquidity, side)?;
                if let Some(d) = s.bnb_discount {
                    standard = standard.checked_mul(d)?;
                }
                standard
                    .checked_add(s.tax.rate(liquidity, side)?)?
                    .checked_add(s.special.rate(liquidity, side)?)
            }
            FeeModel::Futures(f) => {
                let base = match liquidity {
                    Liquidity::Maker => f.maker,
                    Liquidity::Taker => f.taker,
                };
                match f.bnb_discount {
                    Some(d) => base.checked_mul(d),
                    None => Some(base),
                }
            }
        }
    }

    /// 一筆成交的手續費 = 成交金額（價格 × 數量）× 費率。
    pub fn fee(
        &self,
        liquidity: Liquidity,
        side: Side,
        price: Fixed,
        qty: Fixed,
    ) -> Result<Fixed, FeeError> {
        if price <= Fixed::ZERO || qty.is_negative() {
            return Err(FeeError::InvalidFill { price, qty });
        }
        let rate = self.rate(liquidity, side).ok_or(FeeError::Overflow)?;
        price
            .checked_mul(qty)
            .and_then(|notional| notional.checked_mul(rate))
            .ok_or(FeeError::Overflow)
    }
}

/// 整個帳戶的費率表：每個交易對一個模型。
#[derive(Debug, Clone, Default)]
pub struct FeeSchedule {
    models: HashMap<Symbol, FeeModel>,
    /// 最後一次從交易所同步的時間（UTC 毫秒）。`None` 表示還沒同步過，用的是預設值。
    pub synced_at: Option<i64>,
}

impl FeeSchedule {
    pub fn new() -> FeeSchedule {
        FeeSchedule::default()
    }

    /// 設定（或覆蓋）某個交易對的費率。
    pub fn insert(&mut self, symbol: Symbol, model: FeeModel) {
        self.models.insert(symbol, model);
    }

    pub fn get(&self, symbol: &Symbol) -> Option<&FeeModel> {
        self.models.get(symbol)
    }

    /// 某個交易對一筆成交的手續費。
    ///
    /// 找不到交易對時回傳錯誤，而不是默默用 0：少算手續費會讓回測看起來比實際賺。
    pub fn fee(
        &self,
        symbol: &Symbol,
        liquidity: Liquidity,
        side: Side,
        price: Fixed,
        qty: Fixed,
    ) -> Result<Fixed, FeeError> {
        self.get(symbol)
            .ok_or_else(|| FeeError::UnknownSymbol(symbol.clone()))?
            .fee(liquidity, side, price, qty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Symbol {
        Symbol::new(s).unwrap()
    }

    fn spot(standard: &str, tax: &str, special: &str, bnb: Option<&str>) -> FeeModel {
        FeeModel::Spot(SpotFees {
            standard: CommissionRates::maker_taker(fx(standard), fx(standard)),
            tax: CommissionRates::maker_taker(fx(tax), fx(tax)),
            special: CommissionRates::maker_taker(fx(special), fx(special)),
            bnb_discount: bnb.map(fx),
        })
    }

    #[test]
    fn spot_vip0_costs_one_usdt_per_thousand() {
        let fee = FeeModel::spot_vip0()
            .fee(Liquidity::Taker, Side::Buy, fx("50000"), fx("0.02"))
            .unwrap();
        assert_eq!(fee, fx("1"));
    }

    #[test]
    fn futures_vip0_maker_is_cheaper_than_taker() {
        let m = FeeModel::futures_vip0();
        let (price, qty) = (fx("2500"), fx("4")); // 成交金額 10000 USDT
        assert_eq!(m.fee(Liquidity::Maker, Side::Sell, price, qty), Ok(fx("2")));
        assert_eq!(m.fee(Liquidity::Taker, Side::Sell, price, qty), Ok(fx("5")));
    }

    #[test]
    fn spot_rate_adds_three_parts() {
        let m = spot("0.001", "0.0001", "0.00005", None);
        assert_eq!(m.rate(Liquidity::Taker, Side::Buy), Some(fx("0.00115")));
    }

    #[test]
    fn bnb_discount_only_applies_to_standard_part() {
        // 標準 0.1% 打 75 折 = 0.075%，稅費 0.01% 不折
        let m = spot("0.001", "0.0001", "0", Some("0.75"));
        assert_eq!(m.rate(Liquidity::Maker, Side::Sell), Some(fx("0.00085")));
    }

    #[test]
    fn buyer_and_seller_rates_depend_on_side() {
        let m = FeeModel::Spot(SpotFees {
            standard: CommissionRates {
                maker: fx("0.001"),
                taker: fx("0.001"),
                buyer: fx("0.0001"),
                seller: Fixed::ZERO,
            },
            tax: CommissionRates::default(),
            special: CommissionRates::default(),
            bnb_discount: None,
        });
        assert_eq!(m.rate(Liquidity::Taker, Side::Buy), Some(fx("0.0011")));
        assert_eq!(m.rate(Liquidity::Taker, Side::Sell), Some(fx("0.001")));
    }

    #[test]
    fn futures_bnb_discount_applies_to_whole_fee() {
        let m = FeeModel::Futures(FuturesFees {
            maker: fx("0.0002"),
            taker: fx("0.0005"),
            bnb_discount: Some(fx("0.9")),
        });
        assert_eq!(m.rate(Liquidity::Taker, Side::Buy), Some(fx("0.00045")));
    }

    #[test]
    fn negative_maker_rate_is_a_rebate() {
        let m = FeeModel::Futures(FuturesFees {
            maker: fx("-0.00005"),
            taker: fx("0.0003"),
            bnb_discount: None,
        });
        assert_eq!(
            m.fee(Liquidity::Maker, Side::Buy, fx("100"), fx("100")),
            Ok(fx("-0.5"))
        );
    }

    #[test]
    fn tiny_fee_rounds_at_eighth_decimal() {
        // 63880.12 × 0.00008 = 5.1104096 USDT，× 0.1% = 0.0051104096
        // 第 9 位是 9，四捨五入到 8 位小數 = 0.00511041
        let fee = FeeModel::spot_vip0()
            .fee(Liquidity::Taker, Side::Buy, fx("63880.12"), fx("0.00008"))
            .unwrap();
        assert_eq!(fee, fx("0.00511041"));
    }

    #[test]
    fn invalid_fill_is_rejected() {
        let m = FeeModel::spot_vip0();
        assert_eq!(
            m.fee(Liquidity::Taker, Side::Buy, Fixed::ZERO, fx("1")),
            Err(FeeError::InvalidFill {
                price: Fixed::ZERO,
                qty: fx("1")
            })
        );
        assert!(m
            .fee(Liquidity::Taker, Side::Buy, fx("1"), fx("-1"))
            .is_err());
    }

    #[test]
    fn schedule_uses_per_symbol_model() {
        let mut s = FeeSchedule::new();
        s.insert(sym("BTCUSDT"), FeeModel::spot_vip0());
        s.insert(
            sym("ETHUSDT"),
            spot("0.001", "0", "0", Some("0.75")), // 這個交易對有開 BNB 抵扣
        );
        let (p, q) = (fx("1000"), fx("1"));
        assert_eq!(
            s.fee(&sym("BTCUSDT"), Liquidity::Taker, Side::Buy, p, q),
            Ok(fx("1"))
        );
        assert_eq!(
            s.fee(&sym("ETHUSDT"), Liquidity::Taker, Side::Buy, p, q),
            Ok(fx("0.75"))
        );
    }

    #[test]
    fn unknown_symbol_is_an_error_not_zero() {
        let s = FeeSchedule::new();
        assert_eq!(
            s.fee(
                &sym("SOLUSDT"),
                Liquidity::Taker,
                Side::Buy,
                fx("1"),
                fx("1")
            ),
            Err(FeeError::UnknownSymbol(sym("SOLUSDT")))
        );
        assert_eq!(s.synced_at, None);
    }
}
