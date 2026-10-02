//! 測試網自動交易（ROADMAP 6.4）：4.5 的即時行情 → 策略 → 6.3 的風控閘門 →
//! 6.2 的測試網下單 → 等交易所回報 → 更新本地帳本。
//!
//! # 為什麼不是把 `PaperEngine` 包一層
//!
//! 5.1 的 [`PaperEngine`](at_core::PaperEngine) 是**同步、確定性**的：給它一個
//! 目標部位，它當場算出成交價、當場把帳本改完。真實下單沒有這種好事——送出去
//! 之後要等交易所回報，可能回 `NEW`（還在撮合）、可能部分成交、可能被拒絕，
//! 成交價與成交金額都是交易所說了才算。所以這個 crate 是另一套流程
//! 「算目標 → 過閘門 → 送出去 → 等回報 → 用回報更新帳本」，不是模擬成交。
//!
//! 兩者共用的是**策略**（[`Strategy`]）、**型別**（[`Fixed`]、[`at_core::Bar`]、
//! [`SymbolRules`]）和**只有收盤 K 線才處理**這個規則，不共用記帳引擎。
//!
//! # 唯一的送單路徑、兩道並列的閘門
//!
//! 整個 crate 只有 `trader.rs` 的 `Trader::send_gated_order` 會呼叫
//! [`OrderGateway::place_market_order`]，而它的前兩件事是
//! [`RiskLimits::check`](at_risk_control::RiskLimits::check)（6.3，單一 session）
//! 與 [`PortfolioGate::check`](at_portfolio_risk::PortfolioGate::check)
//! （ADR-002，全域熔斷）。兩道都回 `Ok` 才送單。要繞過閘門送單，必須改那個私有
//! 函式的前幾行——不存在「忘記檢查」的呼叫端。
//!
//! 全域閘門是 [`spawn`] 的參數而**不是** `Option`：型別上沒有「這場交易沒有全域
//! 風控」這個狀態。它的實作在 App 層（熔斷累加器要吃 [`TestnetUpdate`] 事件流），
//! 而「行情中斷」規則要的心跳只有這裡的迴圈看得到，所以迴圈每收到一則行情事件
//! （含未收盤 K 線與 ticker）就餵一次
//! [`observe_market_event`](at_portfolio_risk::PortfolioGate::observe_market_event)。
//!
//! 送單對象是 6.2 的 [`BinanceTestnetClient`]，它連網址都寫死在常數裡，
//! 型別上不可能指向正式環境。
//!
//! # 開始交易之前先暖機回放
//!
//! 即時行情是「現在開始」，但策略的指標要一段歷史才有意義
//! （[`Strategy::warmup_bars`]）。所以 [`spawn`] 的第一件事不是等 K 線，而是把
//! 呼叫端給的 [`WarmupBars`] 依序餵給策略、**丟棄這段期間產生的目標部位**，
//! 等指標的內部狀態收斂了才開始真的交易。冷啟動不只是「慢幾十分鐘出訊號」：
//! RSI 這類 Wilder 平滑指標從不同起點餵會收斂到不同的值，訊號會和回測不一樣。
//!
//! **回放期間不可能送單**，而且不是靠紀律：
//! [`WarmupBars::replay`](at_core::WarmupBars::replay) 的參數只有
//! `&mut dyn Strategy`——它拿不到 [`Trader`]、拿不到 [`OrderGateway`]、也拿不到
//! 風控上限，所以根本沒有可以呼叫 [`OrderGateway::place_market_order`] 的東西。
//! 唯一的送單路徑仍然只有 `Trader::send_gated_order`，而回放完全不碰 [`Trader`]。
//!
//! 回放完會記下最後一根的開盤時間當水位線：即時行情裡開盤時間**小於或等於**
//! 它的收盤 K 線一律跳過。抓歷史要花幾百毫秒，這段時間 WebSocket 可能已經把
//! 同一根排進 channel，再處理一次等於拿同一根 K 線重複下單。
//!
//! 歷史 K 線用 `at_binance::market_data::recent_closed_bars` 抓（正式環境的公開
//! 端點——測試網自己的成交太稀疏，拿它暖機等於讓指標收斂到假的市場結構上）。
//! 抓不到要不要擋下啟動，由啟動 session 的那一層決定。
//!
//! # 一鍵停止與停止是兩個開關
//!
//! - [`TestnetTradingHandle::set_kill_switch`]：**只擋送單**。行情連線繼續跑、
//!   快照繼續更新、策略繼續看 K 線。使用者「不再相信這套自動化」時按它，
//!   仍然看得到即時部位與權益。
//! - [`TestnetTradingHandle::stop`]：真正收工。和 5.3 一樣，設的是
//!   [`MarketStreamHandle::stop_flag`] 交出來的同一個旗標，所以按一次會連
//!   WebSocket 一起結束，不留孤兒連線。
//!
//! 一鍵停止被擋下的單不會重試（6.3 的契約），策略照樣每根都被問到——指標的
//! rolling window 少一根就全錯了。
//!
//! # 本地帳本：這個專案沒有「查帳戶餘額」的端點
//!
//! 4.x 只做了唯讀的費率／下單規則／行情，沒有查餘額的端點，所以
//! 「現在有多少現金、權益多少、今天虧多少」只能自己在本地記一本帳：起始資金由
//! 使用者在啟動時提供（[`TestnetConfig::initial_cash`]，比照 5.4 模擬交易頁面
//! 要求輸入虛擬資金），之後每一筆**真實成交回報**更新它。手續費用 4.3 同步到
//! 的帳戶費率估算，不信任測試網回報的 0 手續費。詳細理由寫在
//! `docs/steps/6.4-接上測試網下單.md`。
//!
//! # 用起來像這樣
//!
//! ```no_run
//! use at_binance::testnet::BinanceTestnetClient;
//! use at_core::{FeeModel, FeeSchedule, Fixed, Interval, SmaCross, Symbol, SymbolRules};
//! use at_market_stream::{kline_stream, spawn as spawn_stream};
//! use at_portfolio_risk::{GlobalBlocked, PortfolioGate};
//! use at_risk_control::RiskLimits;
//! use at_testnet_trading::{spawn, TestnetConfig, TestnetUpdate};
//! use std::sync::Arc;
//!
//! // 全域閘門：正式的實作在 App 層（`app/src-tauri/src/risk_control.rs`），它持有
//! // 熔斷規則、累加器與觸發紀錄。這個範例用的是一個只為了讓程式碼編得起來的
//! // 替身——**不要**把這種永遠放行的實作接到真的會送單的路徑上。
//! struct ExampleGate;
//! impl PortfolioGate for ExampleGate {
//!     fn observe_market_event(&self, _at_ms: i64) {}
//!     fn check(&self, _now_ms: i64) -> Result<(), GlobalBlocked> { Ok(()) }
//! }
//!
//! let symbol = Symbol::new("BTCUSDT").unwrap();
//! let mut fees = FeeSchedule::new();
//! fees.insert(symbol.clone(), FeeModel::spot_vip0()); // 實際用 4.3 同步到的費率
//! let config = TestnetConfig {
//!     symbol: symbol.clone(),
//!     initial_cash: Fixed::from_int(10_000).unwrap(),
//!     rules: SymbolRules::new(
//!         "0.01".parse().unwrap(),
//!         "0.00001".parse().unwrap(),
//!         "0.00001".parse().unwrap(),
//!         Fixed::from_int(9_000).unwrap(),
//!         Fixed::from_int(5).unwrap(),
//!     )
//!     .unwrap(),
//!     fees,
//!     limits: RiskLimits::new(
//!         Fixed::from_int(100).unwrap(),
//!         Fixed::from_int(1_000).unwrap(),
//!     )
//!     .unwrap(),
//! };
//!
//! let client = BinanceTestnetClient::from_keychain().unwrap();
//! let strategy = SmaCross::new(10, 30).unwrap();
//!
//! // 暖機：先抓策略宣告需求 × 安全係數根已收盤的歷史 K 線（正式環境公開端點）。
//! // 抓不到就不要開始交易——用沒收斂的指標送真單，訊號和回測不一樣。
//! let warmup = at_binance::market_data::recent_closed_bars(
//!     &symbol,
//!     Interval::M1,
//!     at_core::warmup_fetch_count(&strategy),
//! )
//! .unwrap();
//!
//! let stream = spawn_stream(kline_stream("BTCUSDT", Interval::M1));
//! let live = spawn(
//!     stream,
//!     client,
//!     Box::new(strategy),
//!     &config,
//!     &warmup,
//!     Arc::new(ExampleGate),
//! )
//! .unwrap();
//!
//! // 一鍵停止：只擋送單，行情與快照繼續。
//! live.set_kill_switch(true);
//!
//! for update in &live.updates {
//!     match update {
//!         TestnetUpdate::Order(outcome) => println!("下單結果：{outcome:?}"),
//!         TestnetUpdate::Bar(snapshot) => println!("權益 {}", snapshot.point.equity),
//!         TestnetUpdate::Stopped => break,
//!         TestnetUpdate::Failed(e) => {
//!             eprintln!("測試網交易中止：{e}");
//!             break;
//!         }
//!     }
//! }
//! ```

