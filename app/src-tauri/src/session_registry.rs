//! Phase B：多場並行 session 的 App 層 registry。
//!
//! 對應 `docs/architecture/2026-10-01-session-registry.md`（ADR-001）§4。
//! `at-paper-trading`／`at-testnet-trading` 一行不改（D1）：這裡只是把
//! 「一個 `Mutex<Option<Handle>>`」換成「`HashMap<SessionId, Arc<SessionEntry>>`」，
//! 控制面（`stop`／`set_kill_switch`／`latest_snapshot`）照樣呼叫既有的
//! `&self` 方法。
//!
//! # 鎖的規則（§4.3，這是整個設計最容易出錯的地方）
//!
//! 1. `sessions` 的鎖只在「查找、插入、移除」期間持有，絕不跨 `emit`、
//!    `recv`、檔案 I/O 或網路。拿到 `Arc<SessionEntry>` 就立刻放掉。
//! 2. `control` 鎖與 `live` 鎖不可同時持有。轉發執行緒的循環是：
//!    鎖 `control` → `recv_timeout` → 放鎖 → （有東西才）鎖 `live` 寫快照 →
//!    放鎖 → `emit`。
//! 3. `live` 只用 `RwLock`，而且讀取端一律 clone 出去再用，不在持有讀鎖時
//!    呼叫任何回呼——這是 [`SessionRegistry::aggregate_exposure`] 能安全從
//!    交易執行緒同步呼叫的前提。
//! 4. 轉發執行緒不碰 `sessions` 的寫鎖；session 的移除只在轉發執行緒收尾
//!    （已經不持有任何 entry 鎖）時自己做一次。
//!
//! # 「停止後留在 registry 多久」採 §7.4 建議的簡化版本
//!
//! 已停止／失敗的 session 在寫完 store 收尾紀錄之後立刻從 registry 移除，
//! 不另外保留一份「已停止但還在記憶體」的狀態。`list_live_sessions` 因此
//! 只會回 `running` 的場次；已停止的紀錄一律查 `at_session_store`
//! （Phase C 的總覽／策略庫會用到）。

use at_core::{Fixed, Market, RunMode};
use at_paper_trading::PaperTradingHandle;
use at_session_store::SessionStore;
use at_testnet_trading::TestnetTradingHandle;
use serde::de::DeserializeOwned;
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 同時執行中的 session 上限（ADR D7）：每場吃 3 條 OS 執行緒
/// （`at-market-stream` 的 WebSocket 執行緒、交易迴圈執行緒、App 層的轉發
/// 執行緒）+ 1 條獨立的 WebSocket 連線。20 場是避免連線風暴的誠實上限，
/// 不是隨便抓的數字（ADR §9.1）。
pub const MAX_CONCURRENT_SESSIONS: usize = 20;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 檔名／事件 key 都用它，所以必須是檔案系統安全的字串（跟
/// `at_session_store::validate_session_id` 同一條白名單：`format!` 產生的
/// `"{kind}-{started_at_ms}-{seq:03}"` 本來就只含小寫英文、數字、`-`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// 從前端傳進來的字串包裝成 `SessionId`，用來查 registry 的
    /// `HashMap`。這裡不驗證字元集——查不到就是 `NotFound`，不涉及組檔案
    ///路徑（那是 `at_session_store` 自己的責任與驗證點）。
    pub fn from_raw(id: impl Into<String>) -> Self {
        SessionId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

fn run_mode_slug(kind: RunMode) -> &'static str {
    match kind {
        RunMode::Backtest => "backtest",
        RunMode::Paper => "paper",
        RunMode::Testnet => "testnet",
        RunMode::Live => "live",
    }
}

/// 一場 session 不可變的識別資訊，建立後不再改，所以不用鎖（§4.2）。
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub kind: RunMode,
    pub market: Market,
    pub symbol: String,
    pub interval: String,
    pub strategy_id: String,
    pub strategy_name: String,
    pub params: BTreeMap<String, String>,
    pub starting_capital: Fixed,
    pub started_at_ms: i64,
    /// 「這個數字是用什麼假設算出來的」，收尾寫 store 時要用（§6.1）。
    pub cost_assumptions: at_session_store::CostAssumptions,
}

