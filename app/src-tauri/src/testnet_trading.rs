//! 6.5 測試網交易頁面的 Tauri command 橋接：開始/停止/一鍵停止/查詢狀態 +
//! 事件轉發。架構抄 `paper_trading.rs`（背景執行緒把 `updates` channel 轉成
//! Tauri 事件），這裡是整個專案第一次會在 UI 上真的送出訂單（僅限
//! 測試網 Demo Trading），所以每個 command 的輸入都要先過
//! [`validate_request`] 這一關，不連網路也不開執行緒。
//!
//! # 跟 `paper_trading.rs` 不一樣的地方：怎麼做「一鍵停止」
//!
//! `paper_trading.rs` 的「停止」沒辦法呼叫 `PaperTradingHandle::stop()`，
//! 因為整個 handle（含 `updates`）被搬進了背景執行緒之後，別的 command
//! 就再也拿不到它，只好另外在 spawn 之前跟行情連線要一份停止旗標的複本。
//!
//! 這裡的 [`TestnetTradingHandle`] 整個（含 `updates`）留在
//! [`TestnetTradingState`] 共用狀態裡，**不**搬進背景執行緒——
//! `stop`/`set_kill_switch`/`kill_switch`/`latest_snapshot` 都只要
//! `&self`，其他 command 隨時可以鎖一下共用狀態就呼叫到，不必像 5.4
//! 那樣另外要一份旗標複本，也不需要去動 `at_testnet_trading` 裡本來是
//! 私有欄位的 `kill_switch: Arc<AtomicBool>`（那個 crate 已經審查過，
//! 不多開一個 public getter）。
//!
//! 代價是背景執行緒不能直接 `for update in handle.updates.iter()`
//! 整段佔住共用鎖（`updates: mpsc::Receiver<TestnetUpdate>` 不是
//! `Sync`，`Mutex<Inner>` 需要 `Inner: Send` 才能一起放進 Tauri app
//! state，而 `Send` 沒問題——只有「同時」才不安全）：改成每次只鎖一下、
//! `recv_timeout` 200 毫秒（跟 `at_testnet_trading`/`at_market_stream`
//! 自己的 `STOP_CHECK_INTERVAL` 同一個數字），逾時就放手鎖、讓其他
//! command 有機會插進來，再重新鎖一次繼續等。停止/一鍵停止command
//! 因此最多等 200 毫秒就能拿到鎖，不會被「等下一根 K 線」卡住。
//!
//! 「目前狀態」一樣不呼叫 `latest_snapshot()`：背景執行緒每收到一則
//! [`TestnetUpdate::Bar`] 就順手把它寫進 [`TestnetTradingState`]，
//! `testnet_trading_status` 直接讀這份存檔。

use at_binance::testnet::BinanceTestnetClient;
use at_core::{Fixed, Interval, Strategy, Symbol};
use at_market_stream::{kline_stream, spawn as spawn_stream};
use at_risk_control::RiskLimits;
use at_testnet_trading::{
    spawn as spawn_testnet, FillReport, OrderOutcome, TestnetConfig, TestnetSnapshot,
    TestnetTradingHandle, TestnetUpdate,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// 背景執行緒每次只鎖一下共用狀態等這麼久，逾時就放手讓其他 command
/// 有機會插進來（理由見模組文件）。
const RECV_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// `app.emit` 用的事件名稱，前端用同一個字串 `listen`。
const TESTNET_TRADING_EVENT: &str = "testnet-trading-update";

/// `start_testnet_trading` 的輸入：策略沿用 3.5/5.4 的 strategyId + params；
/// 資金與風控上限都用字串傳遞 `Fixed`（跟專案裡所有金額欄位同一個慣例）。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StartTestnetTradingRequest {
    pub symbol: String,
    pub interval: String,
    pub strategy_id: String,
    pub params: HashMap<String, String>,
    pub initial_cash: String,
    pub max_daily_loss: String,
    pub max_order_notional: String,
}