mod trader;

use at_binance::testnet::{BinanceTestnetClient, OrderResponse, OrderSide, OrderStatus};
use at_binance::BinanceError;
use at_core::{FeeSchedule, Fixed, Strategy, Symbol, SymbolRules, WarmupBars};
use at_market_stream::{MarketEvent, MarketStreamHandle};
use at_portfolio_risk::PortfolioGate;
use at_risk_control::RiskLimits;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use trader::Trader;

pub use trader::{FillReport, OrderGateway, OrderOutcome, TestnetSnapshot};

/// 沒有新行情時，迴圈每隔這麼久回頭看一次停止旗標（同 5.3）。
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// 一次測試網交易 session 的設定。
#[derive(Debug, Clone)]
pub struct TestnetConfig {
    /// 交易對。單一交易對、單一 session（同 5.x 的範圍取捨）。
    pub symbol: Symbol,
    /// 本地帳本的起始現金（報價幣），由使用者在啟動時提供，必須大於 0。
    pub initial_cash: Fixed,
    /// 4.4 同步到的下單規則：數量級距、數量上下限、最小金額。
    pub rules: SymbolRules,
    /// 4.3 同步到的帳戶費率；必須含 [`Self::symbol`]，否則 [`spawn`] 當場回錯誤。
    pub fees: FeeSchedule,
    /// 6.3 的風控上限。
    pub limits: RiskLimits,
}

/// 設定本身有問題，[`spawn`] 當場回報、不會開執行緒也不會送任何單。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    NonPositiveCash,
    UnknownFeeRate(Symbol),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::NonPositiveCash => write!(f, "起始資金必須大於 0"),
            ConfigError::UnknownFeeRate(s) => {
                write!(f, "費率表裡沒有 {s} 的費率，請先同步帳戶費率（4.3）")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// 帳本從這一刻起**不可信**的原因。收到它就停止交易，由人工介入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradingError {
    /// 送單請求本身失敗。訂單**可能已經送到交易所**（例如回應在路上掉了），
    /// 所以這不是「跳過這一根」而是致命錯誤：請到測試網後台確認實際部位。
    PlaceFailed { message: String },
    /// 輪詢用完次數，訂單還沒到終態。已成交的部分已經記進帳本，但這張單還可能
    /// 繼續成交，本地帳本和交易所可能已經不一致。
    OrderNotFinal {
        order_id: u64,
        status: OrderStatus,
        executed_qty: Fixed,
    },
    /// 等回報的過程中使用者要求停止。訂單的最終下場不明，同上。
    StoppedWhileWaiting {
        order_id: u64,
        status: OrderStatus,
        executed_qty: Fixed,
    },
    /// 部位換算或帳本更新溢位／算不出來（含收盤價不是正數）。
    Arithmetic,
}

