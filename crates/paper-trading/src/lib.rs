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
//! # 用起來像這樣
//!
//! ```no_run
//! use at_core::{BacktestConfig, Fixed, SmaCross};
//! use at_market_stream::{kline_stream, spawn as spawn_stream};
//! use at_paper_trading::{spawn, PaperUpdate};
//! use at_core::Interval;
//!
//! let config = BacktestConfig::frictionless(Fixed::from_int(10_000).unwrap());
//! let strategy = SmaCross::new(10, 30).unwrap();
//! let stream = spawn_stream(kline_stream("BTCUSDT", Interval::M1));
//! let paper = spawn(stream, Box::new(strategy), &config).unwrap();
//!
//! for update in paper.updates {
//!     match update {
//!         PaperUpdate::Bar(snapshot) => println!("權益：{}", snapshot.point.equity),
//!         PaperUpdate::Failed(e) => {
//!             eprintln!("模擬交易中止：{e}");
//!             break;
//!         }
//!     }
//! }
//! ```

use at_core::backtest::PaperEngine;
use at_core::{BacktestConfig, BacktestError, EquityPoint, Fixed, Strategy};
use at_market_stream::{MarketEvent, MarketStreamHandle};
use std::sync::mpsc;
use std::thread;

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
    /// 引擎算不下去了。**帳本從這一刻起不完整**，這條串流到此結束，
    /// 之後不會再有任何更新；消費端收到它就該停止顯示、把錯誤告訴使用者。
    Failed(BacktestError),
}

/// [`spawn`] 的控制代碼：`updates` 收更新，把它（連同這個 handle）drop 掉，
/// 背景執行緒會在下一次送出更新時自然結束。
///
/// ponytail: 5.2 只要求「資料管線接得通」，所以這裡沒有主動停止的開關——
/// 使用者按停止、查詢當前狀態這些是 5.3 的事。
pub struct PaperTradingHandle {
    pub updates: mpsc::Receiver<PaperUpdate>,
    _worker: thread::JoinHandle<()>,
}

/// 把一條即時行情串流接上一個新的模擬交易引擎，回傳收更新用的控制代碼。
///
/// 設定有問題（起始資金、滑價、費率、資金費、維持保證金率）會**當場**回錯誤，
/// 不會開執行緒、也不會等到第一根 K 線才發現參數是錯的。
///
/// `stream` 整個被搬進背景執行緒，所以行情連線的生命週期跟著模擬交易走。
pub fn spawn(
    stream: MarketStreamHandle,
    mut strategy: Box<dyn Strategy + Send>,
    config: &BacktestConfig,
) -> Result<PaperTradingHandle, BacktestError> {
    let mut engine = PaperEngine::new(config)?;
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        run(&stream.events, &mut engine, strategy.as_mut(), &tx);
    });
    Ok(PaperTradingHandle {
        updates: rx,
        _worker: worker,
    })
}

/// 接線迴圈本體：收行情 → 過濾 → 餵引擎 → 送更新。
///
/// 三個出口，全部是正常結束、都不 panic：
///
/// 1. 行情 channel 關閉（4.5 的背景執行緒結束了）。
/// 2. `updates.send` 失敗（消費端把 [`PaperTradingHandle`] drop 掉了）。
/// 3. [`PaperEngine::on_bar`] 回錯誤——**送出一次 [`PaperUpdate::Failed`] 就
///    停止**，不再餵任何一根。引擎回錯誤之後帳本已經不完整，繼續餵只會產生
///    看起來像真的、其實是假的權益曲線。
///
/// `sleep`/網路都不在這裡，所以測試可以直接在當前執行緒呼叫它，用一般的
/// channel 塞測資進去。
fn run(
    events: &mpsc::Receiver<MarketEvent>,
    engine: &mut PaperEngine,
    strategy: &mut dyn Strategy,
    updates: &mpsc::Sender<PaperUpdate>,
) {
    while let Ok(event) = events.recv() {
        let MarketEvent::Kline(update) = event else {
            continue;
        };
        if !update.is_closed {
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
    use at_core::{Bar, Interval, Symbol, TargetPosition};
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
    /// 回傳策略看到的 K 線與往外送的更新。
    fn feed(
        events: Vec<MarketEvent>,
        strategy: &mut Recorder,
        config: &BacktestConfig,
    ) -> Vec<PaperUpdate> {
        let (event_tx, event_rx) = mpsc::channel();
        for event in events {
            event_tx.send(event).expect("測試用的 channel 不該關閉");
        }
        drop(event_tx);

        let (update_tx, update_rx) = mpsc::channel();
        let mut engine = PaperEngine::new(config).expect("設定應該合法");
        run(&event_rx, &mut engine, strategy, &update_tx);
        drop(update_tx);
        update_rx.into_iter().collect()
    }

    // ---- 過濾：只有收盤 K 線才餵進引擎 ----

    #[test]
    fn only_closed_klines_are_fed_to_the_engine() {
        let mut strategy = Recorder::default();
        let updates = feed(
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
        let updates = feed(
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
    }

    // ---- 錯誤：往外通報一次就停止餵 ----

    #[test]
    fn engine_error_is_reported_once_and_stops_the_feed() {
        let mut strategy = Recorder::default();
        // 第二根的開盤時間沒有比第一根晚（真實世界的重連可能重送舊 K 線），
        // 5.1 的引擎會回 NonMonotonicTime。
        let updates = feed(
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
        let updates = feed(events, &mut live, &config);

        let mut offline = Recorder {
            seen: Vec::new(),
            want: TargetPosition::FULL_LONG,
        };
        let expected = at_core::run_backtest(&bars, &mut offline, &config).expect("回測應該成功");

        let curve: Vec<EquityPoint> = updates
            .iter()
            .map(|u| match u {
                PaperUpdate::Bar(s) => s.point,
                PaperUpdate::Failed(e) => panic!("不該失敗：{e}"),
            })
            .collect();
        assert_eq!(curve, expected.curve, "權益曲線要和回測逐點相等");

        let PaperUpdate::Bar(last) = updates.last().expect("應該有更新") else {
            panic!("最後一則應該是 Bar 更新");
        };
        assert_eq!(last.trades, expected.trades);
        assert_eq!(last.liquidations, expected.liquidations);
    }

    // ---- 真實連線（需要網路，預設不跑）----

    /// 真的連上 Binance 的 1 分鐘 K 線串流，收到第一則收盤 K 線的更新就結束。
    /// 公開端點，不需要金鑰、沒有任何送單路徑。
    ///
    /// 手動驗證：`cargo test -p at-paper-trading -- --ignored --nocapture`
    /// （最多要等一根 1 分鐘 K 線收盤。）
    #[test]
    #[ignore]
    fn runs_against_the_real_binance_kline_stream() {
        use std::time::Duration;

        let stream =
            at_market_stream::spawn(at_market_stream::kline_stream("BTCUSDT", Interval::M1));
        let paper = spawn(
            stream,
            Box::new(Recorder::default()),
            &BacktestConfig::frictionless(fx("10000")),
        )
        .expect("設定應該合法");

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
            }
            PaperUpdate::Failed(e) => panic!("不該失敗：{e}"),
        }
    }
}
