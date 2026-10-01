//! Phase B 新增的查詢 command（ADR-001 §7.2）：總覽「執行中策略」表格、
//! 最近回測清單、策略庫統計、風控頁的跨策略加總，都走這裡的查詢服務，
//! 不為每個畫面各開一個 command。

use crate::session_registry::{LiveStatus, SessionId, SessionRegistry};
use at_core::RunMode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

fn run_mode_label(kind: RunMode) -> &'static str {
    match kind {
        RunMode::Backtest => "backtest",
        RunMode::Paper => "paper",
        RunMode::Testnet => "testnet",
        RunMode::Live => "live",
    }
}

fn market_label(market: at_core::Market) -> &'static str {
    match market {
        at_core::Market::Spot => "spot",
        at_core::Market::UsdmPerp => "usdmPerp",
    }
}

/// 一根 K 線超過這個倍數的週期沒收盤就算過期（ADR §9.2）。
const STALE_INTERVAL_MULTIPLIER: i64 = 3;

fn stale_threshold_ms(interval: &str) -> i64 {
    interval
        .parse::<at_core::Interval>()
        .map(|i| i.millis() * STALE_INTERVAL_MULTIPLIER)
        .unwrap_or(60_000 * STALE_INTERVAL_MULTIPLIER)
}

/// `list_live_sessions` 的一筆（ADR §7.2 `LiveSessionDto`）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionDto {
    pub session_id: String,
    pub kind: String,
    pub market: String,
    pub symbol: String,
    pub interval: String,
    pub strategy_id: String,
    pub strategy_name: String,
    /// 這個精簡版本只會回 `"running"`（§7.4 的簡化版：已停止的場次已經從
    /// registry 移除，查歷史改用 `list_sessions`）。
    pub status: String,
    pub started_at_ms: i64,
    pub equity: Option<String>,
    pub position: String,
    pub daily_pnl: Option<String>,
    pub as_of_ms: Option<i64>,
    pub stale: bool,
    pub kill_switch: Option<bool>,
    pub bars_seen: u64,
}

/// 總覽「執行中策略」表格、TopBar「執行中策略數」、風控「跨策略加總」共用
/// 這一個。純記憶體讀取，不碰檔案、不碰網路。
#[tauri::command]
pub fn list_live_sessions(app: AppHandle) -> Result<Vec<LiveSessionDto>, String> {
    let registry = app.state::<SessionRegistry>();
    let now = crate::session_registry::now_ms();
    let dtos = registry
        .list_live()
        .into_iter()
        .filter_map(|entry| {
            let live = entry.live_snapshot();
            if live.status != LiveStatus::Running {
                return None;
            }
            let stale = live
                .as_of_ms
                .map(|t| now - t > stale_threshold_ms(&entry.meta.interval))
                .unwrap_or(false);
            Some(LiveSessionDto {
                session_id: entry.id.as_str().to_string(),
                kind: run_mode_label(entry.meta.kind).to_string(),
                market: market_label(entry.meta.market).to_string(),
                symbol: entry.meta.symbol.clone(),
                interval: entry.meta.interval.clone(),
                strategy_id: entry.meta.strategy_id.clone(),
                strategy_name: entry.meta.strategy_name.clone(),
                status: "running".to_string(),
                started_at_ms: entry.meta.started_at_ms,
                equity: live.equity.map(|v| v.to_string()),
                position: live.position.to_string(),
                daily_pnl: live.daily_pnl.map(|v| v.to_string()),
                as_of_ms: live.as_of_ms,
                stale,
                kill_switch: live.kill_switch,
                bars_seen: live.bars_seen,
            })
        })
        .collect();
    Ok(dtos)
}

/// `list_sessions` 的查詢條件，對應 ADR §7.2 `filter`。
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionFilterDto {
    pub kinds: Option<Vec<String>>,
    pub strategy_id: Option<String>,
    pub symbol: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    #[serde(default)]
    pub saved_only: bool,
    pub limit: Option<usize>,
}

