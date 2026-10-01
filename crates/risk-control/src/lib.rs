//! 下單前的風控閘門（ROADMAP 6.3）：每日虧損上限、一鍵停止、單筆下單金額上限。
//!
//! # 為什麼是獨立的 crate
//!
//! 這一層是「要不要把這筆單送出去」的政策，不是回測引擎共用的地基型別。
//! `at-core` 目前零外部依賴，被回測、模擬、下載器一起用；把帳戶層的風控設定塞
//! 進去，等於讓純算帳的回測也背著一份「每日虧損上限」。放成獨立 crate 還有一個
//! 好處：6.4 的下單路徑在 `Cargo.toml` 上就看得出來一定依賴
//! `at-risk-control`，少一條繞過閘門的暗路。
//!
//! # 沒有網路、沒有鑰匙圈、沒有時鐘
//!
//! 這裡只有算數與比較。時間是呼叫端給的毫秒 UTC 時間戳（[`DailyPnl::update`]），
//! 權益也是呼叫端給的數字，所以測試可以餵任意的假時間與假帳本，不需要等到明天
//! 才能驗「跨日重置」。
//!
//! # 兩個設計決定
//!
//! **一鍵停止連平倉都擋。** 一鍵停止存在的理由，正是「我不再相信這套自動化」
//! ——策略寫錯、迴圈失控、餵錯交易對、資料髒掉。如果它放行「降低風險的單」，
//! 閘門就必須相信下單方自己對「這是平倉」的分類；而一個會把加碼誤判成平倉的
//! 程式，剛好就是你最需要按下停止鍵的那個程式。所以這裡選的失效模式是
//! 「按下去之後程式一張單都不送」，而不是「按下去之後程式還會送它認為安全的
//! 單」。既有部位請用交易所自己的 App 手動處理——那條路不依賴這份程式碼正確。
//!
//! **每日虧損上限只擋「增加曝險」的單，不自動強制平倉。** 虧到上限時要做的是
//! 「停止繼續虧更多」，把部位往 0 的方向收的單必須放行，否則使用者會被鎖在
//! 部位裡。反過來說，自動強制平倉本身是另一個有風險的自動化動作（市價砸出去、
//! 可能剛好砸在最差的價格上），這一步不做，交給使用者用一鍵停止 + 手動處理。
//!
//! # 判不出來就擋（fail closed）
//!
//! 今日損益算不出來、下單金額溢位、數量是 0 或負數——任何「閘門自己也不確定」
//! 的情況都回 [`Err`]。風控閘門放行一筆它看不懂的單，比擋下一筆合法的單糟得多。
//!
//! # 用起來像這樣
//!
//! ```
//! use at_core::{Fixed, Side};
//! use at_risk_control::{AccountState, DailyPnl, ProposedOrder, RiskLimits};
//!
//! let limits = RiskLimits::new(
//!     Fixed::from_int(100).unwrap(),   // 每日最多虧 100 USDT
//!     Fixed::from_int(1_000).unwrap(), // 單筆最多 1000 USDT
//! )
//! .unwrap();
//!
//! // 時間與權益都由呼叫端提供，這裡不讀系統時鐘。
//! let mut pnl = DailyPnl::new(1_728_000_000_000, Fixed::from_int(10_000).unwrap());
//! let today = pnl.update(1_728_030_000_000, Fixed::from_int(9_950).unwrap());
//!
//! let state = AccountState {
//!     kill_switch: false,
//!     position: Fixed::ZERO,
//!     daily_pnl: today,
//! };
//! let order = ProposedOrder {
//!     side: Side::Buy,
//!     quantity: Fixed::from_int(1).unwrap(),
//!     reference_price: Fixed::from_int(500).unwrap(),
//! };
//!
//! match limits.check(&state, &order) {
//!     Ok(()) => println!("可以送單"),
//!     Err(blocked) => println!("擋下：{blocked}"),
//! }
//! ```

use at_core::{Fixed, Side};
use std::fmt;

/// UTC 一天的毫秒數。
const MS_PER_DAY: i64 = 86_400_000;

