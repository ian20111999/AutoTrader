//! 模擬交易的接線：4.5 的即時行情 → 5.1 的 [`PaperEngine`] → 往外送的權益更新。
//!
//! # 為什麼是獨立的 crate
//!
//! 這一步要同時用到 `at-core`（引擎、策略、K 線）和 `at-market-stream`
//! （WebSocket 行情），但這兩個 crate 都不應該依賴對方：`at-core` 目前零外部
//! 依賴（沒有 TLS、沒有 JSON、沒有網路），是回測、模擬、測試網、實盤共用的
//! 地基，把 WebSocket 拖進去會讓「純算帳」的程式碼跟著網路函式庫一起編譯；
//! 反過來讓 `at-market-stream` 依賴引擎，則會變成「收行情的人自己決定要不要
//! 記帳」。接線邏輯放在這個只有幾十行的第三個 crate，兩邊都維持單一職責。
//!
//! # 只有收盤 K 線才進帳本
//!
//! Binance 的 kline 串流每秒左右就推一則「這根還在形成中」的訊息（4.5 的
//! [`KlineUpdate::is_closed`]）。策略的契約是「一根 K 線只餵一次、按時間遞增」
//! （見 [`Strategy::on_bar`]），而且 5.1 的引擎會用 `open_time` 檢查時間是否
//! 遞增——把未收盤的 K 線餵進去，等於同一根 K 線用不斷變動的收盤價反覆問策略，
//! 訊號會在一分鐘內閃爍好幾次。所以這裡只在 `is_closed == true` 時才呼叫
//! [`PaperEngine::on_bar`]；ticker 與未收盤的 K 線一律路過。
//!
//! # 沒有任何送單路徑
//!
//! 這裡只讀公開行情、只呼叫記帳引擎，沒有引入 `at-binance`，也沒有任何下單
//! 程式碼路徑可以走到（專案第 6 步之前的硬性規定）。
//!
//! # 開始交易之前先暖機回放
//!
//! 即時行情是「現在開始」，但策略的指標需要一段歷史才有意義
//! （[`Strategy::warmup_bars`]）。所以 [`spawn`] 的第一件事不是等 K 線，而是把
//! 呼叫端給的 [`WarmupBars`] 依序餵給策略、**丟棄這段期間產生的目標部位**，
//! 讓指標的內部狀態（均線的視窗、RSI 的平滑累加值）先收斂，然後才接即時行情。
//!
//! 回放**不經過 [`PaperEngine`]**：`WarmupBars::replay` 只拿得到策略本身，
//! 所以回放期間不可能記帳、不可能產生權益點、也不可能有送單路徑
//! （這個 crate 本來就沒有送單路徑）。
//!
//! 回放完會記下最後一根的開盤時間當分水嶺：即時行情裡開盤時間**小於或等於**
//! 它的收盤 K 線一律跳過。這不是最佳化，是必要的——抓歷史要花幾百毫秒，
//! 這段時間 WebSocket 可能已經把同一根 K 線排進 channel 了，再餵一次會違反
//! 「每根只餵一次」的契約，而且 [`PaperEngine::on_bar`] 會直接回
//! [`BacktestError::NonMonotonicTime`] 讓整個 session 當場死掉。
//!
//! 歷史 K 線從哪裡來、抓不到要怎麼辦，刻意**不在這個 crate**：
//! 用 `at_binance::market_data::recent_closed_bars` 抓，由啟動 session 的那一層
//! 決定失敗時要不要擋下啟動。這樣這個 crate 不必為了暖機而依賴 `at-binance`，
//! 上面「沒有任何送單路徑」那一段才繼續成立——模擬交易的依賴圖裡連
//! `place_market_order` 都不存在。
//!
//! # 開始與停止（5.3）
//!
//! [`spawn`] 就是「開始」。停止是 [`PaperTradingHandle::stop`]：它設定的是
//! 行情連線那一層（4.5）交出來的**同一個**停止旗標，所以按一次會同時結束
//! 模擬交易迴圈和底層的 WebSocket 執行緒，不會留下孤兒連線。
//!
//! 查詢「現在的狀態」用 [`PaperTradingHandle::latest_snapshot`]：`updates`
//! channel 是**事件流**（每根收盤 K 線一則，要逐則消費才不漏），
//! `latest_snapshot` 是**目前狀態**（隨時問、永遠只有最新一份）。5.4 的畫面
//! 兩種都會用到：曲線靠事件流累積，部位／權益數字靠目前狀態。
//!
//! # 用起來像這樣
//!
//! ```no_run
//! use at_core::{warmup_fetch_count, BacktestConfig, Fixed, SmaCross, Strategy, WarmupBars};
//! use at_market_stream::{kline_stream, spawn as spawn_stream};
//! use at_paper_trading::{spawn, PaperUpdate};
//! use at_core::Interval;
//!
//! let config = BacktestConfig::frictionless(Fixed::from_int(10_000).unwrap());
//! let strategy = SmaCross::new(10, 30).unwrap();
//!
//! // 暖機用的歷史 K 線要抓幾根：策略宣告的需求 × 安全係數。
//! let need = warmup_fetch_count(&strategy);
//! assert_eq!(need, 150);
//! // 真的去抓是啟動 session 那一層的事（App 用
//! // `at_binance::market_data::recent_closed_bars(&symbol, interval, need)`），
//! // 抓不到就不要開始：冷啟動的訊號和回測不一樣。這裡用空的示意。
//! let warmup = WarmupBars::none();
//!
//! let stream = spawn_stream(kline_stream("BTCUSDT", Interval::M1));
//! let paper = spawn(stream, Box::new(strategy), &config, &warmup).unwrap();
//!
//! // 使用者按下「停止」時呼叫 paper.stop()；隨時可以問目前部位與權益：
//! if let Some(now) = paper.latest_snapshot() {
//!     println!("目前權益 {}、部位 {}", now.point.equity, now.position);
//! }
//!
//! for update in paper.updates {
//!     match update {
//!         PaperUpdate::Bar(snapshot) => println!("權益：{}", snapshot.point.equity),
//!         PaperUpdate::Stopped => {
//!             println!("已停止");
//!             break;
//!         }
//!         PaperUpdate::Failed(e) => {
//!             eprintln!("模擬交易中止：{e}");
//!             break;
//!         }
//!     }
//! }
//! ```

