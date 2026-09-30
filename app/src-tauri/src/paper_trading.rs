//! 5.4 模擬交易頁面的 Tauri command 橋接：開始/停止/查詢狀態 + 事件轉發。
//!
//! # 為什麼要背景執行緒轉發事件
//!
//! Tauri command 是請求/回應式的，沒辦法直接把 `at_paper_trading::PaperTradingHandle`
//! 的 `updates`（一個會持續收到新東西的 channel）原封不動回給前端。這裡的作法是
//! [`start_paper_trading`] 開一個背景執行緒，把整個 [`PaperTradingHandle`] 移進去，
//! 逐則消費 `updates`，透過 Tauri 的事件系統 [`Emitter::emit`] 轉成前端能 `listen`
//! 的 [`PaperUpdateEvent`]（事件名稱 [`PAPER_TRADING_EVENT`]）。
//!
//! # 「停止」為什麼不呼叫 `PaperTradingHandle::stop`
//!
//! `stop()`／`latest_snapshot()` 都要 `&PaperTradingHandle`，但整個 handle
//! （包含 `updates`）已經被搬進背景執行緒，其他 command（`stop_paper_trading`）
//! 拿不到它。`at_market_stream::MarketStreamHandle::stop_flag()` 在交給
//! `at_paper_trading::spawn` 之前先呼叫一次，拿到的是**同一個** `Arc<AtomicBool>`
//! （5.3 的設計：模擬交易與底層行情連線本來就共用一個停止旗標），
//! 自己存這個 `Arc` 就能在別的 command 裡喊停，效果跟呼叫 `paper.stop()`完全一樣，
//! 不需要碰 `PaperTradingHandle` 私有欄位（本來也碰不到）。
//!
//! 「目前狀態」也是同樣的理由，不呼叫 `latest_snapshot()`：背景執行緒每收到一則
//! [`PaperUpdate::Bar`] 就順手把它寫進 [`PaperTradingState`]，`paper_trading_status`
//! 直接讀這份存檔，不必碰 handle。

use at_core::{BacktestConfig, FeeModel, Fixed, Interval, Strategy, Symbol};
use at_market_stream::{kline_stream, spawn as spawn_stream};
use at_paper_trading::{spawn as spawn_paper, PaperSnapshot, PaperTradingHandle, PaperUpdate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, Manager};

/// `app.emit` 用的事件名稱，前端用同一個字串 `listen`。
const PAPER_TRADING_EVENT: &str = "paper-trading-update";

/// `start_paper_trading` 的輸入：策略沿用 3.5 的 `StrategyConfig`（strategyId +
/// params），資料範圍是交易對／K 線週期／虛擬起始資金。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StartPaperTradingRequest {
    pub symbol: String,
    pub interval: String,
    pub strategy_id: String,
    pub params: HashMap<String, String>,
    pub starting_capital: String,
}

/// 一根收盤 K 線進帳本之後的完整狀態，數字用字串保留 `Fixed` 的精確表示
/// （跟 `backtest.rs` 的 `EquityPointDto` 同一個理由）。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PaperSnapshotDto {
    pub open_time: i64,
    pub equity: String,
    pub cash: String,
    pub position: String,
    pub trades: usize,
    pub liquidations: usize,
}

impl From<PaperSnapshot> for PaperSnapshotDto {
    fn from(snapshot: PaperSnapshot) -> Self {
        PaperSnapshotDto {
            open_time: snapshot.point.open_time,
            equity: snapshot.point.equity.to_string(),
            cash: snapshot.cash.to_string(),
            position: snapshot.position.to_string(),
            trades: snapshot.trades,
            liquidations: snapshot.liquidations,
        }
    }
}

/// 往前端 `emit` 的事件，對應 [`PaperUpdate`] 的三種變體。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PaperUpdateEvent {
    Bar { snapshot: PaperSnapshotDto },
    Stopped,
    Failed { message: String },
}

