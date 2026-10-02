//! 風控頁（設計稿 Risk）＋把 `at-portfolio-risk` 的熔斷器接進測試網送單路徑。
//!
//! 對應 ADR-002（`docs/architecture/2026-10-01-portfolio-risk-engine.md`）落地順序的
//! 第 3 步（接進 `send_gated_order`）、第 4 步（設定與觸發紀錄的讀寫）與第 5 步
//! （風控頁）。引擎那一半（規則評估、紀錄的環形上限與檔案格式）已經在
//! `at-portfolio-risk` 裡，**這裡一行判斷邏輯都不重寫**，只做三件事：
//!
//! 1. **累加器**：把 [`at_testnet_trading::TestnetUpdate`] 事件流算成一個
//!    [`BreakerObservation`]（ADR 5.4：累加器在 App 層，因為事件流在這裡）。
//! 2. **閘門**：實作 [`PortfolioGate`]，由送單路徑在每一筆單之前呼叫。
//! 3. **持久化**：`<app_data_dir>/risk_settings.json`（規則開關）與
//!    `risk_events.json`（觸發紀錄，檔案格式由 `at_portfolio_risk::store` 管）。
//!
//! # 判不出來就擋（這個檔案唯一不能妥協的規則）
//!
//! 每一條「讀不到／算不出來」的路徑都回
//! [`GlobalBlocked::RiskStateUnavailable`]，不是 `Ok(())`：
//!
//! - 規則讀不到（鎖被 panic 毒化）→ 擋。
//! - 累加器讀不到（同上）→ 擋。
//! - 觀測值有洞（還沒收到行情心跳、權益算不出來、回撤算不出來）→ 交給
//!   [`BreakerRules::evaluate`]，而它對 `None` 的答案就是觸發 → 擋。
//!
//! 刻意**不**用專案其他地方常見的 `unwrap_or_else(|p| p.into_inner())`：那是
//! 「毒化了也繼續用」的活性取捨，用在風控狀態上等於「風控壞了就當它放行」。
//!
//! # 觸發是黏著的（sticky），而且不提供「解除」按鈕
//!
//! ADR 5.6：觸發狀態只有人工清除。這一版把「人工清除」定義成**停止這場交易並
//! 重新啟動**（新的 session 有新的累加器），而不是做一顆「解除熔斷」的按鈕——
//! 那顆按鈕會變成出事時最容易被連點兩下的東西。對帳不一致（強制規則）的動作是
//! 「暫停整個帳戶的下單」，它黏在 [`RiskShared::account_pause`] 上，連重開 session
//! 都不會消失，要重啟 App（人工確認過交易所部位之後）。
//!
//! # 這一版刻意沒有做的事（都在 ADR 裡，不是忘了）
//!
//! - **全域上限**（單一幣種／總部位／價格偏離／下單頻率／嚴格模式）：
//!   `GlobalLimits` 還沒落地，而且前三項需要「可信價」與跨 session 曝險的
//!   即時加總。風控頁把它們顯示成「尚未生效」而不是給一個存了也沒人讀的表單
//!   （ADR 13.2：UI 讓使用者以為自己受保護，比任何技術風險都嚴重）。
//! - **合約風控**（槓桿／保證金模式／強平距離／資金費預算／交易所端停損／
//!   套利淨曝險）：下單路徑是現貨市價單，沒有任何查保證金率、強平價的能力
//!   （ADR 第 7 節）。風控頁只說明「需要合約交易（第 7 步之後）」，不顯示假數字。
//! - **一鍵停止的兩種行為**（`EmergencyAction`）：ADR 第 6 節，另一個步驟。
//!   測試網交易頁已經有該場的一鍵停止與停止兩顆按鈕。

use crate::session_registry::SessionRegistry;
use at_core::{Fixed, Symbol};
use at_portfolio_risk::breaker::DEFAULT_REJECT_RATE;
use at_portfolio_risk::{
    consecutive_losses, BreakerAction, BreakerObservation, BreakerRule, BreakerRules,
    BreakerSwitch, BreakerTrigger, BreakerTrip, ClockSource, EquitySample, GlobalBlocked,
    OrderCounts, PortfolioGate, Reconciliation, RiskEvent, RiskEventCause, RiskEventLog,
};
use at_testnet_trading::{OrderOutcome, TradingError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

/// 風控設定檔名（跟 `risk_events.json` 同一個目錄）。
const SETTINGS_FILE: &str = "risk_settings.json";

/// 權益取樣的保留上限。
///
/// `consecutive_losses` 每次都重算整個序列，而序列只用來數「最近連續虧損幾筆」，
/// 留太久的歷史沒有用。1 分鐘 K 線跑滿 24 小時是 1440 根，留 2000 根夠一天多，
/// 而且每根只有三個數字，記憶體可以忽略。
const MAX_EQUITY_SAMPLES: usize = 2_000;

/// 一筆真的送出去過的單，給「拒絕率」的時間窗用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrderRecord {
    /// 送出當下的交易所時間（閘門放行的那一刻）。
    at_ms: i64,
    /// 之後交易所有沒有回報它被拒絕。
    rejected: bool,
}

/// 一場 session 的熔斷累加器。
///
/// 每一個欄位都對應 [`BreakerObservation`] 的一項，而每一個「不知道」都用
/// `Option`／`Reconciliation::Unknown` 表達，讓規則自己決定要不要擋。
#[derive(Debug)]
struct Accumulator {
    /// 最後一則行情事件的交易所時間（含未收盤 K 線與 ticker）。
    last_market_event_ms: Option<i64>,
    /// 權益序列，`consecutive_losses` 的輸入。
    samples: Vec<EquitySample>,
    /// 權益高點，回撤的分母。
    peak_equity: Option<Fixed>,
    /// 送出過的單（時間窗內的留著），拒絕率的輸入。
    orders: VecDeque<OrderRecord>,
    /// 對帳結果，見 [`Accumulator::new`] 對「一開始為什麼是 `Ok`」的說明。
    reconciliation: Reconciliation,
    /// 已經觸發過的熔斷（黏著）。
    tripped: Option<BreakerTrip>,
}

