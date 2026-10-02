//! 6.5 測試網交易頁面的 Tauri command 橋接：開始/停止/一鍵停止/查詢狀態 +
//! 事件轉發。架構抄 `paper_trading.rs`（背景執行緒把 `updates` channel 轉成
//! Tauri 事件）。Phase B（session registry，ADR-001）之後改成多場並行：
//! 每個 command 都要帶 `sessionId`，`start_testnet_trading` 回傳新開的那場的
//! id，這裡是整個專案第一次會在 UI 上真的送出訂單（僅限測試網 Demo
//! Trading），所以每個 command 的輸入都要先過 [`validate_request`] 這一關，
//! 不連網路也不開執行緒。
//!
//! # handle 留在 registry（跟 paper_trading.rs 統一，ADR §4.4）
//!
//! [`TestnetTradingHandle`] 整個（含 `updates`）留在
//! `SessionEntry::control` 裡，**不**搬進背景執行緒——
//! `stop`/`set_kill_switch`/`kill_switch`/`latest_snapshot` 都只要
//! `&self`，其他 command 鎖一下 `SessionEntry::control` 就呼叫到，不需要去
//! 動 `at_testnet_trading` 裡本來是私有欄位的 `kill_switch: Arc<AtomicBool>`
//! （那個 crate 已經審查過，不多開一個 public getter）。
//!
//! 代價是背景執行緒不能直接 `for update in handle.updates.iter()`
//! 整段佔住鎖（`updates: mpsc::Receiver<TestnetUpdate>` 不是 `Sync`）：
//! 改成每次只鎖一下、`recv_timeout` 200 毫秒，逾時就放手鎖、讓其他
//! command 有機會插進來，再重新鎖一次繼續等。停止/一鍵停止 command
//! 因此最多等 200 毫秒就能拿到鎖，不會被「等下一根 K 線」卡住。多 session
//! 之後每場各自一把鎖，不會互相排隊（ADR §4.4：testnet 文件原本擔心的
//! 全域鎖競爭在這個設計下不存在）。