/// session 目前的生命週期狀態。只有 `Running` 的場次會留在 registry 裡
/// 被 `list_live_sessions` 看到；`Stopped`/`Failed` 是轉發執行緒收尾過程中
/// 的短暫中繼狀態，寫完 store 之後整個 entry 就從 registry 移除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveStatus {
    Running,
    Stopped,
    Failed,
}

/// 兩種 session 的共同子集：總覽／風控只需要這些（§4.2）。種類專屬的欄位
/// （testnet 的 `blocked`/`feesPaid`，或完整快照）留在各自的「最後一筆快照」
/// 裡（見 [`SessionEntry::last_snapshot`]），不塞進這個共用結構。
#[derive(Debug, Clone)]
pub struct LiveState {
    pub status: LiveStatus,
    pub equity: Option<Fixed>,
    pub position: Fixed,
    pub daily_pnl: Option<Fixed>,
    /// 最後一根收盤 K 線的 `open_time`。總覽／風控用它判斷資料是不是過期
    /// （ADR §9.2）。
    pub as_of_ms: Option<i64>,
    pub bars_seen: u64,
    /// 只有 testnet 有；paper 永遠是 `None`。
    pub kill_switch: Option<bool>,
    pub status_message: Option<String>,
}

impl Default for LiveState {
    fn default() -> Self {
        LiveState {
            status: LiveStatus::Running,
            equity: None,
            position: Fixed::ZERO,
            daily_pnl: None,
            as_of_ms: None,
            bars_seen: 0,
            kill_switch: None,
            status_message: None,
        }
    }
}

/// handle 本體。用 enum 而不是 trait object：兩種 handle 的控制面不同
/// （kill switch 只有 testnet 有），一個只有兩個實作、而且介面不一致的
/// trait 不值得（KISS，§4.2）。
pub enum SessionControl {
    Paper(PaperTradingHandle),
    Testnet(TestnetTradingHandle),
    /// 只給 registry 自己的單元測試用，不會出現在真正的 session 裡
    /// （真正的 handle 只能透過 `at_paper_trading::spawn`／
    /// `at_testnet_trading::spawn` 取得，無法在測試裡低成本偽造）。
    #[cfg(test)]
    TestOnlyDummy,
}

/// 一場執行中 session 在 registry 裡的格位。
pub struct SessionEntry {
    pub id: SessionId,
    pub meta: SessionMeta,
    /// 只有該場的轉發執行緒與 stop/kill-switch command 會鎖它。
    control: Mutex<SessionControl>,
    /// 讀多寫少，風控／總覽要高頻讀，所以用 `RwLock`。
    /// **這個鎖絕對不能和 `control` 同時持有**（見模組文件第 2 條）。
    live: RwLock<LiveState>,
    /// 種類專屬的「最後一筆完整快照」，給 `*_trading_status` command 補畫面用
    /// （理由同 5.4/6.5 原本的 `Inner.latest`，現在變成每場一份）。存成
    /// `serde_json::Value` 是刻意選簡單：`LiveState` 是兩種 session 的共同
    /// 子集，不該為了這個次要需求塞進種類專屬欄位；要序列化的 DTO 本來就要
    /// 能轉成 JSON 才送得過 IPC，這裡提早轉一次不算額外負擔。
    last_snapshot: Mutex<Option<serde_json::Value>>,
}

impl SessionEntry {
    pub fn new(id: SessionId, meta: SessionMeta, control: SessionControl) -> Self {
        SessionEntry {
            id,
            meta,
            control: Mutex::new(control),
            live: RwLock::new(LiveState::default()),
            last_snapshot: Mutex::new(None),
        }
    }