/// 風控上限設定。兩個上限都用正數表示（「最多虧 100」而不是「−100」）。
///
/// 只能用 [`RiskLimits::new`] 建立，所以手上拿到的一定是檢查過的設定——
/// 這些數字來自 6.5 的設定畫面，是外部輸入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskLimits {
    max_daily_loss: Fixed,
    max_order_notional: Fixed,
}

/// 風控設定本身不合法的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitsError {
    DailyLossNotPositive,
    OrderNotionalNotPositive,
}

impl fmt::Display for LimitsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitsError::DailyLossNotPositive => write!(f, "每日虧損上限必須大於 0"),
            LimitsError::OrderNotionalNotPositive => write!(f, "單筆下單金額上限必須大於 0"),
        }
    }
}

impl std::error::Error for LimitsError {}

/// 送單當下的帳戶狀態。整份是呼叫端給的快照，閘門不會自己去查帳本。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountState {
    /// 一鍵停止旗標：`true` 表示使用者已要求停止，所有下單都會被擋。
    pub kill_switch: bool,
    /// 帶正負號的目前持倉數量：正做多、負做空、`0` 空手。
    ///
    /// 閘門用它自己判斷一筆單是開倉、加碼還是平倉，不接受下單方自己貼的標籤：
    /// 會貼錯標籤的程式正是閘門要防的對象。
    pub position: Fixed,
    /// 今日損益，負數是虧損（[`DailyPnl::update`] 的回傳值）。
    ///
    /// `None` 表示算不出來；閘門會擋下所有下單，不會當成 0。
    pub daily_pnl: Option<Fixed>,
}

/// 準備送出去的一筆市價單。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposedOrder {
    /// 買或賣。
    pub side: Side,
    /// 下單數量，必須大於 0。
    pub quantity: Fixed,
    /// 算下單金額用的參考價：市價單用最新成交價，必須大於 0。
    ///
    /// 市價單的真實成交價要等交易所回報才知道，所以金額上限檢查用的是估算值。
    /// 這是刻意的——閘門要在送單**之前**擋，不能等成交。
    pub reference_price: Fixed,
}

/// 這筆單被擋下的原因。訊息是繁體中文，可以直接顯示給使用者。
#[derive(Debug, Clone, PartialEq)]
pub enum Blocked {
    /// 一鍵停止已啟動。
    KillSwitch,
    /// 今日虧損已達每日上限，而這筆單會增加曝險。
    DailyLossReached { pnl: Fixed, limit: Fixed },
    /// 單筆下單金額超過上限。
    OrderTooLarge { notional: Fixed, limit: Fixed },
    /// 今日損益不明（[`AccountState::daily_pnl`] 是 `None`）。
    DailyPnlUnknown,
    /// 下單數量或參考價不是正數。
    InvalidOrder,
    /// 算下單金額或部位變化時溢位。
    Overflow,
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::KillSwitch => {
                write!(f, "一鍵停止已啟動，所有下單都被擋下（含平倉）")
            }
            Blocked::DailyLossReached { pnl, limit } => write!(
                f,
                "今日損益 {pnl} 已達每日虧損上限 {limit}，擋下會增加曝險的下單（降低曝險的下單不受影響）"
            ),
            Blocked::OrderTooLarge { notional, limit } => {
                write!(f, "單筆下單金額 {notional} 超過上限 {limit}")
            }
            Blocked::DailyPnlUnknown => {
                write!(f, "今日損益算不出來，為安全起見擋下所有下單")
            }
            Blocked::InvalidOrder => write!(f, "下單數量與參考價都必須大於 0"),
            Blocked::Overflow => write!(f, "下單金額或部位變化計算溢位，擋下這筆下單"),
        }
    }
}

impl std::error::Error for Blocked {}

