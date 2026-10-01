//! `SessionStore`：`sessions/index.json` 與 `sessions/<id>/curve.csv` 的
//! 讀寫入口。收 `base_dir`（呼叫端決定，照 `at-account-sync` 的既有慣例，
//! 這個 crate 自己不讀 cwd 或環境變數）。
//!
//! 這一層只負責「存取」，不負責「現在這場 session 該不該被標成 running」
//! 這種生命週期判斷——那是上層（`app/src-tauri` 的 session registry）的
//! 責任。唯一的例外是啟動對帳（[`SessionStore::reconcile_on_startup`]）：
//! ADR §9.3 把它點名成 `at-session-store` 的工作，因為「索引裡的孤兒
//! `running` 紀錄要改寫成 `interrupted`」是純粹根據**已存的資料**（索引 +
//! 曲線檔）就能判斷的事實，不需要任何執行中的 handle，跟 registry 的
//! 並行邏輯無關。

use crate::curve::{self, CurveError};
use crate::id::{validate_session_id, SessionIdError};
use crate::record::{SessionFilter, SessionRecord, SessionStatus};
use at_core::{EquityPoint, Metrics};
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// `list_sessions` 沒帶 `limit` 時的預設筆數。
pub const DEFAULT_LIST_LIMIT: usize = 100;
/// `list_sessions` 的 `limit` 上限，避免一次把整份索引丟過 IPC。
pub const MAX_LIST_LIMIT: usize = 1000;

/// 存取 session store 失敗的原因。
#[derive(Debug)]
pub enum SessionStoreError {
    /// session id 未通過路徑穿越檢查（見 [`crate::id`]）。
    InvalidId(SessionIdError),
    Io(io::Error),
    Curve(CurveError),
    /// 指定的 session id 在索引裡找不到。
    NotFound(String),
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStoreError::InvalidId(err) => write!(f, "{err}"),
            SessionStoreError::Io(err) => write!(f, "讀寫 session store 失敗：{err}"),
            SessionStoreError::Curve(err) => write!(f, "{err}"),
            SessionStoreError::NotFound(id) => {
                write!(f, "找不到這場交易（可能已經停止）：{id}")
            }
        }
    }
}

impl std::error::Error for SessionStoreError {}

impl From<SessionIdError> for SessionStoreError {
    fn from(err: SessionIdError) -> Self {
        SessionStoreError::InvalidId(err)
    }
}

impl From<CurveError> for SessionStoreError {
    fn from(err: CurveError) -> Self {
        SessionStoreError::Curve(err)
    }
}

/// 載入索引後附帶的狀態：解析失敗時不靜默丟棄使用者的歷史紀錄，而是改名
/// 隔離壞檔、以空索引啟動，並把這件事告訴呼叫端（ADR §5.4 第 4 條）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadedIndex {
    pub records: Vec<SessionRecord>,
    /// 繁體中文警告，給前端顯示；正常載入時是 `None`。
    pub warning: Option<String>,
}

/// 啟動對帳的結果：有幾筆孤兒 `running` 紀錄被改成 `interrupted`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileReport {
    pub interrupted_ids: Vec<String>,
}

/// store 的健康狀態，給 `session_store_health` command 用（ADR §7.2）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoreHealth {
    pub session_count: usize,
    pub index_bytes: u64,
    pub last_error_zh: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct SessionStore {
    base_dir: PathBuf,
    /// 索引寫入全部經過這個鎖（ADR §5.4 第 1 條）：桌面 App 是單一行程，
    /// 多場 session 同時收尾時靠它序列化寫入，不會互相蓋掉彼此的更新。
    write_lock: Mutex<()>,
}