impl Accumulator {
    /// 用這場 session 的起始資金開一個累加器。
    ///
    /// # 為什麼要先放一個「起始權益」樣本
    ///
    /// 不放的話，第一筆單送出之前權益序列是空的 → 回撤算不出來（`None`）→
    /// 回撤規則觸發 → **每一場交易的第一張單都會被擋**，而且黏著之後這場就廢了。
    /// 那不是 fail closed，那是壞掉。本地帳本的起點是確定的：現金 =
    /// `initial_cash`、部位 = 0、權益 = 現金。先放這個已知的事實進去，之後每一根
    /// 收盤 K 線再補一個真的樣本。
    ///
    /// `at_ms` 填 0 而不是本機時鐘：[`EquitySample::at_ms`] 不參與任何判斷
    /// （只用來排序與除錯），而把本機時間混進一條交易所時間的序列裡，之後查問題
    /// 會被自己的資料誤導。
    ///
    /// # 為什麼 `reconciliation` 一開始是 `Ok` 而不是 `Unknown`
    ///
    /// 這是本檔最需要被挑戰的一個判斷，所以寫清楚：`Unknown` 和 `Mismatch` 一樣擋
    /// （而且強制規則關不掉），所以如果一開始填 `Unknown`，這個 App 一張單都送不
    /// 出去。ADR 12.1 已經指出這條強制規則目前沒有完整的輸入，並建議窄化成
    /// 「把『本地帳本可能和交易所不一致』的已知訊號當成不一致」。
    ///
    /// 在那個窄化定義下，`Ok` 是一句**真話**而不是樂觀假設：這場 session 的本地帳本
    /// 從「現金 = 起始資金、部位 = 0」開始，一張單都還沒送出去，和交易所之間沒有
    /// 任何可能分岔的狀態。之後只要出現 [`TradingError`] 的任何一種
    /// （送單結果不明／等不到終態／等回報時被停止／帳本算不出來），就永久轉成
    /// `Mismatch`（見 [`SessionBreaker::record_failure`]）。
    ///
    /// **誠實的限制**：真正的「查交易所部位比對本地帳本」需要一個查部位的端點，
    /// `OrderGateway` 沒有。所以這條規則今天抓得到的只有上面那四種訊號，風控頁
    /// 必須照實寫出來，不能讓使用者以為有人在幫他對帳。
    fn new(initial_cash: Fixed) -> Accumulator {
        Accumulator {
            last_market_event_ms: None,
            samples: vec![EquitySample {
                at_ms: 0,
                position: Fixed::ZERO,
                equity: Some(initial_cash),
            }],
            peak_equity: Some(initial_cash),
            orders: VecDeque::new(),
            reconciliation: Reconciliation::Ok,
            tripped: None,
        }
    }

    /// 相對權益高點的回撤比例（正數）。算不出來就是 `None`（規則會擋）。
    fn drawdown(&self) -> Option<Fixed> {
        let peak = self.peak_equity?;
        let equity = self.samples.last()?.equity?;
        // 高點不是正數時除不出有意義的比例（權益歸零也走這條）：不猜。
        if peak <= Fixed::ZERO {
            return None;
        }
        let fall = peak.checked_sub(equity)?;
        if fall.is_negative() {
            // 創新高。理論上 peak 已經跟上了，這裡只是不讓回撤變成負數。
            return Some(Fixed::ZERO);
        }
        fall.checked_div(peak)
    }

    /// 時間窗內的送出／被拒絕筆數，順手把過期的紀錄丟掉。
    fn order_counts(&mut self, now_ms: i64, window_ms: i64) -> OrderCounts {
        // `saturating_sub`：窗長與時間都是正常值時不會用到，用它只是不讓一個
        // 極端的時間戳變成 panic。
        let cutoff = now_ms.saturating_sub(window_ms);
        while self.orders.front().is_some_and(|o| o.at_ms < cutoff) {
            self.orders.pop_front();
        }
        OrderCounts {
            sent: self.orders.len() as u32,
            rejected: self.orders.iter().filter(|o| o.rejected).count() as u32,
        }
    }

    /// 組一次觀測。從 [`BreakerObservation::unknown`] 補起，所以之後引擎多一個
    /// 欄位時，漏填的那一項會讓規則觸發（擋下），不會悄悄變成放行的值。
    fn observe(
        &mut self,
        now_ms: i64,
        last_event_ms: Option<i64>,
        window_ms: i64,
    ) -> BreakerObservation {
        let drawdown = self.drawdown();
        let consecutive = consecutive_losses(&self.samples);
        BreakerObservation {
            last_market_event_ms: last_event_ms,
            consecutive_losses: consecutive,
            recent_orders: self.order_counts(now_ms, window_ms),
            drawdown,
            reconciliation: self.reconciliation,
            ..BreakerObservation::unknown(now_ms)
        }
    }
}

/// 權益序列的修剪：**只能切在「空手」那一刻**。
///
/// 這不是美觀問題，是正確性問題。`consecutive_losses` 用「回到空手」當交易邊界，
/// 序列開頭就已經有部位時它會回 `None`（起點權益不明 → 擋）。如果修剪時隨便砍掉
/// 最舊的 N 筆，剛好砍在「抱著部位」的中間，連續虧損筆數就變成「不明」→ 熔斷觸發
/// → **一場跑了一天多的交易會莫名其妙被自己的修剪邏輯停掉**。
///
/// 所以超過上限時，往後找到第一個 `position == 0` 的樣本再切；找不到（整段都抱著
/// 部位）就先不修剪——那種情況下序列會繼續長，但那是正確性換來的，而且一直不平倉
/// 的策略本來就不會累積「筆數」。
fn trim_samples(samples: &mut Vec<EquitySample>) {
    let Some(excess) = samples.len().checked_sub(MAX_EQUITY_SAMPLES) else {
        return;
    };
    if excess == 0 {
        return;
    }
    let Some(cut) = samples
        .iter()
        .enumerate()
        .skip(excess)
        .find(|(_, sample)| sample.position.is_zero())
        .map(|(index, _)| index)
    else {
        return;
    };
    samples.drain(..cut);
}

/// 全 App 共用的那一份風控狀態：規則、觸發紀錄、帳戶層暫停。
#[derive(Debug)]
struct RiskShared {
    base_dir: PathBuf,
    rules: Mutex<BreakerRules>,
    log: Mutex<RiskEventLog>,
    /// `Some(trip)` = 整個帳戶的下單已被熔斷暫停（[`BreakerAction::PauseAllOrders`]）。
    /// 黏著，而且跨 session：重開一場不會解除，要重啟 App。
    account_pause: Mutex<Option<BreakerTrip>>,
}

impl RiskShared {
    /// 規則的快照。讀不到（鎖毒化）回 `Err`，呼叫端一律擋單。
    fn rules_snapshot(&self) -> Result<BreakerRules, ()> {
        self.rules.lock().map(|r| r.clone()).map_err(|_| ())
    }

    fn account_pause(&self) -> Result<Option<BreakerTrip>, ()> {
        self.account_pause.lock().map(|p| *p).map_err(|_| ())
    }

