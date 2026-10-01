//! 全域風控引擎（ADR-002 `docs/architecture/2026-10-01-portfolio-risk-engine.md`）。
//!
//! 這一版只有**熔斷規則的評估**與**觸發紀錄的持久化**，對應 ADR 9.3 落地順序的
//! 第 2 步與第 4 步的紀錄部分。
//!
//! # 現在有什麼
//!
//! - [`breaker`]：五條熔斷規則（條件／啟用／動作三者分離）、一個觀測結構、
//!   一個評估點。強制開啟的規則在型別與建構子兩層都關不掉。
//! - [`breaker::consecutive_losses`]：用「回到空手」當交易邊界，從權益序列數出
//!   連續虧損筆數——不動已審查過的帳本結構（ADR 5.5 選項 a）。
//! - [`event`]：觸發紀錄的型別。存結構化資料，顯示文字在讀取時才產生。
//! - [`store`]：`<base_dir>/risk_events.json`，環形上限 [`store::MAX_RISK_EVENTS`] 筆。
//!
//! # 現在刻意**沒有**什麼（都是後續的獨立步驟，不是忘了）
//!
//! - `GlobalLimits` / `GlobalBlocked` / `PortfolioGate`（ADR 第 4、8.1 節，落地第 1、3 步）。
//! - 跟 `at-testnet-trading::Trader` 的實際串接（第 3 步）。那會動到已審查過的
//!   money-critical crate，要走跨模型審查。
//! - 跨 session 的曝險加總（`aggregate_exposure`，ADR 8.2／8.3）。它依賴還不存在的
//!   session registry，Phase B 完成後才有東西可加總。
//! - App 層的 `EmergencyAction`（一鍵停止的兩種行為，ADR 第 6 節）。
//!
//! # 兩個延續下來的原則
//!
//! **判不出來就擋（fail closed）。** 和 `at-risk-control`（6.3）一字不改的精神：
//! 必要的觀測值是 `None`、數字自相矛盾、算術溢位，一律當成熔斷觸發。
//!
//! **不讀系統時鐘。** `now_ms` 由呼叫端傳入，所以測試可以餵任意假時間。
//! [`breaker`] 連 `std::fs` 都不碰；唯一碰檔案的是 [`store`]，而它的 `base_dir`
//! 由呼叫端給（和 `at_account_sync::cache_path` 同一個慣例）。
//!
//! ADR 8.4 原本把寫檔也放在 App 層。這裡改成由 [`store`] 自己管，理由：
//! 「觸發紀錄持久化」如果只回傳 `Vec<RiskEvent>`，那環形上限、序號指派、
//! 檔案格式、壞檔處理就全部落在 App 層，變成 `cargo test` 驗不到的程式碼；
//! 而 `at-account-sync` 早就立下「crate 自己管一個小 JSON 檔、`base_dir` 參數化」
//! 的慣例。**何時**寫仍然是 App 層的決定，評估邏輯仍然零 I/O。
//!
//! # 用起來像這樣
//!
//! ```
//! use at_core::Fixed;
//! use at_portfolio_risk::breaker::{
//!     consecutive_losses, BreakerObservation, BreakerRules, EquitySample, OrderCounts,
//!     Reconciliation,
//! };
//! use at_portfolio_risk::event::{ClockSource, RiskEvent, RiskEventCause};
//! use at_portfolio_risk::store::RiskEventLog;
//!
//! // 設計稿的五條規則，強制規則一定在裡面。
//! let rules = BreakerRules::default();
//!
//! // 權益序列：1000 →（建倉）→ 回到空手 980，虧一筆。
//! let samples = [
//!     EquitySample { at_ms: 1, position: Fixed::ZERO, equity: Fixed::from_int(1_000) },
//!     EquitySample { at_ms: 2, position: Fixed::ONE, equity: Fixed::from_int(990) },
//!     EquitySample { at_ms: 3, position: Fixed::ZERO, equity: Fixed::from_int(980) },
//! ];
//!
//! // 從「什麼都不知道」開始填，漏填的那一項會擋而不是放行。
//! let observation = BreakerObservation {
//!     last_market_event_ms: Some(2_900),
//!     consecutive_losses: consecutive_losses(&samples),
//!     recent_orders: OrderCounts { sent: 10, rejected: 0 },
//!     drawdown: Some(Fixed::ZERO),
//!     reconciliation: Reconciliation::Ok,
//!     ..BreakerObservation::unknown(3_000)
//! };
//!
//! // 一筆虧損還沒到 5 筆，行情也沒中斷 → 沒有任何規則觸發。
//! let trips = rules.evaluate(&observation);
//! assert!(trips.is_empty());
//!
//! // 真的觸發時，把它原樣記成一則紀錄（顯示文字讀取時才產生）。
//! let mut log = RiskEventLog::new();
//! for trip in trips {
//!     log.push(RiskEvent::new(
//!         observation.now_ms,
//!         ClockSource::Exchange,
//!         RiskEventCause::BreakerTrip { trigger: trip.trigger, action: trip.action },
//!     ));
//! }
//! ```

pub mod breaker;
pub mod event;
mod serde_at;
pub mod store;

pub use breaker::{
    consecutive_losses, BreakerAction, BreakerObservation, BreakerRule, BreakerRules,
    BreakerSwitch, BreakerTrigger, BreakerTrip, EquitySample, OrderCounts, Reconciliation,
    MANDATORY_RULES,
};
pub use event::{ClockSource, RiskEvent, RiskEventCause};
pub use store::{log_path, read_log, write_log, RiskEventLog, MAX_RISK_EVENTS};