use at_core::backtest::PaperEngine;
use at_core::{BacktestConfig, BacktestError, EquityPoint, Fixed, Strategy, WarmupBars};
use at_market_stream::{MarketEvent, MarketStreamHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 沒有新行情時，迴圈每隔這麼久回頭看一次停止旗標。
///
/// 也是 [`PaperTradingHandle::stop`] 在這一層生效的上限：1 分鐘 K 線兩根之間
/// 有將近一分鐘沒有收盤事件，不能等到下一根才發現使用者按了停止。
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// 餵完一根收盤 K 線之後的完整帳本狀態。
///
/// 只回權益點不夠：消費端要顯示「現在手上有多少倉、還有多少現金、成交過幾筆、
/// 爆倉過幾次」，這些都是曲線上看不出來的資訊（語意同
/// [`at_core::BacktestResult`] 的各欄位）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaperSnapshot {
    /// 這根 K 線收盤時的權益點（時間 + 帳戶總值）。
    pub point: EquityPoint,
    /// 報價幣現金餘額，負數表示有槓桿（跟交易所借錢）。
    pub cash: Fixed,
    /// 帶正負號的持倉數量：正做多、負做空、`0` 空手。
    pub position: Fixed,
    /// 到目前為止的成交筆數（強制平倉也算一筆）。
    pub trades: usize,
    /// 到目前為止被強制平倉的次數。
    pub liquidations: usize,
}

/// 模擬交易往外送的一則更新。
#[derive(Debug, Clone, PartialEq)]
pub enum PaperUpdate {
    /// 一根收盤 K 線進帳本之後的最新狀態。
    Bar(PaperSnapshot),
    /// 使用者主動停止（[`PaperTradingHandle::stop`]）。帳本是**完整**的，只是
    /// 不會再有新的更新了——和 [`Failed`](PaperUpdate::Failed) 的意思完全不同，
    /// 畫面上不該顯示成錯誤。這條串流的最後一則。
    Stopped,
    /// 引擎算不下去了。**帳本從這一刻起不完整**，這條串流到此結束，
    /// 之後不會再有任何更新；消費端收到它就該停止顯示、把錯誤告訴使用者。
    Failed(BacktestError),
}

/// [`spawn`] 的控制代碼：`updates` 收事件流、[`latest_snapshot`](Self::latest_snapshot)
/// 問目前狀態、[`stop`](Self::stop) 收工。
///
/// 把它（連同 `updates`）drop 掉也會讓背景執行緒結束，但那要等到下一次送出更新
/// 才會發現沒人收；要立刻停請呼叫 [`stop`](Self::stop)。
pub struct PaperTradingHandle {
    pub updates: mpsc::Receiver<PaperUpdate>,
    /// 和 4.5 行情連線共用的同一個旗標（見 [`MarketStreamHandle::stop_flag`]）。
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<PaperSnapshot>>>,
    _worker: thread::JoinHandle<()>,
}