/// `paper_trading_status` 查詢命令的回傳：畫面重新掛載/切回來時，補上最後已知狀態。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PaperTradingStatusDto {
    /// 從來沒開始過。
    Idle,
    Running {
        snapshot: Option<PaperSnapshotDto>,
    },
    /// 正常停止（使用者按停止），帳本仍然可信。
    Stopped {
        snapshot: Option<PaperSnapshotDto>,
    },
    /// 引擎中止，帳本從失敗那一刻起不可信。
    Failed {
        snapshot: Option<PaperSnapshotDto>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
enum RunState {
    #[default]
    Idle,
    Running,
    Stopped,
    Failed(String),
}

#[derive(Default)]
struct Inner {
    state: RunState,
    /// 跟背景執行緒裡的 `PaperTradingHandle` 共用同一個旗標（見模組文件）；
    /// `stop_paper_trading` 靠它喊停，不需要碰 handle 本身。
    stop_flag: Option<Arc<AtomicBool>>,
    latest: Option<PaperSnapshotDto>,
}

/// Tauri app state：全域只允許同時跑一場模擬交易。
#[derive(Default)]
pub struct PaperTradingState(Mutex<Inner>);

fn status_dto(inner: &Inner) -> PaperTradingStatusDto {
    match &inner.state {
        RunState::Idle => PaperTradingStatusDto::Idle,
        RunState::Running => PaperTradingStatusDto::Running {
            snapshot: inner.latest.clone(),
        },
        RunState::Stopped => PaperTradingStatusDto::Stopped {
            snapshot: inner.latest.clone(),
        },
        RunState::Failed(message) => PaperTradingStatusDto::Failed {
            snapshot: inner.latest.clone(),
            message: message.clone(),
        },
    }
}

/// 套用一則模擬交易更新到共用狀態，回傳要往前端送的事件。刻意拆成純函式
/// （不碰 Tauri/背景執行緒），方便直接測試狀態機對不對，不必真的連網路。
fn apply_update(inner: &mut Inner, update: PaperUpdate) -> PaperUpdateEvent {
    match update {
        PaperUpdate::Bar(snapshot) => {
            let dto = PaperSnapshotDto::from(snapshot);
            inner.latest = Some(dto.clone());
            PaperUpdateEvent::Bar { snapshot: dto }
        }
        PaperUpdate::Stopped => {
            inner.state = RunState::Stopped;
            PaperUpdateEvent::Stopped
        }
        PaperUpdate::Failed(error) => {
            let message = error.to_string();
            inner.state = RunState::Failed(message.clone());
            PaperUpdateEvent::Failed { message }
        }
    }
}

/// 驗證與轉換前端送來的請求：不碰網路、不開執行緒，方便直接測試每一種輸入錯誤。
fn validate_request(
    request: &StartPaperTradingRequest,
) -> Result<(String, Interval, Box<dyn Strategy + Send>, BacktestConfig), String> {
    let symbol = Symbol::new(&request.symbol).map_err(|e| format!("交易對代號不合法：{e}"))?;
    let interval: Interval = request
        .interval
        .parse()
        .map_err(|e: at_core::ParseIntervalError| e.to_string())?;
    let capital = request
        .starting_capital
        .trim()
        .parse::<Fixed>()
        .map_err(|e| format!("起始資金不是合法數字（{}）：{e}", request.starting_capital))?;
    if capital <= Fixed::ZERO {
        // 提早擋下來，避免等 PaperEngine::new 才發現——那時行情 WebSocket 已經連上了
        // （見下方 start_paper_trading），會留下一條沒人管的孤兒連線。
        return Err("起始資金必須大於 0".to_string());
    }
    let strategy = crate::backtest::build_strategy(&request.strategy_id, &request.params)?;
    let config = BacktestConfig {
        fees: Some(FeeModel::spot_vip0()),
        slippage: crate::backtest::DEFAULT_SLIPPAGE,
        ..BacktestConfig::frictionless(capital)
    };
    Ok((symbol.to_string(), interval, strategy, config))
}

/// 背景執行緒本體：吃掉整個 [`PaperTradingHandle`]，逐則轉發成前端事件、
/// 順手更新共用狀態。四個出口跟 [`at_paper_trading::spawn`] 文件描述的一致，
/// 這裡不重複判斷，只是每收到一則就做「更新狀態 → emit」兩件事。
fn forward_updates(app: AppHandle, paper: PaperTradingHandle) {
    let state = app.state::<PaperTradingState>();
    for update in paper.updates {
        let event = match state.0.lock() {
            Ok(mut inner) => apply_update(&mut inner, update),
            // 鎖被下毒（不該發生，但寧可繼續轉發也不要讓畫面卡住）。
            Err(poisoned) => apply_update(&mut poisoned.into_inner(), update),
        };
        let _ = app.emit(PAPER_TRADING_EVENT, &event);
    }
    // 理論上不會走到這裡（channel 只會在送出 Stopped/Failed 之後才關閉），
    // 保險起見還是把狀態收乾淨，不留下「畫面顯示執行中、背景其實已經停了」的假象。
    if let Ok(mut inner) = state.0.lock() {
        if inner.state == RunState::Running {
            inner.state = RunState::Stopped;
        }
    };
}

/// 模擬交易頁面（5.4）的「開始」command。
#[tauri::command]
pub fn start_paper_trading(
    app: AppHandle,
    request: StartPaperTradingRequest,
) -> Result<(), String> {
    let (symbol, interval, strategy, config) = validate_request(&request)?;

    let state = app.state::<PaperTradingState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "模擬交易狀態鎖定失敗".to_string())?;
    if inner.state == RunState::Running {
        return Err("模擬交易已經在執行中，請先停止目前的模擬".to_string());
    }

    let stream = spawn_stream(kline_stream(&symbol, interval));
    // 在 spawn_paper 之前先拿一份：這樣即使 spawn_paper 失敗，也還握著能叫停
    // 這條已經連上的行情連線的旗標（見模組文件）。
    let stop_flag = stream.stop_flag();
    let paper = match spawn_paper(stream, strategy, &config) {
        Ok(paper) => paper,
        Err(error) => {
            stop_flag.store(true, Ordering::Relaxed);
            return Err(error.to_string());
        }
    };

    inner.state = RunState::Running;
    inner.stop_flag = Some(stop_flag);
    inner.latest = None;
    drop(inner);

    let app_for_thread = app.clone();
    thread::spawn(move || forward_updates(app_for_thread, paper));
    Ok(())
}

