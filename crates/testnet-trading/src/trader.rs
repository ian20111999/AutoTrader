//! 一根收盤 K 線的完整處理：算目標部位 → 過風控閘門 → 送單 → 等回報 → 更新帳本。
//!
//! 這裡沒有執行緒、沒有 channel、也沒有系統時鐘：時間由呼叫端傳進來
//! （用 4.5 行情事件自帶的交易所時間），交易所由 [`OrderGateway`] 注入。
//! 所以除了「真的連上測試網」以外的每一條分支都可以用普通的單元測試驗。
//!
//! # 兩道並列的閘門
//!
//! [`Trader::send_gated_order`] 依序呼叫兩道閘門，兩道都回 `Ok` 才送單
//! （ADR-002 第 4.1 節）：
//!
//! 1. [`RiskLimits::check`]（6.3）：單一 session 的一鍵停止／單日虧損／單筆金額。
//! 2. [`PortfolioGate::check`]（ADR-002）：全域熔斷。擋下時回
//!    [`OrderOutcome::GloballyBlocked`]，和第一道的 [`OrderOutcome::Blocked`] 分開，
//!    這樣「是哪一道閘門擋的」不會在轉成事件的路上消失。
//!
//! 順序只影響「擋下時回報哪一個理由」，不影響安不安全。先跑便宜的那道，而且
//! 6.4 既有的擋單測試因此完全不受影響。

use crate::{ConfigError, TestnetConfig, TradingError};
use at_binance::testnet::{OrderResponse, OrderSide, OrderStatus};
use at_binance::BinanceError;
use at_core::{
    Bar, EquityPoint, FeeModel, Fixed, Liquidity, RuleViolation, Side, Strategy, SymbolRules,
};
use at_portfolio_risk::{GlobalBlocked, PortfolioGate};
use at_risk_control::{AccountState, Blocked, DailyPnl, ProposedOrder, RiskLimits};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 送出去的訂單還沒到終態時，每一次重查之前等多久（毫秒）。
///
/// 等待總長 3.1 秒、最多重查 5 次。市價單在撮合引擎裡幾乎都是立刻成交，
/// 這段退避是給「交易所回了 `NEW` 但成交回報還沒跟上」的少數情況用的，
/// 不是給限價單等掛單用的。
const POLL_BACKOFF_MS: &[u64] = &[100, 200, 400, 800, 1600];

/// 等待回報時，每隔這麼久回頭看一次停止旗標。
///
/// 和 5.3 同一個理由：不能用一次長時間的 `sleep` 擋住停止訊號。
const STOP_CHECK_SLICE: Duration = Duration::from_millis(50);

/// 送單與查單的對外窗口。
///
/// 存在的唯一理由是「讓帳本邏輯能在不連網路的情況下被測到」：正式用的實作
/// 就是 6.2 的 [`BinanceTestnetClient`](at_binance::testnet::BinanceTestnetClient)
/// 直接轉呼叫（見 `lib.rs`），測試用的實作回放事先寫好的回報。
///
/// 刻意只有這兩個方法：撤單不在 6.4 的路徑上（市價單送出去就已經結束），
/// 多開一個方法等於多一條不需要存在的送單類路徑。
pub trait OrderGateway {
    fn place_market_order(
        &self,
        symbol: &str,
        side: OrderSide,
        quantity: Fixed,
    ) -> Result<OrderResponse, BinanceError>;

    fn query_order(&self, symbol: &str, order_id: u64) -> Result<OrderResponse, BinanceError>;
}

/// 一根 K 線上「下單這件事」的結果。沒有這一則表示這根完全沒有要調倉。
#[derive(Debug, Clone, PartialEq)]
pub enum OrderOutcome {
    /// 被 session 內的風控閘門擋下（[`RiskLimits::check`]），這根不送單也不重試。
    Blocked(Blocked),
    /// 被全域閘門擋下（[`PortfolioGate::check`]，熔斷），這根不送單也不重試。
    ///
    /// 和 [`OrderOutcome::Blocked`] 分成兩個 variant 而不是共用一個：觸發紀錄與
    /// UI 都要知道是哪一道閘門擋的，塞進同一個 variant 會把這個資訊弄丟。
    GloballyBlocked(GlobalBlocked),
    /// 算出來的單不符合交易所規則（數量級距、最小金額……），這根跳過。
    Invalid(RuleViolation),
    /// 真的送出去了，而且拿到終態回報。
    Filled(FillReport),
}

/// 一筆真實送出去的單，交易所回報了什麼、帳本怎麼動。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillReport {
    pub order_id: u64,
    pub side: Side,
    /// 送出去的數量。
    pub requested_qty: Fixed,
    /// 交易所回報**真的成交**的數量，可能小於 `requested_qty`，也可能是 0。
    pub executed_qty: Fixed,
    /// 交易所回報的累計成交金額（報價幣，例如 USDT）。
    pub quote_qty: Fixed,
    /// 用 4.3 同步到的帳戶費率估算的手續費（見 `docs/steps/6.4-*.md` 的說明）。
    pub fee: Fixed,
    /// 終態。`Filled` 以外的終態（`CANCELED`/`REJECTED`/`EXPIRED`…）也會走到這裡，
    /// 這時 `executed_qty` 通常是 0 或部分成交。
    pub status: OrderStatus,
}

/// 一根收盤 K 線處理完之後的帳本狀態。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestnetSnapshot {
    /// 這根 K 線收盤時的權益點（時間 + 帳戶總值，以收盤價評價）。
    pub point: EquityPoint,
    /// 報價幣現金餘額（本地帳本，不是交易所餘額——這個專案沒有查餘額的端點）。
    pub cash: Fixed,
    /// 目前持倉數量。現貨只會是 0 或正數。
    pub position: Fixed,
    /// 到目前為止真的有成交量的筆數。
    pub fills: usize,
    /// 到目前為止被風控閘門擋下的次數。
    pub blocked: usize,
    /// 到目前為止估算的累計手續費。
    pub fees_paid: Fixed,
    /// 今日損益（負數是虧損）。`None` 表示算不出來，此時閘門會擋下所有下單。
    pub daily_pnl: Option<Fixed>,
    /// 一鍵停止目前的狀態。
    pub kill_switch: bool,
}