/// 一根收盤 K 線處理完之後的帳本狀態，數字用字串保留 `Fixed` 的精確表示。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestnetSnapshotDto {
    pub open_time: i64,
    pub equity: String,
    pub cash: String,
    pub position: String,
    pub fills: usize,
    pub blocked: usize,
    pub fees_paid: String,
    pub daily_pnl: Option<String>,
    pub kill_switch: bool,
}

impl From<TestnetSnapshot> for TestnetSnapshotDto {
    fn from(snapshot: TestnetSnapshot) -> Self {
        TestnetSnapshotDto {
            open_time: snapshot.point.open_time,
            equity: snapshot.point.equity.to_string(),
            cash: snapshot.cash.to_string(),
            position: snapshot.position.to_string(),
            fills: snapshot.fills,
            blocked: snapshot.blocked,
            fees_paid: snapshot.fees_paid.to_string(),
            daily_pnl: snapshot.daily_pnl.map(|v| v.to_string()),
            kill_switch: snapshot.kill_switch,
        }
    }
}

/// 一根 K 線上「下單這件事」的結果，對應 [`OrderOutcome`] 的三種變體。
/// 被擋下/不合規的原因直接用 [`std::fmt::Display`] 轉成中文訊息字串，
/// 前端不需要自己重新組訊息。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OrderOutcomeDto {
    Blocked {
        message: String,
    },
    Invalid {
        message: String,
    },
    Filled {
        order_id: u64,
        side: String,
        requested_qty: String,
        executed_qty: String,
        quote_qty: String,
        fee: String,
        status: String,
    },
}

impl From<OrderOutcome> for OrderOutcomeDto {
    fn from(outcome: OrderOutcome) -> Self {
        match outcome {
            OrderOutcome::Blocked(reason) => OrderOutcomeDto::Blocked {
                message: reason.to_string(),
            },
            OrderOutcome::Invalid(violation) => OrderOutcomeDto::Invalid {
                message: violation.to_string(),
            },
            OrderOutcome::Filled(FillReport {
                order_id,
                side,
                requested_qty,
                executed_qty,
                quote_qty,
                fee,
                status,
            }) => OrderOutcomeDto::Filled {
                order_id,
                side: format!("{side:?}"),
                requested_qty: requested_qty.to_string(),
                executed_qty: executed_qty.to_string(),
                quote_qty: quote_qty.to_string(),
                fee: fee.to_string(),
                status: status.to_string(),
            },
        }
    }
}

/// 往前端 `emit` 的事件，對應 [`TestnetUpdate`] 的四種變體。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TestnetUpdateEvent {
    Order { outcome: OrderOutcomeDto },
    Bar { snapshot: TestnetSnapshotDto },
    Stopped,
    Failed { message: String },
}

/// `testnet_trading_status` 查詢命令的回傳：畫面重新掛載/切回來時，補上
/// 最後已知狀態，不用等下一個事件才有東西可看。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TestnetTradingStatusDto {
    Idle,
    Running {
        snapshot: Option<TestnetSnapshotDto>,
    },
    /// 正常停止（使用者按停止），帳本仍然可信。
    Stopped {
        snapshot: Option<TestnetSnapshotDto>,
    },
    /// 引擎中止，帳本從失敗那一刻起不可信。
    Failed {
        snapshot: Option<TestnetSnapshotDto>,
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
    /// 整個 handle 留在這裡（見模組文件），`stop_testnet_trading` 與
    /// `set_testnet_kill_switch` 都靠它呼叫對應的 `&self` 方法。
    handle: Option<TestnetTradingHandle>,
    latest: Option<TestnetSnapshotDto>,
}

/// Tauri app state：全域只允許同時跑一場測試網交易。
#[derive(Default)]
pub struct TestnetTradingState(Mutex<Inner>);