    pub fn live_snapshot(&self) -> LiveState {
        self.live
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn update_live<R>(&self, f: impl FnOnce(&mut LiveState) -> R) -> R {
        let mut guard = self
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// 鎖 `control` 並呼叫 `f`；呼叫端不可在 `f` 裡面又去動 `live`
    /// （§4.3 第 2 條不變量）。
    pub fn with_control<R>(&self, f: impl FnOnce(&mut SessionControl) -> R) -> R {
        let mut guard = self
            .control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    pub fn set_last_snapshot(&self, value: &impl Serialize) {
        if let Ok(json) = serde_json::to_value(value) {
            *self
                .last_snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(json);
        }
    }

    pub fn last_snapshot<T: DeserializeOwned>(&self) -> Option<T> {
        let guard = self
            .last_snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .clone()
            .and_then(|json| serde_json::from_value(json).ok())
    }
}

/// Tauri app state：取代原本 `PaperTradingState`／`TestnetTradingState`
/// 各自的 `Mutex<Option<...>>`。
pub struct SessionRegistry {
    sessions: RwLock<HashMap<SessionId, Arc<SessionEntry>>>,
    seq: AtomicU32,
    store: Arc<SessionStore>,
}

impl SessionRegistry {
    pub fn new(store: Arc<SessionStore>) -> Self {
        SessionRegistry {
            sessions: RwLock::new(HashMap::new()),
            seq: AtomicU32::new(0),
            store,
        }
    }

    pub fn store(&self) -> &Arc<SessionStore> {
        &self.store
    }

    /// `format!("{kind}-{started_at_ms}-{seq:03}")`：kind 用 `RunMode` 的
    /// 小寫英文，seq 來自這個 `AtomicU32`，解決同一毫秒開兩場的碰撞
    /// （§4.2）。不引入 uuid/ulid 依賴——一行 `format!` 就夠。
    pub fn generate_id(&self, kind: RunMode, started_at_ms: i64) -> SessionId {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        SessionId(format!("{}-{started_at_ms}-{seq:03}", run_mode_slug(kind)))
    }

    /// 插入一場新 session。超過 [`MAX_CONCURRENT_SESSIONS`] 場執行中 session
    /// 時拒絕（ADR D7）。檢查筆數與插入在同一次寫鎖內完成，避免併發啟動時
    /// 超過上限。
    pub fn insert(&self, entry: Arc<SessionEntry>) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if sessions.len() >= MAX_CONCURRENT_SESSIONS {
            return Err(format!(
                "同時執行的交易已達上限 {MAX_CONCURRENT_SESSIONS} 場，請先停止其中一場"
            ));
        }
        sessions.insert(entry.id.clone(), entry);
        Ok(())
    }

    pub fn get(&self, id: &SessionId) -> Option<Arc<SessionEntry>> {
        let sessions = self
            .sessions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.get(id).cloned()
    }

    /// 轉發執行緒收尾時呼叫，把自己從 registry 移除（模組文件：§7.4 的
    /// 簡化版本）。紀錄已經在這之前寫進 store，不會遺失。
    pub fn remove(&self, id: &SessionId) {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.remove(id);
    }

    pub fn list_live(&self) -> Vec<Arc<SessionEntry>> {
        let sessions = self
            .sessions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.values().cloned().collect()
    }

    /// 跨所有執行中 session 的曝險快照（ADR §8.1）。
    ///
    /// 契約（Phase E 可以依賴）：
    /// 1. 純記憶體讀取，不碰檔案、不碰網路。
    /// 2. 只拿 `RwLock` 的讀鎖，而且全部 clone 出來才回傳。
    /// 3. 絕不呼叫呼叫端提供的任何回呼，從 session 自己的執行緒呼叫不會死鎖。
    /// 4. 每一筆都帶 `as_of_ms`，資料可能過期，呼叫端自己判斷「多舊就不可信」。
    ///
    /// ponytail: Phase B 只要求把介面做出來、回傳正確資料，不接風控邏輯，
    /// 所以目前沒有呼叫端（風控頁顯示用 `list_live_sessions` 就夠，見
    /// ADR §8.2）。Phase E 會從 `at-testnet-trading` 的交易執行緒或一個
    /// 新的風控 command 呼叫它。
    #[allow(dead_code)]
    pub fn aggregate_exposure(&self) -> PortfolioExposure {
        let entries = self.list_live();
        let taken_at_ms = now_ms();
        let sessions = entries
            .iter()
            .map(|entry| {
                let live = entry.live_snapshot();
                SessionExposure {
                    session_id: entry.id.clone(),
                    kind: entry.meta.kind,
                    market: entry.meta.market,
                    symbol: entry.meta.symbol.clone(),
                    position: live.position,
                    equity: live.equity,
                    daily_pnl: live.daily_pnl,
                    kill_switch: live.kill_switch,
                    as_of_ms: live.as_of_ms,
                }
            })
            .collect();
        PortfolioExposure {
            taken_at_ms,
            sessions,
        }
    }
}

/// [`SessionRegistry::aggregate_exposure`] 的回傳：產生快照的時間 + 每場的曝險。
/// ponytail: 跟 `aggregate_exposure` 一樣，Phase E 之前還沒有呼叫端。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PortfolioExposure {
    pub taken_at_ms: i64,
    pub sessions: Vec<SessionExposure>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SessionExposure {
    pub session_id: SessionId,
    pub kind: RunMode,
    pub market: Market,
    pub symbol: String,
    /// 帶正負號的持倉數量（正多負空）。
    pub position: Fixed,
    pub equity: Option<Fixed>,
    pub daily_pnl: Option<Fixed>,
    pub kill_switch: Option<bool>,
    pub as_of_ms: Option<i64>,
}

/// 總覽／TopBar／風控頁收到就重新呼叫 `list_live_sessions`（ADR §7.3）。
pub const SESSION_REGISTRY_CHANGED_EVENT: &str = "session-registry-changed";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionRegistryChangedEvent {
    pub session_id: String,
    /// `"started" | "tick" | "stopped" | "failed"`
    pub change: &'static str,
}

/// 新建一筆 `status: running` 的索引紀錄：`start_*` command 成功後立刻寫一筆，
/// 讓「執行中策略」在 App 當掉時也能被啟動對帳撿到（ADR §9.3）。
pub fn new_running_record(meta: &SessionMeta, id: &SessionId) -> at_session_store::SessionRecord {
    at_session_store::SessionRecord {
        schema_version: at_session_store::CURRENT_SCHEMA_VERSION,
        id: id.as_str().to_string(),
        kind: meta.kind,
        market: meta.market,
        symbol: meta.symbol.clone(),
        interval: meta.interval.clone(),
        strategy_id: meta.strategy_id.clone(),
        strategy_name: meta.strategy_name.clone(),
        params: meta.params.clone(),
        started_at_ms: meta.started_at_ms,
        ended_at_ms: None,
        status: at_session_store::SessionStatus::Running,
        status_message: None,
        starting_capital: meta.starting_capital.to_string(),
        final_equity: None,
        bars_seen: 0,
        metrics: None,
        counters: at_session_store::SessionCounters::default(),
        cost_assumptions: meta.cost_assumptions.clone(),
        saved: false,
        data_source_path: None,
        notes: None,
    }
}

/// 把曲線（如果有）重算成 `SessionMetrics`，收尾寫 store 時共用（跟
/// `at_session_store::SessionStore::reconcile_on_startup` 同一個換算邏輯）。
pub fn metrics_from_curve(
    curve: &[at_core::EquityPoint],
) -> Option<at_session_store::SessionMetrics> {
    if curve.is_empty() {
        return None;
    }
    let metrics = at_core::Metrics::from_curve(curve);
    Some(at_session_store::SessionMetrics {
        total_return: metrics.total_return.map(|v| v.to_string()),
        annualized_return: metrics.annualized_return.map(|v| v.to_string()),
        max_drawdown: Some(metrics.max_drawdown.to_string()),
        sharpe: metrics.sharpe.map(|v| v.to_string()),
        span_years: metrics.span_years.map(|v| v.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_meta(kind: RunMode, started_at_ms: i64) -> SessionMeta {
        SessionMeta {
            kind,
            market: Market::Spot,
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy_id: "sma_cross".to_string(),
            strategy_name: "均線交叉".to_string(),
            params: BTreeMap::new(),
            starting_capital: "10000".parse().unwrap(),
            started_at_ms,
            cost_assumptions: at_session_store::CostAssumptions::default(),
        }
    }

    fn temp_store(name: &str) -> Arc<SessionStore> {
        let dir = std::env::temp_dir().join(format!(
            "at_app_session_registry_test_{}_{name}_{}",
            std::process::id(),
            now_ms()
        ));
        Arc::new(SessionStore::new(dir))
    }

    fn dummy_entry(
        registry: &SessionRegistry,
        kind: RunMode,
        started_at_ms: i64,
    ) -> Arc<SessionEntry> {
        let id = registry.generate_id(kind, started_at_ms);
        let meta = dummy_meta(kind, started_at_ms);
        Arc::new(SessionEntry::new(id, meta, SessionControl::TestOnlyDummy))
    }

    #[test]
    fn generated_ids_are_unique_even_within_the_same_millisecond() {
        let registry = SessionRegistry::new(temp_store("ids"));
        let a = registry.generate_id(RunMode::Paper, 1_000);
        let b = registry.generate_id(RunMode::Paper, 1_000);
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("paper-1000-"));
    }

    #[test]
    fn insert_then_get_round_trips() {
        let registry = SessionRegistry::new(temp_store("roundtrip"));
        let entry = dummy_entry(&registry, RunMode::Paper, 1_000);
        let id = entry.id.clone();
        registry.insert(entry).unwrap();
        assert!(registry.get(&id).is_some());
        assert_eq!(registry.list_live().len(), 1);
    }

    #[test]
    fn remove_takes_it_out_of_list_live() {
        let registry = SessionRegistry::new(temp_store("remove"));
        let entry = dummy_entry(&registry, RunMode::Paper, 1_000);
        let id = entry.id.clone();
        registry.insert(entry).unwrap();
        registry.remove(&id);
        assert!(registry.get(&id).is_none());
        assert!(registry.list_live().is_empty());
    }

    #[test]
    fn insert_rejects_the_21st_concurrent_session() {
        let registry = SessionRegistry::new(temp_store("cap"));
        for i in 0..MAX_CONCURRENT_SESSIONS {
            let entry = dummy_entry(&registry, RunMode::Paper, i as i64);
            registry.insert(entry).unwrap();
        }
        let overflow = dummy_entry(&registry, RunMode::Paper, 999);
        let err = registry.insert(overflow).unwrap_err();
        assert!(err.contains("上限 20 場"), "{err}");
        assert_eq!(registry.list_live().len(), MAX_CONCURRENT_SESSIONS);
    }

    #[test]
    fn aggregate_exposure_reflects_live_state_without_touching_control() {
        let registry = SessionRegistry::new(temp_store("exposure"));
        let entry = dummy_entry(&registry, RunMode::Testnet, 1_000);
        entry.update_live(|live| {
            live.equity = Some("10050".parse().unwrap());
            live.position = "1.5".parse().unwrap();
            live.as_of_ms = Some(2_000);
        });
        registry.insert(entry).unwrap();

        let exposure = registry.aggregate_exposure();
        assert_eq!(exposure.sessions.len(), 1);
        let s = &exposure.sessions[0];
        assert_eq!(s.kind, RunMode::Testnet);
        assert_eq!(s.position, "1.5".parse().unwrap());
        assert_eq!(s.equity, Some("10050".parse().unwrap()));
        assert_eq!(s.as_of_ms, Some(2_000));
    }

    #[test]
    fn last_snapshot_round_trips_through_json() {
        let registry = SessionRegistry::new(temp_store("snapshot"));
        let entry = dummy_entry(&registry, RunMode::Paper, 1_000);

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Dto {
            equity: String,
        }
        entry.set_last_snapshot(&Dto {
            equity: "10000".to_string(),
        });
        let back: Option<Dto> = entry.last_snapshot();
        assert_eq!(
            back,
            Some(Dto {
                equity: "10000".to_string()
            })
        );
    }

    #[test]
    fn new_running_record_has_no_end_time_and_is_not_saved() {
        let meta = dummy_meta(RunMode::Paper, 1_000);
        let id = SessionId::from_raw("paper-1000-001");
        let record = new_running_record(&meta, &id);
        assert_eq!(record.status, at_session_store::SessionStatus::Running);
        assert!(record.ended_at_ms.is_none());
        assert!(!record.saved);
        assert_eq!(record.id, "paper-1000-001");
    }
}
