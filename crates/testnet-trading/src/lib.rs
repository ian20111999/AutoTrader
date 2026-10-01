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
//! # 唯一的送單路徑
//!
//! 整個 crate 只有 `trader.rs` 的 `Trader::send_gated_order` 會呼叫
//! [`OrderGateway::place_market_order`]，而它的第一件事就是
//! [`RiskLimits::check`](at_risk_control::RiskLimits::check)。要繞過閘門送單，
//! 必須改那個私有函式的前幾行——不存在「忘記檢查」的呼叫端。
//!
//! 送單對象是 6.2 的 [`BinanceTestnetClient`]，它連網址都寫死在常數裡，
//! 型別上不可能指向正式環境。
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
//! use at_risk_control::RiskLimits;
//! use at_testnet_trading::{spawn, TestnetConfig, TestnetUpdate};
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
//! let stream = spawn_stream(kline_stream("BTCUSDT", Interval::M1));
//! let live = spawn(stream, client, Box::new(SmaCross::new(10, 30).unwrap()), &config).unwrap();
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
use at_core::{FeeSchedule, Fixed, Strategy, Symbol, SymbolRules};
use at_market_stream::{MarketEvent, MarketStreamHandle};
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
/// 設定有問題會**當場**回錯誤，不會開執行緒、也不會送出任何單。
pub fn spawn(
    stream: MarketStreamHandle,
    client: BinanceTestnetClient,
    strategy: Box<dyn Strategy + Send>,
    config: &TestnetConfig,
) -> Result<TestnetTradingHandle, ConfigError> {
    let stop = stream.stop_flag();
    spawn_with(stream.events, stop, Box::new(client), strategy, config)
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
) -> Result<TestnetTradingHandle, ConfigError> {
    let kill_switch = Arc::new(AtomicBool::new(false));
    let mut trader = Trader::new(config, gateway, Arc::clone(&kill_switch), Arc::clone(&stop))?;
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
fn run(
    events: &mpsc::Receiver<MarketEvent>,
    trader: &mut Trader,
    strategy: &mut dyn Strategy,
    updates: &mpsc::Sender<TestnetUpdate>,
    latest: &Mutex<Option<TestnetSnapshot>>,
    stop: &AtomicBool,
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
        let MarketEvent::Kline(update) = event else {
            continue;
        };
        if !update.is_closed {
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
        )
        .expect("設定應該合法");
        (event_tx, stop, gateway, seen, live)
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
        let error = spawn_with(
            rx,
            Arc::new(AtomicBool::new(false)),
            Box::new(Arc::new(FakeExchange::default())),
            Box::new(Recorder {
                want: TargetPosition::FLAT,
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
            &config,
        )
        .err();
        assert_eq!(error, Some(ConfigError::NonPositiveCash));
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