impl PaperTradingHandle {
    /// 要求停止：模擬交易迴圈和底層的行情連線都會結束。
    ///
    /// 迴圈結束前會往 `updates` 送一則 [`PaperUpdate::Stopped`]。重複呼叫沒有
    /// 副作用。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 現在的部位與權益：最後一根收盤 K 線進帳本之後的完整狀態。
    ///
    /// 還沒有任何一根收盤 K 線進帳本時回 `None`（不是「權益等於起始資金」，
    /// 因為那是猜的——真正的第一個權益點要等引擎算過才算數）。
    ///
    /// 這是**目前狀態**的查詢，和 `updates` 這條**事件流**互補：畫面重畫、
    /// 使用者切回這一頁時不需要重播整條 channel 就能顯示現在的數字。
    pub fn latest_snapshot(&self) -> Option<PaperSnapshot> {
        // 鎖被下毒（持有鎖的執行緒 panic 過）時寧可回 None，也不要跟著 panic。
        self.latest.lock().ok().and_then(|slot| *slot)
    }
}

/// 把一條即時行情串流接上一個新的模擬交易引擎，回傳控制代碼。
///
/// `warmup` 是開始處理即時行情**之前**要先餵給策略的歷史 K 線（見 crate 文件的
/// 「開始交易之前先暖機回放」）。沒有歷史資料可用時傳 [`WarmupBars::none`]，
/// 那就是明確選擇冷啟動：策略從第一根即時 K 線開始看，指標收斂之前照契約空手。
///
/// 設定有問題（起始資金、滑價、費率、資金費、維持保證金率）會**當場**回錯誤，
/// 不會開執行緒、也不會等到第一根 K 線才發現參數是錯的。
///
/// 行情的 receiver 被搬進背景執行緒，所以行情連線的生命週期跟著模擬交易走：
/// 這裡結束（停止、出錯、或沒人收更新了）→ receiver 被 drop → 4.5 的背景執行
/// 緒下一次送出事件時也會結束。停止旗標是共用的，所以正常停止的時候兩層會一起
/// 收工，不必等那一次送出。
pub fn spawn(
    stream: MarketStreamHandle,
    strategy: Box<dyn Strategy + Send>,
    config: &BacktestConfig,
    warmup: &WarmupBars,
) -> Result<PaperTradingHandle, BacktestError> {
    let stop = stream.stop_flag();
    spawn_with(stream.events, stop, strategy, config, warmup)
}

/// [`spawn`] 的本體，只要「事件從哪來」和「停止旗標」兩樣東西。
///
/// 分出這一層純粹是為了測試：測試塞一個普通的 [`mpsc::channel`] 進來，就能驗
/// [`PaperTradingHandle`] 的每個方法，完全不需要連 WebSocket。
fn spawn_with(
    events: mpsc::Receiver<MarketEvent>,
    stop: Arc<AtomicBool>,
    mut strategy: Box<dyn Strategy + Send>,
    config: &BacktestConfig,
    warmup: &WarmupBars,
) -> Result<PaperTradingHandle, BacktestError> {
    let mut engine = PaperEngine::new(config)?;
    // 先驗設定再回放：設定是錯的就不該白跑一遍暖機。
    //
    // 回放在這裡（開執行緒之前）同步做完，所以「回放結束才開始處理即時行情」
    // 不是靠執行順序的巧合，而是結構上的先後。幾百根 on_bar 是微秒級的工作。
    let replayed_through = warmup.replay(strategy.as_mut());
    let latest = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::channel();
    let worker_latest = Arc::clone(&latest);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        run(
            &events,
            &mut engine,
            strategy.as_mut(),
            &tx,
            &worker_latest,
            &worker_stop,
            replayed_through,
        );
    });
    Ok(PaperTradingHandle {
        updates: rx,
        stop,
        latest,
        _worker: worker,
    })
}