impl fmt::Display for TradingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TradingError::PlaceFailed { message } => write!(
                f,
                "送單失敗：{message}。訂單可能已經送達交易所，請到測試網後台確認實際部位"
            ),
            TradingError::OrderNotFinal {
                order_id,
                status,
                executed_qty,
            } => write!(
                f,
                "訂單 {order_id} 等不到終態（目前 {status}，已成交 {executed_qty}），本地帳本可能和交易所不一致"
            ),
            TradingError::StoppedWhileWaiting {
                order_id,
                status,
                executed_qty,
            } => write!(
                f,
                "等訂單 {order_id} 回報時被要求停止（目前 {status}，已成交 {executed_qty}），請確認這張單的最終下場"
            ),
            TradingError::Arithmetic => {
                write!(f, "部位換算或帳本更新算不出來，已停止交易")
            }
        }
    }
}

impl std::error::Error for TradingError {}

/// 測試網交易往外送的一則更新。
#[derive(Debug, Clone, PartialEq)]
pub enum TestnetUpdate {
    /// 這根 K 線上「下單這件事」的結果（被擋下／不合規／真的送出去了）。
    /// 這根不用調倉時不會有這一則。排在同一根的 [`Bar`](TestnetUpdate::Bar) 之前。
    Order(OrderOutcome),
    /// 一根收盤 K 線處理完之後的帳本狀態。
    Bar(TestnetSnapshot),
    /// 使用者主動停止（[`TestnetTradingHandle::stop`]）。帳本是完整的，
    /// 不是錯誤。這條串流的最後一則。
    Stopped,
    /// 帳本從這一刻起不可信，串流到此結束。消費端要把錯誤告訴使用者。
    Failed(TradingError),
}

/// [`spawn`] 的控制代碼。
pub struct TestnetTradingHandle {
    pub updates: mpsc::Receiver<TestnetUpdate>,
    /// 和 4.5 行情連線共用的同一個旗標。
    stop: Arc<AtomicBool>,
    kill_switch: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<TestnetSnapshot>>>,
    _worker: thread::JoinHandle<()>,
}

impl TestnetTradingHandle {
    /// 一鍵停止：**只擋送單**，行情連線與快照照常更新。
    ///
    /// 設成 `false` 就恢復送單（風控的其他上限仍然有效）。重複呼叫沒有副作用。
    /// 送單途中（已經送出、正在等回報）按下去不會把那張單撤掉——它已經在
    /// 交易所了；擋的是「下一張」。
    pub fn set_kill_switch(&self, on: bool) {
        self.kill_switch.store(on, Ordering::Relaxed);
    }

    /// 一鍵停止目前的狀態。
    pub fn kill_switch(&self) -> bool {
        self.kill_switch.load(Ordering::Relaxed)
    }

    /// 真正收工：交易迴圈和底層的行情連線都會結束。
    ///
    /// 迴圈結束前會送一則 [`TestnetUpdate::Stopped`]。這**不會**撤掉已經送出去的
    /// 訂單；正在等回報時按下去會收到
    /// [`TradingError::StoppedWhileWaiting`]，請自行確認那張單的下場。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 現在的帳本狀態；還沒有任何一根收盤 K 線處理完時回 `None`。
    pub fn latest_snapshot(&self) -> Option<TestnetSnapshot> {
        self.latest.lock().ok().and_then(|slot| *slot)
    }
}

/// 6.2 的測試網 client 就是正式用的送單窗口，直接轉呼叫。
impl OrderGateway for BinanceTestnetClient {
    fn place_market_order(
        &self,
        symbol: &str,
        side: OrderSide,
        quantity: Fixed,
    ) -> Result<OrderResponse, BinanceError> {
        BinanceTestnetClient::place_market_order(self, symbol, side, quantity)
    }

    fn query_order(&self, symbol: &str, order_id: u64) -> Result<OrderResponse, BinanceError> {
        BinanceTestnetClient::query_order(self, symbol, order_id)
    }
}

/// 把一條即時行情串流 + 一個策略 + 測試網 client 接成一個自動交易迴圈。
///
/// `warmup` 是開始處理即時行情**之前**先餵給策略的歷史 K 線（見 crate 文件的
/// 「開始交易之前先暖機回放」）。回放期間不送單、不記帳。沒有歷史資料可用時
/// 傳 [`WarmupBars::none`]，那是明確選擇冷啟動。
///
/// 設定有問題會**當場**回錯誤，不會開執行緒、也不會送出任何單。
pub fn spawn(
    stream: MarketStreamHandle,
    client: BinanceTestnetClient,
    strategy: Box<dyn Strategy + Send>,
    config: &TestnetConfig,
    warmup: &WarmupBars,
    gate: Arc<dyn PortfolioGate>,
) -> Result<TestnetTradingHandle, ConfigError> {
    let stop = stream.stop_flag();
    spawn_with(
        stream.events,
        stop,
        Box::new(client),
        strategy,
        config,
        warmup,
        gate,
    )
}