    /// 拒絕率規則的時間窗長度。
    ///
    /// 從規則本身讀，不寫死（`at_portfolio_risk::BreakerTrigger::RejectRate` 的文件
    /// 明確要求這件事：設定調了窗長而累加器沒跟上，拒絕率就不是設定裡那個意思）。
    fn reject_window_ms(rules: &BreakerRules) -> i64 {
        rules
            .rules()
            .iter()
            .find_map(|rule| match rule.trigger {
                BreakerTrigger::RejectRate { window_ms, .. } => Some(window_ms),
                _ => None,
            })
            .unwrap_or(match DEFAULT_REJECT_RATE {
                BreakerTrigger::RejectRate { window_ms, .. } => window_ms,
                _ => 60_000,
            })
    }

    /// 記一筆觸發紀錄並**立刻**寫檔（ADR 8.4：不要等關閉才寫）。
    ///
    /// 寫不進去只能 best-effort 回報到 stderr：這個函式是在「已經決定要擋單」之後
    /// 才被呼叫的，紀錄寫不進去不可以反過來讓那筆單放行。
    fn record(&self, event: RiskEvent) {
        let Ok(mut log) = self.log.lock() else {
            eprintln!("風控觸發紀錄的鎖已毒化，這一筆沒有寫入：{}", event.cause);
            return;
        };
        log.push(event);
        if let Err(e) = at_portfolio_risk::write_log(&self.base_dir, &log) {
            eprintln!("寫入風控觸發紀錄失敗：{e}");
        }
    }
}

/// 一場 session 的熔斷閘門。送單路徑拿到的就是它（當成 [`PortfolioGate`]）。
#[derive(Debug)]
pub struct SessionBreaker {
    shared: Arc<RiskShared>,
    session: String,
    symbol: Symbol,
    state: Mutex<Accumulator>,
}

impl SessionBreaker {
    /// 評估一次並處理觸發，回傳「現在要不要擋」。
    ///
    /// 三個呼叫端（行情心跳、送單前檢查、事件流餵完之後重新評估）都走這裡，所以
    /// 「規則怎麼判」仍然只有 [`BreakerRules::evaluate`] 一個地方。
    fn evaluate(&self, now_ms: i64, last_event_ms: Option<i64>) -> Result<(), GlobalBlocked> {
        let rules = self
            .shared
            .rules_snapshot()
            .map_err(|()| GlobalBlocked::RiskStateUnavailable)?;
        let window_ms = RiskShared::reject_window_ms(&rules);

        let trips = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| GlobalBlocked::RiskStateUnavailable)?;
            // 已經觸發過就不再重複評估也不再重複記錄：第一次觸發的那一則紀錄才是
            // 證據，之後每一根 K 線各記一筆只會把紀錄沖掉（環形上限 500 筆）。
            if let Some(trip) = state.tripped {
                return Err(GlobalBlocked::BreakerTripped(trip));
            }
            let observation = state.observe(now_ms, last_event_ms, window_ms);
            let trips = rules.evaluate(&observation);
            if let Some(first) = trips.first() {
                state.tripped = Some(*first);
            }
            trips
        };

        let Some(first) = trips.first().copied() else {
            return Ok(());
        };
        // 鎖已經放掉才寫檔：記錄要碰檔案系統，不可以讓它卡在送單路徑的鎖裡面。
        for trip in &trips {
            self.shared.record(RiskEvent {
                session: Some(self.session.clone()),
                symbol: Some(self.symbol.clone()),
                ..RiskEvent::new(
                    now_ms,
                    ClockSource::Exchange,
                    RiskEventCause::BreakerTrip {
                        trigger: trip.trigger,
                        action: trip.action,
                    },
                )
            });
            if trip.action == BreakerAction::PauseAllOrders {
                match self.shared.account_pause.lock() {
                    Ok(mut pause) => *pause = Some(*trip),
                    // 暫停旗標寫不進去：那就讓每一次 check 都因為讀不到而擋單
                    // （`account_pause()` 對毒化的鎖也回 `Err`）。
                    Err(_) => eprintln!("帳戶層暫停旗標的鎖已毒化，之後一律擋單"),
                }
            }
        }
        Err(GlobalBlocked::BreakerTripped(first))
    }

    /// 狀態變了（收到新的快照／下單結果／錯誤）之後重新評估一次。
    ///
    /// 用「最後一則行情事件的時間」當 `now_ms`，不自己讀本機時鐘：和心跳同一個
    /// 時鐘來源，否則算出來的中斷時間是錯的（而且可能算出負數）。
    ///
    /// 還沒收到過任何行情時直接跳過：這時候沒有一個誠實的「現在」可以用，而且
    /// 這條路徑不送單——真的要送單時 [`Self::check`] 會因為
    /// `last_market_event_ms == None` 而擋下。
    fn reevaluate(&self) {
        let last = match self.state.lock() {
            Ok(state) => state.last_market_event_ms,
            // 讀不到就不評估：送單前的 `check` 會因為同一個毒化的鎖擋單。
            Err(_) => return,
        };
        let Some(now_ms) = last else {
            return;
        };
        let _ = self.evaluate(now_ms, last);
    }

    /// 一根收盤 K 線處理完的帳本狀態（[`at_testnet_trading::TestnetUpdate::Bar`]）。
    pub fn record_bar(&self, open_time_ms: i64, position: Fixed, equity: Fixed) {
        if let Ok(mut state) = self.state.lock() {
            state.samples.push(EquitySample {
                at_ms: open_time_ms,
                position,
                equity: Some(equity),
            });
            trim_samples(&mut state.samples);
            let higher = state.peak_equity.is_none_or(|peak| equity > peak);
            if higher {
                state.peak_equity = Some(equity);
            }
        }
        self.reevaluate();
    }

    /// 一根 K 線的下單結果（[`at_testnet_trading::TestnetUpdate::Order`]）。
    ///
    /// 只有「真的送出去而且交易所回了拒絕」才算拒絕；被閘門擋下的單根本沒送出去，
    /// 不能算進拒絕率（否則熔斷會自己餵自己：擋一筆 → 拒絕率升高 → 再擋更多）。
    pub fn record_order_outcome(&self, outcome: &OrderOutcome) {
        let rejected = match outcome {
            OrderOutcome::Filled(report) => {
                report.status == at_binance::testnet::OrderStatus::Rejected
            }
            OrderOutcome::Blocked(_)
            | OrderOutcome::GloballyBlocked(_)
            | OrderOutcome::Invalid(_) => return,
        };
        if rejected {
            if let Ok(mut state) = self.state.lock() {
                // 這一刻最近送出的那一筆就是被拒絕的那一筆：同一場 session 一次只有
                // 一張在途的單（送出之後要等到終態才處理下一根 K 線）。
                if let Some(last) = state.orders.back_mut() {
                    last.rejected = true;
                }
            }
        }
        self.reevaluate();
    }

    /// 交易迴圈回報的致命錯誤（[`at_testnet_trading::TestnetUpdate::Failed`]）。
    ///
    /// 四種 [`TradingError`] 全部代表「本地帳本從這一刻起不可信」，所以一律轉成
    /// 對帳不一致 → 強制規則觸發 → **暫停整個帳戶的下單**。這是今天唯一真的會
    /// 讓那條強制規則觸發的輸入（ADR 12.1 的窄化版本）。
    pub fn record_failure(&self, error: &TradingError) {
        let _ = error;
        if let Ok(mut state) = self.state.lock() {
            state.reconciliation = Reconciliation::Mismatch;
        }
        self.reevaluate();
    }
}