impl RiskLimits {
    /// 建立風控設定。兩個上限都必須大於 0。
    ///
    /// `max_daily_loss` 是正數的虧損上限：今日損益 ≤ `−max_daily_loss` 時觸發。
    /// `max_order_notional` 是單筆下單金額（數量 × 參考價）的上限。
    pub fn new(
        max_daily_loss: Fixed,
        max_order_notional: Fixed,
    ) -> Result<RiskLimits, LimitsError> {
        if max_daily_loss <= Fixed::ZERO {
            return Err(LimitsError::DailyLossNotPositive);
        }
        if max_order_notional <= Fixed::ZERO {
            return Err(LimitsError::OrderNotionalNotPositive);
        }
        Ok(RiskLimits {
            max_daily_loss,
            max_order_notional,
        })
    }

    /// 送單前的檢查：`Ok(())` 可以送，`Err` 帶擋下的原因。
    ///
    /// 檢查順序是「最絕對的先講」：一鍵停止 → 下單數字合不合法 → 每日虧損上限
    /// → 單筆金額上限。多個條件同時成立時，回報的是順序在前的那一個，因為那是
    /// 使用者比較需要先知道的事（「今天不再開新倉了」比「這筆太大」重要）。
    pub fn check(&self, state: &AccountState, order: &ProposedOrder) -> Result<(), Blocked> {
        if state.kill_switch {
            return Err(Blocked::KillSwitch);
        }
        if order.quantity <= Fixed::ZERO || order.reference_price <= Fixed::ZERO {
            return Err(Blocked::InvalidOrder);
        }

        let pnl = state.daily_pnl.ok_or(Blocked::DailyPnlUnknown)?;
        let loss_floor = Fixed::ZERO
            .checked_sub(self.max_daily_loss)
            .ok_or(Blocked::Overflow)?;
        // 剛好等於上限就算觸發：風控的邊界往保守那邊倒。
        if pnl <= loss_floor && !reduces_exposure(state.position, order).ok_or(Blocked::Overflow)? {
            return Err(Blocked::DailyLossReached {
                pnl,
                limit: self.max_daily_loss,
            });
        }

        let notional = order
            .quantity
            .checked_mul(order.reference_price)
            .ok_or(Blocked::Overflow)?;
        if notional > self.max_order_notional {
            return Err(Blocked::OrderTooLarge {
                notional,
                limit: self.max_order_notional,
            });
        }

        Ok(())
    }
}

/// 這筆單是不是「純粹降低曝險」：把部位往 0 的方向移動，而且不穿越 0。
///
/// 判準刻意從嚴——只有明確降風險才算降風險。反向翻倉（做多 1 直接賣 3）就算
/// 讓部位絕對值變小，開的仍然是一個新方向的部位；已經觸及每日虧損上限的時候
/// 放行它，策略只要靠翻倉就能繼續交易、繼續虧。
///
/// 溢位回 `None`，由呼叫端擋單。
fn reduces_exposure(position: Fixed, order: &ProposedOrder) -> Option<bool> {
    let signed = match order.side {
        Side::Buy => order.quantity,
        Side::Sell => Fixed::ZERO.checked_sub(order.quantity)?,
    };
    let before = position.raw();
    let after = position.checked_add(signed)?.raw();
    let crosses_zero = (before > 0 && after < 0) || (before < 0 && after > 0);
    // `checked_abs` 而不是 `abs`：`i64::MIN` 的 `abs` 在 debug build 會 panic。
    Some(!crosses_zero && after.checked_abs()? < before.checked_abs()?)
}

/// 今日損益追蹤器：今日損益 =「目前權益 − 今天第一次回報的權益」。
///
/// 用權益差而不是分開累加已實現與未實現損益，因為權益本身已經同時含兩者
/// （見 5.1 `PaperSnapshot::point`），少一份會跟帳本對不起來的平行帳。
///
/// # 「今天」是 UTC 日
///
/// 選 UTC 而不是本地時區：Binance 的資金費結算、24 小時統計、日 K 都以 UTC
/// 劃界，用 UTC 對帳時間才對得上；本地時區還會隨使用者所在地與日光節約時間
/// 變動，同一份設定在不同機器上會有不同的重置時刻。
///
/// 這個型別不讀系統時鐘——時間一律由呼叫端傳進來。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyPnl {
    day: i64,
    day_open_equity: Fixed,
}