use crate::risk_control::RiskControlState;
use crate::session_registry::{
    new_running_record, now_ms, LiveStatus, SessionControl, SessionEntry, SessionId, SessionMeta,
    SessionRegistry, SessionRegistryChangedEvent, SESSION_REGISTRY_CHANGED_EVENT,
};
use at_binance::testnet::BinanceTestnetClient;
use at_core::{Fixed, Interval, Strategy, Symbol};
use at_market_stream::{kline_stream, spawn as spawn_stream};
use at_risk_control::RiskLimits;
use at_testnet_trading::{
    spawn as spawn_testnet, FillReport, OrderOutcome, TestnetConfig, TestnetSnapshot, TestnetUpdate,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
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
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
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
    /// 被全域風控（熔斷）擋下。和 `Blocked` 分開，使用者才看得出來這筆單是
    /// 「這場的風控上限」擋的還是「整台機器的熔斷」擋的——兩者要做的事不一樣。
    GloballyBlocked {
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
            OrderOutcome::GloballyBlocked(reason) => OrderOutcomeDto::GloballyBlocked {
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

/// 事件 payload 外層多包一個 `sessionId`，前端依它過濾（ADR §7.3）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct TestnetUpdateEnvelope<'a> {
    session_id: &'a str,
    #[serde(flatten)]
    event: TestnetUpdateEvent,
}

/// `testnet_trading_status` 查詢命令的回傳：畫面重新掛載/切回來時，補上
/// 最後已知狀態，不用等下一個事件才有東西可看。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TestnetTradingStatusDto {
    /// 從來沒開始過，或這場 session 已經停止並離開了 registry。
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

/// 把一則 [`TestnetUpdate`] 轉成要往前端送的事件。純函式（不碰 Tauri/背景
/// 執行緒/網路），方便直接測試轉換對不對。
fn to_event(update: &TestnetUpdate) -> TestnetUpdateEvent {
    match update {
        TestnetUpdate::Order(outcome) => TestnetUpdateEvent::Order {
            outcome: outcome.clone().into(),
        },
        TestnetUpdate::Bar(snapshot) => TestnetUpdateEvent::Bar {
            snapshot: TestnetSnapshotDto::from(*snapshot),
        },
        TestnetUpdate::Stopped => TestnetUpdateEvent::Stopped,
        TestnetUpdate::Failed(error) => TestnetUpdateEvent::Failed {
            message: error.to_string(),
        },
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

/// 收尾：把這場 session 的最終狀態寫進 store、從 registry 移除、
/// emit 詳細事件與 `session-registry-changed`。
fn finalize(
    app: &AppHandle,
    registry: &SessionRegistry,
    entry: &SessionEntry,
    status: LiveStatus,
    message: Option<String>,
    last_snapshot: Option<&TestnetSnapshotDto>,
    event: TestnetUpdateEvent,
) {
    entry.update_live(|live| {
        live.status = status;
        live.status_message = message.clone();
    });

    let curve = registry
        .store()
        .read_curve(entry.id.as_str(), None)
        .unwrap_or_default();
    let mut record = new_running_record(&entry.meta, &entry.id);
    record.ended_at_ms = Some(last_snapshot.map(|s| s.open_time).unwrap_or_else(now_ms));
    record.status = match status {
        LiveStatus::Stopped => at_session_store::SessionStatus::Stopped,
        LiveStatus::Failed => at_session_store::SessionStatus::Failed,
        LiveStatus::Running => at_session_store::SessionStatus::Running,
    };
    record.status_message = message;
    record.final_equity = last_snapshot.map(|s| s.equity.clone());
    record.bars_seen = entry.live_snapshot().bars_seen;
    record.metrics = crate::session_registry::metrics_from_curve(&curve);
    record.counters = at_session_store::SessionCounters {
        fills: last_snapshot.map(|s| s.fills as u64),
        blocked: last_snapshot.map(|s| s.blocked as u64),
        fees_paid: last_snapshot.map(|s| s.fees_paid.clone()),
        ..Default::default()
    };
    if let Err(e) = registry.store().upsert_session(record) {
        eprintln!("寫入測試網交易收尾紀錄失敗：{e}");
    }

    let _ = app.emit(
        TESTNET_TRADING_EVENT,
        &TestnetUpdateEnvelope {
            session_id: entry.id.as_str(),
            event,
        },
    );
    let change = match status {
        LiveStatus::Failed => "failed",
        _ => "stopped",
    };
    let _ = app.emit(
        SESSION_REGISTRY_CHANGED_EVENT,
        &SessionRegistryChangedEvent {
            session_id: entry.id.as_str().to_string(),
            change,
        },
    );
    registry.remove(&entry.id);
    // 這場的熔斷累加器也收掉（不然每開一場就留一份）。帳戶層的暫停**不**跟著
    // 消失：那是跨 session 的狀態，解除方式是確認交易所部位後重啟 App。
    app.state::<RiskControlState>()
        .close_session(entry.id.as_str());
}

/// 背景執行緒本體：逐則轉發成前端事件、順手更新 `LiveState`／
/// `last_snapshot`、寫曲線檔。每次只短暫鎖一下 [`SessionEntry::control`]
/// （理由見模組文件），不是整段佔住。出口跟 [`at_testnet_trading::spawn`]
/// 文件描述的一致：停止旗標、行情 channel 關閉、消費端不在了、交易邏輯回
/// 錯誤，全部正常結束、不 panic——這裡對應的是
/// `Err(RecvTimeoutError::Disconnected)`，channel 關閉就結束這個執行緒。
fn forward_updates(app: AppHandle, id: SessionId) {
    let registry = app.state::<SessionRegistry>();
    let risk = app.state::<RiskControlState>();
    let entry = match registry.get(&id) {
        Some(entry) => entry,
        None => return,
    };
    // 這場 session 的熔斷累加器（ADR §5.4：累加器在 App 層，因為事件流在這裡）。
    // 拿不到的話不是「風控關掉」，而是送單路徑那一端會因為同一份狀態讀不到而
    // 擋單——這裡只是沒有東西可以餵。
    let breaker = risk.session(id.as_str());
    let mut last_snapshot: Option<TestnetSnapshotDto> = None;

    loop {
        let received = entry.with_control(|control| match control {
            SessionControl::Testnet(handle) => handle.updates.recv_timeout(RECV_POLL_INTERVAL),
            _ => Err(RecvTimeoutError::Disconnected),
        });

        match received {
            Ok(update) => {
                let event = to_event(&update);
                // 先餵熔斷累加器再處理事件：下一根 K 線的送單前檢查要看到這一根的
                // 結果（連續虧損筆數、權益高點、拒絕率都是從這串事件算出來的）。
                if let Some(breaker) = &breaker {
                    match &update {
                        TestnetUpdate::Order(outcome) => breaker.record_order_outcome(outcome),
                        TestnetUpdate::Bar(snapshot) => breaker.record_bar(
                            snapshot.point.open_time,
                            snapshot.position,
                            snapshot.point.equity,
                        ),
                        TestnetUpdate::Failed(error) => breaker.record_failure(error),
                        TestnetUpdate::Stopped => {}
                    }
                }
                match &update {
                    TestnetUpdate::Order(_) => {
                        // 下單結果不改變執行狀態，只轉發事件，不動 LiveState/曲線。
                        let _ = app.emit(
                            TESTNET_TRADING_EVENT,
                            &TestnetUpdateEnvelope {
                                session_id: id.as_str(),
                                event,
                            },
                        );
                    }
                    TestnetUpdate::Bar(snapshot) => {
                        let dto = TestnetSnapshotDto::from(*snapshot);
                        last_snapshot = Some(dto.clone());
                        entry.set_last_snapshot(&dto);
                        entry.update_live(|live| {
                            live.equity = Some(snapshot.point.equity);
                            live.position = snapshot.position;
                            live.daily_pnl = snapshot.daily_pnl;
                            live.as_of_ms = Some(snapshot.point.open_time);
                            live.bars_seen += 1;
                            live.kill_switch = Some(snapshot.kill_switch);
                        });
                        let _ = registry
                            .store()
                            .append_curve_point(id.as_str(), snapshot.point);
                        let _ = app.emit(
                            TESTNET_TRADING_EVENT,
                            &TestnetUpdateEnvelope {
                                session_id: id.as_str(),
                                event,
                            },
                        );
                        let _ = app.emit(
                            SESSION_REGISTRY_CHANGED_EVENT,
                            &SessionRegistryChangedEvent {
                                session_id: id.as_str().to_string(),
                                change: "tick",
                            },
                        );
                    }
                    TestnetUpdate::Stopped => {
                        finalize(
                            &app,
                            &registry,
                            &entry,
                            LiveStatus::Stopped,
                            None,
                            last_snapshot.as_ref(),
                            event,
                        );
                        return;
                    }
                    TestnetUpdate::Failed(error) => {
                        let message = error.to_string();
                        finalize(
                            &app,
                            &registry,
                            &entry,
                            LiveStatus::Failed,
                            Some(message),
                            last_snapshot.as_ref(),
                            event,
                        );
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                finalize(
                    &app,
                    &registry,
                    &entry,
                    LiveStatus::Stopped,
                    None,
                    last_snapshot.as_ref(),
                    TestnetUpdateEvent::Stopped,
                );
                return;
            }
        }
    }
}

/// 測試網交易頁面（6.5）的「開始」command：讀測試網金鑰、用正式環境金鑰
/// 同步費率/下單規則（6.4 既定設計：測試網手續費不可信，下單規則用公開
/// 端點）、組設定、接上即時行情、送進 [`at_testnet_trading::spawn`]。
/// 回傳新開的這場 session 的 id。
#[tauri::command]
pub fn start_testnet_trading(
    app: AppHandle,
    request: StartTestnetTradingRequest,
) -> Result<String, String> {
    let (symbol, interval, strategy, initial_cash, limits) = validate_request(&request)?;
    let strategy_name = crate::backtest::strategy_display_name(&request.strategy_id)?;

    let registry = app.state::<SessionRegistry>();
    let started_at_ms = now_ms();
    let id = registry.generate_id(at_core::RunMode::Testnet, started_at_ms);

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

    // 先讓行情連線開始把 K 線排進 channel，再抓暖機用的歷史 K 線：抓歷史的那
    // 幾百毫秒內剛收盤的 K 線不會漏掉（重複的部分由 at-testnet-trading 的暖機
    // 水位線擋掉）。抓不到就擋下啟動——這裡會真的送單，用沒收斂的指標交易等於
    // 拿一套沒驗證過的訊號下單（理由見 `warmup.rs`）。
    let warmup = match crate::warmup::fetch(&symbol, interval, strategy.as_ref()) {
        Ok(warmup) => warmup,
        Err(message) => {
            stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(message);
        }
    };

    // 熔斷閘門要在 spawn 之前就存在：送單路徑的第二道閘門是建構參數，不是之後
    // 可以補上的東西（型別上沒有「這場沒有全域風控」的狀態）。
    let breaker =
        app.state::<RiskControlState>()
            .open_session(id.as_str(), symbol.clone(), initial_cash);

    let handle = match spawn_testnet(stream, client, strategy, &config, &warmup, breaker) {
        Ok(handle) => handle,
        Err(error) => {
            stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            app.state::<RiskControlState>().close_session(id.as_str());
            return Err(error.to_string());
        }
    };

    let meta = SessionMeta {
        kind: at_core::RunMode::Testnet,
        market: at_core::Market::Spot,
        symbol: symbol.to_string(),
        interval: request.interval.clone(),
        strategy_id: request.strategy_id.clone(),
        strategy_name,
        params: request.params.clone().into_iter().collect(),
        starting_capital: initial_cash,
        started_at_ms,
        cost_assumptions: at_session_store::CostAssumptions {
            fee_model: Some("testnet_synced_fees".to_string()),
            slippage: Some("0".to_string()),
            funding_rate: Some("0".to_string()),
            maintenance_margin_rate: None,
        },
    };

    let entry = Arc::new(SessionEntry::new(
        id.clone(),
        meta.clone(),
        SessionControl::Testnet(handle),
    ));
    if let Err(err) = registry.insert(entry.clone()) {
        entry.with_control(|control| {
            if let SessionControl::Testnet(handle) = control {
                handle.stop();
            }
        });
        app.state::<RiskControlState>().close_session(id.as_str());
        return Err(err);
    }

    if let Err(e) = registry
        .store()
        .upsert_session(new_running_record(&meta, &id))
    {
        eprintln!("寫入測試網交易初始紀錄失敗：{e}");
    }
    let _ = app.emit(
        SESSION_REGISTRY_CHANGED_EVENT,
        &SessionRegistryChangedEvent {
            session_id: id.as_str().to_string(),
            change: "started",
        },
    );

    let app_for_thread = app.clone();
    let id_for_thread = id.clone();
    thread::spawn(move || forward_updates(app_for_thread, id_for_thread));
    Ok(id.into_string())
}

/// 真正收工：連底層行情連線都結束。跟一鍵停止是兩個獨立的按鈕
/// （見模組文件與 `at_testnet_trading` 的 crate 文件）。
#[tauri::command]
pub fn stop_testnet_trading(app: AppHandle, session_id: String) -> Result<(), String> {
    let registry = app.state::<SessionRegistry>();
    let id = SessionId::from_raw(session_id);
    let entry = registry
        .get(&id)
        .ok_or_else(|| format!("找不到這場交易（可能已經停止）：{}", id.as_str()))?;
    entry.with_control(|control| match control {
        SessionControl::Testnet(handle) => {
            handle.stop();
            Ok(())
        }
        _ => Err("這場 session 不是測試網交易".to_string()),
    })
}

/// 一鍵停止：只擋送單，行情與快照照常跑。跟 [`stop_testnet_trading`]
/// 是兩個獨立的開關，可以在交易執行中隨時切換。
#[tauri::command]
pub fn set_testnet_kill_switch(app: AppHandle, session_id: String, on: bool) -> Result<(), String> {
    let registry = app.state::<SessionRegistry>();
    let id = SessionId::from_raw(session_id);
    let entry = registry
        .get(&id)
        .ok_or_else(|| format!("找不到這場交易（可能已經停止）：{}", id.as_str()))?;
    entry.with_control(|control| match control {
        SessionControl::Testnet(handle) => {
            handle.set_kill_switch(on);
            Ok(())
        }
        _ => Err("這場 session 不是測試網交易".to_string()),
    })
}

/// 測試網交易頁面（6.5）的查詢 command：畫面重新掛載/切回來時，補上最後
/// 已知狀態，不用等下一個事件才有東西可看。
///
/// 已經停止/失敗的 session 會在收尾時離開 registry（§7.4 簡化版），但畫面會在
/// 使用者切頁籤時整個 unmount/remount，這時候不能直接回 `Idle`——這裡是會
/// 真的送出測試網訂單的頁面，使用者更需要知道「這場到底停在哪裡、有沒有
/// 失敗」，不能讓畫面悄悄變回「從沒開始過」。查 registry 落空時退而查 store
/// 裡的收尾紀錄，真的連紀錄都沒有才是 `Idle`。
#[tauri::command]
pub fn testnet_trading_status(
    app: AppHandle,
    session_id: String,
) -> Result<TestnetTradingStatusDto, String> {
    let registry = app.state::<SessionRegistry>();
    let id = SessionId::from_raw(session_id);
    let Some(entry) = registry.get(&id) else {
        return Ok(status_from_store(&registry, id.as_str()));
    };
    let live = entry.live_snapshot();
    let snapshot = entry.last_snapshot::<TestnetSnapshotDto>();
    Ok(match live.status {
        LiveStatus::Running => TestnetTradingStatusDto::Running { snapshot },
        LiveStatus::Stopped => TestnetTradingStatusDto::Stopped { snapshot },
        LiveStatus::Failed => TestnetTradingStatusDto::Failed {
            snapshot,
            message: live.status_message.unwrap_or_default(),
        },
    })
}

/// 在 registry 裡找不到這場 session 時的退路：查 `at_session_store` 的收尾
/// 紀錄。沒有詳細帳本快照，但至少讓畫面照實顯示「這場已經停止/失敗」而不是
/// 「從沒開始過」——這個頁面會真的送單，誤導使用者以為自己沒跑過的代價比
/// 其他頁面高。
fn status_from_store(registry: &SessionRegistry, id: &str) -> TestnetTradingStatusDto {
    let filter = at_session_store::SessionFilter {
        limit: Some(at_session_store::MAX_LIST_LIMIT),
        ..Default::default()
    };
    let record = registry
        .store()
        .list_sessions(&filter)
        .ok()
        .and_then(|records| records.into_iter().find(|r| r.id == id));
    match record {
        Some(r) => match r.status {
            at_session_store::SessionStatus::Stopped
            | at_session_store::SessionStatus::Completed => {
                TestnetTradingStatusDto::Stopped { snapshot: None }
            }
            at_session_store::SessionStatus::Failed => TestnetTradingStatusDto::Failed {
                snapshot: None,
                message: r
                    .status_message
                    .unwrap_or_else(|| "交易中止，原因不明".to_string()),
            },
            at_session_store::SessionStatus::Interrupted => TestnetTradingStatusDto::Failed {
                snapshot: None,
                message:
                    "App 關閉時這場還在執行，帳本從中斷那一刻起不可信，請到測試網後台確認實際部位"
                        .to_string(),
            },
            // 紀錄還是 Running 但 registry 已經沒有它：只會發生在收尾寫檔跟
            // registry.remove 短暫不同步的瞬間。不確定就回 Idle。
            at_session_store::SessionStatus::Running => TestnetTradingStatusDto::Idle,
        },
        None => TestnetTradingStatusDto::Idle,
    }
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

    // ---- 回歸測試：離開 registry 之後，畫面重新掛載不該看到 Idle ----
    // （比照 paper_trading.rs 的同一個回歸測試：這個頁面會真的送出測試網
    // 訂單，誤導使用者以為自己沒跑過這場的代價比其他頁面更高。）

    fn status_test_registry(name: &str) -> SessionRegistry {
        let dir = std::env::temp_dir().join(format!(
            "at_app_testnet_trading_status_from_store_test_{}_{name}_{}",
            std::process::id(),
            now_ms(),
        ));
        SessionRegistry::new(Arc::new(at_session_store::SessionStore::new(dir)))
    }

    fn stub_record(
        id: &str,
        status: at_session_store::SessionStatus,
    ) -> at_session_store::SessionRecord {
        at_session_store::SessionRecord {
            schema_version: at_session_store::CURRENT_SCHEMA_VERSION,
            id: id.to_string(),
            kind: at_core::RunMode::Testnet,
            market: at_core::Market::Spot,
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy_id: "sma_cross".to_string(),
            strategy_name: "均線交叉".to_string(),
            params: Default::default(),
            started_at_ms: 1_000,
            ended_at_ms: Some(2_000),
            status,
            status_message: None,
            starting_capital: "10000".to_string(),
            final_equity: Some("10123.45".to_string()),
            bars_seen: 42,
            metrics: None,
            counters: Default::default(),
            cost_assumptions: Default::default(),
            saved: false,
            data_source_path: None,
            notes: None,
        }
    }

    #[test]
    fn a_stopped_session_gone_from_the_registry_still_reports_stopped() {
        let registry = status_test_registry("stopped");
        registry
            .store()
            .upsert_session(stub_record(
                "testnet-stopped-001",
                at_session_store::SessionStatus::Stopped,
            ))
            .unwrap();

        let result = status_from_store(&registry, "testnet-stopped-001");
        assert!(
            matches!(result, TestnetTradingStatusDto::Stopped { snapshot: None }),
            "應該照 store 裡的紀錄回報已停止，不是 Idle：{result:?}"
        );
    }

    #[test]
    fn a_failed_session_gone_from_the_registry_still_reports_the_failure_message() {
        let registry = status_test_registry("failed");
        let mut record = stub_record(
            "testnet-failed-001",
            at_session_store::SessionStatus::Failed,
        );
        record.status_message = Some("送單失敗：HTTP 狀態碼 502".to_string());
        registry.store().upsert_session(record).unwrap();

        let result = status_from_store(&registry, "testnet-failed-001");
        let TestnetTradingStatusDto::Failed { snapshot, message } = result else {
            panic!("應該照 store 裡的紀錄回報失敗，不是 Idle：{result:?}");
        };
        assert!(snapshot.is_none());
        assert_eq!(message, "送單失敗：HTTP 狀態碼 502");
    }

    #[test]
    fn an_interrupted_session_warns_about_unreliable_positions() {
        let registry = status_test_registry("interrupted");
        registry
            .store()
            .upsert_session(stub_record(
                "testnet-interrupted-001",
                at_session_store::SessionStatus::Interrupted,
            ))
            .unwrap();

        let result = status_from_store(&registry, "testnet-interrupted-001");
        let TestnetTradingStatusDto::Failed { message, .. } = result else {
            panic!("App 當掉留下的孤兒紀錄應該回報失敗，不是 Idle：{result:?}");
        };
        assert!(message.contains("中斷"));
        assert!(
            message.contains("測試網後台"),
            "應該提醒使用者去測試網後台確認實際部位"
        );
    }

    #[test]
    fn a_session_id_with_no_record_anywhere_is_genuinely_idle() {
        let registry = status_test_registry("never-existed");
        let result = status_from_store(&registry, "testnet-never-existed-001");
        assert!(matches!(result, TestnetTradingStatusDto::Idle));
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

    // ---- to_event：TestnetUpdate → 前端事件的轉換 ----

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
    fn bar_update_translates_to_a_bar_event_with_the_dto() {
        let event = to_event(&TestnetUpdate::Bar(snapshot(1000, "10050", false)));
        match event {
            TestnetUpdateEvent::Bar { snapshot } => assert_eq!(snapshot.equity, "10050"),
            other => panic!("預期 Bar 事件，收到 {other:?}"),
        }
    }

    #[test]
    fn order_update_translates_to_an_order_event() {
        let event = to_event(&TestnetUpdate::Order(OrderOutcome::Blocked(
            Blocked::KillSwitch,
        )));
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
    fn stopped_update_translates_to_a_stopped_event() {
        assert!(matches!(
            to_event(&TestnetUpdate::Stopped),
            TestnetUpdateEvent::Stopped
        ));
    }

    #[test]
    fn failed_update_translates_to_a_failed_event_with_the_error_message() {
        let event = to_event(&TestnetUpdate::Failed(TradingError::Arithmetic));
        let TestnetUpdateEvent::Failed { message } = event else {
            panic!("預期 Failed 事件");
        };
        assert!(!message.is_empty());
    }

    #[test]
    fn kill_switch_flag_is_carried_through_the_snapshot_dto() {
        let dto = TestnetSnapshotDto::from(snapshot(1000, "10050", true));
        assert!(dto.kill_switch, "一鍵停止狀態要跟著快照一起顯示");
    }
}