impl PortfolioGate for SessionBreaker {
    fn observe_market_event(&self, at_ms: i64) {
        // 心跳的評估用「上一則事件」當基準，才抓得到剛剛那段空白；更新成最新時間
        // 之後再評估就永遠是 0 毫秒，中斷規則會變成裝飾品。
        let previous = match self.state.lock() {
            Ok(mut state) => {
                let previous = state.last_market_event_ms;
                state.last_market_event_ms = Some(previous.map_or(at_ms, |p| p.max(at_ms)));
                previous
            }
            Err(_) => return,
        };
        // **第一則**心跳沒有「上一則」可以比，基準用它自己（間隔 0）。
        // 這不是放寬 fail-closed：它說的是一句真話——這一刻剛收到行情，連線是活的。
        // 真正該擋的情況是「從來沒收到過行情就要送單」，那由 [`Self::check`] 擋
        // （它看到的 `last_market_event_ms` 是 `None`，行情中斷規則會觸發）。
        let base = previous.unwrap_or(at_ms);
        // 這裡刻意忽略結果：真的觸發了，黏著狀態已經記下來，下一次送單前的
        // `check` 會擋。心跳本身不送單，沒有東西可以擋。
        let _ = self.evaluate(at_ms, Some(base));
    }

    fn check(&self, now_ms: i64) -> Result<(), GlobalBlocked> {
        // 帳戶層的暫停先看：它是別場 session 觸發的強制規則留下的。
        match self.shared.account_pause() {
            Err(()) => return Err(GlobalBlocked::RiskStateUnavailable),
            Ok(Some(trip)) => return Err(GlobalBlocked::BreakerTripped(trip)),
            Ok(None) => {}
        }
        let last = match self.state.lock() {
            Ok(state) => state.last_market_event_ms,
            Err(_) => return Err(GlobalBlocked::RiskStateUnavailable),
        };
        self.evaluate(now_ms, last)?;
        // 放行 = 這筆單馬上就會被送出去，所以「送出筆數」記在這裡：送單路徑之外
        // 沒有任何地方知道「這一刻真的要送一張單」。
        if let Ok(mut state) = self.state.lock() {
            state.orders.push_back(OrderRecord {
                at_ms: now_ms,
                rejected: false,
            });
        }
        Ok(())
    }
}

/// Tauri app state：風控設定、觸發紀錄，以及每一場 session 的熔斷累加器。
#[derive(Debug)]
pub struct RiskControlState {
    shared: Arc<RiskShared>,
    /// key 是 `SessionId` 的字串。只在「開一場／查一場／收一場」時短暫鎖住，
    /// 絕不跨檔案 I/O 或 `emit`（跟 `session_registry` 的鎖規則同一條）。
    sessions: Mutex<HashMap<String, Arc<SessionBreaker>>>,
}

impl RiskControlState {
    /// 從 `base_dir` 讀設定與觸發紀錄。
    ///
    /// 設定檔讀不到／解析失敗時用**全部規則都開著**的預設值，不是「當成沒有限制」
    /// （ADR 8.5）。
    pub fn new(base_dir: impl Into<PathBuf>) -> RiskControlState {
        let base_dir = base_dir.into();
        let settings = read_settings(&base_dir);
        let log = at_portfolio_risk::read_log(&base_dir);
        RiskControlState {
            shared: Arc::new(RiskShared {
                base_dir,
                rules: Mutex::new(settings.breaker_rules),
                log: Mutex::new(log),
                account_pause: Mutex::new(None),
            }),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 開一場 session 的累加器，回傳要注入送單路徑的閘門。
    pub fn open_session(
        &self,
        session_id: &str,
        symbol: Symbol,
        initial_cash: Fixed,
    ) -> Arc<SessionBreaker> {
        let breaker = Arc::new(SessionBreaker {
            shared: Arc::clone(&self.shared),
            session: session_id.to_string(),
            symbol,
            state: Mutex::new(Accumulator::new(initial_cash)),
        });
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session_id.to_string(), Arc::clone(&breaker));
        }
        breaker
    }