/// 模擬交易頁面（5.4）的「停止」command：只是把共用旗標設成 true，
/// 背景執行緒與底層行情連線會自己收工（5.3 已經驗證過這個機制）。
#[tauri::command]
pub fn stop_paper_trading(app: AppHandle) -> Result<(), String> {
    let state = app.state::<PaperTradingState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "模擬交易狀態鎖定失敗".to_string())?;
    if inner.state != RunState::Running {
        return Err("目前沒有正在執行的模擬交易".to_string());
    }
    if let Some(flag) = &inner.stop_flag {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// 模擬交易頁面（5.4）的查詢 command：畫面重新掛載/切回來時，補上最後已知狀態，
/// 不用等下一個事件才有東西可看。
#[tauri::command]
pub fn paper_trading_status(app: AppHandle) -> Result<PaperTradingStatusDto, String> {
    let state = app.state::<PaperTradingState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "模擬交易狀態鎖定失敗".to_string())?;
    Ok(status_dto(&inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{BacktestError, EquityPoint};
    use std::time::Duration;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn param_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn request(
        symbol: &str,
        interval: &str,
        strategy_id: &str,
        params: &[(&str, &str)],
        starting_capital: &str,
    ) -> StartPaperTradingRequest {
        StartPaperTradingRequest {
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            strategy_id: strategy_id.to_string(),
            params: param_map(params),
            starting_capital: starting_capital.to_string(),
        }
    }

    // ---- validate_request：每一種輸入錯誤都要有清楚的中文訊息 ----

    #[test]
    fn valid_request_normalizes_symbol_and_builds_a_valid_config() {
        let req = request(
            "btcusdt",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
        );
        let (symbol, interval, _strategy, config) = validate_request(&req).expect("應該合法");
        assert_eq!(symbol, "BTCUSDT", "交易對代號要正規化成大寫");
        assert_eq!(interval, Interval::M1);
        assert_eq!(config.initial_capital, fx("10000"));
        assert_eq!(config.slippage, crate::backtest::DEFAULT_SLIPPAGE);
    }

    #[test]
    fn empty_symbol_is_a_clear_chinese_error() {
        let req = request(
            "",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
        );
        let err = validate_request(&req).err().unwrap();
        assert!(err.contains("交易對代號"), "{err}");
    }

    #[test]
    fn unsupported_interval_is_a_clear_error() {
        let req = request(
            "BTCUSDT",
            "not-an-interval",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
        );
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn non_numeric_capital_is_a_clear_chinese_error() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "not-a-number",
        );
        let err = validate_request(&req).err().unwrap();
        assert!(err.contains("起始資金"), "{err}");
    }

    #[test]
    fn zero_capital_is_rejected_before_touching_the_network() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "0",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "起始資金必須大於 0");
    }

    #[test]
    fn unsupported_strategy_id_is_a_clear_chinese_error() {
        let req = request("BTCUSDT", "1m", "not_a_strategy", &[], "10000");
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "不支援的策略代號：not_a_strategy");
    }

    #[test]
    fn missing_strategy_param_is_a_clear_chinese_error() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10")],
            "10000",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "缺少參數：slowPeriod");
    }

    // ---- apply_update / status_dto：狀態機本身 ----

    fn snapshot(open_time: i64, equity: &str) -> PaperSnapshot {
        PaperSnapshot {
            point: EquityPoint {
                open_time,
                equity: fx(equity),
            },
            cash: fx(equity),
            position: Fixed::ZERO,
            trades: 1,
            liquidations: 0,
        }
    }

    #[test]
    fn default_state_is_idle() {
        let inner = Inner::default();
        assert!(matches!(status_dto(&inner), PaperTradingStatusDto::Idle));
    }

    #[test]
    fn bar_update_records_latest_snapshot_without_changing_a_running_state() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        let event = apply_update(&mut inner, PaperUpdate::Bar(snapshot(1000, "10050")));

        assert!(matches!(event, PaperUpdateEvent::Bar { .. }));
        assert_eq!(inner.state, RunState::Running);
        assert_eq!(
            inner.latest.as_ref().map(|s| s.equity.as_str()),
            Some("10050")
        );
        assert!(matches!(
            status_dto(&inner),
            PaperTradingStatusDto::Running { snapshot: Some(_) }
        ));
    }

    #[test]
    fn stopped_update_moves_state_to_stopped_and_keeps_the_last_snapshot() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        apply_update(&mut inner, PaperUpdate::Bar(snapshot(1000, "10050")));
        let event = apply_update(&mut inner, PaperUpdate::Stopped);

        assert!(matches!(event, PaperUpdateEvent::Stopped));
        assert_eq!(inner.state, RunState::Stopped);
        match status_dto(&inner) {
            PaperTradingStatusDto::Stopped {
                snapshot: Some(dto),
            } => {
                assert_eq!(dto.equity, "10050", "正常停止不代表帳本要清空")
            }
            other => panic!("預期 Stopped 並帶著最後快照，收到 {other:?}"),
        }
    }

    #[test]
    fn failed_update_moves_state_to_failed_with_the_error_message() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        let event = apply_update(
            &mut inner,
            PaperUpdate::Failed(BacktestError::NonMonotonicTime { index: 3 }),
        );

        let PaperUpdateEvent::Failed { message } = event else {
            panic!("預期 Failed 事件");
        };
        assert_eq!(message, "第 3 根 K 線的開盤時間沒有比前一根晚");
        assert_eq!(inner.state, RunState::Failed(message.clone()));
        match status_dto(&inner) {
            PaperTradingStatusDto::Failed { message: m, .. } => assert_eq!(m, message),
            other => panic!("預期 Failed，收到 {other:?}"),
        }
    }

    // ---- 真實連線（需要網路，預設不跑）----

    /// 真的連上 Binance，用 `validate_request` 產生的設定跑一次
    /// spawn 行情 → spawn 模擬交易 → apply_update，確認這個模組自己寫的橋接邏輯
    /// （驗證、DTO 轉換、狀態機）對得上真實資料。不建立真的 Tauri App/mock
    /// AppHandle：`app.emit` 是 Tauri 框架自己的責任，這裡只驗證這個檔案寫的邏輯。
    ///
    /// 手動驗證：`cargo test -p app -- --ignored --nocapture paper_trading`
    #[test]
    #[ignore]
    fn real_binance_stream_produces_well_formed_snapshots_and_stops_cleanly() {
        let req = request(
            "btcusdt",
            "1m",
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        let (symbol, interval, strategy, config) = validate_request(&req).expect("設定應該合法");

        let stream = at_market_stream::spawn(at_market_stream::kline_stream(&symbol, interval));
        let stop_flag = stream.stop_flag();
        let paper = spawn_paper(stream, strategy, &config).expect("設定應該合法");

        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };

        let update = paper
            .updates
            .recv_timeout(Duration::from_secs(150))
            .expect("兩分半內應該至少有一根 1 分鐘 K 線收盤");
        match apply_update(&mut inner, update) {
            PaperUpdateEvent::Bar { snapshot } => {
                println!("真實模擬交易快照：{snapshot:?}");
                assert_eq!(snapshot.equity, "10000", "策略暖機中、空手，權益不該變");
                assert_eq!(inner.latest, Some(snapshot));
            }
            other => panic!("不該收到 {other:?}"),
        }

        stop_flag.store(true, Ordering::Relaxed);
        loop {
            match paper.updates.recv_timeout(Duration::from_secs(5)) {
                Ok(update @ PaperUpdate::Bar(_)) => {
                    apply_update(&mut inner, update);
                    continue;
                }
                Ok(PaperUpdate::Stopped) => break,
                Ok(PaperUpdate::Failed(e)) => panic!("不該失敗：{e}"),
                Err(e) => panic!("按停止之後 5 秒內應該收到 Stopped：{e}"),
            }
        }
    }
}