/// 本地帳本 + 送單路徑。**這個型別裡只有一個地方會呼叫
/// [`OrderGateway::place_market_order`]**（[`Trader::send_gated_order`]），
/// 而那個地方的前兩件事就是 [`RiskLimits::check`] 與 [`PortfolioGate::check`]。
pub(crate) struct Trader {
    symbol: at_core::Symbol,
    rules: SymbolRules,
    fee_model: FeeModel,
    limits: RiskLimits,
    /// 第二道閘門（全域熔斷）。**不是 `Option`**：型別上不存在「這條路沒有全域
    /// 風控」的狀態（ADR 第 11 節第 10 項）。
    gate: Arc<dyn PortfolioGate>,
    gateway: Box<dyn OrderGateway + Send>,
    kill_switch: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// 等回報的退避表。只有測試會換掉它（換成全 0，不然每個測試都要等好幾秒）。
    backoff_ms: &'static [u64],
    cash: Fixed,
    position: Fixed,
    fills: usize,
    blocked: usize,
    fees_paid: Fixed,
    /// 第一根收盤 K 線才建立：日界要用交易所的時間，不是本機時鐘。
    pnl: Option<DailyPnl>,
}

impl Trader {
    pub(crate) fn new(
        config: &TestnetConfig,
        gateway: Box<dyn OrderGateway + Send>,
        gate: Arc<dyn PortfolioGate>,
        kill_switch: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
    ) -> Result<Trader, ConfigError> {
        if config.initial_cash <= Fixed::ZERO {
            return Err(ConfigError::NonPositiveCash);
        }
        // 費率在這裡就解析掉：跑起來之後才發現「費率表裡沒有這個交易對」等於
        // 送單當下才失敗，太晚了。
        let fee_model = *config
            .fees
            .get(&config.symbol)
            .ok_or_else(|| ConfigError::UnknownFeeRate(config.symbol.clone()))?;
        Ok(Trader {
            symbol: config.symbol.clone(),
            rules: config.rules,
            fee_model,
            limits: config.limits,
            gate,
            gateway,
            kill_switch,
            stop,
            backoff_ms: POLL_BACKOFF_MS,
            cash: config.initial_cash,
            position: Fixed::ZERO,
            fills: 0,
            blocked: 0,
            fees_paid: Fixed::ZERO,
            pnl: None,
        })
    }

    /// 把一則行情事件的交易所時間轉給全域閘門當心跳（「行情中斷」熔斷規則唯一的
    /// 輸入，理由見 [`at_portfolio_risk::gate`] 的模組文件）。
    ///
    /// **未收盤的 K 線與 ticker 也要餵**：心跳是「這條連線還活著」，不是「又有一根
    /// 收盤 K 線」。這裡不記帳、不下單、不碰策略。
    pub(crate) fn observe_market_event(&self, at_ms: i64) {
        self.gate.observe_market_event(at_ms);
    }

    /// 處理一根收盤 K 線。`now_ms` 是這根 K 線的交易所事件時間（UTC 毫秒）。
    ///
    /// 回 `Err` 表示**帳本從這一刻起不可信**（送單結果不明、輪詢逾時、算術溢位），
    /// 呼叫端收到就該停止整個迴圈、把錯誤告訴使用者。
    pub(crate) fn on_closed_bar(
        &mut self,
        bar: &Bar,
        now_ms: i64,
        strategy: &mut dyn Strategy,
    ) -> Result<(Option<OrderOutcome>, TestnetSnapshot), TradingError> {
        let price = bar.close;
        if price <= Fixed::ZERO {
            return Err(TradingError::Arithmetic);
        }
        let equity = self.equity(price)?;
        let daily_pnl = self.update_daily_pnl(now_ms, equity);

        // 不管閘門會不會擋，策略每一根都要被問到：指標的 rolling window 少一根
        // 就全錯了，「今天不下單」不等於「今天不看盤」。
        let target = strategy.on_bar(bar);

        let outcome = match self.plan_order(target, price, equity)? {
            None => None,
            Some((side, qty)) => match self.rules.check(self.reference_price(side, price)?, qty) {
                Err(violation) => Some(OrderOutcome::Invalid(violation)),
                Ok(()) => Some(self.send_gated_order(side, qty, price, daily_pnl, now_ms)?),
            },
        };

        // 成交會改現金與部位，所以權益與今日損益都要重算一次。
        let equity = self.equity(price)?;
        let daily_pnl = self.update_daily_pnl(now_ms, equity);
        Ok((
            outcome,
            TestnetSnapshot {
                point: EquityPoint {
                    open_time: bar.open_time,
                    equity,
                },
                cash: self.cash,
                position: self.position,
                fills: self.fills,
                blocked: self.blocked,
                fees_paid: self.fees_paid,
                daily_pnl,
                kill_switch: self.kill_switch.load(Ordering::Relaxed),
            },
        ))
    }

    /// 權益 = 現金 + 持倉市值（用收盤價評價）。
    fn equity(&self, price: Fixed) -> Result<Fixed, TradingError> {
        self.position
            .checked_mul(price)
            .and_then(|value| self.cash.checked_add(value))
            .ok_or(TradingError::Arithmetic)
    }

    /// 回報權益並取得今日損益；第一次呼叫時順便建立追蹤器。
    fn update_daily_pnl(&mut self, now_ms: i64, equity: Fixed) -> Option<Fixed> {
        match &mut self.pnl {
            Some(pnl) => pnl.update(now_ms, equity),
            none => {
                *none = Some(DailyPnl::new(now_ms, equity));
                Some(Fixed::ZERO)
            }
        }
    }

    /// 算下單金額要用的參考價：收盤價取整到交易所的價格跳動。
    ///
    /// 市價單不帶價格，但 [`SymbolRules::check`] 要用一個價格算最小金額；
    /// 收盤價理論上一定落在 tick 上，取整只是不信任外部輸入。
    fn reference_price(&self, side: Side, price: Fixed) -> Result<Fixed, TradingError> {
        self.rules
            .round_price(side, price)
            .ok_or(TradingError::Arithmetic)
    }

