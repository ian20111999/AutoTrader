//! 5.4 模擬交易頁面的 Tauri command 橋接：開始/停止/查詢狀態 + 事件轉發。
//! Phase B（session registry，ADR-001）之後改成多場並行：每個 command 都要
//! 帶 `sessionId`，`start_paper_trading` 回傳新開的那場的 id。
//!
//! # 為什麼要背景執行緒轉發事件
//!
//! Tauri command 是請求/回應式的，沒辦法直接把 `at_paper_trading::PaperTradingHandle`
//! 的 `updates`（一個會持續收到新東西的 channel）原封不動回給前端。
//! [`start_paper_trading`] 開一個背景執行緒，逐則消費 `updates`，透過 Tauri
//! 的事件系統 [`Emitter::emit`] 轉成前端能 `listen` 的 [`PaperUpdateEvent`]
//! （事件名稱 [`PAPER_TRADING_EVENT`]，payload 外層多包一個 `sessionId`）。
//!
//! # 為什麼 handle 留在 registry（不是搬進背景執行緒）
//!
//! Phase B 之前的版本把整個 `PaperTradingHandle` 搬進背景執行緒，另外存一份
//! 停止旗標給 `stop_paper_trading` 用。多 session 之後這個手法跟 testnet 的
//! 「handle 留在共用狀態、短鎖輪詢」統一（ADR §4.4）：對稱性比較好維護，而且
//! 每場一個鎖之後不再有全域鎖競爭的問題。背景執行緒的循環因此改成：鎖
//! `SessionEntry::control` → `recv_timeout(200ms)` → 放鎖 → 有東西才鎖
//! `live` 寫快照 → 放鎖 → `emit`（`session_registry.rs` 模組文件的鎖不變量）。
//!
//! 「目前狀態」查詢：由 [`SessionEntry::last_snapshot`] 補上最後一筆完整快照
//! （含 cash/trades/liquidations，這些不是兩種 session 的共同子集，不放進
//! `LiveState`），`LiveState` 本身只給總覽／風控用的共同欄位。

use crate::session_registry::{
    new_running_record, now_ms, LiveStatus, SessionControl, SessionEntry, SessionId, SessionMeta,
    SessionRegistry, SessionRegistryChangedEvent, SESSION_REGISTRY_CHANGED_EVENT,
};
use at_core::{BacktestConfig, FeeModel, Fixed, Interval, Strategy, Symbol};
use at_market_stream::{kline_stream, spawn as spawn_stream};
use at_paper_trading::{spawn as spawn_paper, PaperSnapshot, PaperUpdate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// 背景執行緒每次只鎖一下共用狀態等這麼久，逾時就放手讓其他 command
/// 有機會插進來（跟 `testnet_trading.rs` 同一個數字）。
const RECV_POLL_INTERVAL: Duration = Duration::from_millis(200);

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
    /// `strategy_id == "custom"`（Phase F3 積木編輯器）時，整份 DSL 策略的
    /// JSON 字串；其他策略不用填。跟 `backtest.rs::BacktestRequest.dsl_json`
    /// 同一個理由：不塞進 `params`，那是扁平的 key→字串參數表。
    ///
    /// 模擬交易目前只有現貨（`meta.market` 下面寫死 `Market::Spot`），所以
    /// 送進來的自訂策略即使 `direction == long_short` 也編譯得過——
    /// `CustomStrategy` 不知道執行層是現貨還是合約——但做空只有在合約才有
    /// 意義。這裡不另外擋，由前端編輯器在 UI 層勸退（不送出 long_short 的
    /// 策略去模擬交易），因為「現貨能不能做空」是執行層的事，不該讓這個
    /// command 幫策略引擎做市場假設。
    #[serde(default)]
    pub dsl_json: Option<String>,
}

/// 一根收盤 K 線進帳本之後的完整狀態，數字用字串保留 `Fixed` 的精確表示
/// （跟 `backtest.rs` 的 `EquityPointDto` 同一個理由）。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
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

/// 事件 payload 外層多包一個 `sessionId`，前端依它過濾（ADR §7.3）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct PaperUpdateEnvelope<'a> {
    session_id: &'a str,
    #[serde(flatten)]
    event: PaperUpdateEvent,
}

/// `paper_trading_status` 查詢命令的回傳：畫面重新掛載/切回來時，補上最後已知狀態。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PaperTradingStatusDto {
    /// 從來沒開始過，或這場 session 已經停止並離開了 registry
    /// （歷史紀錄留在 `at_session_store`，查詢是 Phase C 的事）。
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