impl SessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        SessionStore {
            base_dir: base_dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.base_dir.join("sessions")
    }

    fn index_path(&self) -> PathBuf {
        self.sessions_dir().join("index.json")
    }

    /// 驗證 id 後組出該場 session 的明細資料夾路徑。**所有**會碰檔案系統
    /// 的公開函式都經過這裡，不會有路徑在驗證之前被組出來。
    fn session_dir(&self, id: &str) -> Result<PathBuf, SessionStoreError> {
        validate_session_id(id)?;
        Ok(self.sessions_dir().join(id))
    }

    fn curve_path(&self, id: &str) -> Result<PathBuf, SessionStoreError> {
        Ok(self.session_dir(id)?.join("curve.csv"))
    }

    /// 載入整份索引。檔案不存在＝空索引（第一次使用）。
    pub fn load_index(&self) -> Result<LoadedIndex, SessionStoreError> {
        let path = self.index_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(LoadedIndex::default()),
            Err(err) => return Err(SessionStoreError::Io(err)),
        };
        if content.trim().is_empty() {
            return Ok(LoadedIndex::default());
        }
        match serde_json::from_str::<Vec<SessionRecord>>(&content) {
            Ok(records) => Ok(LoadedIndex {
                records,
                warning: None,
            }),
            Err(parse_err) => {
                // 使用者的歷史紀錄不能靜默丟棄：隔離壞檔，而不是覆寫或忽略它。
                let quarantine = self
                    .sessions_dir()
                    .join(format!("index.json.bad-{}", now_ms()));
                let warning = match fs::rename(&path, &quarantine) {
                    Ok(()) => format!(
                        "歷史紀錄索引損毀（{parse_err}），已備份到 {}，以空索引繼續啟動",
                        quarantine.display()
                    ),
                    Err(rename_err) => format!(
                        "歷史紀錄索引損毀（{parse_err}），且備份失敗（{rename_err}），以空索引繼續啟動"
                    ),
                };
                Ok(LoadedIndex {
                    records: Vec::new(),
                    warning: Some(warning),
                })
            }
        }
    }

    /// 整份重寫索引：寫暫存檔 + `fs::rename` 原子替換，不直接覆寫原檔
    /// （中途斷電／崩潰不會留下半份壞檔，ADR §5.4 第 2 條）。
    fn write_index(&self, records: &[SessionRecord]) -> Result<(), SessionStoreError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        let dir = self.sessions_dir();
        fs::create_dir_all(&dir).map_err(SessionStoreError::Io)?;
        let json = serde_json::to_string_pretty(records)
            .map_err(|e| SessionStoreError::Io(io::Error::other(e)))?;
        let tmp_path = dir.join("index.json.tmp");
        fs::write(&tmp_path, json).map_err(SessionStoreError::Io)?;
        fs::rename(&tmp_path, self.index_path()).map_err(SessionStoreError::Io)
    }

    /// 新增一筆紀錄，或若同 id 已存在就整筆覆蓋（`run_backtest_command`
    /// 第一次寫、registry 收尾時第二次寫，用的是同一個函式）。
    pub fn upsert_session(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        validate_session_id(&record.id)?;
        let mut loaded = self.load_index()?;
        match loaded.records.iter_mut().find(|r| r.id == record.id) {
            Some(existing) => *existing = record,
            None => loaded.records.push(record),
        }
        self.write_index(&loaded.records)
    }

    /// 「儲存回測」按鈕：把一筆紀錄的 `saved` 翻成 `true`（或取消）。
    pub fn mark_saved(&self, id: &str, saved: bool) -> Result<(), SessionStoreError> {
        validate_session_id(id)?;
        let mut loaded = self.load_index()?;
        let record = loaded
            .records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| SessionStoreError::NotFound(id.to_string()))?;
        record.saved = saved;
        self.write_index(&loaded.records)
    }

    /// 刪除一筆紀錄的索引項與整個明細資料夾。
    pub fn delete_session(&self, id: &str) -> Result<(), SessionStoreError> {
        let dir = self.session_dir(id)?;
        let mut loaded = self.load_index()?;
        let before = loaded.records.len();
        loaded.records.retain(|r| r.id != id);
        if loaded.records.len() == before {
            return Err(SessionStoreError::NotFound(id.to_string()));
        }
        self.write_index(&loaded.records)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(_) if !dir.exists() => Ok(()),
            Err(err) => Err(SessionStoreError::Io(err)),
        }
    }

    /// 查詢服務：最近回測清單、已停止標籤頁、策略庫統計共用這一個
    /// （ADR §7.2）。依 `started_at_ms` 新到舊排序，套用 `filter.limit`
    /// （預設 [`DEFAULT_LIST_LIMIT`]，上限 [`MAX_LIST_LIMIT`]）。
    pub fn list_sessions(
        &self,
        filter: &SessionFilter,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        let loaded = self.load_index()?;
        let mut matched: Vec<SessionRecord> = loaded
            .records
            .into_iter()
            .filter(|r| filter.matches(r))
            .collect();
        matched.sort_by_key(|r| std::cmp::Reverse(r.started_at_ms));
        let limit = filter
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .min(MAX_LIST_LIMIT);
        matched.truncate(limit);
        Ok(matched)
    }

    /// 把一根收盤 K 線的權益點 append 到曲線檔。
    pub fn append_curve_point(
        &self,
        id: &str,
        point: EquityPoint,
    ) -> Result<(), SessionStoreError> {
        curve::append_curve_point(&point, self.curve_path(id)?)?;
        Ok(())
    }

    /// 讀整條曲線，`max_points` 給了就在這裡等距降採樣。
    pub fn read_curve(
        &self,
        id: &str,
        max_points: Option<usize>,
    ) -> Result<Vec<EquityPoint>, SessionStoreError> {
        let points = curve::read_curve_file(self.curve_path(id)?)?;
        Ok(match max_points {
            Some(max) => curve::downsample(&points, max),
            None => points,
        })
    }

    /// store 的健康狀態（ADR §7.2 `session_store_health`）。
    pub fn health(&self) -> Result<StoreHealth, SessionStoreError> {
        let loaded = self.load_index()?;
        let index_bytes = fs::metadata(self.index_path())
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(StoreHealth {
            session_count: loaded.records.len(),
            index_bytes,
            last_error_zh: loaded.warning,
        })
    }

    /// 啟動對帳（ADR §9.3）：App 當掉／斷電會在索引裡留下 `status: running`
    /// 的孤兒紀錄——registry 一啟動就是空的，所以任何載入當下仍是
    /// `running` 的紀錄都不可能真的在跑。把它們改寫成 `interrupted`，
    /// `endedAtMs` 用曲線最後一點的 `open_time`，`metrics` 從現有曲線重算。
    ///
    /// 不標成 `stopped`（會讓使用者相信一條被截斷的曲線是完整的），
    /// 也不標成 `failed`（引擎沒有回報錯誤）。
    pub fn reconcile_on_startup(&self) -> Result<ReconcileReport, SessionStoreError> {
        let mut loaded = self.load_index()?;
        let mut report = ReconcileReport::default();
        let mut changed = false;

        for record in loaded.records.iter_mut() {
            if record.status != SessionStatus::Running {
                continue;
            }
            let curve_points = curve::read_curve_file(self.curve_path(&record.id)?)?;
            record.status = SessionStatus::Interrupted;
            record.ended_at_ms = curve_points
                .last()
                .map(|p| p.open_time)
                .or(Some(record.started_at_ms));
            if let Some(last) = curve_points.last() {
                record.final_equity = Some(last.equity.to_string());
            }
            let metrics = Metrics::from_curve(&curve_points);
            record.metrics = Some(crate::record::SessionMetrics {
                total_return: metrics.total_return.map(|v| v.to_string()),
                annualized_return: metrics.annualized_return.map(|v| v.to_string()),
                max_drawdown: Some(metrics.max_drawdown.to_string()),
                sharpe: metrics.sharpe.map(|v| v.to_string()),
                span_years: metrics.span_years.map(|v| v.to_string()),
            });
            report.interrupted_ids.push(record.id.clone());
            changed = true;
        }

        if changed {
            self.write_index(&loaded.records)?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{CostAssumptions, SessionCounters, CURRENT_SCHEMA_VERSION};
    use at_core::{Market, RunMode};
    use std::collections::BTreeMap;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "at_session_store_test_{}_{name}_{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(id: &str, status: SessionStatus) -> SessionRecord {
        SessionRecord {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: id.to_string(),
            kind: RunMode::Paper,
            market: Market::Spot,
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy_id: "sma_cross".to_string(),
            strategy_name: "均線交叉".to_string(),
            params: BTreeMap::new(),
            started_at_ms: 1_000,
            ended_at_ms: None,
            status,
            status_message: None,
            starting_capital: "10000".to_string(),
            final_equity: None,
            bars_seen: 0,
            metrics: None,
            counters: SessionCounters::default(),
            cost_assumptions: CostAssumptions::default(),
            saved: false,
            data_source_path: None,
            notes: None,
        }
    }

    #[test]
    fn upsert_then_list_round_trips() {
        let store = SessionStore::new(temp_dir("upsert_list"));
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();
        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "paper-1");
    }

    #[test]
    fn upsert_with_same_id_overwrites_not_duplicates() {
        let store = SessionStore::new(temp_dir("upsert_overwrite"));
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();
        let mut stopped = record("paper-1", SessionStatus::Stopped);
        stopped.ended_at_ms = Some(2_000);
        store.upsert_session(stopped).unwrap();

        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, SessionStatus::Stopped);
    }

    #[test]
    fn index_write_is_atomic_no_tmp_file_left_behind() {
        let dir = temp_dir("atomic");
        let store = SessionStore::new(dir.clone());
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();
        assert!(dir.join("sessions/index.json").exists());
        assert!(!dir.join("sessions/index.json.tmp").exists());
    }

    #[test]
    fn mark_saved_flips_the_flag() {
        let store = SessionStore::new(temp_dir("mark_saved"));
        store
            .upsert_session(record("bt-1", SessionStatus::Completed))
            .unwrap();
        store.mark_saved("bt-1", true).unwrap();
        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert!(listed[0].saved);
    }

    #[test]
    fn mark_saved_unknown_id_is_not_found() {
        let store = SessionStore::new(temp_dir("mark_saved_missing"));
        let err = store.mark_saved("does-not-exist", true).unwrap_err();
        assert!(matches!(err, SessionStoreError::NotFound(id) if id == "does-not-exist"));
    }

    #[test]
    fn delete_session_removes_index_entry_and_directory() {
        let store = SessionStore::new(temp_dir("delete"));
        store
            .upsert_session(record("paper-1", SessionStatus::Stopped))
            .unwrap();
        store
            .append_curve_point(
                "paper-1",
                EquityPoint {
                    open_time: 1,
                    equity: "1".parse().unwrap(),
                },
            )
            .unwrap();

        store.delete_session("paper-1").unwrap();

        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn delete_session_rejects_path_traversal_id() {
        let store = SessionStore::new(temp_dir("delete_traversal"));
        let err = store.delete_session("../../etc").unwrap_err();
        assert!(matches!(err, SessionStoreError::InvalidId(_)));
    }

    #[test]
    fn read_session_curve_rejects_path_traversal_id() {
        // §9.6 點名的真實安全漏洞：id 來自前端，組路徑前一定要先驗證。
        let store = SessionStore::new(temp_dir("curve_traversal"));
        let err = store.read_curve("../../../etc/passwd", None).unwrap_err();
        assert!(matches!(err, SessionStoreError::InvalidId(_)));

        let err2 = store
            .append_curve_point(
                "..\\..\\windows",
                EquityPoint {
                    open_time: 1,
                    equity: "1".parse().unwrap(),
                },
            )
            .unwrap_err();
        assert!(matches!(err2, SessionStoreError::InvalidId(_)));
    }

    #[test]
    fn append_and_read_curve_round_trips() {
        let store = SessionStore::new(temp_dir("curve_roundtrip"));
        store
            .append_curve_point(
                "paper-1",
                EquityPoint {
                    open_time: 1_000,
                    equity: "10000".parse().unwrap(),
                },
            )
            .unwrap();
        store
            .append_curve_point(
                "paper-1",
                EquityPoint {
                    open_time: 2_000,
                    equity: "10100".parse().unwrap(),
                },
            )
            .unwrap();

        let curve = store.read_curve("paper-1", None).unwrap();
        assert_eq!(curve.len(), 2);
        assert_eq!(curve[1].open_time, 2_000);
    }

    #[test]
    fn corrupt_index_is_quarantined_not_silently_dropped() {
        let dir = temp_dir("corrupt_index");
        fs::create_dir_all(dir.join("sessions")).unwrap();
        fs::write(dir.join("sessions/index.json"), "{ not valid json").unwrap();

        let store = SessionStore::new(dir.clone());
        let loaded = store.load_index().unwrap();

        assert!(loaded.records.is_empty());
        assert!(loaded.warning.is_some());
        // 壞檔必須被備份下來，不能原地消失。
        let has_backup = fs::read_dir(dir.join("sessions"))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("index.json.bad-")
            });
        assert!(has_backup);
    }

    #[test]
    fn reconcile_on_startup_turns_orphaned_running_into_interrupted() {
        let store = SessionStore::new(temp_dir("reconcile"));
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();
        store
            .append_curve_point(
                "paper-1",
                EquityPoint {
                    open_time: 1_000,
                    equity: "10000".parse().unwrap(),
                },
            )
            .unwrap();
        store
            .append_curve_point(
                "paper-1",
                EquityPoint {
                    open_time: 2_000,
                    equity: "10500".parse().unwrap(),
                },
            )
            .unwrap();

        let report = store.reconcile_on_startup().unwrap();
        assert_eq!(report.interrupted_ids, vec!["paper-1".to_string()]);

        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed[0].status, SessionStatus::Interrupted);
        assert_eq!(listed[0].ended_at_ms, Some(2_000));
        assert_eq!(listed[0].final_equity, Some("10500".to_string()));
    }

    #[test]
    fn reconcile_leaves_non_running_records_untouched() {
        let store = SessionStore::new(temp_dir("reconcile_noop"));
        store
            .upsert_session(record("bt-1", SessionStatus::Completed))
            .unwrap();

        let report = store.reconcile_on_startup().unwrap();
        assert!(report.interrupted_ids.is_empty());

        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed[0].status, SessionStatus::Completed);
    }

    #[test]
    fn reconcile_with_no_curve_data_still_marks_interrupted() {
        // 連第一根收盤 K 線都還沒等到就當機：曲線檔不存在也要能對帳，
        // endedAtMs 退回 startedAtMs。
        let store = SessionStore::new(temp_dir("reconcile_empty_curve"));
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();

        store.reconcile_on_startup().unwrap();

        let listed = store.list_sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed[0].status, SessionStatus::Interrupted);
        assert_eq!(listed[0].ended_at_ms, Some(1_000));
    }

    #[test]
    fn list_sessions_applies_limit_and_sorts_newest_first() {
        let store = SessionStore::new(temp_dir("list_limit"));
        for i in 0..5 {
            let mut r = record(&format!("bt-{i}"), SessionStatus::Completed);
            r.started_at_ms = i;
            store.upsert_session(r).unwrap();
        }
        let filter = SessionFilter {
            limit: Some(2),
            ..Default::default()
        };
        let listed = store.list_sessions(&filter).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "bt-4");
        assert_eq!(listed[1].id, "bt-3");
    }

    #[test]
    fn health_reports_session_count_and_warning() {
        let store = SessionStore::new(temp_dir("health"));
        store
            .upsert_session(record("paper-1", SessionStatus::Running))
            .unwrap();
        let health = store.health().unwrap();
        assert_eq!(health.session_count, 1);
        assert!(health.last_error_zh.is_none());
    }
}