    /// 這場 session 的閘門（轉發執行緒用它餵事件流）。
    pub fn session(&self, session_id: &str) -> Option<Arc<SessionBreaker>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(session_id).cloned())
    }

    /// session 收尾時移除累加器。
    ///
    /// 帳戶層的暫停**不會**跟著消失（那是跨 session 的狀態）。
    pub fn close_session(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    fn rules_dto(&self) -> Result<Vec<BreakerRuleDto>, String> {
        let rules = self
            .shared
            .rules_snapshot()
            .map_err(|()| "風控規則讀不到（內部狀態已毀損），請重新啟動 App".to_string())?;
        Ok(rules
            .rules()
            .iter()
            .map(BreakerRuleDto::from_rule)
            .collect())
    }

    /// 開關一條規則並寫回設定檔。強制規則回錯誤（而不是默默忽略）。
    fn set_rule(&self, trigger_id: &str, enabled: bool) -> Result<Vec<BreakerRuleDto>, String> {
        let mut guard = self
            .shared
            .rules
            .lock()
            .map_err(|_| "風控規則讀不到（內部狀態已毀損），請重新啟動 App".to_string())?;
        let current: Vec<BreakerRule> = guard.rules().to_vec();
        let target = current
            .iter()
            .find(|rule| trigger_id_of(rule.trigger) == trigger_id)
            .ok_or_else(|| format!("沒有這條熔斷規則：{trigger_id}"))?;
        if target.switch == BreakerSwitch::Mandatory {
            return Err(format!("「{}」是強制開啟的規則，不能關閉", target.trigger));
        }
        // 重建整組規則：`BreakerRules::new` 會把強制規則補回去，所以就算這份清單
        // 被改壞了，拿回來的規則集裡強制規則還在。
        let next: Vec<BreakerRule> = current
            .into_iter()
            .map(|rule| {
                if trigger_id_of(rule.trigger) == trigger_id {
                    BreakerRule {
                        switch: BreakerSwitch::Optional(enabled),
                        ..rule
                    }
                } else {
                    rule
                }
            })
            .collect();
        *guard = BreakerRules::new(next);
        let saved = guard.clone();
        drop(guard);

        if let Err(e) = write_settings(&self.shared.base_dir, &saved) {
            return Err(format!("風控設定寫入失敗：{e}"));
        }
        Ok(saved
            .rules()
            .iter()
            .map(BreakerRuleDto::from_rule)
            .collect())
    }

    fn events_dto(&self, limit: usize) -> Result<Vec<RiskEventDto>, String> {
        let log = self
            .shared
            .log
            .lock()
            .map_err(|_| "觸發紀錄讀不到（內部狀態已毀損），請重新啟動 App".to_string())?;
        let mut events: Vec<RiskEventDto> = log
            .events()
            .iter()
            .rev()
            .take(limit)
            .map(RiskEventDto::from)
            .collect();
        events.sort_by_key(|event| std::cmp::Reverse(event.seq));
        Ok(events)
    }

    fn status_dto(&self, registry: &SessionRegistry) -> Result<RiskStatusDto, String> {
        let account_pause = self
            .shared
            .account_pause()
            .map_err(|()| "風控狀態讀不到（內部狀態已毀損），請重新啟動 App".to_string())?;
        let exposure = registry.aggregate_exposure();
        let sessions = exposure
            .sessions
            .iter()
            .map(|s| {
                let breaker_trip = self
                    .session(s.session_id.as_str())
                    .and_then(|breaker| breaker.state.lock().ok().and_then(|st| st.tripped))
                    .map(|trip| trip.to_string());
                SessionExposureDto {
                    session_id: s.session_id.as_str().to_string(),
                    kind: format!("{:?}", s.kind),
                    symbol: s.symbol.clone(),
                    position: s.position.to_string(),
                    equity: s.equity.map(|v| v.to_string()),
                    daily_pnl: s.daily_pnl.map(|v| v.to_string()),
                    kill_switch: s.kill_switch,
                    as_of_ms: s.as_of_ms,
                    breaker_trip,
                }
            })
            .collect();
        Ok(RiskStatusDto {
            taken_at_ms: exposure.taken_at_ms,
            account_pause: account_pause.map(|trip| trip.to_string()),
            sessions,
        })
    }
}

/// 風控設定檔的內容。
///
/// 只有熔斷規則：全域上限（`GlobalLimits`）還沒落地，先寫一個「存了沒人讀」的
/// 欄位進去，等於把 ADR 13.2 擔心的事做出來。`#[serde(default)]` 讓之後補欄位時
/// 舊檔案照樣讀得起來。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RiskSettings {
    breaker_rules: BreakerRules,
}

fn settings_path(base_dir: impl AsRef<Path>) -> PathBuf {
    base_dir.as_ref().join(SETTINGS_FILE)
}

/// 讀設定檔。讀不到／壞掉一律回預設值（四條可選規則全部開著）。
fn read_settings(base_dir: impl AsRef<Path>) -> RiskSettings {
    let path = settings_path(&base_dir);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return RiskSettings::default();
    };
    match serde_json::from_str::<RiskSettings>(&content) {
        Ok(settings) => settings,
        Err(e) => {
            // 不可以「解析失敗就當成沒有限制」（ADR 8.5）。壞檔留在原地讓使用者
            // 自己看，程式用保守的預設值跑。
            eprintln!("風控設定檔解析失敗（改用預設值：所有熔斷規則開啟）：{e}");
            RiskSettings::default()
        }
    }
}

fn write_settings(base_dir: impl AsRef<Path>, rules: &BreakerRules) -> std::io::Result<()> {
    let dir = base_dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let settings = RiskSettings {
        breaker_rules: rules.clone(),
    };
    let json = serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?;
    std::fs::write(settings_path(dir), json)
}

/// 規則的穩定識別字串，前端用它開關規則（不要用顯示文字當 key）。
fn trigger_id_of(trigger: BreakerTrigger) -> &'static str {
    match trigger {
        BreakerTrigger::ConsecutiveLosses { .. } => "consecutiveLosses",
        BreakerTrigger::MarketStalled { .. } => "marketStalled",
        BreakerTrigger::RejectRate { .. } => "rejectRate",
        BreakerTrigger::ReconciliationMismatch => "reconciliationMismatch",
        BreakerTrigger::StrategyDrawdown { .. } => "strategyDrawdown",
    }
}

/// 這條規則今天的輸入是什麼、有什麼限制。**UI 必須照實顯示**（ADR 13.2）。
fn trigger_note(trigger: BreakerTrigger) -> &'static str {
    match trigger {
        BreakerTrigger::ConsecutiveLosses { .. } => {
            "一「筆」= 一次從空手到空手的來回（用權益差算，含手續費）。一直抱著部位不平倉的策略不會累積筆數。"
        }
        BreakerTrigger::MarketStalled { .. } => {
            "心跳來源是即時行情的每一則事件（含未收盤 K 線與 ticker），不是收盤 K 線。中斷過就不會自動恢復。"
        }
        BreakerTrigger::RejectRate { .. } => {
            "只算真的送到交易所、而且被交易所拒絕的單；被閘門擋下的單不算（否則熔斷會自己餵自己）。"
        }
        BreakerTrigger::ReconciliationMismatch => {
            "強制開啟。目前的輸入只有「送單結果不明／等不到終態／等回報時被停止／帳本算不出來」這四種訊號；這個專案還沒有「查交易所部位比對本地帳本」的能力。觸發後會暫停整個帳戶的下單，確認交易所實際部位並重新啟動 App 才會解除。"
        }
        BreakerTrigger::StrategyDrawdown { .. } => {
            "相對這場 session 自己的權益高點（起點是你輸入的起始資金）。"
        }
    }
}

/// 一條熔斷規則給前端看的樣子。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BreakerRuleDto {
    /// 穩定識別字串（開關時傳回來的那個）。
    pub id: String,
    /// 條件，例如「連續虧損 5 筆」。
    pub condition: String,
    /// 觸發後的動作，例如「暫停這個策略」。
    pub action: String,
    pub enabled: bool,
    /// 強制開啟（UI 不可以給關閉的控制元件）。
    pub mandatory: bool,
    /// 這條規則的輸入與限制，直接顯示給使用者。
    pub note: String,
}