fn status_dto(inner: &Inner) -> TestnetTradingStatusDto {
    match &inner.state {
        RunState::Idle => TestnetTradingStatusDto::Idle,
        RunState::Running => TestnetTradingStatusDto::Running {
            snapshot: inner.latest.clone(),
        },
        RunState::Stopped => TestnetTradingStatusDto::Stopped {
            snapshot: inner.latest.clone(),
        },
        RunState::Failed(message) => TestnetTradingStatusDto::Failed {
            snapshot: inner.latest.clone(),
            message: message.clone(),
        },
    }
}

/// 套用一則測試網交易更新到共用狀態，回傳要往前端送的事件。刻意拆成純函式
/// （不碰 Tauri/背景執行緒/網路），方便直接測試狀態機對不對。
fn apply_update(inner: &mut Inner, update: TestnetUpdate) -> TestnetUpdateEvent {
    match update {
        TestnetUpdate::Order(outcome) => TestnetUpdateEvent::Order {
            outcome: outcome.into(),
        },
        TestnetUpdate::Bar(snapshot) => {
            let dto = TestnetSnapshotDto::from(snapshot);
            inner.latest = Some(dto.clone());
            TestnetUpdateEvent::Bar { snapshot: dto }
        }
        TestnetUpdate::Stopped => {
            inner.state = RunState::Stopped;
            TestnetUpdateEvent::Stopped
        }
        TestnetUpdate::Failed(error) => {
            let message = error.to_string();
            inner.state = RunState::Failed(message.clone());
            TestnetUpdateEvent::Failed { message }
        }
    }
}

/// 驗證與轉換前端送來的請求：不碰網路、不開執行緒、不讀 Keychain，
/// 方便直接測試每一種輸入錯誤。真的要連網路同步費率/規則、讀測試網金鑰，
/// 是 [`start_testnet_trading`] 自己的事。
#[allow(clippy::type_complexity)]
fn validate_request(
    request: &StartTestnetTradingRequest,
) -> Result<
    (
        Symbol,
        Interval,
        Box<dyn Strategy + Send>,
        Fixed,
        RiskLimits,
    ),
    String,
> {
    let symbol = Symbol::new(&request.symbol).map_err(|e| format!("交易對代號不合法：{e}"))?;
    let interval: Interval = request
        .interval
        .parse()
        .map_err(|e: at_core::ParseIntervalError| e.to_string())?;
    let initial_cash = request
        .initial_cash
        .trim()
        .parse::<Fixed>()
        .map_err(|e| format!("起始資金不是合法數字（{}）：{e}", request.initial_cash))?;
    if initial_cash <= Fixed::ZERO {
        return Err("起始資金必須大於 0".to_string());
    }
    let max_daily_loss = request
        .max_daily_loss
        .trim()
        .parse::<Fixed>()
        .map_err(|e| {
            format!(
                "每日最大虧損不是合法數字（{}）：{e}",
                request.max_daily_loss
            )
        })?;
    let max_order_notional = request
        .max_order_notional
        .trim()
        .parse::<Fixed>()
        .map_err(|e| {
            format!(
                "單筆最大下單金額不是合法數字（{}）：{e}",
                request.max_order_notional
            )
        })?;
    let limits = RiskLimits::new(max_daily_loss, max_order_notional).map_err(|e| e.to_string())?;
    let strategy = crate::backtest::build_strategy(&request.strategy_id, &request.params)?;
    Ok((symbol, interval, strategy, initial_cash, limits))
}