    /// 從目標部位算出「這根要送什麼單」，`None` 表示不用動。
    ///
    /// ```text
    /// 目標數量 = 目標比例 × 權益 ÷ (收盤價 × (1 + 吃單費率))   // 往下取整到數量級距
    /// 要成交的量 = 目標數量 − 目前數量
    /// ```
    ///
    /// 費率一律用**買方**吃單費率，不分這一筆是買是賣：目的是留住付手續費的錢，
    /// 寧可少買一點也不要算出一個現金付不出手續費的數量。
    fn plan_order(
        &self,
        target: at_core::TargetPosition,
        price: Fixed,
        equity: Fixed,
    ) -> Result<Option<(Side, Fixed)>, TradingError> {
        // 現貨不能做空：負的目標部位當成空手處理（見 docs/steps/6.4）。
        let ratio = if target.ratio().is_negative() {
            Fixed::ZERO
        } else {
            target.ratio()
        };
        let budget = ratio
            .checked_mul(equity)
            .ok_or(TradingError::Arithmetic)?
            .max(Fixed::ZERO);
        let rate = self
            .fee_model
            .rate(Liquidity::Taker, Side::Buy)
            .ok_or(TradingError::Arithmetic)?;
        let unit_cost = Fixed::ONE
            .checked_add(rate)
            .and_then(|mult| price.checked_mul(mult))
            .ok_or(TradingError::Arithmetic)?;
        if unit_cost <= Fixed::ZERO {
            return Err(TradingError::Arithmetic);
        }
        // qty_for_quote 已經包含「往下取整到數量級距」。
        let want = self
            .rules
            .qty_for_quote(budget, unit_cost)
            .ok_or(TradingError::Arithmetic)?;
        let delta = want
            .checked_sub(self.position)
            .ok_or(TradingError::Arithmetic)?;
        let side = if delta.is_negative() {
            Side::Sell
        } else {
            Side::Buy
        };
        let qty = self
            .rules
            .round_qty(delta.abs())
            .ok_or(TradingError::Arithmetic)?;
        if qty.is_zero() {
            return Ok(None);
        }
        Ok(Some((side, qty)))
    }

    /// **全專案唯一會送出訂單的函式。**
    ///
    /// 前兩件事是過兩道閘門：[`RiskLimits::check`]（6.3）然後
    /// [`PortfolioGate::check`]（全域熔斷）。任何一道擋下就記一筆、回對應的
    /// `*Blocked`，不重試、也不會往下走到 [`OrderGateway::place_market_order`]。
    ///
    /// `now_ms` 是這根 K 線的交易所事件時間，原樣交給全域閘門——閘門的時間一定要
    /// 和心跳同一個時鐘來源，所以這裡不自己讀時鐘、也不轉換。
    fn send_gated_order(
        &mut self,
        side: Side,
        qty: Fixed,
        price: Fixed,
        daily_pnl: Option<Fixed>,
        now_ms: i64,
    ) -> Result<OrderOutcome, TradingError> {
        let state = AccountState {
            kill_switch: self.kill_switch.load(Ordering::Relaxed),
            position: self.position,
            daily_pnl,
        };
        let order = ProposedOrder {
            side,
            quantity: qty,
            reference_price: price,
        };
        if let Err(blocked) = self.limits.check(&state, &order) {
            self.blocked += 1;
            return Ok(OrderOutcome::Blocked(blocked));
        }
        // 第二道閘門。擋下的理由（含「風控狀態讀不到」）一律當成「不送單」，
        // 不是致命錯誤：什麼都還沒送出去，下一根 K 線仍然會被評估。
        if let Err(blocked) = self.gate.check(now_ms) {
            self.blocked += 1;
            return Ok(OrderOutcome::GloballyBlocked(blocked));
        }

        let placed = self
            .gateway
            .place_market_order(self.symbol.as_str(), to_order_side(side), qty)
            // 送單請求失敗時「訂單到底有沒有進交易所」是不確定的，所以這是致命錯誤，
            // 不是跳過這一根：繼續用一本可能已經錯的帳交易比停下來危險得多。
            .map_err(|e| TradingError::PlaceFailed {
                message: e.to_string(),
            })?;
        // 不論成交多少、也不論最後是哪個終態，先把已成交的部分寫進帳本——
        // 漏記已成交的量，之後每一筆調倉都會算錯。
        let final_state = self.await_final(placed);
        let report = match &final_state {
            Ok(response) | Err((_, response)) => self.apply_fill(side, qty, response)?,
        };
        match final_state {
            Ok(_) => Ok(OrderOutcome::Filled(report)),
            Err((error, _)) => Err(error),
        }
    }

    /// 輪詢到終態。沒到終態就回 `Err((原因, 最後看到的回報))`，讓呼叫端照樣把
    /// 已成交的部分記進帳本之後才往外通報。
    fn await_final(
        &self,
        placed: OrderResponse,
    ) -> Result<OrderResponse, (TradingError, OrderResponse)> {
        let mut latest = placed;
        let mut attempt = 0;
        loop {
            if is_final(latest.status) {
                return Ok(latest);
            }
            if attempt >= self.backoff_ms.len() {
                return Err((
                    TradingError::OrderNotFinal {
                        order_id: latest.order_id,
                        status: latest.status,
                        executed_qty: latest.executed_qty,
                    },
                    latest,
                ));
            }
            if !self.sleep_watching_stop(self.backoff_ms[attempt]) {
                return Err((
                    TradingError::StoppedWhileWaiting {
                        order_id: latest.order_id,
                        status: latest.status,
                        executed_qty: latest.executed_qty,
                    },
                    latest,
                ));
            }
            attempt += 1;
            // 查單是唯讀、可重複的操作，查失敗就用掉一次退避額度再試；
            // 訂單已經在交易所了，不知道它的下場比多查一次糟。
            if let Ok(response) = self
                .gateway
                .query_order(self.symbol.as_str(), latest.order_id)
            {
                latest = response;
            }
        }
    }

    /// 睡 `ms` 毫秒，但每 [`STOP_CHECK_SLICE`] 回頭看一次停止旗標。
    /// 回 `false` 表示使用者要求停止了。
    fn sleep_watching_stop(&self, ms: u64) -> bool {
        let mut left = Duration::from_millis(ms);
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            if left.is_zero() {
                return true;
            }
            let slice = left.min(STOP_CHECK_SLICE);
            std::thread::sleep(slice);
            left -= slice;
        }
    }

    /// 用交易所回報的成交量與成交金額更新帳本。手續費是估算的，不是交易所回報的。
    fn apply_fill(
        &mut self,
        side: Side,
        requested_qty: Fixed,
        response: &OrderResponse,
    ) -> Result<FillReport, TradingError> {
        let executed = response.executed_qty;
        let quote = response.cummulative_quote_qty;
        if executed.is_negative() || quote.is_negative() {
            return Err(TradingError::Arithmetic);
        }
        // 兩個欄位必須同時是 0 或同時不是 0：一個有量一個沒有是交易所回應自相矛盾，
        // 判不出來就擋，不要猜哪個欄位才是對的。
        if executed.is_zero() != quote.is_zero() {
            return Err(TradingError::Arithmetic);
        }
        let rate = self
            .fee_model
            .rate(Liquidity::Taker, side)
            .ok_or(TradingError::Arithmetic)?;
        let fee = quote.checked_mul(rate).ok_or(TradingError::Arithmetic)?;

        if !executed.is_zero() {
            let (cash, position) = match side {
                Side::Buy => (
                    self.cash
                        .checked_sub(quote)
                        .and_then(|c| c.checked_sub(fee))
                        .ok_or(TradingError::Arithmetic)?,
                    self.position
                        .checked_add(executed)
                        .ok_or(TradingError::Arithmetic)?,
                ),
                Side::Sell => {
                    // 真實的 Binance 現貨不會讓賣單成交超過實際持倉（餘額不足會直接被
                    // 拒絕），但閘門不該假設下游一定遵守這件事——賣超會讓部位變成負的，
                    // 下一輪 `plan_order` 又會拿一個已經不對的部位去算，錯誤會越滾越大。
                    if executed > self.position {
                        return Err(TradingError::Arithmetic);
                    }
                    (
                        self.cash
                            .checked_add(quote)
                            .and_then(|c| c.checked_sub(fee))
                            .ok_or(TradingError::Arithmetic)?,
                        self.position
                            .checked_sub(executed)
                            .ok_or(TradingError::Arithmetic)?,
                    )
                }
            };
            self.cash = cash;
            self.position = position;
            self.fees_paid = self
                .fees_paid
                .checked_add(fee)
                .ok_or(TradingError::Arithmetic)?;
            self.fills += 1;
        }

        Ok(FillReport {
            order_id: response.order_id,
            side,
            requested_qty,
            executed_qty: executed,
            quote_qty: quote,
            // 沒成交就沒有手續費。
            fee: if executed.is_zero() { Fixed::ZERO } else { fee },
            status: response.status,
        })
    }
}