impl DailyPnl {
    /// 從「現在的時間與權益」開始追蹤。`now_ms` 是毫秒 UTC 時間戳。
    pub fn new(now_ms: i64, equity: Fixed) -> DailyPnl {
        DailyPnl {
            day: utc_day(now_ms),
            day_open_equity: equity,
        }
    }

    /// 回報最新的時間與權益，回傳今日損益（負數是虧損）。
    ///
    /// 跨到新的 UTC 日時先把基準換成這次的 `equity`，所以跨日後第一次回報一定
    /// 是 0 ——昨天的虧損不會把今天也鎖住。
    ///
    /// 時間往回跳不會重置：只有日序**變大**才換基準。NTP 校時往回跳一下就把
    /// 「今天已經虧到上限」清掉，是風控不能有的漏洞。
    ///
    /// `None` = 權益數字大到減不出來。呼叫端要原樣塞進
    /// [`AccountState::daily_pnl`]，閘門會擋下所有下單。
    pub fn update(&mut self, now_ms: i64, equity: Fixed) -> Option<Fixed> {
        let day = utc_day(now_ms);
        if day > self.day {
            self.day = day;
            self.day_open_equity = equity;
        }
        equity.checked_sub(self.day_open_equity)
    }
}

/// 從 epoch 起算的 UTC 日序。`div_euclid` 往負無限大取整，1970 年之前也對。
fn utc_day(ms: i64) -> i64 {
    ms.div_euclid(MS_PER_DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真實的某個 UTC 午夜（日序 20000 = 2024-10-04T00:00:00Z）。
    const DAY_A: i64 = 20_000 * MS_PER_DAY;
    const DAY_B: i64 = DAY_A + MS_PER_DAY;
    const HOUR: i64 = 3_600_000;

    fn f(n: i64) -> Fixed {
        Fixed::from_int(n).unwrap()
    }

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 每日最多虧 100、單筆最多 1000。
    fn limits() -> RiskLimits {
        RiskLimits::new(f(100), f(1_000)).unwrap()
    }

    fn state(kill_switch: bool, position: Fixed, daily_pnl: Fixed) -> AccountState {
        AccountState {
            kill_switch,
            position,
            daily_pnl: Some(daily_pnl),
        }
    }

    fn order(side: Side, quantity: Fixed) -> ProposedOrder {
        ProposedOrder {
            side,
            quantity,
            reference_price: f(100),
        }
    }

    #[test]
    fn limits_must_be_positive() {
        assert_eq!(
            RiskLimits::new(Fixed::ZERO, f(1_000)),
            Err(LimitsError::DailyLossNotPositive)
        );
        assert_eq!(
            RiskLimits::new(f(-100), f(1_000)),
            Err(LimitsError::DailyLossNotPositive)
        );
        assert_eq!(
            RiskLimits::new(f(100), Fixed::ZERO),
            Err(LimitsError::OrderNotionalNotPositive)
        );
    }

    #[test]
    fn allows_when_every_check_passes() {
        // 空手、今天小虧 10、買 1 顆 100 塊 → 金額 100，三關都過。
        let decision = limits().check(&state(false, Fixed::ZERO, f(-10)), &order(Side::Buy, f(1)));
        assert_eq!(decision, Ok(()));
    }

    #[test]
    fn daily_loss_blocks_opening_and_adding() {
        let limits = limits();
        // 空手開倉
        assert_eq!(
            limits.check(&state(false, Fixed::ZERO, f(-120)), &order(Side::Buy, f(1))),
            Err(Blocked::DailyLossReached {
                pnl: f(-120),
                limit: f(100)
            })
        );
        // 已經做多還加碼
        assert!(limits
            .check(&state(false, f(1), f(-120)), &order(Side::Buy, f(1)))
            .is_err());
        // 已經做空還加空
        assert!(limits
            .check(&state(false, f(-1), f(-120)), &order(Side::Sell, f(1)))
            .is_err());
    }

    #[test]
    fn daily_loss_at_exactly_the_limit_blocks() {
        // 剛好 −100 就該停，不是「超過才停」。
        assert!(limits()
            .check(&state(false, Fixed::ZERO, f(-100)), &order(Side::Buy, f(1)))
            .is_err());
    }

    #[test]
    fn daily_loss_does_not_block_reducing_exposure() {
        let limits = limits();
        // 做多 1 全平
        assert_eq!(
            limits.check(&state(false, f(1), f(-120)), &order(Side::Sell, f(1))),
            Ok(())
        );
        // 做多 1 減一半
        assert_eq!(
            limits.check(&state(false, f(1), f(-120)), &order(Side::Sell, fx("0.5"))),
            Ok(())
        );
        // 做空 1 全平（買回來）
        assert_eq!(
            limits.check(&state(false, f(-1), f(-120)), &order(Side::Buy, f(1))),
            Ok(())
        );
    }

    #[test]
    fn daily_loss_blocks_flipping_even_when_position_shrinks() {
        // 做多 1 → 賣 3 → 變成做空 2：絕對值反而變大，當然要擋。
        assert!(limits()
            .check(&state(false, f(1), f(-120)), &order(Side::Sell, f(3)))
            .is_err());
        // 做多 3 → 賣 5 → 變成做空 2：絕對值變小，但開的是新方向的部位，也要擋。
        assert!(limits()
            .check(&state(false, f(3), f(-120)), &order(Side::Sell, f(5)))
            .is_err());
        // 做多 1 → 賣 2 → 做空 1：絕對值一樣，靠翻倉繞過停損的經典路徑，要擋。
        assert!(limits()
            .check(&state(false, f(1), f(-120)), &order(Side::Sell, f(2)))
            .is_err());
    }

    #[test]
    fn kill_switch_blocks_everything_including_closing() {
        let limits = limits();
        // 一切正常、沒虧錢、金額也小，但旗標是開的。
        assert_eq!(
            limits.check(
                &state(true, Fixed::ZERO, Fixed::ZERO),
                &order(Side::Buy, f(1))
            ),
            Err(Blocked::KillSwitch)
        );
        // 平倉一樣擋：一鍵停止的契約是「一張單都不送」。
        assert_eq!(
            limits.check(&state(true, f(1), Fixed::ZERO), &order(Side::Sell, f(1))),
            Err(Blocked::KillSwitch)
        );
    }

    #[test]
    fn order_above_notional_cap_is_blocked() {
        let limits = limits();
        let too_big = ProposedOrder {
            side: Side::Buy,
            quantity: f(2),
            reference_price: f(600), // 1200 > 1000
        };
        assert_eq!(
            limits.check(&state(false, Fixed::ZERO, Fixed::ZERO), &too_big),
            Err(Blocked::OrderTooLarge {
                notional: f(1_200),
                limit: f(1_000)
            })
        );
        // 剛好等於上限可以過。
        let exact = ProposedOrder {
            reference_price: f(500),
            ..too_big
        };
        assert_eq!(
            limits.check(&state(false, Fixed::ZERO, Fixed::ZERO), &exact),
            Ok(())
        );
    }

    #[test]
    fn notional_cap_applies_to_closing_orders_too() {
        // 金額上限是「單筆下單」的上限，跟方向無關：一次砸太大本身就是風險。
        let big_close = ProposedOrder {
            side: Side::Sell,
            quantity: f(20),
            reference_price: f(100),
        };
        assert!(limits()
            .check(&state(false, f(20), Fixed::ZERO), &big_close)
            .is_err());
    }

    #[test]
    fn unknown_daily_pnl_blocks() {
        let unknown = AccountState {
            kill_switch: false,
            position: Fixed::ZERO,
            daily_pnl: None,
        };
        assert_eq!(
            limits().check(&unknown, &order(Side::Buy, f(1))),
            Err(Blocked::DailyPnlUnknown)
        );
    }

    #[test]
    fn non_positive_order_numbers_are_blocked() {
        let limits = limits();
        let zero_qty = order(Side::Buy, Fixed::ZERO);
        assert_eq!(
            limits.check(&state(false, Fixed::ZERO, Fixed::ZERO), &zero_qty),
            Err(Blocked::InvalidOrder)
        );
        let zero_price = ProposedOrder {
            reference_price: Fixed::ZERO,
            ..order(Side::Buy, f(1))
        };
        assert_eq!(
            limits.check(&state(false, Fixed::ZERO, Fixed::ZERO), &zero_price),
            Err(Blocked::InvalidOrder)
        );
    }

    #[test]
    fn notional_overflow_blocks_instead_of_wrapping() {
        let huge = ProposedOrder {
            side: Side::Buy,
            quantity: Fixed::from_raw(i64::MAX),
            reference_price: Fixed::from_raw(i64::MAX),
        };
        assert_eq!(
            limits().check(&state(false, Fixed::ZERO, Fixed::ZERO), &huge),
            Err(Blocked::Overflow)
        );
    }

    #[test]
    fn daily_pnl_tracks_equity_difference() {
        let mut pnl = DailyPnl::new(DAY_A + 12 * HOUR, f(10_000));
        assert_eq!(pnl.update(DAY_A + 13 * HOUR, f(9_900)), Some(f(-100)));
        assert_eq!(pnl.update(DAY_A + 14 * HOUR, f(10_050)), Some(f(50)));
    }

    #[test]
    fn daily_pnl_resets_on_new_utc_day() {
        let mut pnl = DailyPnl::new(DAY_A + 12 * HOUR, f(10_000));
        assert_eq!(pnl.update(DAY_A + 20 * HOUR, f(9_900)), Some(f(-100)));

        // 跨進新的 UTC 日：基準換成當下權益，今日損益歸零。
        assert_eq!(pnl.update(DAY_B, f(9_900)), Some(Fixed::ZERO));
        assert_eq!(pnl.update(DAY_B + HOUR, f(9_850)), Some(f(-50)));
    }

    #[test]
    fn daily_loss_gate_reopens_after_the_reset() {
        let limits = limits();
        let mut pnl = DailyPnl::new(DAY_A, f(10_000));

        let today = pnl.update(DAY_A + 20 * HOUR, f(9_880)); // −120，超過上限
        let blocked = AccountState {
            kill_switch: false,
            position: Fixed::ZERO,
            daily_pnl: today,
        };
        assert!(limits.check(&blocked, &order(Side::Buy, f(1))).is_err());

        let tomorrow = pnl.update(DAY_B, f(9_880)); // 跨日：損益歸零
        let allowed = AccountState {
            daily_pnl: tomorrow,
            ..blocked
        };
        assert_eq!(limits.check(&allowed, &order(Side::Buy, f(1))), Ok(()));
    }

    #[test]
    fn daily_pnl_ignores_a_clock_that_jumps_backwards() {
        let mut pnl = DailyPnl::new(DAY_B, f(10_000));
        assert_eq!(pnl.update(DAY_B + HOUR, f(9_900)), Some(f(-100)));

        // 時間跳回前一天：基準不換，今日虧損不會被清掉。
        assert_eq!(pnl.update(DAY_A + 23 * HOUR, f(9_900)), Some(f(-100)));
    }

    #[test]
    fn utc_day_handles_timestamps_before_epoch() {
        assert_eq!(utc_day(0), 0);
        assert_eq!(utc_day(MS_PER_DAY - 1), 0);
        assert_eq!(utc_day(MS_PER_DAY), 1);
        assert_eq!(utc_day(-1), -1);
    }

    #[test]
    fn block_reasons_are_readable() {
        assert_eq!(
            Blocked::KillSwitch.to_string(),
            "一鍵停止已啟動，所有下單都被擋下（含平倉）"
        );
        assert!(Blocked::OrderTooLarge {
            notional: f(1_200),
            limit: f(1_000),
        }
        .to_string()
        .contains("1200"));
    }
}