/// 背景執行緒本體：逐則轉發成前端事件、順手更新共用狀態。每次只短暫鎖一下
/// 共用狀態（理由見模組文件），不是整段佔住。出口跟
/// [`at_testnet_trading::spawn`] 文件描述的一致：停止旗標、行情 channel
/// 關閉、消費端不在了、交易邏輯回錯誤，全部正常結束、不 panic——這裡對應的
/// 是 `Err(RecvTimeoutError::Disconnected)`，channel 關閉就結束這個執行緒。
fn forward_updates(app: AppHandle) {
    let state = app.state::<TestnetTradingState>();
    loop {
        let received = {
            let inner = match state.0.lock() {
                Ok(inner) => inner,
                Err(poisoned) => poisoned.into_inner(),
            };
            match &inner.handle {
                Some(handle) => handle.updates.recv_timeout(RECV_POLL_INTERVAL),
                // 理論上不會發生：這個執行緒是 start_testnet_trading 設好
                // handle 之後才開的。保守起見還是結束，不要空轉。
                None => return,
            }
        };
        match received {
            Ok(update) => {
                let event = match state.0.lock() {
                    Ok(mut inner) => apply_update(&mut inner, update),
                    Err(poisoned) => apply_update(&mut poisoned.into_inner(), update),
                };
                let _ = app.emit(TESTNET_TRADING_EVENT, &event);
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// 測試網交易頁面（6.5）的「開始」command：讀測試網金鑰、用正式環境金鑰
/// 同步費率/下單規則（6.4 既定設計：測試網手續費不可信，下單規則用公開
/// 端點）、組設定、接上即時行情、送進 [`at_testnet_trading::spawn`]。
#[tauri::command]
pub fn start_testnet_trading(
    app: AppHandle,
    request: StartTestnetTradingRequest,
) -> Result<(), String> {
    let (symbol, interval, strategy, initial_cash, limits) = validate_request(&request)?;

    let state = app.state::<TestnetTradingState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "測試網交易狀態鎖定失敗".to_string())?;
    if inner.state == RunState::Running {
        return Err("測試網交易已經在執行中，請先停止目前的交易".to_string());
    }

    let client = BinanceTestnetClient::from_keychain()
        .map_err(|e| format!("讀取測試網 API 金鑰失敗：{e}。請先在本頁設定測試網金鑰"))?;

    let base_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("找不到應用程式資料目錄：{e}"))?;
    let synced_fees = at_account_sync::sync_spot_fees(&base_dir, &symbol)
        .map_err(|e| format!("同步帳戶費率失敗：{e}"))?;
    let synced_rules = at_account_sync::rules::sync_symbol_rules(&base_dir, &symbol)
        .map_err(|e| format!("同步下單規則失敗：{e}"))?;

    let config = TestnetConfig {
        symbol: symbol.clone(),
        initial_cash,
        rules: synced_rules.rules,
        fees: synced_fees.schedule,
        limits,
    };

    let stream = spawn_stream(kline_stream(symbol.as_str(), interval));
    // 在 spawn_testnet 之前先拿一份：spawn_testnet 失敗時還能叫停已經連上
    // 的行情連線（理由同 `paper_trading.rs`）。
    let stop_flag = stream.stop_flag();
    let handle = match spawn_testnet(stream, client, strategy, &config) {
        Ok(handle) => handle,
        Err(error) => {
            stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(error.to_string());
        }
    };

    inner.state = RunState::Running;
    inner.handle = Some(handle);
    inner.latest = None;
    drop(inner);

    let app_for_thread = app.clone();
    thread::spawn(move || forward_updates(app_for_thread));
    Ok(())
}

/// 真正收工：連底層行情連線都結束。跟一鍵停止是兩個獨立的按鈕
/// （見模組文件與 `at_testnet_trading` 的 crate 文件）。
#[tauri::command]
pub fn stop_testnet_trading(app: AppHandle) -> Result<(), String> {
    let state = app.state::<TestnetTradingState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "測試網交易狀態鎖定失敗".to_string())?;
    if inner.state != RunState::Running {
        return Err("目前沒有正在執行的測試網交易".to_string());
    }
    if let Some(handle) = &inner.handle {
        handle.stop();
    }
    Ok(())
}

/// 一鍵停止：只擋送單，行情與快照照常跑。跟 [`stop_testnet_trading`]
/// 是兩個獨立的開關，可以在交易執行中隨時切換。
#[tauri::command]
pub fn set_testnet_kill_switch(app: AppHandle, on: bool) -> Result<(), String> {
    let state = app.state::<TestnetTradingState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "測試網交易狀態鎖定失敗".to_string())?;
    let handle = inner
        .handle
        .as_ref()
        .ok_or_else(|| "目前沒有正在執行的測試網交易".to_string())?;
    handle.set_kill_switch(on);
    Ok(())
}