/// 接線迴圈本體：收行情 → 過濾 → 餵引擎 → 記下目前狀態 → 送更新。
///
/// 四個出口，全部是正常結束、都不 panic：
///
/// 1. `stop` 旗標被設定（使用者按停止）——送出一則 [`PaperUpdate::Stopped`]。
/// 2. 行情 channel 關閉（4.5 的背景執行緒結束了）。
/// 3. `updates.send` 失敗（消費端把 [`PaperTradingHandle`] drop 掉了）。
/// 4. [`PaperEngine::on_bar`] 回錯誤——**送出一次 [`PaperUpdate::Failed`] 就
///    停止**，不再餵任何一根。引擎回錯誤之後帳本已經不完整，繼續餵只會產生
///    看起來像真的、其實是假的權益曲線。
///
/// 用 `recv_timeout` 而不是 `recv`：1 分鐘 K 線兩根之間將近一分鐘沒有事件，
/// 阻塞在 `recv` 上就看不到停止旗標，使用者按停止會像沒反應。
///
/// 網路不在這裡，所以測試可以直接在當前執行緒呼叫它，用一般的 channel 塞測資
/// 進去。
///
/// `replayed_through` 是暖機回放最後一根的開盤時間（沒回放過是 `None`）：
/// 開盤時間小於或等於它的收盤 K 線已經算在策略的狀態裡了，必須跳過。
fn run(
    events: &mpsc::Receiver<MarketEvent>,
    engine: &mut PaperEngine,
    strategy: &mut dyn Strategy,
    updates: &mpsc::Sender<PaperUpdate>,
    latest: &Mutex<Option<PaperSnapshot>>,
    stop: &AtomicBool,
    replayed_through: Option<i64>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            // 送不出去也沒關係：消費端已經不在了。
            let _ = updates.send(PaperUpdate::Stopped);
            return;
        }
        let event = match events.recv_timeout(STOP_CHECK_INTERVAL) {
            Ok(event) => event,
            // 這段時間沒有新行情，回頭看一次旗標再等。
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let MarketEvent::Kline(update) = event else {
            continue;
        };
        if !update.is_closed {
            continue;
        }
        // 暖機回放已經餵過的那幾根：抓歷史要花幾百毫秒，這段時間 WebSocket
        // 可能已經把同一根排進 channel 了。再餵一次會違反「每根只餵一次」，
        // 而且引擎會回 NonMonotonicTime 讓整個 session 當場死掉。
        if replayed_through.is_some_and(|through| update.bar.open_time <= through) {
            continue;
        }
        match engine.on_bar(&update.bar, strategy) {
            Ok(point) => {
                let snapshot = PaperSnapshot {
                    point,
                    cash: engine.cash(),
                    position: engine.position(),
                    trades: engine.trades(),
                    liquidations: engine.liquidations(),
                };
                // 先更新「目前狀態」再送事件：消費端收到 Bar 之後馬上問
                // latest_snapshot，看到的不會是上一根。
                // 鎖被下毒時放棄更新也不 panic——事件流本身還是正確的。
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(snapshot);
                }
                if updates.send(PaperUpdate::Bar(snapshot)).is_err() {
                    return;
                }
            }
            Err(error) => {
                // 送不出去也一樣要停：消費端已經不在了。
                let _ = updates.send(PaperUpdate::Failed(error));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{warmup_fetch_count, Bar, Interval, SmaCross, Symbol, TargetPosition};
    use at_market_stream::KlineUpdate;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;
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
        }
    }

    /// 包成一則 4.5 的行情事件。
    fn kline(bar: Bar, is_closed: bool) -> MarketEvent {
        MarketEvent::Kline(KlineUpdate {
            symbol: Symbol::new("BTCUSDT").unwrap(),
            interval: Interval::M1,
            bar,
            is_closed,
            event_time_ms: bar.open_time + MINUTE_MS,
        })
    }

    fn ticker(price: &str) -> MarketEvent {
        MarketEvent::Ticker(at_market_stream::TickerUpdate {
            symbol: Symbol::new("BTCUSDT").unwrap(),
            last_price: fx(price),
            event_time_ms: T0,
        })
    }

    /// 記下自己被問過哪幾根 K 線的開盤時間——這就是「引擎真的餵了什麼」的證據，
    /// 因為 [`PaperEngine::on_bar`] 每餵一根就會問策略一次。
    #[derive(Default)]
    struct Recorder {
        seen: Vec<i64>,
        want: TargetPosition,
    }

    impl Strategy for Recorder {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.seen.push(bar.open_time);
            self.want
        }
    }

    fn frictionless() -> BacktestConfig {
        BacktestConfig::frictionless(fx("10000"))
    }

    /// 把一串事件餵完（送完就關掉發送端，`run` 收到 channel 關閉會自然結束），
    /// 回傳往外送的更新，以及結束時「目前狀態」那一份快照。
    fn feed(
        events: Vec<MarketEvent>,
        strategy: &mut Recorder,
        config: &BacktestConfig,
    ) -> (Vec<PaperUpdate>, Option<PaperSnapshot>) {
        let (event_tx, event_rx) = mpsc::channel();
        for event in events {
            event_tx.send(event).expect("測試用的 channel 不該關閉");
        }
        drop(event_tx);

        let (update_tx, update_rx) = mpsc::channel();
        let mut engine = PaperEngine::new(config).expect("設定應該合法");
        let latest = Mutex::new(None);
        run(
            &event_rx,
            &mut engine,
            strategy,
            &update_tx,
            &latest,
            &AtomicBool::new(false),
            None,
        );
        drop(update_tx);
        let snapshot = latest.into_inner().expect("測試裡的鎖不會被下毒");
        (update_rx.into_iter().collect(), snapshot)
    }

    /// 用一個普通的 channel 當行情來源開一個**真的** [`PaperTradingHandle`]，
    /// 完全不連 WebSocket，這樣可以驗 `stop()`／`latest_snapshot()` 本身。
    /// 回傳的 sender 要留著（drop 掉就等於行情斷了）。
    fn fake_stream_handle(
        strategy: Box<dyn Strategy + Send>,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<AtomicBool>,
        PaperTradingHandle,
    ) {
        fake_stream_handle_warmed(strategy, &WarmupBars::none())
    }

    /// 同上，但先暖機回放一段歷史 K 線。走的是真正的 [`spawn_with`]，所以
    /// 「回放在哪裡發生、水位線怎麼傳下去」都是被測到的，不是測試自己模擬的。
    fn fake_stream_handle_warmed(
        strategy: Box<dyn Strategy + Send>,
        warmup: &WarmupBars,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<AtomicBool>,
        PaperTradingHandle,
    ) {
        let (event_tx, event_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let paper = spawn_with(
            event_rx,
            Arc::clone(&stop),
            strategy,
            &frictionless(),
            warmup,
        )
        .expect("設定應該合法");
        (event_tx, stop, paper)
    }

    /// 和 [`Recorder`] 一樣記下看過哪幾根，但狀態放在 [`Arc`] 裡：策略被搬進
    /// 背景執行緒之後，測試還要看得到它到底被餵了什麼。
    struct SharedRecorder {
        want: TargetPosition,
        seen: Arc<Mutex<Vec<i64>>>,
    }

    impl Strategy for SharedRecorder {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.seen
                .lock()
                .expect("測試裡的鎖不會被下毒")
                .push(bar.open_time);
            self.want
        }
    }

    /// 開一個暖機過的 handle，順便拿到「策略看過哪幾根」。
    #[allow(clippy::type_complexity)]
    fn warmed_handle(
        want: TargetPosition,
        warmup: &WarmupBars,
    ) -> (
        mpsc::Sender<MarketEvent>,
        Arc<Mutex<Vec<i64>>>,
        PaperTradingHandle,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let strategy = SharedRecorder {
            want,
            seen: Arc::clone(&seen),
        };
        let (events, _stop, paper) = fake_stream_handle_warmed(Box::new(strategy), warmup);
        (events, seen, paper)
    }

    fn warmup_of(indexes: &[i64]) -> WarmupBars {
        let bars: Vec<Bar> = indexes.iter().map(|i| bar(*i, "100")).collect();
        WarmupBars::new(bars, Interval::M1).expect("測試的暖機資料應該合法")
    }

    // ---- 過濾：只有收盤 K 線才餵進引擎 ----

    #[test]
    fn only_closed_klines_are_fed_to_the_engine() {
        let mut strategy = Recorder::default();
        let (updates, _) = feed(
            vec![
                // 同一根 K 線的「還在形成中」會來好幾則，收盤價一直在變。
                kline(bar(0, "100"), false),
                kline(bar(0, "101"), false),
                ticker("101.5"),
                kline(bar(0, "102"), true),
                kline(bar(1, "103"), false),
                ticker("104"),
                kline(bar(1, "105"), true),
            ],
            &mut strategy,
            &frictionless(),
        );

        assert_eq!(
            strategy.seen,
            vec![T0, T0 + MINUTE_MS],
            "引擎只能收到兩根收盤 K 線；未收盤的 K 線與 ticker 不可以進帳本"
        );
        assert_eq!(updates.len(), 2, "每餵進一根收盤 K 線才送一則更新");
        let PaperUpdate::Bar(first) = &updates[0] else {
            panic!("第一則應該是 Bar 更新");
        };
        assert_eq!(
            first.point.open_time, T0,
            "權益點的時間要是那根收盤 K 線的開盤時間"
        );
        // 空手 + 零成本：權益就是起始資金，而且用的是**收盤那一則**的收盤價
        // （102）而不是未收盤訊息裡的 100／101。
        assert_eq!(first.point.equity, fx("10000"));
        assert_eq!(first.cash, fx("10000"));
        assert_eq!(first.position, Fixed::ZERO);
        assert_eq!(first.trades, 0);
        assert_eq!(first.liquidations, 0);
    }

    #[test]
    fn a_stream_with_no_closed_klines_produces_no_updates() {
        let mut strategy = Recorder::default();
        let (updates, latest) = feed(
            vec![
                ticker("100"),
                kline(bar(0, "100"), false),
                kline(bar(1, "101"), false),
            ],
            &mut strategy,
            &frictionless(),
        );
        assert!(strategy.seen.is_empty(), "一根都還沒收盤，策略不該被問");
        assert!(updates.is_empty());
        assert_eq!(
            latest, None,
            "還沒有任何一根收盤 K 線進帳本時，不該有「目前狀態」可查"
        );
    }

    // ---- 錯誤：往外通報一次就停止餵 ----

    #[test]
    fn engine_error_is_reported_once_and_stops_the_feed() {
        let mut strategy = Recorder::default();
        // 第二根的開盤時間沒有比第一根晚（真實世界的重連可能重送舊 K 線），
        // 5.1 的引擎會回 NonMonotonicTime。
        let (updates, _) = feed(
            vec![
                kline(bar(5, "100"), true),
                kline(bar(5, "101"), true),
                kline(bar(6, "102"), true),
            ],
            &mut strategy,
            &frictionless(),
        );

        assert_eq!(
            strategy.seen,
            vec![T0 + 5 * MINUTE_MS],
            "出錯之後不可以再餵任何一根，包括後面那根時間正常的 K 線"
        );
        assert_eq!(updates.len(), 2, "一則正常更新 + 一則錯誤通報，然後結束");
        assert!(matches!(updates[0], PaperUpdate::Bar(_)));
        assert_eq!(
            updates[1],
            PaperUpdate::Failed(BacktestError::NonMonotonicTime { index: 1 }),
            "消費端要知道帳本從哪一根開始不可信"
        );
    }

    // ---- 和回測跑同一套帳：同樣的 K 線，數字必須一模一樣 ----

    #[test]
    fn same_bars_give_the_same_numbers_as_a_backtest() {
        let config = frictionless();
        let closes = ["100", "110", "105", "120", "130"];
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, c)| bar(i as i64, c))
            .collect();

        // 一半以上的事件是雜訊（未收盤 K 線與 ticker），引擎只能看到收盤的那些。
        let mut events = Vec::new();
        for b in &bars {
            events.push(ticker("1"));
            events.push(kline(*b, false));
            events.push(kline(*b, true));
        }

        let mut live = Recorder {
            seen: Vec::new(),
            want: TargetPosition::FULL_LONG,
        };
        let (updates, latest) = feed(events, &mut live, &config);

        let mut offline = Recorder {
            seen: Vec::new(),
            want: TargetPosition::FULL_LONG,
        };
        let expected = at_core::run_backtest(&bars, &mut offline, &config).expect("回測應該成功");

        let curve: Vec<EquityPoint> = updates
            .iter()
            .map(|u| match u {
                PaperUpdate::Bar(s) => s.point,
                other => panic!("不該收到 {other:?}"),
            })
            .collect();
        assert_eq!(curve, expected.curve, "權益曲線要和回測逐點相等");

        let PaperUpdate::Bar(last) = updates.last().expect("應該有更新") else {
            panic!("最後一則應該是 Bar 更新");
        };
        assert_eq!(last.trades, expected.trades);
        assert_eq!(last.liquidations, expected.liquidations);
        assert_eq!(
            latest,
            Some(*last),
            "「目前狀態」要等於最後一則 Bar 更新，不是上一根、也不是另外算一份"
        );
    }

    // ---- 5.3 生命週期：停止與查詢目前狀態 ----

    #[test]
    fn stop_ends_the_loop_and_says_so() {
        // 行情 channel 一直開著、但一則事件都不送：只有 stop() 能讓迴圈結束。
        let (_events, _stop, paper) = fake_stream_handle(Box::new(Recorder::default()));

        assert_eq!(
            paper.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "沒有行情、也沒人喊停時，迴圈要繼續等下去"
        );

        paper.stop();
        assert_eq!(
            paper.updates.recv_timeout(Duration::from_secs(5)),
            Ok(PaperUpdate::Stopped),
            "喊停之後要收到 Stopped（正常停止，不是 Failed）"
        );
        assert_eq!(
            paper.updates.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "Stopped 是最後一則：背景執行緒結束、sender 跟著 drop"
        );
    }

    #[test]
    fn stopping_the_paper_loop_raises_the_shared_market_stream_flag() {
        // spawn() 交給這一層的旗標就是 4.5 行情連線自己在看的那一個
        // （MarketStreamHandle::stop_flag），所以按一次停止兩層都會收工。
        let (_events, stop, paper) = fake_stream_handle(Box::new(Recorder::default()));
        assert!(!stop.load(Ordering::Relaxed));
        paper.stop();
        assert!(
            stop.load(Ordering::Relaxed),
            "模擬交易的 stop() 必須連帶讓行情連線那一層也停"
        );
    }

    #[test]
    fn a_fresh_handle_has_no_current_state() {
        let (_events, _stop, paper) = fake_stream_handle(Box::new(Recorder::default()));
        assert_eq!(
            paper.latest_snapshot(),
            None,
            "還沒餵過任何 K 線時，目前狀態是「沒有」，不是起始資金"
        );
    }

    #[test]
    fn latest_snapshot_matches_the_last_bar_update() {
        let (events, _stop, paper) = fake_stream_handle(Box::new(Recorder {
            seen: Vec::new(),
            want: TargetPosition::FULL_LONG,
        }));

        // 一直滿倉做多、價格有漲有跌，權益和部位每根都在動。
        for (i, close) in ["100", "110", "105"].iter().enumerate() {
            events
                .send(kline(bar(i as i64, close), true))
                .expect("測試用的 channel 不該關閉");
        }

        let mut last = None;
        for _ in 0..3 {
            match paper
                .updates
                .recv_timeout(Duration::from_secs(5))
                .expect("三根收盤 K 線應該送出三則更新")
            {
                PaperUpdate::Bar(snapshot) => last = Some(snapshot),
                other => panic!("不該收到 {other:?}"),
            }
        }

        assert_ne!(last, None);
        assert_eq!(
            paper.latest_snapshot(),
            last,
            "latest_snapshot() 要和最後一則 Bar 更新的內容完全一致"
        );
    }

    // ---- 暖機回放 ----

    #[test]
    fn the_warmup_replay_never_touches_the_ledger() {
        // 策略每一根都要求滿倉做多。如果回放有經過引擎，帳本就會有成交、
        // 有權益點、有「目前狀態」——這個測試會抓到。
        let (_events, seen, paper) =
            warmed_handle(TargetPosition::FULL_LONG, &warmup_of(&[0, 1, 2]));

        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS],
            "spawn 回來的時候三根歷史 K 線應該已經餵完了（回放是同步做的）"
        );
        assert_eq!(
            paper.latest_snapshot(),
            None,
            "回放不進帳本：回放完還是沒有任何「目前狀態」可查"
        );
        assert_eq!(
            paper.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "回放期間不可以送出任何一則更新（包括權益點）"
        );
    }

    #[test]
    fn live_bars_are_only_processed_after_the_replay_finishes() {
        let (events, seen, paper) = warmed_handle(TargetPosition::FLAT, &warmup_of(&[0, 1, 2]));

        events
            .send(kline(bar(3, "103"), true))
            .expect("測試用的 channel 不該關閉");

        match paper
            .updates
            .recv_timeout(Duration::from_secs(5))
            .expect("第一根即時 K 線應該送出一則更新")
        {
            PaperUpdate::Bar(snapshot) => assert_eq!(
                snapshot.point.open_time,
                T0 + 3 * MINUTE_MS,
                "第一個權益點必須是第一根**即時** K 線，不是回放的任何一根"
            ),
            other => panic!("不該收到 {other:?}"),
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS, T0 + 3 * MINUTE_MS],
            "策略看到的順序是：回放的三根，然後才是即時那根"
        );
    }

    #[test]
    fn live_bars_already_covered_by_the_warmup_are_skipped() {
        // 抓歷史要花幾百毫秒，這段時間 WebSocket 可能已經把同一根 K 線排進
        // channel。沒有這個水位線，引擎會因為時間沒有遞增而回 NonMonotonicTime，
        // 整個 session 在啟動的瞬間就死掉。
        let (events, seen, paper) = warmed_handle(TargetPosition::FLAT, &warmup_of(&[0, 1, 2]));

        for event in [
            kline(bar(1, "101"), true), // 回放過了
            kline(bar(2, "102"), true), // 回放過了（最後一根，邊界）
            kline(bar(3, "103"), true), // 真的是新的
        ] {
            events.send(event).expect("測試用的 channel 不該關閉");
        }

        match paper
            .updates
            .recv_timeout(Duration::from_secs(5))
            .expect("應該有一則更新")
        {
            PaperUpdate::Bar(snapshot) => assert_eq!(
                snapshot.point.open_time,
                T0 + 3 * MINUTE_MS,
                "只有超過水位線的那根可以進帳本"
            ),
            other => panic!("重複的 K 線不該造成 {other:?}"),
        }
        assert_eq!(
            paper.updates.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "三根只有一根是新的，不該有第二則更新"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![T0, T0 + MINUTE_MS, T0 + 2 * MINUTE_MS, T0 + 3 * MINUTE_MS],
            "回放過的 K 線不可以再餵一次（違反「每根只餵一次」的契約）"
        );
    }

    #[test]
    fn no_warmup_data_is_a_cold_start_not_a_failure() {
        // 暖機資料不足到「完全沒有」時的行為：照舊從第一根即時 K 線開始，
        // 和這個改動之前一模一樣。要不要因為抓不到歷史而擋下啟動，是啟動
        // session 那一層的決定，不是這裡。
        let (events, seen, paper) = warmed_handle(TargetPosition::FLAT, &WarmupBars::none());
        assert_eq!(paper.latest_snapshot(), None);

        events
            .send(kline(bar(0, "100"), true))
            .expect("測試用的 channel 不該關閉");
        match paper
            .updates
            .recv_timeout(Duration::from_secs(5))
            .expect("冷啟動也要能正常處理即時行情")
        {
            PaperUpdate::Bar(snapshot) => assert_eq!(snapshot.point.open_time, T0),
            other => panic!("不該收到 {other:?}"),
        }
        assert_eq!(*seen.lock().unwrap(), vec![T0]);
    }

    #[test]
    fn a_warmup_shorter_than_the_strategy_asks_for_still_replays_what_there_is() {
        let strategy = SmaCross::new(10, 30).unwrap();
        assert_eq!(warmup_fetch_count(&strategy), 150, "宣告需求 30 × 係數 5");

        // 只拿到 2 根（例如新上市的交易對）：不是錯誤，有多少餵多少。
        let (_events, seen, paper) = warmed_handle(TargetPosition::FLAT, &warmup_of(&[0, 1]));
        assert_eq!(*seen.lock().unwrap(), vec![T0, T0 + MINUTE_MS]);
        assert_eq!(paper.latest_snapshot(), None);
    }

    #[test]
    fn a_warmed_strategy_signals_sooner_than_a_cold_one() {
        // 這是整件事的重點：同樣的即時 K 線，暖機過的策略和冷啟動的策略
        // 做出不同的決定。快線 2／慢線 3，暖機三根上漲的 K 線。
        let warmup = {
            let bars = vec![bar(0, "100"), bar(1, "101"), bar(2, "102")];
            WarmupBars::new(bars, Interval::M1).expect("暖機資料應該合法")
        };

        let position_after = |warmup: &WarmupBars| -> Fixed {
            let strategy = SmaCross::new(2, 3).expect("參數合法");
            let (events, _stop, paper) = fake_stream_handle_warmed(Box::new(strategy), warmup);
            for event in [kline(bar(3, "103"), true), kline(bar(4, "104"), true)] {
                events.send(event).expect("測試用的 channel 不該關閉");
            }
            let mut last = None;
            for _ in 0..2 {
                match paper
                    .updates
                    .recv_timeout(Duration::from_secs(5))
                    .expect("兩根即時 K 線應該送出兩則更新")
                {
                    PaperUpdate::Bar(snapshot) => last = Some(snapshot),
                    other => panic!("不該收到 {other:?}"),
                }
            }
            last.expect("應該有更新").position
        };

        assert!(
            position_after(&warmup) > Fixed::ZERO,
            "暖機過的均線交叉策略在第一根即時 K 線就該有訊號（下一根成交）"
        );
        assert_eq!(
            position_after(&WarmupBars::none()),
            Fixed::ZERO,
            "冷啟動的同一個策略連均線都還算不出來，只能空手——這正是暖機要修的問題"
        );
    }

    // ---- 真實連線（需要網路，預設不跑）----

    /// 真的連上 Binance 的 1 分鐘 K 線串流，收到第一則收盤 K 線的更新之後，
    /// 查一次目前狀態、按停止，確認兩層都收工。
    /// 公開端點，不需要金鑰、沒有任何送單路徑。
    ///
    /// 手動驗證：`cargo test -p at-paper-trading -- --ignored --nocapture`
    /// （最多要等一根 1 分鐘 K 線收盤。）
    #[test]
    #[ignore]
    fn runs_against_the_real_binance_kline_stream() {
        let stream =
            at_market_stream::spawn(at_market_stream::kline_stream("BTCUSDT", Interval::M1));
        let paper = spawn(
            stream,
            Box::new(Recorder::default()),
            &BacktestConfig::frictionless(fx("10000")),
            // 這條驗的是「真實行情接得上」，不是暖機；Recorder 不需要暖機。
            &WarmupBars::none(),
        )
        .expect("設定應該合法");

        assert_eq!(
            paper.latest_snapshot(),
            None,
            "第一根收盤之前沒有「目前狀態」"
        );

        let update = paper
            .updates
            .recv_timeout(Duration::from_secs(150))
            .expect("兩分半內應該至少有一根 1 分鐘 K 線收盤");
        match update {
            PaperUpdate::Bar(s) => {
                println!(
                    "收到真實收盤 K 線的模擬交易更新：時間 {}、權益 {}、現金 {}、部位 {}",
                    s.point.open_time, s.point.equity, s.cash, s.position
                );
                assert_eq!(s.point.equity, fx("10000"), "策略空手、零成本，權益不該變");
                assert_eq!(
                    paper.latest_snapshot(),
                    Some(s),
                    "查得到的目前狀態要和剛收到的那則更新一致"
                );
            }
            other => panic!("不該收到 {other:?}"),
        }

        let started = std::time::Instant::now();
        paper.stop();
        loop {
            match paper.updates.recv_timeout(Duration::from_secs(5)) {
                // 按停止的瞬間可能剛好又有一根收盤、已經排在 channel 裡。
                Ok(PaperUpdate::Bar(_)) => continue,
                Ok(PaperUpdate::Stopped) => break,
                Ok(PaperUpdate::Failed(e)) => panic!("不該失敗：{e}"),
                Err(e) => panic!("按停止之後 5 秒內應該收到 Stopped：{e}"),
            }
        }
        assert_eq!(
            paper.updates.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "Stopped 之後背景執行緒就該結束"
        );
        println!("按下停止到模擬交易迴圈結束：{:?}", started.elapsed());
    }
}