impl BreakerRuleDto {
    fn from_rule(rule: &BreakerRule) -> BreakerRuleDto {
        BreakerRuleDto {
            id: trigger_id_of(rule.trigger).to_string(),
            condition: rule.trigger.to_string(),
            action: rule.action.to_string(),
            enabled: rule.switch.is_armed(),
            mandatory: rule.switch == BreakerSwitch::Mandatory,
            note: trigger_note(rule.trigger).to_string(),
        }
    }
}

/// 一則觸發紀錄給前端看的樣子。顯示文字在這裡才產生（紀錄本身存結構化資料）。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RiskEventDto {
    pub seq: u64,
    pub at_ms: i64,
    /// 「交易所時間」或「本機時間」——混在一起而不說，之後查問題會被誤導。
    pub clock: String,
    pub session: Option<String>,
    pub symbol: Option<String>,
    pub message: String,
}

impl From<&RiskEvent> for RiskEventDto {
    fn from(event: &RiskEvent) -> RiskEventDto {
        RiskEventDto {
            seq: event.seq,
            at_ms: event.at_ms,
            clock: event.clock.to_string(),
            session: event.session.clone(),
            symbol: event.symbol.as_ref().map(|s| s.as_str().to_string()),
            message: event.cause.to_string(),
        }
    }
}

/// 風控頁的即時狀態：帳戶層暫停 + 每一場執行中 session 的曝險與熔斷狀態。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RiskStatusDto {
    pub taken_at_ms: i64,
    /// `Some` = 整個帳戶的下單已被暫停，內容是觸發的那條規則。
    pub account_pause: Option<String>,
    pub sessions: Vec<SessionExposureDto>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionExposureDto {
    pub session_id: String,
    pub kind: String,
    pub symbol: String,
    pub position: String,
    pub equity: Option<String>,
    pub daily_pnl: Option<String>,
    pub kill_switch: Option<bool>,
    /// 這一筆資料有多舊，由前端自己判斷「多舊就不可信」（ADR-001 §8.1 的契約）。
    pub as_of_ms: Option<i64>,
    /// `Some` = 這場 session 已經被熔斷擋住送單。
    pub breaker_trip: Option<String>,
}

/// 五條熔斷規則目前的開關狀態。
#[tauri::command]
pub fn get_breaker_rules(app: AppHandle) -> Result<Vec<BreakerRuleDto>, String> {
    app.state::<RiskControlState>().rules_dto()
}

/// 開關一條熔斷規則（強制開啟的那條會回錯誤）。回傳更新後的整組規則。
#[tauri::command]
pub fn set_breaker_rule(
    app: AppHandle,
    trigger_id: String,
    enabled: bool,
) -> Result<Vec<BreakerRuleDto>, String> {
    app.state::<RiskControlState>()
        .set_rule(&trigger_id, enabled)
}

/// 觸發紀錄（新的在前）。
#[tauri::command]
pub fn list_risk_events(app: AppHandle, limit: Option<usize>) -> Result<Vec<RiskEventDto>, String> {
    app.state::<RiskControlState>()
        .events_dto(limit.unwrap_or(50).min(at_portfolio_risk::MAX_RISK_EVENTS))
}