fn parse_kind(raw: &str) -> Result<RunMode, String> {
    match raw {
        "backtest" => Ok(RunMode::Backtest),
        "paper" => Ok(RunMode::Paper),
        "testnet" => Ok(RunMode::Testnet),
        "live" => Ok(RunMode::Live),
        other => Err(format!("不認得的 session 種類：{other}")),
    }
}

impl SessionFilterDto {
    fn into_filter(self) -> Result<at_session_store::SessionFilter, String> {
        let kinds = self
            .kinds
            .map(|raw| raw.iter().map(|k| parse_kind(k)).collect())
            .transpose()?;
        Ok(at_session_store::SessionFilter {
            kinds,
            strategy_id: self.strategy_id,
            symbol: self.symbol,
            since_ms: self.since_ms,
            until_ms: self.until_ms,
            saved_only: self.saved_only,
            limit: self.limit,
        })
    }
}

/// 最近回測清單、模擬交易「已停止」標籤頁、策略庫的回測次數／近 30 天報酬，
/// 都走這一個查詢（ADR §7.2）。`SessionRecord` 本身已經是 camelCase 的扁平
/// DTO，直接回傳，不另外包一層。
#[tauri::command]
pub fn list_sessions(
    app: AppHandle,
    filter: SessionFilterDto,
) -> Result<Vec<at_session_store::SessionRecord>, String> {
    let registry = app.state::<SessionRegistry>();
    let filter = filter.into_filter()?;
    registry
        .store()
        .list_sessions(&filter)
        .map_err(|e| e.to_string())
}

/// 明細頁的曲線、策略庫 sparkline、總覽 60 天曲線。`max_points` 給了就在
/// Rust 端等距降採樣（sparkline 要 30 點，不要把幾萬點丟過 IPC）。
#[tauri::command]
pub fn read_session_curve(
    app: AppHandle,
    session_id: String,
    max_points: Option<usize>,
) -> Result<Vec<crate::backtest::EquityPointDto>, String> {
    let registry = app.state::<SessionRegistry>();
    let points = registry
        .store()
        .read_curve(&session_id, max_points)
        .map_err(|e| e.to_string())?;
    Ok(points
        .into_iter()
        .map(|p| crate::backtest::EquityPointDto {
            open_time: p.open_time,
            equity: p.equity.to_string(),
        })
        .collect())
}

/// 「儲存回測」按鈕（ADR §6.4）。
#[tauri::command]
pub fn mark_session_saved(app: AppHandle, session_id: String, saved: bool) -> Result<(), String> {
    let registry = app.state::<SessionRegistry>();
    registry
        .store()
        .mark_saved(&session_id, saved)
        .map_err(|e| e.to_string())
}

/// 刪索引紀錄 + 整個明細資料夾。執行中的 session 拒絕刪除（先停止）。
#[tauri::command]
pub fn delete_session(app: AppHandle, session_id: String) -> Result<(), String> {
    let registry = app.state::<SessionRegistry>();
    if registry
        .get(&SessionId::from_raw(session_id.clone()))
        .is_some()
    {
        return Err("這場交易還在執行中，請先停止再刪除".to_string());
    }
    registry
        .store()
        .delete_session(&session_id)
        .map_err(|e| e.to_string())
}

/// store 的健康狀態（ADR §7.2）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StoreHealthDto {
    pub session_count: usize,
    pub index_bytes: u64,
    pub last_error_zh: Option<String>,
}

#[tauri::command]
pub fn session_store_health(app: AppHandle) -> Result<StoreHealthDto, String> {
    let store = app.state::<Arc<at_session_store::SessionStore>>();
    let health = store.health().map_err(|e| e.to_string())?;
    Ok(StoreHealthDto {
        session_count: health.session_count,
        index_bytes: health.index_bytes,
        last_error_zh: health.last_error_zh,
    })
}
