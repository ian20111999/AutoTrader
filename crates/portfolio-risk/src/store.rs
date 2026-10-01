//! 觸發紀錄的持久化：`<base_dir>/risk_events.json`（ADR-002 第 8.4 節）。
//!
//! # 為什麼是 JSON、為什麼是環形上限
//!
//! 需求是一個「只會附加一筆、全部讀出來顯示」的清單，上限 [`MAX_RISK_EVENTS`] 筆。
//! 整檔重寫 500 筆 JSON 的成本可以忽略，所以不引入資料庫（那是框架級依賴），
//! 也不用 JSON Lines（環形上限要刪最舊的，JSON Lines 得另外做截斷邏輯，
//! 重寫一個 `Vec` 更短）。慣例照 `at-account-sync`：`serde_json` + `to_string_pretty`。
//!
//! # `base_dir` 由呼叫端決定
//!
//! 和 `at_account_sync::cache_path`、`at_downloader::local_path` 同一個慣例：
//! 這個 crate 不讀環境變數、不用相對路徑。相對路徑會跟著行程的工作目錄跑，
//! 同一份紀錄在不同啟動方式下會寫到不同地方。
//!
//! # 這是整個 crate 唯一碰 `std::fs` 的地方
//!
//! [`crate::breaker`] 的評估邏輯仍然零 I/O、零時鐘，所以每一條規則都能用純單元測試
//! 驗到邊界。ADR 8.4 原本把寫檔放在 App 層；這裡改成 crate 自己管，理由見 crate 文件。
//! **何時**寫仍然是 App 層的決定（ADR：每產生一則就立刻寫，不要等關閉才寫）。

use crate::event::RiskEvent;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 紀錄的環形上限。超過就丟最舊的。
pub const MAX_RISK_EVENTS: usize = 500;

/// 紀錄檔檔名。
const FILE_NAME: &str = "risk_events.json";

/// 解析失敗的檔案會被改名成這個，而不是直接被覆蓋掉。
const BROKEN_FILE_NAME: &str = "risk_events.broken.json";

/// 觸發紀錄的清單，含環形上限與序號指派。
///
/// JSON 形狀就是 [`RiskEvent`] 的陣列（`next_seq` 由最大的 `seq` 推回來，不另外存一欄
/// 會和內容對不起來的數字）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<RiskEvent>", into = "Vec<RiskEvent>")]
pub struct RiskEventLog {
    events: Vec<RiskEvent>,
    next_seq: u64,
}

impl RiskEventLog {
    /// 空的紀錄清單。
    pub fn new() -> RiskEventLog {
        RiskEventLog::default()
    }