/// [`spawn`] 的本體：只要「事件從哪來」、「停止旗標」、「送單窗口」三樣東西。
///
/// 分出這一層是為了測試：塞一個普通的 [`mpsc::channel`] 和一個假交易所進來，
/// 就能驗整條迴圈，完全不連網路。
fn spawn_with(
    events: mpsc::Receiver<MarketEvent>,
    stop: Arc<AtomicBool>,
    gateway: Box<dyn OrderGateway + Send>,
    mut strategy: Box<dyn Strategy + Send>,
    config: &TestnetConfig,
    warmup: &WarmupBars,
    gate: Arc<dyn PortfolioGate>,
) -> Result<TestnetTradingHandle, ConfigError> {
    let kill_switch = Arc::new(AtomicBool::new(false));
    let mut trader = Trader::new(
        config,
        gateway,
        gate,
        Arc::clone(&kill_switch),
        Arc::clone(&stop),
    )?;
    // 設定先驗完再回放：設定是錯的就不該白跑一遍暖機。
    //
    // 回放在這裡（開執行緒之前）同步做完，所以「回放結束才開始交易」是結構上的
    // 先後，不是執行順序的巧合。這一行拿得到的只有策略本身——`trader` 在旁邊，
    // 但回放碰不到它，所以這段期間不存在送單路徑。
    let replayed_through = warmup.replay(strategy.as_mut());
    let latest = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::channel();
    let worker_latest = Arc::clone(&latest);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        run(
            &events,
            &mut trader,
            strategy.as_mut(),
            &tx,
            &worker_latest,
            &worker_stop,
            replayed_through,
        );
    });
    Ok(TestnetTradingHandle {
        updates: rx,
        stop,
        kill_switch,
        latest,
        _worker: worker,
    })
}

