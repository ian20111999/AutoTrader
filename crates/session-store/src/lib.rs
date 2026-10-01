//! Session 歷史持久化：`sessions/index.json`（全量載入的索引）+
//! `sessions/<id>/curve.csv`（權益曲線）。
//!
//! 對應 `docs/architecture/2026-10-01-session-registry.md`（ADR-001）的
//! 問題二、方案 P3。這個 crate 不碰 Tauri、不讀 cwd／環境變數——`base_dir`
//! 一律由呼叫端傳入（照 `at-account-sync` 的既有慣例），桌面 App 會傳
//! `app.path().app_data_dir()`。
//!
//! `fills.jsonl`（成交明細）在 ADR 裡是選配、留給 Phase D，這個 crate
//! 目前沒有實作它的讀寫。

pub mod curve;
pub mod id;
pub mod record;
mod store;

pub use curve::{append_curve_point, downsample, format_curve, parse_curve, CurveError};
pub use id::{validate_session_id, SessionIdError};
pub use record::{
    CostAssumptions, SessionCounters, SessionFilter, SessionMetrics, SessionRecord, SessionStatus,
    CURRENT_SCHEMA_VERSION,
};
pub use store::{
    LoadedIndex, ReconcileReport, SessionStore, SessionStoreError, StoreHealth, DEFAULT_LIST_LIMIT,
    MAX_LIST_LIMIT,
};