    /// 由舊到新的紀錄。
    pub fn events(&self) -> &[RiskEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 下一筆會拿到的序號。
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// 附加一則紀錄，回傳指派給它的序號。
    ///
    /// 序號由這裡指派（傳進來的 `seq` 會被覆蓋），所以呼叫端不可能兩則用同一個號。
    /// 超過 [`MAX_RISK_EVENTS`] 時丟掉最舊的——序號不會回頭，所以丟掉的紀錄在序號上
    /// 看得出缺口。
    pub fn push(&mut self, event: RiskEvent) -> u64 {
        let seq = self.next_seq;
        self.events.push(RiskEvent { seq, ..event });
        // `saturating_add`：u64 用完要先送出 1.8×10^19 則紀錄，真的到了就停在最大值，
        // 排序仍然正確（新的不會小於舊的），總比繞回 0 讓順序整個錯掉好。
        self.next_seq = self.next_seq.saturating_add(1);
        trim(&mut self.events);
        seq
    }
}

/// 只留最後 [`MAX_RISK_EVENTS`] 筆。
fn trim(events: &mut Vec<RiskEvent>) {
    if let Some(excess) = events.len().checked_sub(MAX_RISK_EVENTS) {
        events.drain(..excess);
    }
}

impl From<Vec<RiskEvent>> for RiskEventLog {
    /// 反序列化的入口：順便套用環形上限，並把序號接續到最大值之後。
    ///
    /// 用「最大值 + 1」而不是「最後一筆 + 1」：手改過的檔案可能順序被打亂，
    /// 接在最大值之後才保證不會發出重複的序號。
    fn from(mut events: Vec<RiskEvent>) -> RiskEventLog {
        trim(&mut events);
        let next_seq = events
            .iter()
            .map(|event| event.seq)
            .max()
            .map_or(0, |max| max.saturating_add(1));
        RiskEventLog { events, next_seq }
    }
}

impl From<RiskEventLog> for Vec<RiskEvent> {
    fn from(log: RiskEventLog) -> Vec<RiskEvent> {
        log.events
    }
}

/// 紀錄檔路徑：`<base_dir>/risk_events.json`。
pub fn log_path(base_dir: impl AsRef<Path>) -> PathBuf {
    base_dir.as_ref().join(FILE_NAME)
}

/// 讀紀錄檔。檔案不存在、讀不到、內容壞掉，一律回傳空的清單。
///
/// 內容壞掉時會先盡量把原檔改名成 `risk_events.broken.json` 再回空的——紀錄是查問題
/// 用的證據，直接讓下一次寫入覆蓋掉它比留著更糟。改名失敗就算了（best-effort），
/// 不會因此讓讀取失敗。
pub fn read_log(base_dir: impl AsRef<Path>) -> RiskEventLog {
    let path = log_path(&base_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return RiskEventLog::new();
    };
    match serde_json::from_str(&content) {
        Ok(log) => log,
        Err(_) => {
            let _ = fs::rename(&path, base_dir.as_ref().join(BROKEN_FILE_NAME));
            RiskEventLog::new()
        }
    }
}

/// 寫紀錄檔（整檔重寫）。需要時會建出 `base_dir`。
///
/// 回傳 `Err` 而不是吞掉：寫不進去代表「對帳不一致」這種紀錄沒有落地，App 層應該
/// 有機會在畫面上說一聲。要當成 best-effort 的呼叫端自己 `let _ = ...`。
pub fn write_log(base_dir: impl AsRef<Path>, log: &RiskEventLog) -> io::Result<()> {
    let dir = base_dir.as_ref();
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(log).map_err(io::Error::other)?;
    fs::write(log_path(dir), json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker::{BreakerAction, DEFAULT_CONSECUTIVE_LOSSES};
    use crate::event::{ClockSource, RiskEventCause};
    use at_core::Symbol;
    use std::env;

    /// 每個測試用自己的目錄，平行跑測試才不會互相覆寫。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "at_portfolio_risk_test_{}_{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn kill_switch(on: bool) -> RiskEvent {
        RiskEvent::new(
            1_790_000_000_000,
            ClockSource::Local,
            RiskEventCause::KillSwitch { on },
        )
    }

    fn trip() -> RiskEvent {
        RiskEvent {
            session: Some("session-1".to_string()),
            symbol: Some(Symbol::new("BTCUSDT").unwrap()),
            ..RiskEvent::new(
                1_790_000_001_000,
                ClockSource::Exchange,
                RiskEventCause::BreakerTrip {
                    trigger: DEFAULT_CONSECUTIVE_LOSSES,
                    action: BreakerAction::PauseStrategy,
                },
            )
        }
    }

    #[test]
    fn the_file_sits_in_the_given_directory() {
        assert_eq!(
            log_path("data").display().to_string(),
            format!("data{}risk_events.json", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn push_assigns_monotonic_sequence_numbers() {
        let mut log = RiskEventLog::new();
        assert_eq!(log.push(kill_switch(true)), 0);
        assert_eq!(log.push(trip()), 1);
        // 呼叫端自己填的 seq 不算：序號由紀錄簿指派。
        let forged = RiskEvent {
            seq: 999,
            ..kill_switch(false)
        };
        assert_eq!(log.push(forged), 2);
        assert_eq!(
            log.events().iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn the_ring_keeps_the_newest_entries_and_the_sequence_shows_the_gap() {
        let mut log = RiskEventLog::new();
        for _ in 0..MAX_RISK_EVENTS + 10 {
            log.push(kill_switch(true));
        }
        assert_eq!(log.len(), MAX_RISK_EVENTS);
        // 最舊的 10 筆被丟掉，序號不回頭，所以缺口看得出來。
        assert_eq!(log.events()[0].seq, 10);
        assert_eq!(log.events()[MAX_RISK_EVENTS - 1].seq, 509);
        assert_eq!(log.next_seq(), 510);
    }

    #[test]
    fn events_round_trip_through_the_file() {
        let dir = temp_dir("round_trip");
        let mut log = RiskEventLog::new();
        log.push(kill_switch(true));
        log.push(trip());

        write_log(&dir, &log).unwrap();
        let read = read_log(&dir);
        assert_eq!(read, log);
        assert_eq!(
            read.events()[1].symbol.as_ref().unwrap().as_str(),
            "BTCUSDT"
        );
        // 讀回來之後繼續附加，序號接得上。
        let mut read = read;
        assert_eq!(read.push(kill_switch(false)), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_the_array_of_events() {
        let dir = temp_dir("shape");
        let mut log = RiskEventLog::new();
        log.push(trip());
        write_log(&dir, &log).unwrap();

        let content = fs::read_to_string(log_path(&dir)).unwrap();
        assert!(content.trim_start().starts_with('['), "{content}");
        let as_vec: Vec<RiskEvent> = serde_json::from_str(&content).unwrap();
        assert_eq!(as_vec.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_log() {
        let dir = temp_dir("missing");
        let log = read_log(&dir);
        assert!(log.is_empty());
        assert_eq!(log.next_seq(), 0);
    }

    #[test]
    fn a_broken_file_is_set_aside_instead_of_being_overwritten() {
        let dir = temp_dir("broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(log_path(&dir), "{ 這不是紀錄 ").unwrap();

        assert!(read_log(&dir).is_empty());
        // 原本的內容被留在旁邊，不是直接消失。
        let kept = fs::read_to_string(dir.join("risk_events.broken.json")).unwrap();
        assert_eq!(kept, "{ 這不是紀錄 ");
        assert!(!log_path(&dir).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_edited_file_over_the_limit_is_trimmed_on_read() {
        let dir = temp_dir("oversized");
        let events: Vec<RiskEvent> = (0..MAX_RISK_EVENTS + 5)
            .map(|i| RiskEvent {
                seq: i as u64,
                ..kill_switch(true)
            })
            .collect();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            log_path(&dir),
            serde_json::to_string_pretty(&events).unwrap(),
        )
        .unwrap();

        let log = read_log(&dir);
        assert_eq!(log.len(), MAX_RISK_EVENTS);
        assert_eq!(log.events()[0].seq, 5);
        assert_eq!(log.next_seq(), MAX_RISK_EVENTS as u64 + 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shuffled_file_does_not_hand_out_a_duplicate_sequence_number() {
        let dir = temp_dir("shuffled");
        let events = vec![
            RiskEvent {
                seq: 7,
                ..kill_switch(true)
            },
            RiskEvent {
                seq: 3,
                ..kill_switch(false)
            },
        ];
        fs::create_dir_all(&dir).unwrap();
        fs::write(log_path(&dir), serde_json::to_string(&events).unwrap()).unwrap();

        let mut log = read_log(&dir);
        assert_eq!(log.push(trip()), 8);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_creates_the_directory() {
        let base = temp_dir("nested");
        let dir = base.join("a").join("b");
        write_log(&dir, &RiskEventLog::new()).unwrap();
        assert!(log_path(&dir).exists());

        let _ = fs::remove_dir_all(&base);
    }
}