/// 迴圈本體：收行情 → 只留收盤 K 線 → 交給 [`Trader`] → 記下狀態 → 送更新。
///
/// 出口和 5.3 一樣有四個，全部是正常結束、都不 panic：停止旗標、行情 channel
/// 關閉、消費端不在了、[`Trader`] 回錯誤（送一次
/// [`TestnetUpdate::Failed`] 就停，不再處理任何一根）。
///
/// `replayed_through` 是暖機回放最後一根的開盤時間（沒回放過是 `None`）：
/// 開盤時間小於或等於它的收盤 K 線已經算在策略的狀態裡了，必須跳過，否則同一根
/// K 線會被拿去下第二次單。
fn run(
    events: &mpsc::Receiver<MarketEvent>,
    trader: &mut Trader,
    strategy: &mut dyn Strategy,
    updates: &mpsc::Sender<TestnetUpdate>,
    latest: &Mutex<Option<TestnetSnapshot>>,
    stop: &AtomicBool,
    replayed_through: Option<i64>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = updates.send(TestnetUpdate::Stopped);
            return;
        }
        let event = match events.recv_timeout(STOP_CHECK_INTERVAL) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        // 心跳先餵給全域閘門，再決定這一則要不要處理：「行情中斷」熔斷規則要的是
        // 「這條連線最後一次有聲音是什麼時候」，未收盤的 K 線與 ticker 同樣算
        // （1 分鐘 K 線的收盤間隔遠大於 3 秒門檻，只拿收盤 K 線當心跳這條規則就
        // 只剩兩種下場：永遠觸發，或門檻失去意義）。
        //
        // 用交易所給的事件時間，不讀本機時鐘——和閘門收到的 `now_ms` 必須同一個
        // 時鐘來源，混用的話算出來的中斷時間是錯的。
        match &event {
            MarketEvent::Kline(update) => trader.observe_market_event(update.event_time_ms),
            MarketEvent::Ticker(ticker) => trader.observe_market_event(ticker.event_time_ms),
        }
        let MarketEvent::Kline(update) = event else {
            continue;
        };
        if !update.is_closed {
            continue;
        }
        // 暖機回放已經餵過的那幾根：抓歷史要花幾百毫秒，這段時間 WebSocket
        // 可能已經把同一根排進 channel 了。沒有這道水位線，同一根 K 線會被
        // 重新評估一次並可能送出第二張單。
        if replayed_through.is_some_and(|through| update.bar.open_time <= through) {
            continue;
        }
        // 時間用交易所給的事件時間，不讀本機時鐘：UTC 日界要和交易所對得上。
        match trader.on_closed_bar(&update.bar, update.event_time_ms, strategy) {
            Ok((outcome, snapshot)) => {
                if let Some(outcome) = outcome {
                    if updates.send(TestnetUpdate::Order(outcome)).is_err() {
                        return;
                    }
                }
                // 先更新「目前狀態」再送事件：消費端收到 Bar 之後馬上問
                // latest_snapshot，看到的不會是上一根。
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(snapshot);
                }
                if updates.send(TestnetUpdate::Bar(snapshot)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = updates.send(TestnetUpdate::Failed(error));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{Bar, FeeModel, Interval, TargetPosition};
    use at_market_stream::KlineUpdate;
    use at_portfolio_risk::GlobalBlocked;
    use std::collections::VecDeque;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-10-04T00:00:00Z
    const T0: i64 = 20_000 * 86_400_000;
    const MINUTE_MS: i64 = 60_000;

    fn symbol() -> Symbol {
        Symbol::new("BTCUSDT").unwrap()
    }

    fn bar(index: i64, close: &str) -> Bar {
        let close = fx(close);
        Bar {
            open_time: T0 + index * MINUTE_MS,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    fn kline(bar: Bar, is_closed: bool) -> MarketEvent {
        MarketEvent::Kline(KlineUpdate {
            symbol: symbol(),
            interval: Interval::M1,
            bar,
            is_closed,
            event_time_ms: bar.open_time + MINUTE_MS,
        })
    }

    fn config() -> TestnetConfig {
        let mut fees = FeeSchedule::new();
        fees.insert(symbol(), FeeModel::spot_vip0());
        TestnetConfig {
            symbol: symbol(),
            initial_cash: fx("10000"),
            rules: SymbolRules::new(fx("0.01"), fx("0.01"), fx("0.01"), fx("9000"), fx("10"))
                .unwrap(),
            fees,
            limits: RiskLimits::new(fx("1000"), fx("50000")).unwrap(),
        }
    }

    /// 回放回報的假交易所，並記下真的被送出去的單。
    #[derive(Default)]
    struct FakeExchange {
        places: Mutex<VecDeque<OrderResponse>>,
        sent: Mutex<Vec<(OrderSide, Fixed)>>,
    }

    impl OrderGateway for Arc<FakeExchange> {
        fn place_market_order(
            &self,
            _symbol: &str,
            side: OrderSide,
            quantity: Fixed,
        ) -> Result<OrderResponse, BinanceError> {
            self.sent.lock().unwrap().push((side, quantity));
            self.places
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| BinanceError::Http("測試沒有準備回報".to_string()))
        }

        fn query_order(
            &self,
            _symbol: &str,
            _order_id: u64,
        ) -> Result<OrderResponse, BinanceError> {
            Err(BinanceError::Http("測試不該查單".to_string()))
        }
    }

    fn filled(qty: &str, quote: &str) -> OrderResponse {
        OrderResponse {
            symbol: "BTCUSDT".to_string(),
            order_id: 7,
            client_order_id: "test".to_string(),
            status: OrderStatus::Filled,
            orig_qty: fx(qty),
            executed_qty: fx(qty),
            cummulative_quote_qty: fx(quote),
        }
    }

    /// 記下自己被問過哪幾根 K 線。
    struct Recorder {
        want: TargetPosition,
        seen: Arc<Mutex<Vec<i64>>>,
    }

    impl Strategy for Recorder {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.seen.lock().unwrap().push(bar.open_time);
            self.want
        }
    }

    /// 測試用的全域閘門：記下心跳、可以設定成擋單。
    #[derive(Default)]
    struct FakeGate {
        block_with: Mutex<Option<GlobalBlocked>>,
        beats: Mutex<Vec<i64>>,
    }

    impl FakeGate {
        fn allowing() -> Arc<FakeGate> {
            Arc::new(FakeGate::default())
        }

        fn beats(&self) -> Vec<i64> {
            self.beats.lock().unwrap().clone()
        }
    }

    impl PortfolioGate for FakeGate {
        fn observe_market_event(&self, at_ms: i64) {
            self.beats.lock().unwrap().push(at_ms);
        }

        fn check(&self, _now_ms: i64) -> Result<(), GlobalBlocked> {
            match self.block_with.lock().unwrap().clone() {
                Some(blocked) => Err(blocked),
                None => Ok(()),
            }
        }
    }

    /// 開一個真的 [`TestnetTradingHandle`]，行情來源是普通的 channel、
    /// 交易所是假的。回傳的 sender 要留著（drop 掉等於行情斷了）。
    #[allow(clippy::type_complexity)]
    fn handle(
        want: TargetPosition,
        responses: Vec<OrderResponse>,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<AtomicBool>,
        Arc<FakeExchange>,
        Arc<Mutex<Vec<i64>>>,
        TestnetTradingHandle,
    ) {
        handle_warmed(want, responses, &WarmupBars::none())
    }

    /// 同上，但先暖機回放一段歷史 K 線。走真正的 [`spawn_with`]，所以
    /// 「回放在哪裡發生、水位線怎麼傳下去」都是被測到的。
    #[allow(clippy::type_complexity)]
    fn handle_warmed(
        want: TargetPosition,
        responses: Vec<OrderResponse>,
        warmup: &WarmupBars,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<AtomicBool>,
        Arc<FakeExchange>,
        Arc<Mutex<Vec<i64>>>,
        TestnetTradingHandle,
    ) {
        let (tx, stop, gateway, seen, _gate, live) =
            handle_gated(want, responses, warmup, FakeGate::allowing());
        (tx, stop, gateway, seen, live)
    }

    /// 同上，但指定全域閘門，並把它一起回傳給測試斷言用。
    #[allow(clippy::type_complexity)]
    fn handle_gated(
        want: TargetPosition,
        responses: Vec<OrderResponse>,
        warmup: &WarmupBars,
        gate: Arc<FakeGate>,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<AtomicBool>,
        Arc<FakeExchange>,
        Arc<Mutex<Vec<i64>>>,
        Arc<FakeGate>,
        TestnetTradingHandle,
    ) {
        let (event_tx, event_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let gateway = Arc::new(FakeExchange::default());
        gateway.places.lock().unwrap().extend(responses);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let strategy = Recorder {
            want,
            seen: Arc::clone(&seen),
        };
        let live = spawn_with(
            event_rx,
            Arc::clone(&stop),
            Box::new(Arc::clone(&gateway)),
            Box::new(strategy),
            &config(),
            warmup,
            Arc::clone(&gate) as Arc<dyn PortfolioGate>,
        )
        .expect("設定應該合法");
        (event_tx, stop, gateway, seen, gate, live)
    }

    fn warmup_of(indexes: &[i64]) -> WarmupBars {
        let bars: Vec<Bar> = indexes.iter().map(|i| bar(*i, "100")).collect();
        WarmupBars::new(bars, Interval::M1).expect("測試的暖機資料應該合法")
    }

    fn recv(live: &TestnetTradingHandle) -> TestnetUpdate {
        live.updates
            .recv_timeout(Duration::from_secs(5))
            .expect("應該要有一則更新")
    }

    // ---- 設定 ----

    #[test]
    fn a_bad_config_fails_before_any_thread_or_order() {
        let (_tx, rx) = mpsc::channel();
        let mut config = config();
        config.initial_cash = fx("-1");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let error = spawn_with(
            rx,
            Arc::new(AtomicBool::new(false)),
            Box::new(Arc::new(FakeExchange::default())),
            Box::new(Recorder {
                want: TargetPosition::FLAT,
                seen: Arc::clone(&seen),
            }),
            &config,
            &warmup_of(&[0, 1]),
            FakeGate::allowing(),
        )
        .err();
        assert_eq!(error, Some(ConfigError::NonPositiveCash));
        assert!(
            seen.lock().unwrap().is_empty(),
            "設定是錯的就不該白跑一遍暖機回放"
        );
    }

    // ---- 只處理收盤 K 線 ----

    #[test]
    fn only_closed_klines_are_processed() {
        let (events, _stop, gateway, seen, live) = handle(TargetPosition::FLAT, Vec::new());
        for event in [
            kline(bar(0, "100"), false),
            kline(bar(0, "101"), false),
            kline(bar(0, "102"), true),
            MarketEvent::Ticker(at_market_stream::TickerUpdate {
                symbol: symbol(),
                last_price: fx("103"),
                event_time_ms: T0,
            }),
            kline(bar(1, "104"), true),
        ] {
            events.send(event).expect("測試 channel 不該關閉");
        }

        let first = recv(&live);
        let second = recv(&live);
        assert!(matches!(first, TestnetUpdate::Bar(_)));
        assert!(matches!(second, TestnetUpdate::Bar(_)));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS],
            "未收盤的 K 線與 ticker 不可以餵給策略"
        );
        assert!(gateway.sent.lock().unwrap().is_empty());
    }

    // ---- 送單與事件順序 ----

    #[test]
    fn an_order_event_comes_before_the_bar_snapshot() {
        // 0.1% 費率、價格 100、滿倉 → 買 99.9 顆
        let (events, _stop, gateway, _seen, live) =
            handle(TargetPosition::FULL_LONG, vec![filled("99.9", "9990")]);
        events
            .send(kline(bar(0, "100"), true))
            .expect("測試 channel 不該關閉");

        match recv(&live) {
            TestnetUpdate::Order(OrderOutcome::Filled(report)) => {
                assert_eq!(report.executed_qty, fx("99.9"));
            }
            other => panic!("第一則應該是下單結果，實際是 {other:?}"),
        }
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => {
                assert_eq!(snapshot.position, fx("99.9"));
                assert_eq!(
                    live.latest_snapshot(),
                    Some(snapshot),
                    "收到 Bar 之後馬上問目前狀態，要看到同一份"
                );
            }
            other => panic!("第二則應該是帳本快照，實際是 {other:?}"),
        }
        assert_eq!(
            *gateway.sent.lock().unwrap(),
            vec![(OrderSide::Buy, fx("99.9"))]
        );
    }

    #[test]
    fn a_fresh_handle_has_no_snapshot_yet() {
        let (_events, _stop, _gateway, _seen, live) = handle(TargetPosition::FLAT, Vec::new());
        assert_eq!(live.latest_snapshot(), None);
    }

    // ---- 全域閘門：心跳與擋單 ----

    #[test]
    fn every_market_event_feeds_the_breaker_heartbeat_not_just_closed_bars() {
        // 「行情中斷超過 3 秒」這條規則的輸入就是這串心跳。未收盤的 K 線與
        // ticker 也算：只拿收盤 K 線當心跳的話，1 分鐘 K 線永遠看起來像中斷。
        let (events, _stop, _gateway, seen, gate, live) = handle_gated(
            TargetPosition::FLAT,
            Vec::new(),
            &WarmupBars::none(),
            FakeGate::allowing(),
        );
        for event in [
            kline(bar(0, "100"), false),
            MarketEvent::Ticker(at_market_stream::TickerUpdate {
                symbol: symbol(),
                last_price: fx("101"),
                event_time_ms: T0 + 10,
            }),
            kline(bar(0, "102"), true),
        ] {
            events.send(event).expect("測試 channel 不該關閉");
        }

        assert!(matches!(recv(&live), TestnetUpdate::Bar(_)));
        assert_eq!(
            gate.beats(),
            vec![T0 + MINUTE_MS, T0 + 10, T0 + MINUTE_MS],
            "三則行情事件都要餵心跳（未收盤 K 線、ticker、收盤 K 線）"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0],
            "心跳不影響「只有收盤 K 線才餵策略」"
        );
    }

    #[test]
    fn a_tripped_breaker_blocks_the_order_through_the_whole_loop() {
        let gate = Arc::new(FakeGate::default());
        *gate.block_with.lock().unwrap() = Some(GlobalBlocked::RiskStateUnavailable);
        // 假交易所準備了一個會成交的回報：閘門沒擋住就會真的送出去。
        let (events, _stop, gateway, _seen, _gate, live) = handle_gated(
            TargetPosition::FULL_LONG,
            vec![filled("99.9", "9990")],
            &WarmupBars::none(),
            gate,
        );
        events
            .send(kline(bar(0, "100"), true))
            .expect("測試 channel 不該關閉");

        match recv(&live) {
            TestnetUpdate::Order(OrderOutcome::GloballyBlocked(reason)) => {
                assert_eq!(reason, GlobalBlocked::RiskStateUnavailable);
            }
            other => panic!("應該被全域閘門擋下，實際是 {other:?}"),
        }
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => {
                assert_eq!(snapshot.position, Fixed::ZERO);
                assert_eq!(snapshot.blocked, 1);
            }
            other => panic!("擋下之後快照照常送出，實際是 {other:?}"),
        }
        assert!(
            gateway.sent.lock().unwrap().is_empty(),
            "熔斷／風控狀態不明時一張單都不可以送出去"
        );
    }

    // ---- 一鍵停止 vs 停止：兩個獨立的開關 ----

    #[test]
    fn the_kill_switch_blocks_orders_but_keeps_the_stream_running() {
        let (events, stop, gateway, seen, live) =
            handle(TargetPosition::FULL_LONG, vec![filled("99.9", "9990")]);
        live.set_kill_switch(true);
        assert!(live.kill_switch());

        events
            .send(kline(bar(0, "100"), true))
            .expect("測試 channel 不該關閉");
        match recv(&live) {
            TestnetUpdate::Order(OrderOutcome::Blocked(blocked)) => {
                assert_eq!(blocked, at_risk_control::Blocked::KillSwitch);
            }
            other => panic!("應該被一鍵停止擋下，實際是 {other:?}"),
        }
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => {
                assert!(snapshot.kill_switch);
                assert_eq!(snapshot.position, Fixed::ZERO);
                assert_eq!(snapshot.blocked, 1);
            }
            other => panic!("一鍵停止之後行情與快照要照常跑，實際是 {other:?}"),
        }
        assert!(
            gateway.sent.lock().unwrap().is_empty(),
            "一鍵停止期間一張單都不可以送出去"
        );
        assert!(
            !stop.load(Ordering::Relaxed),
            "一鍵停止不可以連帶把行情連線關掉"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0],
            "一鍵停止期間策略還是要看到每一根 K 線"
        );

        // 放開之後下一根就能正常送單。
        live.set_kill_switch(false);
        events
            .send(kline(bar(1, "100"), true))
            .expect("測試 channel 不該關閉");
        match recv(&live) {
            TestnetUpdate::Order(OrderOutcome::Filled(_)) => {}
            other => panic!("放開一鍵停止之後應該能送單，實際是 {other:?}"),
        }
    }

    #[test]
    fn stop_ends_the_loop_and_raises_the_shared_market_stream_flag() {
        let (_events, stop, _gateway, _seen, live) = handle(TargetPosition::FLAT, Vec::new());
        assert_eq!(
            live.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "沒有行情也沒人喊停時，迴圈要繼續等"
        );

        live.stop();
        assert!(
            stop.load(Ordering::Relaxed),
            "stop() 必須連帶讓 4.5 的行情連線也收工"
        );
        assert_eq!(recv(&live), TestnetUpdate::Stopped);
        assert_eq!(
            live.updates.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "Stopped 是最後一則"
        );
    }

    // ---- 錯誤：通報一次就結束 ----

    #[test]
    fn a_failure_is_reported_once_and_ends_the_stream() {
        // 沒有準備任何回報 → 送單失敗 → Failed
        let (events, _stop, _gateway, _seen, live) = handle(TargetPosition::FULL_LONG, Vec::new());
        events
            .send(kline(bar(0, "100"), true))
            .expect("測試 channel 不該關閉");
        events
            .send(kline(bar(1, "100"), true))
            .expect("測試 channel 不該關閉");

        match recv(&live) {
            TestnetUpdate::Failed(TradingError::PlaceFailed { .. }) => {}
            other => panic!("應該是送單失敗，實際是 {other:?}"),
        }
        assert_eq!(
            live.updates.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "出錯之後不可以再處理任何一根 K 線"
        );
    }

    // ---- 暖機回放 ----

    #[test]
    fn the_warmup_replay_never_sends_an_order() {
        // 策略每一根都要求滿倉做多，而假交易所**沒有準備任何回報**：回放只要
        // 碰到送單路徑，FakeExchange 就會回錯誤，迴圈會送出 Failed。
        // 所以「沒有 Failed、沒有送出任何單」就是回放沒有送單路徑的證據。
        let (_events, _stop, gateway, seen, live) = handle_warmed(
            TargetPosition::FULL_LONG,
            Vec::new(),
            &warmup_of(&[0, 1, 2]),
        );

        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS],
            "spawn 回來的時候三根歷史 K 線應該已經餵完了（回放是同步做的）"
        );
        assert!(
            gateway.sent.lock().unwrap().is_empty(),
            "回放期間一張單都不可以送出去——這是暖機，不是交易"
        );
        assert_eq!(
            live.latest_snapshot(),
            None,
            "回放不記帳：回放完還是沒有任何帳本快照"
        );
        assert_eq!(
            live.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "回放期間不送 Order、不送 Bar、更不該送 Failed"
        );
    }

    #[test]
    fn live_bars_are_only_traded_after_the_replay_finishes() {
        let (events, _stop, gateway, seen, live) = handle_warmed(
            TargetPosition::FULL_LONG,
            vec![filled("99.9", "9990")],
            &warmup_of(&[0, 1, 2]),
        );

        events
            .send(kline(bar(3, "100"), true))
            .expect("測試 channel 不該關閉");

        match recv(&live) {
            TestnetUpdate::Order(OrderOutcome::Filled(report)) => {
                assert_eq!(report.executed_qty, fx("99.9"));
            }
            other => panic!("第一根即時 K 線才該送出第一張單，實際是 {other:?}"),
        }
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => assert_eq!(
                snapshot.point.open_time,
                T0 + 3 * MINUTE_MS,
                "第一份帳本快照必須是第一根**即時** K 線，不是回放的任何一根"
            ),
            other => panic!("第二則應該是帳本快照，實際是 {other:?}"),
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS, T0 + 3 * MINUTE_MS],
            "策略看到的順序是：回放的三根，然後才是即時那根"
        );
        assert_eq!(
            gateway.sent.lock().unwrap().len(),
            1,
            "四根 K 線只有一根是即時的，只該送一張單"
        );
    }

    #[test]
    fn live_bars_already_covered_by_the_warmup_are_not_traded_again() {
        // 抓歷史要花幾百毫秒，這段時間 WebSocket 可能已經把同一根排進 channel。
        // 沒有水位線，這些 K 線會被重新評估並送出重複的單。
        let (events, _stop, gateway, seen, live) = handle_warmed(
            TargetPosition::FULL_LONG,
            vec![filled("99.9", "9990")],
            &warmup_of(&[0, 1, 2]),
        );

        for event in [
            kline(bar(1, "101"), true), // 回放過了
            kline(bar(2, "102"), true), // 回放過了（最後一根，邊界）
            kline(bar(3, "100"), true), // 真的是新的
        ] {
            events.send(event).expect("測試 channel 不該關閉");
        }

        assert!(matches!(
            recv(&live),
            TestnetUpdate::Order(OrderOutcome::Filled(_))
        ));
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => {
                assert_eq!(snapshot.point.open_time, T0 + 3 * MINUTE_MS)
            }
            other => panic!("重複的 K 線不該造成 {other:?}"),
        }
        assert_eq!(
            live.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "三根只有一根是新的，不該有更多更新"
        );
        assert_eq!(
            gateway.sent.lock().unwrap().len(),
            1,
            "回放過的 K 線絕對不可以再下一次單"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS, T0 + 3 * MINUTE_MS],
            "回放過的 K 線不可以再餵一次（違反「每根只餵一次」的契約）"
        );
    }

    #[test]
    fn no_warmup_data_is_a_cold_start_not_a_failure() {
        // 暖機資料不足到「完全沒有」時：照舊從第一根即時 K 線開始交易，行為和
        // 這個改動之前一模一樣。要不要因為抓不到歷史而擋下啟動，是啟動 session
        // 那一層的決定。
        let (events, _stop, gateway, seen, live) =
            handle_warmed(TargetPosition::FLAT, Vec::new(), &WarmupBars::none());
        assert_eq!(live.latest_snapshot(), None);

        events
            .send(kline(bar(0, "100"), true))
            .expect("測試 channel 不該關閉");
        match recv(&live) {
            TestnetUpdate::Bar(snapshot) => assert_eq!(snapshot.point.open_time, T0),
            other => panic!("冷啟動也要能正常處理即時行情，實際是 {other:?}"),
        }
        assert_eq!(*seen.lock().unwrap(), vec![T0]);
        assert!(gateway.sent.lock().unwrap().is_empty(), "空手策略不送單");
    }

    // ---- 真實測試網（需要金鑰與網路，預設不跑）----

    /// 真的連上測試網跑一輪：公開 K 線串流 + 6.1 的測試網 Keychain 憑證，
    /// 等一根 1 分鐘 K 線收盤，確認「送單 → 等回報 → 更新帳本」整條路徑是通的。
    ///
    /// **這條還沒有實際跑過**（6.4 的開發環境沒有測試網金鑰），由 6.6 負責驗證。
    /// 跑之前請先確認：
    /// - Keychain 裡有 6.1 的測試網金鑰；
    /// - 測試網帳戶有足夠的 USDT；
    /// - `rules` 與 `fees` 換成 4.3/4.4 對測試網同步到的真實值
    ///   （下面寫死的只是大致合理的起點）。
    ///
    /// 手動驗證：`cargo test -p at-testnet-trading -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn trades_against_the_real_testnet() {
        let client = BinanceTestnetClient::from_keychain()
            .expect("讀取測試網 Keychain 憑證失敗，請先存入 6.1 的測試網金鑰");
        let mut fees = FeeSchedule::new();
        fees.insert(symbol(), FeeModel::spot_vip0());
        let config = TestnetConfig {
            symbol: symbol(),
            initial_cash: fx("1000"),
            rules: SymbolRules::new(
                fx("0.01"),
                fx("0.00001"),
                fx("0.00001"),
                fx("9000"),
                fx("5"),
            )
            .unwrap(),
            fees,
            // 刻意設得很緊：真的跑起來時單筆最多 200 USDT。
            limits: RiskLimits::new(fx("50"), fx("200")).unwrap(),
        };
        let stream =
            at_market_stream::spawn(at_market_stream::kline_stream("BTCUSDT", Interval::M1));
        let live = spawn(
            stream,
            client,
            Box::new(Recorder {
                want: TargetPosition::long(fx("0.1")),
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
            &config,
            // 這條驗的是「送單→等回報→記帳」整條鏈，不是暖機；`Recorder`
            // 沒有指標要收斂。真的要跑策略時用
            // `at_binance::market_data::recent_closed_bars` 抓。
            &WarmupBars::none(),
            // 這條手動測試驗的是送單鏈本身，熔斷由 App 層的實作負責（它有自己的
            // 測試）；這裡用放行的替身，否則第一根 K 線就會因為「還沒收到過行情
            // 心跳」而被擋下，驗不到要驗的東西。
            FakeGate::allowing(),
        )
        .expect("設定應該合法");

        let mut saw_order = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            match live.updates.recv_timeout(Duration::from_secs(180)) {
                Ok(TestnetUpdate::Order(outcome)) => {
                    println!("下單結果：{outcome:?}");
                    saw_order = true;
                }
                Ok(TestnetUpdate::Bar(snapshot)) => {
                    println!(
                        "收盤 {}：現金 {}、部位 {}、權益 {}、手續費合計 {}",
                        snapshot.point.open_time,
                        snapshot.cash,
                        snapshot.position,
                        snapshot.point.equity,
                        snapshot.fees_paid
                    );
                    if saw_order {
                        break;
                    }
                }
                Ok(TestnetUpdate::Stopped) => break,
                Ok(TestnetUpdate::Failed(e)) => panic!("測試網交易失敗：{e}"),
                Err(e) => panic!("等不到更新：{e}"),
            }
        }
        assert!(saw_order, "三分鐘內應該至少送出一張單");

        live.stop();
        loop {
            match live.updates.recv_timeout(Duration::from_secs(10)) {
                Ok(TestnetUpdate::Stopped) => break,
                Ok(_) => continue,
                Err(e) => panic!("按停止之後應該收到 Stopped：{e}"),
            }
        }
    }
}