/// 這個狀態之後還會不會變。
fn is_final(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::Filled
            | OrderStatus::Canceled
            | OrderStatus::Rejected
            | OrderStatus::Expired
            | OrderStatus::ExpiredInMatch
    )
}

fn to_order_side(side: Side) -> OrderSide {
    match side {
        Side::Buy => OrderSide::Buy,
        Side::Sell => OrderSide::Sell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{FeeSchedule, Symbol, TargetPosition};
    use at_portfolio_risk::breaker::DEFAULT_CONSECUTIVE_LOSSES;
    use at_portfolio_risk::{BreakerAction, BreakerTrip};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-10-04T00:00:00Z（UTC 日序 20000），加上幾分鐘當事件時間。
    const T0: i64 = 20_000 * 86_400_000;
    const MINUTE_MS: i64 = 60_000;

    fn bar(index: i64, close: &str) -> Bar {
        let close = fx(close);
        Bar {
            open_time: T0 + index * MINUTE_MS,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            order_flow: None,
        }
    }

    /// 刻意用好算的級距：數量 0.01 一跳、最小金額 10 USDT。
    fn rules() -> SymbolRules {
        SymbolRules::new(fx("0.01"), fx("0.01"), fx("0.01"), fx("9000"), fx("10")).unwrap()
    }

    fn symbol() -> Symbol {
        Symbol::new("BTCUSDT").unwrap()
    }

    /// 0.1% 吃單費（Binance 現貨 VIP 0）。
    fn fees() -> FeeSchedule {
        let mut fees = FeeSchedule::new();
        fees.insert(symbol(), FeeModel::spot_vip0());
        fees
    }

    fn zero_fees() -> FeeSchedule {
        let mut fees = FeeSchedule::new();
        fees.insert(
            symbol(),
            FeeModel::Spot(at_core::SpotFees {
                standard: at_core::CommissionRates::default(),
                tax: at_core::CommissionRates::default(),
                special: at_core::CommissionRates::default(),
                bnb_discount: None,
            }),
        );
        fees
    }

    /// 每日最多虧 1000、單筆最多 50000。
    fn config(fees: FeeSchedule) -> TestnetConfig {
        TestnetConfig {
            symbol: symbol(),
            initial_cash: fx("10000"),
            rules: rules(),
            fees,
            limits: RiskLimits::new(fx("1000"), fx("50000")).unwrap(),
        }
    }

    /// 回放事先寫好的交易所回報，並記下「真的被送出去的單」。
    #[derive(Default)]
    struct FakeExchange {
        places: Mutex<VecDeque<Result<OrderResponse, BinanceError>>>,
        queries: Mutex<VecDeque<Result<OrderResponse, BinanceError>>>,
        sent: Mutex<Vec<(String, OrderSide, Fixed)>>,
        /// 查單次數（驗證真的有輪詢）。
        polls: Mutex<usize>,
    }

    impl FakeExchange {
        fn with_place(response: OrderResponse) -> Arc<FakeExchange> {
            let fake = FakeExchange::default();
            fake.places.lock().unwrap().push_back(Ok(response));
            Arc::new(fake)
        }

        fn sent(&self) -> Vec<(String, OrderSide, Fixed)> {
            self.sent.lock().unwrap().clone()
        }

        fn polls(&self) -> usize {
            *self.polls.lock().unwrap()
        }
    }

    impl OrderGateway for Arc<FakeExchange> {
        fn place_market_order(
            &self,
            symbol: &str,
            side: OrderSide,
            quantity: Fixed,
        ) -> Result<OrderResponse, BinanceError> {
            self.sent
                .lock()
                .unwrap()
                .push((symbol.to_string(), side, quantity));
            self.places
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(BinanceError::Http("測試沒有準備回報".to_string())))
        }

        fn query_order(
            &self,
            _symbol: &str,
            _order_id: u64,
        ) -> Result<OrderResponse, BinanceError> {
            *self.polls.lock().unwrap() += 1;
            let mut queries = self.queries.lock().unwrap();
            // 最後一則留著重複回放：「永遠不到終態」的情境要用它。
            if queries.len() > 1 {
                queries.pop_front().unwrap()
            } else {
                match queries.front() {
                    Some(Ok(r)) => Ok(r.clone()),
                    Some(Err(_)) | None => Err(BinanceError::Http("查不到".to_string())),
                }
            }
        }
    }

    fn response(status: OrderStatus, orig: &str, executed: &str, quote: &str) -> OrderResponse {
        OrderResponse {
            symbol: "BTCUSDT".to_string(),
            order_id: 42,
            client_order_id: "test".to_string(),
            status,
            orig_qty: fx(orig),
            executed_qty: fx(executed),
            cummulative_quote_qty: fx(quote),
        }
    }

    /// 固定回同一個目標部位，並記下自己被問過哪幾根。
    struct Fixedly {
        want: TargetPosition,
        seen: Vec<i64>,
    }

    impl Fixedly {
        fn new(want: TargetPosition) -> Fixedly {
            Fixedly {
                want,
                seen: Vec::new(),
            }
        }
    }

    impl Strategy for Fixedly {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.seen.push(bar.open_time);
            self.want
        }
    }

    /// 測試用的全域閘門：預設放行，記下每一次心跳與檢查，可以切換成擋單。
    ///
    /// 刻意不在 production 程式碼裡提供「永遠放行」的實作：那等於在 crate 裡放一條
    /// 繞過全域風控的現成暗路。
    #[derive(Default)]
    struct FakeGate {
        block_with: Mutex<Option<GlobalBlocked>>,
        beats: Mutex<Vec<i64>>,
        checks: Mutex<Vec<i64>>,
    }

    impl FakeGate {
        fn allowing() -> Arc<FakeGate> {
            Arc::new(FakeGate::default())
        }

        fn blocking(blocked: GlobalBlocked) -> Arc<FakeGate> {
            let gate = FakeGate::default();
            *gate.block_with.lock().unwrap() = Some(blocked);
            Arc::new(gate)
        }

        fn checks(&self) -> Vec<i64> {
            self.checks.lock().unwrap().clone()
        }

        fn beats(&self) -> Vec<i64> {
            self.beats.lock().unwrap().clone()
        }
    }

    impl PortfolioGate for FakeGate {
        fn observe_market_event(&self, at_ms: i64) {
            self.beats.lock().unwrap().push(at_ms);
        }

        fn check(&self, now_ms: i64) -> Result<(), GlobalBlocked> {
            self.checks.lock().unwrap().push(now_ms);
            match self.block_with.lock().unwrap().clone() {
                Some(blocked) => Err(blocked),
                None => Ok(()),
            }
        }
    }

    fn a_trip() -> BreakerTrip {
        BreakerTrip {
            trigger: DEFAULT_CONSECUTIVE_LOSSES,
            action: BreakerAction::PauseStrategy,
        }
    }

    /// 建一個 [`Trader`]：退避表換成全 0，測試不用真的等。
    fn trader(
        config: &TestnetConfig,
        gateway: Arc<FakeExchange>,
    ) -> (Trader, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (trader, kill, stop, _gate) = trader_gated(config, gateway, FakeGate::allowing());
        (trader, kill, stop)
    }

    /// 同上，但指定全域閘門，並把它一起回傳給測試斷言用。
    fn trader_gated(
        config: &TestnetConfig,
        gateway: Arc<FakeExchange>,
        gate: Arc<FakeGate>,
    ) -> (Trader, Arc<AtomicBool>, Arc<AtomicBool>, Arc<FakeGate>) {
        let kill = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let mut trader = Trader::new(
            config,
            Box::new(gateway),
            Arc::clone(&gate) as Arc<dyn PortfolioGate>,
            Arc::clone(&kill),
            Arc::clone(&stop),
        )
        .expect("設定應該合法");
        trader.backoff_ms = &[0, 0, 0];
        (trader, kill, stop, gate)
    }

    // ---- 設定檢查 ----

    #[test]
    fn non_positive_initial_cash_is_rejected() {
        let mut config = config(fees());
        config.initial_cash = Fixed::ZERO;
        let Err(error) = Trader::new(
            &config,
            Box::new(Arc::new(FakeExchange::default())),
            FakeGate::allowing(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ) else {
            panic!("起始資金是 0 應該被擋下");
        };
        assert_eq!(error, ConfigError::NonPositiveCash);
    }

    #[test]
    fn a_symbol_without_a_fee_rate_is_rejected_before_trading() {
        let mut config = config(FeeSchedule::new());
        config.symbol = symbol();
        let Err(error) = Trader::new(
            &config,
            Box::new(Arc::new(FakeExchange::default())),
            FakeGate::allowing(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ) else {
            panic!("費率表裡沒有這個交易對，應該在跑起來之前就被擋下");
        };
        assert_eq!(error, ConfigError::UnknownFeeRate(symbol()));
    }

    // ---- 正常情境 ----

    #[test]
    fn going_long_sends_a_buy_and_the_ledger_follows_the_exchange_report() {
        // 10000 USDT、價格 100、0.1% 吃單費
        // → 目標數量 = 10000 / (100 × 1.001) = 99.9000999… → 取整到 0.01 = 99.9
        // 交易所回報 99.9 顆、花了 9990 USDT，手續費估 9990 × 0.001 = 9.99
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "99.9", "99.9", "9990"));
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("正常成交不該回錯誤");

        assert_eq!(
            gateway.sent(),
            vec![("BTCUSDT".to_string(), OrderSide::Buy, fx("99.9"))],
            "送出去的必須是 BUY 99.9（交易對、方向、數量都要對）"
        );
        assert_eq!(
            outcome,
            Some(OrderOutcome::Filled(FillReport {
                order_id: 42,
                side: Side::Buy,
                requested_qty: fx("99.9"),
                executed_qty: fx("99.9"),
                quote_qty: fx("9990"),
                fee: fx("9.99"),
                status: OrderStatus::Filled,
            }))
        );
        assert_eq!(snapshot.position, fx("99.9"), "部位來自交易所回報的成交量");
        assert_eq!(snapshot.cash, fx("0.01"), "10000 − 9990 − 9.99");
        assert_eq!(snapshot.fees_paid, fx("9.99"));
        assert_eq!(snapshot.fills, 1);
        assert_eq!(snapshot.blocked, 0);
        // 權益 = 0.01 + 99.9 × 100 = 9990.01（少掉的就是手續費）
        assert_eq!(snapshot.point.equity, fx("9990.01"));
        assert_eq!(snapshot.point.open_time, T0);
        assert_eq!(snapshot.daily_pnl, Some(fx("-9.99")));
        assert_eq!(strategy.seen, vec![T0]);
    }

    #[test]
    fn a_flat_strategy_never_sends_an_order() {
        let gateway = Arc::new(FakeExchange::default());
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FLAT);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("空手不該出錯");

        assert_eq!(outcome, None, "沒有要調倉就不該有下單這件事");
        assert!(gateway.sent().is_empty(), "一張單都不可以送出去");
        assert_eq!(snapshot.cash, fx("10000"));
        assert_eq!(snapshot.position, Fixed::ZERO);
        assert_eq!(snapshot.point.equity, fx("10000"));
    }

    #[test]
    fn holding_the_same_target_does_not_re_send_the_same_order() {
        // 零費率：滿倉做多剛好 100 顆，之後價格不動就不必調倉。
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "100", "100", "10000"));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("第一根應該買進");
        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(1, "100"), T0 + 2 * MINUTE_MS, &mut strategy)
            .expect("第二根不該出錯");

        assert_eq!(outcome, None, "目標沒變、價格沒變 → 不送單");
        assert_eq!(gateway.sent().len(), 1, "只能送過一張單");
        assert_eq!(snapshot.position, fx("100"));
        assert_eq!(snapshot.point.equity, fx("10000"));
    }

    #[test]
    fn selling_back_to_flat_sends_a_sell_for_the_whole_position() {
        let gateway = Arc::new(FakeExchange::default());
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "10000",
        )));
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "11000",
        )));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));

        let mut long = Fixedly::new(TargetPosition::FULL_LONG);
        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("買進");
        let mut flat = Fixedly::new(TargetPosition::FLAT);
        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(1, "110"), T0 + 2 * MINUTE_MS, &mut flat)
            .expect("賣出");

        assert_eq!(
            gateway.sent()[1],
            ("BTCUSDT".to_string(), OrderSide::Sell, fx("100")),
            "回到空手要賣掉全部部位"
        );
        assert!(matches!(outcome, Some(OrderOutcome::Filled(_))));
        assert_eq!(snapshot.position, Fixed::ZERO);
        assert_eq!(snapshot.cash, fx("11000"), "10000 − 10000 + 11000");
        assert_eq!(snapshot.daily_pnl, Some(fx("1000")), "今日獲利 1000");
    }

    #[test]
    fn a_short_target_is_treated_as_flat_on_spot() {
        let gateway = Arc::new(FakeExchange::default());
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "10000",
        )));
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "10000",
        )));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));

        let mut long = Fixedly::new(TargetPosition::FULL_LONG);
        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("先買進");
        let mut short = Fixedly::new(TargetPosition::short(Fixed::ONE));
        trader
            .on_closed_bar(&bar(1, "100"), T0 + 2 * MINUTE_MS, &mut short)
            .expect("做空目標");

        assert_eq!(
            gateway.sent()[1],
            ("BTCUSDT".to_string(), OrderSide::Sell, fx("100")),
            "現貨不能做空：只賣到空手，不可以賣出超過持倉的量"
        );
        assert_eq!(trader.position, Fixed::ZERO);
    }

    #[test]
    fn a_rebalance_below_the_exchange_minimum_is_skipped() {
        // 先滿倉 100 顆，再把目標降到 99.99%：差 0.01 顆 × 100 = 1 USDT < 最小金額 10。
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "100", "100", "10000"));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));

        let mut long = Fixedly::new(TargetPosition::FULL_LONG);
        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("先買進");
        let mut trim = Fixedly::new(TargetPosition::long(fx("0.9999")));
        let (outcome, _) = trader
            .on_closed_bar(&bar(1, "100"), T0 + 2 * MINUTE_MS, &mut trim)
            .expect("跳過不是錯誤");

        assert_eq!(
            outcome,
            Some(OrderOutcome::Invalid(RuleViolation::NotionalTooSmall {
                notional: fx("1"),
                min: fx("10"),
            })),
            "不合規的單要明確回報跳過的原因，不是硬送出去讓交易所拒絕"
        );
        assert_eq!(gateway.sent().len(), 1, "第二根不可以送單");
    }

    // ---- 風控閘門 ----

    #[test]
    fn the_kill_switch_stops_every_order_from_being_sent() {
        let gateway = Arc::new(FakeExchange::default());
        let config = config(fees());
        let (mut trader, kill, _stop) = trader(&config, Arc::clone(&gateway));
        kill.store(true, Ordering::Relaxed);
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("被擋下不是錯誤");

        assert_eq!(outcome, Some(OrderOutcome::Blocked(Blocked::KillSwitch)));
        assert!(
            gateway.sent().is_empty(),
            "一鍵停止之後絕對不可以有任何單被送出去"
        );
        assert_eq!(snapshot.blocked, 1);
        assert!(snapshot.kill_switch);
        assert_eq!(
            strategy.seen,
            vec![T0],
            "閘門擋下送單，但策略還是要看到每一根 K 線，不然指標會算錯"
        );
    }

    #[test]
    fn the_daily_loss_limit_blocks_opening_a_new_position() {
        // 帳本先虧超過 1000：買進後價格腰斬。
        let gateway = Arc::new(FakeExchange::default());
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "10000",
        )));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));

        let mut long = Fixedly::new(TargetPosition::FULL_LONG);
        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("先買進 100 顆");
        // 價格掉到 50：權益 5000，今日虧 5000 > 上限 1000。
        // 目標比例 1 在新的權益下只需要 100 顆，剛好不用調倉，所以改成加碼的情境：
        // 目標 2 倍（現貨其實做不到，但足以讓閘門看到一張「增加曝險」的買單）。
        let mut add = Fixedly::new(TargetPosition::new(fx("2")));
        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(1, "50"), T0 + 2 * MINUTE_MS, &mut add)
            .expect("被擋下不是錯誤");

        assert_eq!(
            outcome,
            Some(OrderOutcome::Blocked(Blocked::DailyLossReached {
                pnl: fx("-5000"),
                limit: fx("1000"),
            }))
        );
        assert_eq!(gateway.sent().len(), 1, "虧到上限之後不可以再加碼");
        assert_eq!(snapshot.blocked, 1);
    }

    #[test]
    fn the_daily_loss_limit_still_lets_the_position_be_closed() {
        let gateway = Arc::new(FakeExchange::default());
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "10000",
        )));
        gateway.places.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "100",
            "100",
            "5000",
        )));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));

        let mut long = Fixedly::new(TargetPosition::FULL_LONG);
        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("先買進");
        let mut flat = Fixedly::new(TargetPosition::FLAT);
        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(1, "50"), T0 + 2 * MINUTE_MS, &mut flat)
            .expect("平倉應該放行");

        assert!(
            matches!(outcome, Some(OrderOutcome::Filled(_))),
            "虧到上限只擋增加曝險的單，平倉必須放行，否則使用者被鎖在部位裡"
        );
        assert_eq!(snapshot.position, Fixed::ZERO);
        assert_eq!(snapshot.cash, fx("5000"));
    }

    // ---- 全域閘門（熔斷）----

    #[test]
    fn a_tripped_breaker_really_stops_the_order_from_being_sent() {
        // 假交易所準備了一個「會成交」的回報：如果閘門沒擋，這張單就會送出去。
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "99.9", "99.9", "9990"));
        let config = config(fees());
        let (mut trader, _kill, _stop, gate) = trader_gated(
            &config,
            Arc::clone(&gateway),
            FakeGate::blocking(GlobalBlocked::BreakerTripped(a_trip())),
        );
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("被擋下不是錯誤");

        assert_eq!(
            outcome,
            Some(OrderOutcome::GloballyBlocked(
                GlobalBlocked::BreakerTripped(a_trip())
            )),
            "熔斷擋下的單要回 GloballyBlocked，不能混進 6.3 的 Blocked"
        );
        assert!(
            gateway.sent().is_empty(),
            "熔斷觸發時絕對不可以有任何單被送出去"
        );
        assert_eq!(snapshot.blocked, 1, "被全域閘門擋下也要算進擋單次數");
        assert_eq!(snapshot.position, Fixed::ZERO, "沒送單就不該有部位");
        assert_eq!(snapshot.cash, fx("10000"), "沒送單就不該動現金");
        assert_eq!(
            gate.checks(),
            vec![T0 + MINUTE_MS],
            "閘門收到的時間必須是這根 K 線的交易所事件時間"
        );
        assert_eq!(
            strategy.seen,
            vec![T0],
            "熔斷擋下送單，但策略還是要看到每一根 K 線"
        );
    }

    #[test]
    fn an_unreadable_risk_state_fails_closed_and_blocks_the_order() {
        // 「熔斷器自己壞掉」（鎖被毒化、累加器不存在）走的是同一條擋單路徑，
        // 不是放行——這是 fail-closed 的核心斷言。
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "99.9", "99.9", "9990"));
        let config = config(fees());
        let (mut trader, _kill, _stop, _gate) = trader_gated(
            &config,
            Arc::clone(&gateway),
            FakeGate::blocking(GlobalBlocked::RiskStateUnavailable),
        );
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("擋下不是致命錯誤：什麼都沒送出去");

        assert_eq!(
            outcome,
            Some(OrderOutcome::GloballyBlocked(
                GlobalBlocked::RiskStateUnavailable
            ))
        );
        assert!(
            gateway.sent().is_empty(),
            "風控狀態讀不到時放行，等於風控從來沒存在過"
        );
        assert_eq!(snapshot.blocked, 1);
    }

    #[test]
    fn an_armed_but_quiet_breaker_lets_the_order_through() {
        // 對照組：同樣的設定、閘門放行，單就真的送出去——證明上面兩個測試擋下的
        // 原因是閘門，不是別的東西。
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "99.9", "99.9", "9990"));
        let config = config(fees());
        let (mut trader, _kill, _stop, gate) =
            trader_gated(&config, Arc::clone(&gateway), FakeGate::allowing());
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("正常成交不該回錯誤");

        assert!(
            matches!(outcome, Some(OrderOutcome::Filled(_))),
            "閘門放行時這張單應該真的送出去，實際是 {outcome:?}"
        );
        assert_eq!(
            gateway.sent(),
            vec![("BTCUSDT".to_string(), OrderSide::Buy, fx("99.9"))]
        );
        assert_eq!(snapshot.blocked, 0);
        assert_eq!(gate.checks().len(), 1, "送單前必須問過閘門，而且只問一次");
    }

    #[test]
    fn the_session_gate_speaks_first_when_both_gates_would_block() {
        // 兩道閘門都會擋時，回報的是第一道的理由（ADR 4.1）：6.4 既有的擋單
        // 測試因此完全不受這個改動影響。
        let gateway = Arc::new(FakeExchange::default());
        let config = config(fees());
        let (mut trader, kill, _stop, gate) = trader_gated(
            &config,
            Arc::clone(&gateway),
            FakeGate::blocking(GlobalBlocked::BreakerTripped(a_trip())),
        );
        kill.store(true, Ordering::Relaxed);
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("被擋下不是錯誤");

        assert_eq!(outcome, Some(OrderOutcome::Blocked(Blocked::KillSwitch)));
        assert!(gateway.sent().is_empty());
        assert_eq!(snapshot.blocked, 1, "一張單被擋下只算一次");
        assert!(
            gate.checks().is_empty(),
            "第一道閘門已經擋下，不需要也不該再問第二道"
        );
    }

    #[test]
    fn a_bar_that_needs_no_order_never_asks_the_gate() {
        // 不用調倉的那根 K 線沒有「這筆單」可言，閘門不該被問（也不會因此記下
        // 一筆沒發生過的檢查）。
        let gateway = Arc::new(FakeExchange::default());
        let config = config(fees());
        let (mut trader, _kill, _stop, gate) =
            trader_gated(&config, Arc::clone(&gateway), FakeGate::allowing());
        let mut strategy = Fixedly::new(TargetPosition::FLAT);

        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("空手不送單");

        assert!(gate.checks().is_empty());
        assert!(gateway.sent().is_empty());
    }

    #[test]
    fn a_market_event_is_forwarded_to_the_gate_as_a_heartbeat() {
        let config = config(fees());
        let (trader, _kill, _stop, gate) = trader_gated(
            &config,
            Arc::new(FakeExchange::default()),
            FakeGate::allowing(),
        );

        trader.observe_market_event(T0 + 1_000);
        trader.observe_market_event(T0 + 2_000);

        assert_eq!(gate.beats(), vec![T0 + 1_000, T0 + 2_000]);
    }

    // ---- 等回報 ----

    #[test]
    fn a_non_final_status_is_polled_until_it_becomes_final() {
        let gateway = FakeExchange::with_place(response(OrderStatus::New, "99.9", "0", "0"));
        gateway.queries.lock().unwrap().push_back(Ok(response(
            OrderStatus::PartiallyFilled,
            "99.9",
            "50",
            "5000",
        )));
        gateway.queries.lock().unwrap().push_back(Ok(response(
            OrderStatus::Filled,
            "99.9",
            "99.9",
            "9990",
        )));
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("輪詢到終態不該出錯");

        assert_eq!(gateway.polls(), 2, "NEW → PARTIALLY_FILLED → FILLED 查兩次");
        let Some(OrderOutcome::Filled(report)) = outcome else {
            panic!("應該是成交回報");
        };
        assert_eq!(report.status, OrderStatus::Filled);
        assert_eq!(report.executed_qty, fx("99.9"));
        assert_eq!(
            snapshot.position,
            fx("99.9"),
            "帳本只能記終態那一則的成交量，不可以把中途的部分成交重複記一次"
        );
        assert_eq!(snapshot.fills, 1);
    }

    #[test]
    fn polling_gives_up_and_reports_the_order_as_not_final() {
        let gateway = FakeExchange::with_place(response(OrderStatus::New, "99.9", "0", "0"));
        // 永遠停在部分成交：輪詢額度用完也不會到終態。
        gateway.queries.lock().unwrap().push_back(Ok(response(
            OrderStatus::PartiallyFilled,
            "99.9",
            "30",
            "3000",
        )));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let error = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect_err("輪詢逾時必須往外通報，不可以吞掉");

        assert_eq!(
            error,
            TradingError::OrderNotFinal {
                order_id: 42,
                status: OrderStatus::PartiallyFilled,
                executed_qty: fx("30"),
            }
        );
        assert_eq!(gateway.polls(), 3, "退避表有三格就查三次，不會無限重試");
        assert_eq!(
            trader.position,
            fx("30"),
            "已經成交的部分還是要記進帳本，不然之後每次調倉都會算錯"
        );
    }

    #[test]
    fn asking_to_stop_while_waiting_for_the_report_is_reported() {
        let gateway = FakeExchange::with_place(response(OrderStatus::New, "99.9", "0", "0"));
        gateway
            .queries
            .lock()
            .unwrap()
            .push_back(Ok(response(OrderStatus::New, "99.9", "0", "0")));
        let config = config(zero_fees());
        let (mut trader, _kill, stop) = trader(&config, Arc::clone(&gateway));
        stop.store(true, Ordering::Relaxed);
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let error = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect_err("等回報的時候被要求停止，要說清楚訂單的下場不明");

        assert_eq!(
            error,
            TradingError::StoppedWhileWaiting {
                order_id: 42,
                status: OrderStatus::New,
                executed_qty: Fixed::ZERO,
            }
        );
        assert_eq!(gateway.polls(), 0, "停止之後不該再查");
    }

    #[test]
    fn a_failing_query_uses_up_a_retry_instead_of_aborting() {
        let gateway = FakeExchange::with_place(response(OrderStatus::New, "99.9", "0", "0"));
        {
            let mut queries = gateway.queries.lock().unwrap();
            queries.push_back(Err(BinanceError::Http("502".to_string())));
            queries.push_back(Ok(response(OrderStatus::Filled, "99.9", "99.9", "9990")));
        }
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, _) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("查單失敗一次之後應該還能查到終態");

        assert_eq!(gateway.polls(), 2);
        assert!(matches!(outcome, Some(OrderOutcome::Filled(_))));
    }

    // ---- 錯誤與拒絕 ----

    #[test]
    fn a_rejected_order_leaves_the_ledger_untouched() {
        let gateway = FakeExchange::with_place(response(OrderStatus::Rejected, "99.9", "0", "0"));
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("被拒絕是終態，不是致命錯誤");

        let Some(OrderOutcome::Filled(report)) = outcome else {
            panic!("應該有一則回報說明訂單被拒絕");
        };
        assert_eq!(report.status, OrderStatus::Rejected);
        assert_eq!(report.executed_qty, Fixed::ZERO);
        assert_eq!(report.fee, Fixed::ZERO, "沒成交就不該記手續費");
        assert_eq!(snapshot.cash, fx("10000"));
        assert_eq!(snapshot.position, Fixed::ZERO);
        assert_eq!(snapshot.fills, 0, "沒有成交量不算一筆成交");
    }

    #[test]
    fn a_response_with_executed_qty_but_no_quote_is_rejected_as_inconsistent() {
        let gateway = FakeExchange::with_place(response(OrderStatus::Filled, "100", "100", "0"));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, gateway);
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let error = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect_err("成交量跟成交金額一個有一個沒有，是回應自相矛盾，不該假裝沒事");
        assert_eq!(error, TradingError::Arithmetic);
    }

    #[test]
    fn a_sell_cannot_apply_a_fill_larger_than_the_tracked_position() {
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(
            &config,
            FakeExchange::with_place(response(OrderStatus::Filled, "100", "100", "10000")),
        );
        // 自己手動把帳本上的部位設成比回報的賣出量還小，模擬下游回報比實際持倉多賣的情境
        // （真實 Binance 現貨不會這樣，但帳本不該信任這件事永遠成立）。
        trader.position = fx("50");

        let error = trader
            .apply_fill(
                Side::Sell,
                fx("100"),
                &response(OrderStatus::Filled, "100", "100", "10000"),
            )
            .expect_err("賣出量超過帳上持倉，擋下來，不要讓部位變負的");
        assert_eq!(error, TradingError::Arithmetic);
        assert_eq!(trader.position, fx("50"), "被擋下就不該動到帳本");
    }

    #[test]
    fn a_partially_filled_order_that_was_canceled_still_updates_the_position() {
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Canceled, "100", "40", "4000"));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let (outcome, snapshot) = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect("CANCELED 是終態");

        let Some(OrderOutcome::Filled(report)) = outcome else {
            panic!("應該有成交回報");
        };
        assert_eq!(report.requested_qty, fx("100"));
        assert_eq!(report.executed_qty, fx("40"));
        assert_eq!(snapshot.position, fx("40"), "只記真的成交的 40 顆");
        assert_eq!(snapshot.cash, fx("6000"));
    }

    #[test]
    fn a_failed_place_request_is_reported_not_swallowed() {
        let gateway = Arc::new(FakeExchange::default());
        gateway
            .places
            .lock()
            .unwrap()
            .push_back(Err(BinanceError::Http("HTTP 狀態碼 418".to_string())));
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);

        let error = trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut strategy)
            .expect_err("送單失敗必須往外通報");

        assert!(matches!(error, TradingError::PlaceFailed { .. }));
        assert_eq!(trader.cash, fx("10000"), "送單失敗不可以動帳本");
        assert_eq!(trader.position, Fixed::ZERO);
    }

    #[test]
    fn a_non_positive_close_price_is_an_error_not_a_division() {
        let config = config(fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::new(FakeExchange::default()));
        let mut strategy = Fixedly::new(TargetPosition::FULL_LONG);
        assert_eq!(
            trader.on_closed_bar(&bar(0, "0"), T0 + MINUTE_MS, &mut strategy),
            Err(TradingError::Arithmetic)
        );
    }

    // ---- 今日損益 ----

    #[test]
    fn the_daily_pnl_resets_on_a_new_utc_day() {
        let gateway =
            FakeExchange::with_place(response(OrderStatus::Filled, "100", "100", "10000"));
        let config = config(zero_fees());
        let (mut trader, _kill, _stop) = trader(&config, Arc::clone(&gateway));
        let mut long = Fixedly::new(TargetPosition::FULL_LONG);

        trader
            .on_closed_bar(&bar(0, "100"), T0 + MINUTE_MS, &mut long)
            .expect("買進");
        // 同一天價格掉到 90：權益 9000，今日虧 1000。
        let (_, same_day) = trader
            .on_closed_bar(&bar(1, "90"), T0 + 2 * MINUTE_MS, &mut long)
            .expect("同一天");
        assert_eq!(same_day.daily_pnl, Some(fx("-1000")));

        // 跨到下一個 UTC 日：基準換成當下權益，今日損益歸零。
        let (_, next_day) = trader
            .on_closed_bar(&bar(2, "90"), T0 + 86_400_000, &mut long)
            .expect("隔天");
        assert_eq!(
            next_day.daily_pnl,
            Some(Fixed::ZERO),
            "昨天的虧損不可以把今天也鎖住"
        );
    }
}