/// 測試網交易頁面（6.5）的查詢 command：畫面重新掛載/切回來時，補上最後
/// 已知狀態，不用等下一個事件才有東西可看。
#[tauri::command]
pub fn testnet_trading_status(app: AppHandle) -> Result<TestnetTradingStatusDto, String> {
    let state = app.state::<TestnetTradingState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "測試網交易狀態鎖定失敗".to_string())?;
    Ok(status_dto(&inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_binance::testnet::OrderStatus;
    use at_core::{EquityPoint, Side};
    use at_risk_control::Blocked;
    use at_testnet_trading::TradingError;

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
        initial_cash: &str,
        max_daily_loss: &str,
        max_order_notional: &str,
    ) -> StartTestnetTradingRequest {
        StartTestnetTradingRequest {
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            strategy_id: strategy_id.to_string(),
            params: param_map(params),
            initial_cash: initial_cash.to_string(),
            max_daily_loss: max_daily_loss.to_string(),
            max_order_notional: max_order_notional.to_string(),
        }
    }

    // ---- validate_request：每一種輸入錯誤都要有清楚的中文訊息 ----

    #[test]
    fn valid_request_normalizes_symbol_and_builds_valid_limits() {
        let req = request(
            "btcusdt",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
            "500",
            "2000",
        );
        let (symbol, interval, _strategy, cash, _limits) =
            validate_request(&req).expect("應該合法");
        assert_eq!(symbol.as_str(), "BTCUSDT", "交易對代號要正規化成大寫");
        assert_eq!(interval, Interval::M1);
        assert_eq!(cash, fx("10000"));
    }

    #[test]
    fn empty_symbol_is_a_clear_chinese_error() {
        let req = request(
            "",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
            "500",
            "2000",
        );
        let err = validate_request(&req).err().unwrap();
        assert!(err.contains("交易對代號"), "{err}");
    }

    #[test]
    fn non_numeric_initial_cash_is_a_clear_chinese_error() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "not-a-number",
            "500",
            "2000",
        );
        let err = validate_request(&req).err().unwrap();
        assert!(err.contains("起始資金"), "{err}");
    }

    #[test]
    fn zero_initial_cash_is_rejected_before_touching_the_network() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "0",
            "500",
            "2000",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "起始資金必須大於 0");
    }

    #[test]
    fn non_positive_max_daily_loss_is_rejected_with_the_risk_control_message() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
            "0",
            "2000",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "每日虧損上限必須大於 0");
    }

    #[test]
    fn non_positive_max_order_notional_is_rejected_with_the_risk_control_message() {
        let req = request(
            "BTCUSDT",
            "1m",
            "sma_cross",
            &[("fastPeriod", "10"), ("slowPeriod", "50")],
            "10000",
            "500",
            "-1",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "單筆下單金額上限必須大於 0");
    }

    #[test]
    fn unsupported_strategy_id_is_a_clear_chinese_error() {
        let req = request(
            "BTCUSDT",
            "1m",
            "not_a_strategy",
            &[],
            "10000",
            "500",
            "2000",
        );
        let err = validate_request(&req).err().unwrap();
        assert_eq!(err, "不支援的策略代號：not_a_strategy");
    }

    // ---- apply_update / status_dto：狀態機本身 ----

    fn snapshot(open_time: i64, equity: &str, kill_switch: bool) -> TestnetSnapshot {
        TestnetSnapshot {
            point: EquityPoint {
                open_time,
                equity: fx(equity),
            },
            cash: fx(equity),
            position: Fixed::ZERO,
            fills: 1,
            blocked: 0,
            fees_paid: Fixed::ZERO,
            daily_pnl: Some(Fixed::ZERO),
            kill_switch,
        }
    }

    #[test]
    fn default_state_is_idle() {
        let inner = Inner::default();
        assert!(matches!(status_dto(&inner), TestnetTradingStatusDto::Idle));
    }

    #[test]
    fn bar_update_records_latest_snapshot_without_changing_a_running_state() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        let event = apply_update(
            &mut inner,
            TestnetUpdate::Bar(snapshot(1000, "10050", false)),
        );

        assert!(matches!(event, TestnetUpdateEvent::Bar { .. }));
        assert_eq!(inner.state, RunState::Running);
        assert_eq!(
            inner.latest.as_ref().map(|s| s.equity.as_str()),
            Some("10050")
        );
    }

    #[test]
    fn order_update_does_not_touch_the_run_state() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        let event = apply_update(
            &mut inner,
            TestnetUpdate::Order(OrderOutcome::Blocked(Blocked::KillSwitch)),
        );
        match event {
            TestnetUpdateEvent::Order { outcome } => {
                assert_eq!(
                    outcome,
                    OrderOutcomeDto::Blocked {
                        message: Blocked::KillSwitch.to_string()
                    }
                );
            }
            other => panic!("預期 Order 事件，收到 {other:?}"),
        }
        assert_eq!(inner.state, RunState::Running, "下單結果不該改變執行狀態");
    }

    #[test]
    fn filled_order_outcome_round_trips_every_field() {
        let outcome = OrderOutcome::Filled(FillReport {
            order_id: 42,
            side: Side::Buy,
            requested_qty: fx("1.5"),
            executed_qty: fx("1.5"),
            quote_qty: fx("150"),
            fee: fx("0.15"),
            status: OrderStatus::Filled,
        });
        let dto: OrderOutcomeDto = outcome.into();
        assert_eq!(
            dto,
            OrderOutcomeDto::Filled {
                order_id: 42,
                side: "Buy".to_string(),
                requested_qty: "1.5".to_string(),
                executed_qty: "1.5".to_string(),
                quote_qty: "150".to_string(),
                fee: "0.15".to_string(),
                status: "FILLED".to_string(),
            }
        );
    }

    #[test]
    fn stopped_update_moves_state_to_stopped_and_keeps_the_last_snapshot() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        apply_update(
            &mut inner,
            TestnetUpdate::Bar(snapshot(1000, "10050", false)),
        );
        let event = apply_update(&mut inner, TestnetUpdate::Stopped);

        assert!(matches!(event, TestnetUpdateEvent::Stopped));
        assert_eq!(inner.state, RunState::Stopped);
        match status_dto(&inner) {
            TestnetTradingStatusDto::Stopped {
                snapshot: Some(dto),
            } => assert_eq!(dto.equity, "10050"),
            other => panic!("預期 Stopped 並帶著最後快照，收到 {other:?}"),
        }
    }

    #[test]
    fn failed_update_moves_state_to_failed_with_the_error_message() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        let event = apply_update(&mut inner, TestnetUpdate::Failed(TradingError::Arithmetic));

        let TestnetUpdateEvent::Failed { message } = event else {
            panic!("預期 Failed 事件");
        };
        assert_eq!(inner.state, RunState::Failed(message.clone()));
        match status_dto(&inner) {
            TestnetTradingStatusDto::Failed { message: m, .. } => assert_eq!(m, message),
            other => panic!("預期 Failed，收到 {other:?}"),
        }
    }

    #[test]
    fn kill_switch_flag_is_carried_through_the_snapshot() {
        let mut inner = Inner {
            state: RunState::Running,
            ..Inner::default()
        };
        apply_update(
            &mut inner,
            TestnetUpdate::Bar(snapshot(1000, "10050", true)),
        );
        assert!(
            inner.latest.as_ref().unwrap().kill_switch,
            "一鍵停止狀態要跟著快照一起顯示，不必額外呼叫 handle"
        );
    }
}