/// 風控頁的即時狀態（帳戶層暫停 + 跨 session 曝險）。
#[tauri::command]
pub fn risk_control_status(app: AppHandle) -> Result<RiskStatusDto, String> {
    let registry = app.state::<SessionRegistry>();
    app.state::<RiskControlState>().status_dto(&registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_binance::testnet::OrderStatus;
    use at_core::Side;
    use at_portfolio_risk::breaker::{
        DEFAULT_CONSECUTIVE_LOSSES, DEFAULT_MARKET_STALLED, DEFAULT_STRATEGY_DRAWDOWN,
    };
    use at_testnet_trading::FillReport;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "at_app_risk_control_test_{}_{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn state(name: &str) -> (RiskControlState, PathBuf) {
        let dir = temp_dir(name);
        (RiskControlState::new(dir.clone()), dir)
    }

    fn breaker(state: &RiskControlState, id: &str) -> Arc<SessionBreaker> {
        state.open_session(id, Symbol::new("BTCUSDT").unwrap(), fx("10000"))
    }

    /// 2024-10-04T00:00:00Z
    const T0: i64 = 20_000 * 86_400_000;

    /// 模擬真實迴圈的一根 K 線：先餵心跳（迴圈收到行情事件就餵），再餵快照。
    ///
    /// 心跳間隔刻意用 1 秒：Binance 的 K 線串流每秒左右就推一次更新（含未收盤的
    /// 那一根），所以 3 秒的中斷門檻在真實環境不會因為「K 線週期是 1 分鐘」而
    /// 誤觸發。測試照這個節奏走，才不會驗到一個不存在的情境。
    fn tick_bar(breaker: &SessionBreaker, at_ms: i64, position: Fixed, equity: i64) {
        breaker.observe_market_event(at_ms);
        breaker.record_bar(at_ms, position, Fixed::from_int(equity).unwrap());
    }

    fn rejected_fill() -> OrderOutcome {
        OrderOutcome::Filled(FillReport {
            order_id: 1,
            side: Side::Buy,
            requested_qty: fx("1"),
            executed_qty: Fixed::ZERO,
            quote_qty: Fixed::ZERO,
            fee: Fixed::ZERO,
            status: OrderStatus::Rejected,
        })
    }

    // ---- 這一組是整個任務的核心：規則觸發就真的擋下訂單 ----

    #[test]
    fn a_fresh_healthy_session_lets_the_first_order_through() {
        // 回歸測試：起始樣本與對帳初值如果填錯，每一場的第一張單都會被擋，
        // 整個 App 就廢了。
        let (state, dir) = state("fresh");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);

        assert_eq!(breaker.check(T0), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn five_losing_round_trips_trip_the_breaker_and_block_the_order() {
        let (state, dir) = state("losses");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);

        // 五次「空手 → 建倉 → 回到空手而且權益更低」的來回。
        let mut equity = 10_000i64;
        for i in 0..5 {
            let at = T0 + i * 2_000;
            tick_bar(&breaker, at, fx("1"), equity);
            equity -= 100;
            tick_bar(&breaker, at + 1_000, Fixed::ZERO, equity);
        }

        let at = T0 + 11_000;
        breaker.observe_market_event(at);
        let blocked = breaker.check(at).expect_err("應該被擋下");
        assert_eq!(
            blocked,
            GlobalBlocked::BreakerTripped(BreakerTrip {
                trigger: DEFAULT_CONSECUTIVE_LOSSES,
                action: BreakerAction::PauseStrategy,
            }),
            "連續虧損 5 筆應該觸發，而且擋下這筆單"
        );
        // 觸發紀錄要落地，而且看得出是哪一條規則。
        let events = state.events_dto(10).unwrap();
        assert!(
            events.iter().any(|e| e.message.contains("連續虧損 5 筆")),
            "{events:?}"
        );
        assert_eq!(events[0].session.as_deref(), Some("testnet-1"));
        assert!(
            at_portfolio_risk::log_path(&dir).exists(),
            "紀錄必須立刻寫檔"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn four_losing_round_trips_do_not_trip_the_breaker() {
        // 邊界的另一邊：少一筆就不該擋，否則「連續虧損 5 筆」只是個幌子。
        let (state, dir) = state("four_losses");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);

        let mut equity = 10_000i64;
        for i in 0..4 {
            let at = T0 + i * 2_000;
            tick_bar(&breaker, at, fx("1"), equity);
            equity -= 100;
            tick_bar(&breaker, at + 1_000, Fixed::ZERO, equity);
        }

        let at = T0 + 9_000;
        breaker.observe_market_event(at);
        assert_eq!(breaker.check(at), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_market_stall_trips_and_stays_tripped_after_the_feed_comes_back() {
        let (state, dir) = state("stall");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        // 空白 4 秒（預設門檻 3 秒）。
        breaker.observe_market_event(T0 + 4_000);

        let blocked = breaker.check(T0 + 4_000).expect_err("中斷過就該擋");
        assert_eq!(
            blocked,
            GlobalBlocked::BreakerTripped(BreakerTrip {
                trigger: DEFAULT_MARKET_STALLED,
                action: BreakerAction::StopAndCancel,
            })
        );
        // 行情回來了也不自動恢復（ADR 5.6）。
        breaker.observe_market_event(T0 + 5_000);
        assert!(
            breaker.check(T0 + 5_000).is_err(),
            "行情恢復不代表中斷期間沒有漏掉成交，不可以自動解除"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quiet_feed_within_the_threshold_does_not_trip() {
        let (state, dir) = state("no_stall");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        breaker.observe_market_event(T0 + 2_999);

        assert_eq!(breaker.check(T0 + 2_999), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rejected_order_pushes_the_reject_rate_over_the_limit_and_blocks_the_next_one() {
        let (state, dir) = state("rejects");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);

        // 第一張單放行（閘門在這裡記下「送出一筆」），交易所回拒絕。
        assert_eq!(breaker.check(T0), Ok(()));
        breaker.record_order_outcome(&rejected_fill());

        // 1/1 = 100% ≥ 20% → 下一張單被擋。
        let blocked = breaker.check(T0 + 1_000).expect_err("拒絕率 100% 應該擋下");
        assert!(
            matches!(blocked, GlobalBlocked::BreakerTripped(trip)
                if matches!(trip.trigger, BreakerTrigger::RejectRate { .. })),
            "{blocked:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_order_blocked_by_the_gate_is_not_counted_as_a_rejection() {
        // 擋下的單沒有送到交易所，不能算進拒絕率——否則第一次擋單會把拒絕率
        // 推高，熔斷開始自己餵自己。
        let (state, dir) = state("not_a_rejection");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        assert_eq!(breaker.check(T0), Ok(()));

        breaker.record_order_outcome(&OrderOutcome::GloballyBlocked(
            GlobalBlocked::RiskStateUnavailable,
        ));
        breaker.record_order_outcome(&OrderOutcome::Blocked(
            at_risk_control::Blocked::DailyPnlUnknown,
        ));

        assert_eq!(breaker.check(T0 + 1_000), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- fail-closed ----

    #[test]
    fn an_equity_sample_that_cannot_be_computed_fails_closed() {
        // 權益算不出來 → 回撤算不出來 → 擋。不可以當成「沒有回撤」。
        let (state, dir) = state("unknown_equity");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        breaker.state.lock().unwrap().samples.push(EquitySample {
            at_ms: T0,
            position: Fixed::ZERO,
            equity: None,
        });

        let blocked = breaker.check(T0).expect_err("算不出來就要擋");
        assert_eq!(
            blocked,
            GlobalBlocked::BreakerTripped(BreakerTrip {
                trigger: DEFAULT_STRATEGY_DRAWDOWN,
                action: BreakerAction::StopAndCancel,
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poisoned_accumulator_blocks_instead_of_passing() {
        let (state, dir) = state("poisoned_state");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        assert_eq!(breaker.check(T0), Ok(()), "毒化之前是放行的");

        let victim = Arc::clone(&breaker);
        let _ = std::thread::spawn(move || {
            let _guard = victim.state.lock().unwrap();
            panic!("刻意在持有鎖的時候 panic，把鎖毒化");
        })
        .join();

        assert_eq!(
            breaker.check(T0 + 1_000),
            Err(GlobalBlocked::RiskStateUnavailable),
            "風控狀態讀不到時必須擋單，不是放行"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poisoned_rule_set_blocks_instead_of_passing() {
        let (state, dir) = state("poisoned_rules");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);

        let shared = Arc::clone(&breaker.shared);
        let _ = std::thread::spawn(move || {
            let _guard = shared.rules.lock().unwrap();
            panic!("刻意毒化規則的鎖");
        })
        .join();

        assert_eq!(
            breaker.check(T0 + 1_000),
            Err(GlobalBlocked::RiskStateUnavailable)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_that_lost_track_of_its_ledger_pauses_the_whole_account() {
        // 強制規則（對帳不一致）的動作是「暫停整個帳戶的下單」：另一場 session
        // 也要被擋住，而且重開一場不會解除。
        let (state, dir) = state("account_pause");
        let a = breaker(&state, "testnet-a");
        let b = breaker(&state, "testnet-b");
        a.observe_market_event(T0);
        b.observe_market_event(T0);
        assert_eq!(b.check(T0), Ok(()));

        a.record_failure(&TradingError::OrderNotFinal {
            order_id: 7,
            status: OrderStatus::New,
            executed_qty: Fixed::ZERO,
        });

        let blocked = b.check(T0 + 1_000).expect_err("帳戶層暫停要擋住每一場");
        assert_eq!(
            blocked,
            GlobalBlocked::BreakerTripped(BreakerTrip {
                trigger: BreakerTrigger::ReconciliationMismatch,
                action: BreakerAction::PauseAllOrders,
            })
        );
        state.close_session("testnet-b");
        let c = breaker(&state, "testnet-c");
        c.observe_market_event(T0 + 2_000);
        assert!(c.check(T0 + 2_000).is_err(), "重開一場不該解除帳戶層的暫停");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trip_is_recorded_once_not_on_every_bar() {
        let (state, dir) = state("record_once");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        breaker.observe_market_event(T0 + 10_000); // 中斷 → 觸發

        for i in 0..5 {
            let _ = breaker.check(T0 + 10_000 + i);
        }
        let events = state.events_dto(50).unwrap();
        assert_eq!(
            events.len(),
            1,
            "同一次觸發只該留一則紀錄（環形上限 500 筆，重複記會把證據沖掉）：{events:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 權益序列的修剪 ----

    #[test]
    fn trimming_the_equity_history_only_cuts_at_a_flat_point() {
        // 回歸測試：切在「抱著部位」的中間，連續虧損筆數會變成「不明」→ 熔斷觸發，
        // 一場跑了一天多的交易會被自己的修剪邏輯停掉。
        let mut samples: Vec<EquitySample> = (0..MAX_EQUITY_SAMPLES + 10)
            .map(|i| EquitySample {
                at_ms: i as i64,
                // 每 10 根裡有一根是空手，所以一定找得到可以切的地方。
                position: if i % 10 == 0 { Fixed::ZERO } else { Fixed::ONE },
                equity: Some(fx("10000")),
            })
            .collect();

        trim_samples(&mut samples);

        assert!(samples.len() <= MAX_EQUITY_SAMPLES + 10);
        assert!(samples.len() < MAX_EQUITY_SAMPLES + 10, "應該真的有修剪");
        assert!(
            samples[0].position.is_zero(),
            "修剪後的第一筆必須是空手，否則連續虧損筆數會變成不明"
        );
        assert!(consecutive_losses(&samples).is_some());
    }

    #[test]
    fn a_series_that_never_goes_flat_is_not_trimmed_at_all() {
        let mut samples: Vec<EquitySample> = (0..MAX_EQUITY_SAMPLES + 10)
            .map(|i| EquitySample {
                at_ms: i as i64,
                position: if i == 0 { Fixed::ZERO } else { Fixed::ONE },
                equity: Some(fx("10000")),
            })
            .collect();

        trim_samples(&mut samples);

        assert_eq!(
            samples.len(),
            MAX_EQUITY_SAMPLES + 10,
            "找不到可以安全切的地方時寧可不修剪（正確性優先於記憶體）"
        );
    }

    // ---- 規則開關與持久化 ----

    #[test]
    fn the_default_rule_set_has_five_rules_with_the_mandatory_one_on() {
        let (state, dir) = state("defaults");
        let rules = state.rules_dto().unwrap();
        assert_eq!(rules.len(), 5);
        assert!(rules.iter().all(|r| r.enabled), "預設五條都開著：{rules:?}");
        let mandatory: Vec<&BreakerRuleDto> = rules.iter().filter(|r| r.mandatory).collect();
        assert_eq!(mandatory.len(), 1);
        assert_eq!(mandatory[0].id, "reconciliationMismatch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turning_a_rule_off_persists_and_stops_it_from_tripping() {
        let (state, dir) = state("toggle");
        let breaker = breaker(&state, "testnet-1");
        state.set_rule("marketStalled", false).unwrap();

        breaker.observe_market_event(T0);
        breaker.observe_market_event(T0 + 10_000);
        assert_eq!(breaker.check(T0 + 10_000), Ok(()), "關掉的規則不該再擋單");

        // 下一次啟動 App 要讀到同一個設定。
        let reloaded = RiskControlState::new(dir.clone());
        let rules = reloaded.rules_dto().unwrap();
        let stalled = rules.iter().find(|r| r.id == "marketStalled").unwrap();
        assert!(!stalled.enabled, "開關要寫進檔案：{rules:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mandatory_rule_cannot_be_turned_off_through_the_command() {
        let (state, dir) = state("mandatory");
        let error = state.set_rule("reconciliationMismatch", false).unwrap_err();
        assert!(error.contains("強制開啟"), "{error}");
        assert!(
            state
                .rules_dto()
                .unwrap()
                .iter()
                .find(|r| r.id == "reconciliationMismatch")
                .unwrap()
                .enabled
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_edited_settings_file_cannot_delete_the_mandatory_rule() {
        let dir = temp_dir("hand_edited");
        std::fs::create_dir_all(&dir).unwrap();
        // 只留一條規則，強制規則被整條刪掉。
        std::fs::write(
            settings_path(&dir),
            r#"{"breaker_rules":[{"trigger":{"ConsecutiveLosses":{"count":5}},"switch":{"Optional":false},"action":"PauseStrategy"}]}"#,
        )
        .unwrap();

        let state = RiskControlState::new(dir.clone());
        let rules = state.rules_dto().unwrap();
        assert!(
            rules
                .iter()
                .any(|r| r.id == "reconciliationMismatch" && r.enabled && r.mandatory),
            "強制規則必須被建構子補回來：{rules:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_settings_file_falls_back_to_every_rule_armed() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(settings_path(&dir), "{ 這不是設定 ").unwrap();

        let state = RiskControlState::new(dir.clone());
        let rules = state.rules_dto().unwrap();
        assert_eq!(rules.len(), 5);
        assert!(
            rules.iter().all(|r| r.enabled),
            "解析失敗不可以當成『沒有限制』：{rules:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_rule_id_is_a_clear_chinese_error() {
        let (state, dir) = state("unknown_rule");
        let error = state.set_rule("notARule", false).unwrap_err();
        assert!(error.contains("沒有這條熔斷規則"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rule_dto_tells_the_user_what_the_input_actually_is() {
        // ADR 13.2：UI 不能讓使用者以為自己受到保護。對帳那條的限制必須寫明。
        let (state, dir) = state("notes");
        let rules = state.rules_dto().unwrap();
        let reconciliation = rules
            .iter()
            .find(|r| r.id == "reconciliationMismatch")
            .unwrap();
        assert!(
            reconciliation.note.contains("還沒有"),
            "{}",
            reconciliation.note
        );
        assert!(rules.iter().all(|r| !r.note.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 觸發紀錄 ----

    #[test]
    fn events_come_back_newest_first_and_survive_a_restart() {
        let (state, dir) = state("events");
        let breaker = breaker(&state, "testnet-1");
        breaker.observe_market_event(T0);
        breaker.observe_market_event(T0 + 10_000);
        let _ = breaker.check(T0 + 10_000);

        let events = state.events_dto(50).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].clock, "交易所時間");
        assert_eq!(events[0].symbol.as_deref(), Some("BTCUSDT"));

        let reloaded = RiskControlState::new(dir.clone());
        assert_eq!(reloaded.events_dto(50).unwrap(), events, "紀錄要能讀回來");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