/// 把一則 [`PaperUpdate`] 轉成要往前端送的事件。純函式（不碰 Tauri/背景執行緒），
/// 方便直接測試轉換對不對，不必真的連網路。
fn to_event(update: &PaperUpdate) -> PaperUpdateEvent {
    match update {
        PaperUpdate::Bar(snapshot) => PaperUpdateEvent::Bar {
            snapshot: PaperSnapshotDto::from(*snapshot),
        },
        PaperUpdate::Stopped => PaperUpdateEvent::Stopped,
        PaperUpdate::Failed(error) => PaperUpdateEvent::Failed {
            message: error.to_string(),
        },
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
    let strategy: Box<dyn Strategy + Send> =
        if request.strategy_id == crate::backtest::CUSTOM_STRATEGY_ID {
            let dsl_json = request
                .dsl_json
                .as_deref()
                .ok_or_else(|| "自訂策略缺少 DSL JSON（dslJson）".to_string())?;
            Box::new(crate::backtest::build_custom_strategy(dsl_json)?)
        } else {
            crate::backtest::build_strategy(&request.strategy_id, &request.params)?
        };
    let config = BacktestConfig {
        fees: Some(FeeModel::spot_vip0()),
        slippage: crate::backtest::DEFAULT_SLIPPAGE,
        ..BacktestConfig::frictionless(capital)
    };
    Ok((symbol.to_string(), interval, strategy, config))
}

/// 收尾：把這場 session 的最終狀態寫進 store、從 registry 移除、
/// emit 詳細事件與 `session-registry-changed`。
fn finalize(
    app: &AppHandle,
    registry: &SessionRegistry,
    entry: &SessionEntry,
    status: LiveStatus,
    message: Option<String>,
    last_snapshot: Option<&PaperSnapshotDto>,
    event: PaperUpdateEvent,
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
        trades: last_snapshot.map(|s| s.trades as u64),
        liquidations: last_snapshot.map(|s| s.liquidations as u64),
        ..Default::default()
    };
    if let Err(e) = registry.store().upsert_session(record) {
        eprintln!("寫入模擬交易收尾紀錄失敗：{e}");
    }

    let _ = app.emit(
        PAPER_TRADING_EVENT,
        &PaperUpdateEnvelope {
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
}

/// 背景執行緒本體：每次只短暫鎖一下 [`SessionEntry::control`]
/// （理由見模組文件），逐則轉發成前端事件、順手更新 `LiveState`／
/// `last_snapshot`、寫曲線檔。
fn forward_updates(app: AppHandle, id: SessionId) {
    let registry = app.state::<SessionRegistry>();
    let entry = match registry.get(&id) {
        Some(entry) => entry,
        // 理論上不會發生：這個執行緒是 start_paper_trading 插入 registry 之後才開的。
        None => return,
    };
    let mut last_snapshot: Option<PaperSnapshotDto> = None;

    loop {
        let received = entry.with_control(|control| match control {
            SessionControl::Paper(handle) => handle.updates.recv_timeout(RECV_POLL_INTERVAL),
            _ => Err(RecvTimeoutError::Disconnected),
        });

        match received {
            Ok(update) => {
                let event = to_event(&update);
                match &update {
                    PaperUpdate::Bar(snapshot) => {
                        let dto = PaperSnapshotDto::from(*snapshot);
                        last_snapshot = Some(dto.clone());
                        entry.set_last_snapshot(&dto);
                        entry.update_live(|live| {
                            live.equity = Some(snapshot.point.equity);
                            live.position = snapshot.position;
                            live.as_of_ms = Some(snapshot.point.open_time);
                            live.bars_seen += 1;
                        });
                        let _ = registry
                            .store()
                            .append_curve_point(id.as_str(), snapshot.point);
                        let _ = app.emit(
                            PAPER_TRADING_EVENT,
                            &PaperUpdateEnvelope {
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
                    PaperUpdate::Stopped => {
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
                    PaperUpdate::Failed(error) => {
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
                // 理論上不會走到這裡（channel 只會在送出 Stopped/Failed 之後
                // 才關閉），保守起見仍視為正常停止收尾，不留下「畫面顯示
                // 執行中、背景其實已經停了」的假象。
                finalize(
                    &app,
                    &registry,
                    &entry,
                    LiveStatus::Stopped,
                    None,
                    last_snapshot.as_ref(),
                    PaperUpdateEvent::Stopped,
                );
                return;
            }
        }
    }
}

/// 模擬交易頁面（5.4）的「開始」command：回傳新開的這場 session 的 id。
#[tauri::command]
pub fn start_paper_trading(
    app: AppHandle,
    request: StartPaperTradingRequest,
) -> Result<String, String> {
    let (symbol, interval, strategy, config) = validate_request(&request)?;
    let strategy_name = crate::backtest::resolve_strategy_name(&request.strategy_id)?;

    let registry = app.state::<SessionRegistry>();
    let started_at_ms = now_ms();
    let id = registry.generate_id(at_core::RunMode::Paper, started_at_ms);

    let stream = spawn_stream(kline_stream(&symbol, interval));
    // 在 spawn_paper 之前先拿一份：這樣即使 spawn_paper 失敗，也還握著能叫停
    // 這條已經連上的行情連線的旗標。
    let stop_flag = stream.stop_flag();

    // 先讓行情連線開始把 K 線排進 channel，再去抓暖機用的歷史：抓歷史的那幾百
    // 毫秒內剛收盤的 K 線不會漏掉（重複的部分由 at-paper-trading 的暖機水位線
    // 擋掉）。抓不到就連行情一起收工，不留孤兒連線，也不默默冷啟動。
    let symbol_for_warmup = Symbol::new(&symbol).map_err(|e| format!("交易對代號不合法：{e}"))?;
    let warmup = match crate::warmup::fetch(&symbol_for_warmup, interval, strategy.as_ref()) {
        Ok(warmup) => warmup,
        Err(message) => {
            stop_flag.store(true, Ordering::Relaxed);
            return Err(message);
        }
    };

    let paper = match spawn_paper(stream, strategy, &config, &warmup) {
        Ok(paper) => paper,
        Err(error) => {
            stop_flag.store(true, Ordering::Relaxed);
            return Err(error.to_string());
        }
    };

    let meta = SessionMeta {
        kind: at_core::RunMode::Paper,
        market: at_core::Market::Spot,
        symbol: symbol.clone(),
        interval: request.interval.clone(),
        strategy_id: request.strategy_id.clone(),
        strategy_name,
        params: request.params.clone().into_iter().collect(),
        starting_capital: config.initial_capital,
        started_at_ms,
        cost_assumptions: at_session_store::CostAssumptions {
            fee_model: Some("spot_vip0".to_string()),
            slippage: Some(config.slippage.to_string()),
            funding_rate: Some(config.funding_rate.to_string()),
            maintenance_margin_rate: config.maintenance_margin_rate.map(|v| v.to_string()),
        },
    };

    let entry = Arc::new(SessionEntry::new(
        id.clone(),
        meta.clone(),
        SessionControl::Paper(paper),
    ));
    if let Err(err) = registry.insert(entry.clone()) {
        // 達到上限：registry 沒收下這個 entry，顯式停止，避免留下孤兒連線。
        entry.with_control(|control| {
            if let SessionControl::Paper(handle) = control {
                handle.stop();
            }
        });
        return Err(err);
    }

    if let Err(e) = registry
        .store()
        .upsert_session(new_running_record(&meta, &id))
    {
        // 寫入失敗不擋啟動（執行緒已經在跑），只記一行給之後除錯用。
        eprintln!("寫入模擬交易初始紀錄失敗：{e}");
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

/// 模擬交易頁面（5.4）的「停止」command：鎖一下該場的 `control`、呼叫
/// `PaperTradingHandle::stop()`，背景執行緒與底層行情連線會自己收工。
#[tauri::command]
pub fn stop_paper_trading(app: AppHandle, session_id: String) -> Result<(), String> {
    let registry = app.state::<SessionRegistry>();
    let id = SessionId::from_raw(session_id);
    let entry = registry
        .get(&id)
        .ok_or_else(|| format!("找不到這場交易（可能已經停止）：{}", id.as_str()))?;
    entry.with_control(|control| match control {
        SessionControl::Paper(handle) => {
            handle.stop();
            Ok(())
        }
        _ => Err("這場 session 不是模擬交易".to_string()),
    })
}

/// 模擬交易頁面（5.4）的查詢 command：畫面重新掛載/切回來時，補上最後已知狀態，
/// 不用等下一個事件才有東西可看。
///
/// 已經停止/失敗的 session 會在收尾時離開 registry（§7.4 簡化版），但畫面會在
/// 使用者切頁籤時整個 unmount/remount（`App.tsx` 是條件渲染，不是常駐隱藏），
/// 這時候不能直接回 `Idle`——那會讓使用者以為自己從沒跑過這場，明明
/// `at_session_store` 裡好端端留著收尾紀錄。查 registry 落空時，退而查
/// store 裡的歷史紀錄：找得到就照紀錄的終態回報（沒有詳細帳本快照，但狀態與
/// 失敗訊息還在，畫面不會憑空消失），真的連紀錄都沒有（從沒開始過）才是真的
/// `Idle`。
#[tauri::command]
pub fn paper_trading_status(
    app: AppHandle,
    session_id: String,
) -> Result<PaperTradingStatusDto, String> {
    let registry = app.state::<SessionRegistry>();
    let id = SessionId::from_raw(session_id);
    let Some(entry) = registry.get(&id) else {
        return Ok(status_from_store(&registry, id.as_str()));
    };
    let live = entry.live_snapshot();
    let snapshot = entry.last_snapshot::<PaperSnapshotDto>();
    Ok(match live.status {
        LiveStatus::Running => PaperTradingStatusDto::Running { snapshot },
        LiveStatus::Stopped => PaperTradingStatusDto::Stopped { snapshot },
        LiveStatus::Failed => PaperTradingStatusDto::Failed {
            snapshot,
            message: live.status_message.unwrap_or_default(),
        },
    })
}

/// 在 registry 裡找不到這場 session 時的退路：查 `at_session_store` 的收尾
/// 紀錄。沒有詳細帳本快照（store 只存 `final_equity`，不存 cash/position 的
/// 完整拆解），但至少讓畫面照實顯示「這場已經停止/失敗」而不是「從沒開始過」。
///
/// 上限拉到 [`at_session_store::MAX_LIST_LIMIT`]（而不是預設的 100）：這是
/// 盡力而為的查詢，不是正確性關鍵路徑（真正的資料永遠留在 store 裡不會丟），
/// 但沒道理讓它在使用者短時間內跑很多場時白白漏掉剛結束的那一場。
fn status_from_store(registry: &SessionRegistry, id: &str) -> PaperTradingStatusDto {
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
                PaperTradingStatusDto::Stopped { snapshot: None }
            }
            at_session_store::SessionStatus::Failed => PaperTradingStatusDto::Failed {
                snapshot: None,
                message: r
                    .status_message
                    .unwrap_or_else(|| "交易中止，原因不明".to_string()),
            },
            at_session_store::SessionStatus::Interrupted => PaperTradingStatusDto::Failed {
                snapshot: None,
                message: "App 關閉時這場還在執行，帳本從中斷那一刻起不可信".to_string(),
            },
            // 紀錄還是 Running 但 registry 已經沒有它：只會發生在 registry 跟
            // store 短暫不同步的瞬間（收尾寫檔跟 registry.remove 不是同一個
            // 原子操作）。不確定就回 Idle，不要憑一筆可能過期的紀錄說「還在跑」。
            at_session_store::SessionStatus::Running => PaperTradingStatusDto::Idle,
        },
        None => PaperTradingStatusDto::Idle,
    }
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
            dsl_json: None,
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

    // ---- to_event：PaperUpdate → 前端事件的轉換 ----

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
    fn bar_update_translates_to_a_bar_event_with_the_dto() {
        let event = to_event(&PaperUpdate::Bar(snapshot(1000, "10050")));
        match event {
            PaperUpdateEvent::Bar { snapshot } => assert_eq!(snapshot.equity, "10050"),
            other => panic!("預期 Bar 事件，收到 {other:?}"),
        }
    }

    #[test]
    fn stopped_update_translates_to_a_stopped_event() {
        assert!(matches!(
            to_event(&PaperUpdate::Stopped),
            PaperUpdateEvent::Stopped
        ));
    }

    #[test]
    fn failed_update_translates_to_a_failed_event_with_the_error_message() {
        let event = to_event(&PaperUpdate::Failed(BacktestError::NonMonotonicTime {
            index: 3,
        }));
        let PaperUpdateEvent::Failed { message } = event else {
            panic!("預期 Failed 事件");
        };
        assert_eq!(message, "第 3 根 K 線的開盤時間沒有比前一根晚");
    }

    // ---- SessionEntry 的 last_snapshot 整合（不需要真正的網路連線）----

    #[test]
    fn status_dto_without_a_registered_session_is_idle() {
        // paper_trading_status 本身需要 AppHandle，這裡只驗證查不到時的語意：
        // 用 SessionRegistry 直接查一個沒插入過的 id，應該回 None，
        // command 層再轉成 Idle。
        let registry = SessionRegistry::new(Arc::new(at_session_store::SessionStore::new(
            std::env::temp_dir().join(format!(
                "at_app_paper_trading_status_test_{}",
                std::process::id()
            )),
        )));
        assert!(registry
            .get(&SessionId::from_raw("does-not-exist"))
            .is_none());
    }

    // ---- 回歸測試：離開 registry 之後，畫面重新掛載不該看到 Idle ----
    // （QA 在 Phase B 審查時發現：切頁籤會讓畫面整個 unmount/remount，
    // 已經停止/失敗的 session 這時候已經離開 registry，原本直接回 Idle，
    // 使用者會以為自己從沒跑過這場。）

    fn status_test_registry(name: &str) -> SessionRegistry {
        let dir = std::env::temp_dir().join(format!(
            "at_app_paper_trading_status_from_store_test_{}_{name}_{}",
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
            kind: at_core::RunMode::Paper,
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
                "paper-stopped-001",
                at_session_store::SessionStatus::Stopped,
            ))
            .unwrap();

        let result = status_from_store(&registry, "paper-stopped-001");
        assert!(
            matches!(result, PaperTradingStatusDto::Stopped { snapshot: None }),
            "應該照 store 裡的紀錄回報已停止，不是 Idle：{result:?}"
        );
    }

    #[test]
    fn a_failed_session_gone_from_the_registry_still_reports_the_failure_message() {
        let registry = status_test_registry("failed");
        let mut record = stub_record("paper-failed-001", at_session_store::SessionStatus::Failed);
        record.status_message = Some("第 7 根 K 線的開盤時間沒有比前一根晚".to_string());
        registry.store().upsert_session(record).unwrap();

        let result = status_from_store(&registry, "paper-failed-001");
        let PaperTradingStatusDto::Failed { snapshot, message } = result else {
            panic!("應該照 store 裡的紀錄回報失敗，不是 Idle：{result:?}");
        };
        assert!(snapshot.is_none());
        assert_eq!(message, "第 7 根 K 線的開盤時間沒有比前一根晚");
    }

    #[test]
    fn an_interrupted_session_is_reported_as_failed_with_a_clear_reason() {
        let registry = status_test_registry("interrupted");
        registry
            .store()
            .upsert_session(stub_record(
                "paper-interrupted-001",
                at_session_store::SessionStatus::Interrupted,
            ))
            .unwrap();

        let result = status_from_store(&registry, "paper-interrupted-001");
        let PaperTradingStatusDto::Failed { message, .. } = result else {
            panic!("App 當掉留下的孤兒紀錄應該回報失敗，不是 Idle：{result:?}");
        };
        assert!(message.contains("中斷"));
    }

    #[test]
    fn a_session_id_with_no_record_anywhere_is_genuinely_idle() {
        let registry = status_test_registry("never-existed");
        let result = status_from_store(&registry, "paper-never-existed-001");
        assert!(matches!(result, PaperTradingStatusDto::Idle));
    }

    // ---- 真實連線（需要網路，預設不跑）----

    /// 真的連上 Binance，用 `validate_request` 產生的設定跑一次
    /// spawn 行情 → spawn 模擬交易 → to_event，確認這個模組自己寫的橋接邏輯
    /// （驗證、DTO 轉換）對得上真實資料。不建立真的 Tauri App/mock
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
        let paper = spawn_paper(stream, strategy, &config, &at_core::WarmupBars::none())
            .expect("設定應該合法");

        let update = paper
            .updates
            .recv_timeout(Duration::from_secs(150))
            .expect("兩分半內應該至少有一根 1 分鐘 K 線收盤");
        match to_event(&update) {
            PaperUpdateEvent::Bar { snapshot } => {
                println!("真實模擬交易快照：{snapshot:?}");
                assert_eq!(snapshot.equity, "10000", "策略暖機中、空手，權益不該變");
            }
            other => panic!("不該收到 {other:?}"),
        }

        stop_flag.store(true, Ordering::Relaxed);
        loop {
            match paper.updates.recv_timeout(Duration::from_secs(5)) {
                Ok(update @ PaperUpdate::Bar(_)) => {
                    to_event(&update);
                    continue;
                }
                Ok(PaperUpdate::Stopped) => break,
                Ok(PaperUpdate::Failed(e)) => panic!("不該失敗：{e}"),
                Err(e) => panic!("按停止之後 5 秒內應該收到 Stopped：{e}"),
            }
        }
    }
}
