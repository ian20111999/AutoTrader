//! 熔斷規則（ADR-002 第 5 節）。
//!
//! # 三者分離：條件 / 啟用 / 動作
//!
//! 一條規則是 [`BreakerRule`] = [`BreakerTrigger`]（條件 + 它自己的參數）
//! ＋ [`BreakerSwitch`]（開著還是關著）＋ [`BreakerAction`]（觸發後做什麼）。
//! 參數是**資料**而不是寫死的常數，所以設定檔調得動、測試餵得進去。
//!
//! # 一個觀測結構、一個評估點
//!
//! 五條規則都是同一個 [`BreakerObservation`] 的函式，統一在
//! [`BreakerRules::evaluate`] 裡評估，吐出同一個形狀的 [`BreakerTrip`]。
//! 這樣觸發紀錄、UI 列表、暫停動作的套用都只要寫一份；加第六條規則時
//! `match` 不再 exhaustive，編譯器會逼人處理每一個地方。
//!
//! # 強制開啟的規則在型別上關不掉
//!
//! 「對帳不一致 → 暫停下單」是強制的，而且**不是 UI 層的約定**：
//!
//! 1. [`BreakerSwitch::Mandatory`] 是沒有 payload 的 variant，所以
//!    「強制規則被關閉」這個值根本不存在；[`BreakerSwitch::is_armed`] 對它永遠 `true`。
//! 2. [`BreakerRules`] 的欄位是私有的，只能由 [`BreakerRules::new`] 建立，
//!    而建構子**永遠把強制規則加回去**。反序列化也走同一個建構子，所以手改 JSON
//!    把那條規則整條刪掉、或把它改成 `Optional(false)`，拿回來的規則集裡它都還在。
//!
//! 第 2 條比第 1 條重要：型別層防得住「設成 false」，集合層防得住「整條刪掉」。
//!
//! # 判不出來就擋（fail closed）
//!
//! 延續 `at-risk-control`（6.3）已經確立的精神：必要的觀測值是 `None`、數字自相
//! 矛盾、算術溢位，一律當成「規則觸發」。熔斷誤觸發的代價是錯過行情，漏掉的代價
//! 是繼續虧錢。邊界也一律往保守那邊倒（「剛好到上限」就算觸發，和 6.3 的
//! `daily_loss_at_exactly_the_limit_blocks` 同一個判準）。
//!
//! # 這裡沒有 I/O、沒有時鐘
//!
//! `now_ms` 由呼叫端傳進來。累加器（連續虧損筆數、拒絕率的時間窗、權益高點）
//! 在 App 層，因為 App 層已經在收 `TestnetUpdate` 事件流（ADR 5.4）。

use crate::serde_at;
use at_core::Fixed;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 設計稿的預設值：連續虧損 5 筆。
pub const DEFAULT_CONSECUTIVE_LOSSES: BreakerTrigger =
    BreakerTrigger::ConsecutiveLosses { count: 5 };
/// 設計稿的預設值：行情中斷超過 3 秒。
pub const DEFAULT_MARKET_STALLED: BreakerTrigger = BreakerTrigger::MarketStalled { ms: 3_000 };
/// 設計稿的預設值：1 分鐘內拒絕率超過 20%。
pub const DEFAULT_REJECT_RATE: BreakerTrigger = BreakerTrigger::RejectRate {
    window_ms: 60_000,
    // 0.2 = 20%
    ratio: Fixed::from_raw(20_000_000),
};
/// 單策略回撤上限的預設值 10%。
///
/// 設計稿只寫「超過上限」沒給數字，這個 10% 是本實作挑的保守起點，不是設計稿的值。
pub const DEFAULT_STRATEGY_DRAWDOWN: BreakerTrigger = BreakerTrigger::StrategyDrawdown {
    // 0.1 = 10%
    ratio: Fixed::from_raw(10_000_000),
};

/// 強制開啟的規則。[`BreakerRules::new`] 永遠把這些加回規則集。
///
/// 設計稿點名強制開啟的有兩項：「對帳不一致 → 暫停下單」與「交易所端停損」。
/// 後者是下合約單時帶的參數，不是本機閘門，等合約路徑接上時由送單點讀取
/// （ADR 7.2），所以這裡只有前者。
pub const MANDATORY_RULES: [BreakerRule; 1] = [BreakerRule {
    trigger: BreakerTrigger::ReconciliationMismatch,
    switch: BreakerSwitch::Mandatory,
    action: BreakerAction::PauseAllOrders,
}];

/// 熔斷的條件，以及這個條件自己的參數。
///
/// 閉集合、exhaustive match、可序列化。刻意不用 `Box<dyn ...>`：五條規則是封閉的、
/// 輸入相同、沒有第三方擴充需求，trait object 換來的可擴充性沒人要，換掉的是
/// exhaustive match、免費的 serde、以及不用 mock 的測試。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerTrigger {
    /// 連續虧損 `count` 筆 → 暫停策略。一「筆」的定義見 [`consecutive_losses`]。
    ConsecutiveLosses { count: u32 },
    /// 行情中斷超過 `ms` 毫秒。
    MarketStalled { ms: i64 },
    /// `window_ms` 毫秒內的拒絕率達到 `ratio`。
    ///
    /// `window_ms` 是給 App 層累加器用的（它決定時間窗要開多大），評估本身只看
    /// 已經算好的筆數——累加器請從 [`BreakerRules::rules`] 讀這個值，不要自己寫死，
    /// 否則設定檔調了窗長而累加器沒跟上，拒絕率就不是設定裡那個意思了。
    RejectRate {
        window_ms: i64,
        #[serde(with = "serde_at::fixed")]
        ratio: Fixed,
    },
    /// 對帳不一致。沒有參數——「不一致」就是不一致。
    ReconciliationMismatch,
    /// 單一策略相對自己權益高點的回撤達到 `ratio`。
    StrategyDrawdown {
        #[serde(with = "serde_at::fixed")]
        ratio: Fixed,
    },
}

impl BreakerTrigger {
    /// 這個條件是不是強制開啟的。
    pub fn is_mandatory(self) -> bool {
        matches!(self, BreakerTrigger::ReconciliationMismatch)
    }

    /// 把不可能是本意的參數換成預設值。
    ///
    /// 設定檔是使用者可以手改的檔案，也就是一個信任邊界。`count: 0` 會變成「永遠
    /// 觸發」（策略被永久暫停）、`ratio: 5`（500% 拒絕率）會變成「永遠不觸發」
    /// （看起來有保護其實沒有）——後者是危險的那一邊。兩種都換回預設值。
    ///
    /// 代價：使用者改錯了不會收到錯誤，只會發現值被換掉。選換掉而不是回報錯誤，是
    /// 為了讓 [`BreakerRules::new`] 永遠成功——它必須永遠能把強制規則補回去。
    fn normalized(self) -> BreakerTrigger {
        match self {
            BreakerTrigger::ConsecutiveLosses { count: 0 } => DEFAULT_CONSECUTIVE_LOSSES,
            BreakerTrigger::MarketStalled { ms } if ms <= 0 => DEFAULT_MARKET_STALLED,
            BreakerTrigger::RejectRate { window_ms, ratio }
                if window_ms <= 0 || ratio <= Fixed::ZERO || ratio > Fixed::ONE =>
            {
                DEFAULT_REJECT_RATE
            }
            BreakerTrigger::StrategyDrawdown { ratio }
                if ratio <= Fixed::ZERO || ratio > Fixed::ONE =>
            {
                DEFAULT_STRATEGY_DRAWDOWN
            }
            unchanged => unchanged,
        }
    }
}

impl fmt::Display for BreakerTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BreakerTrigger::ConsecutiveLosses { count } => write!(f, "連續虧損 {count} 筆"),
            BreakerTrigger::MarketStalled { ms } => write!(f, "行情中斷超過 {ms} 毫秒"),
            BreakerTrigger::RejectRate { window_ms, ratio } => {
                write!(f, "{window_ms} 毫秒內的拒絕率達到 {}", percent(*ratio))
            }
            BreakerTrigger::ReconciliationMismatch => write!(f, "對帳不一致"),
            BreakerTrigger::StrategyDrawdown { ratio } => {
                write!(f, "單策略回撤達到 {}", percent(*ratio))
            }
        }
    }
}

/// 比例轉成百分比字串給人看。乘不出來（只有極端值才會）就顯示原始比例。
fn percent(ratio: Fixed) -> String {
    Fixed::from_int(100)
        .and_then(|hundred| ratio.checked_mul(hundred))
        .map_or_else(|| ratio.to_string(), |pct| format!("{pct}%"))
}

/// 規則的啟用狀態。
///
/// **關鍵：`Mandatory` 沒有 payload，所以「強制規則被關閉」在型別上不存在。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerSwitch {
    /// 強制開啟，關不掉。
    Mandatory,
    /// 使用者可以開關。
    Optional(bool),
}

impl BreakerSwitch {
    /// 這條規則現在要不要評估。
    pub fn is_armed(self) -> bool {
        match self {
            BreakerSwitch::Mandatory => true,
            BreakerSwitch::Optional(on) => on,
        }
    }
}

/// 觸發後做什麼。範圍（單一策略／全帳戶）由動作本身表達，不另開 scope 欄位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerAction {
    /// 暫停這一個策略（session），其他策略照跑。
    PauseStrategy,
    /// 暫停整個帳戶的下單（等效於自動按下一鍵停止）。
    PauseAllOrders,
    /// 停止這個策略並撤單。目前只有市價單，所以「撤單」這一步是 no-op（ADR 6.3）。
    StopAndCancel,
}

impl fmt::Display for BreakerAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BreakerAction::PauseStrategy => write!(f, "暫停這個策略"),
            BreakerAction::PauseAllOrders => write!(f, "暫停整個帳戶的下單"),
            BreakerAction::StopAndCancel => write!(f, "停止這個策略並撤銷掛單"),
        }
    }
}

/// 一條熔斷規則。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerRule {
    pub trigger: BreakerTrigger,
    pub switch: BreakerSwitch,
    pub action: BreakerAction,
}

/// 一整套熔斷規則。
///
/// 欄位是私有的，只能用 [`BreakerRules::new`]、[`BreakerRules::default`] 或
/// 反序列化（也走 `new`）取得，所以手上拿到的規則集**一定**含強制規則。
/// JSON 形狀就是規則的陣列（`#[serde(from/into)]`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<BreakerRule>", into = "Vec<BreakerRule>")]
pub struct BreakerRules {
    rules: Vec<BreakerRule>,
}

impl BreakerRules {
    /// 用一組「可開關的規則」建立規則集。
    ///
    /// 做三件事，而且永遠成功：
    /// 1. 丟掉傳進來的強制條件（不管它被設成什麼開關、什麼動作）；
    /// 2. 把其餘規則的參數過一次 [`BreakerTrigger::normalized`]；
    /// 3. 把 [`MANDATORY_RULES`] 的正本加回去。
    ///
    /// 第 1 步刻意比「缺了就補」更強：傳進來一條
    /// `ReconciliationMismatch` + `Optional(false)` 不會留下，也不會變成兩條；
    /// 留下的是正本（`Mandatory` + `PauseAllOrders`）。
    pub fn new(optional: Vec<BreakerRule>) -> BreakerRules {
        let mut rules: Vec<BreakerRule> = optional
            .into_iter()
            .filter(|rule| !rule.trigger.is_mandatory())
            .map(|rule| BreakerRule {
                trigger: rule.trigger.normalized(),
                ..rule
            })
            .collect();
        rules.extend(MANDATORY_RULES);
        BreakerRules { rules }
    }

    /// 規則清單（唯讀）。App 層的累加器從這裡讀參數，例如拒絕率的時間窗長度。
    pub fn rules(&self) -> &[BreakerRule] {
        &self.rules
    }

    /// 評估一次觀測，回傳所有觸發的規則。沒有任何規則觸發時回傳空的 `Vec`。
    ///
    /// 這是唯一的評估點：五條規則都在這裡用同一個 `match` 判斷。
    pub fn evaluate(&self, observation: &BreakerObservation) -> Vec<BreakerTrip> {
        self.rules
            .iter()
            .filter(|rule| rule.switch.is_armed())
            .filter(|rule| trips(rule.trigger, observation))
            .map(|rule| BreakerTrip {
                trigger: rule.trigger,
                action: rule.action,
            })
            .collect()
    }
}

impl Default for BreakerRules {
    /// 設計稿的五條規則，可開關的四條預設都開著。
    ///
    /// 預設開著而不是關著：一個預設關掉的熔斷器等於沒有熔斷器，而使用者不會在
    /// 出事之前想起來要去開它。
    fn default() -> BreakerRules {
        BreakerRules::new(vec![
            BreakerRule {
                trigger: DEFAULT_CONSECUTIVE_LOSSES,
                switch: BreakerSwitch::Optional(true),
                action: BreakerAction::PauseStrategy,
            },
            BreakerRule {
                trigger: DEFAULT_MARKET_STALLED,
                switch: BreakerSwitch::Optional(true),
                action: BreakerAction::StopAndCancel,
            },
            BreakerRule {
                trigger: DEFAULT_REJECT_RATE,
                switch: BreakerSwitch::Optional(true),
                action: BreakerAction::PauseStrategy,
            },
            BreakerRule {
                trigger: DEFAULT_STRATEGY_DRAWDOWN,
                switch: BreakerSwitch::Optional(true),
                action: BreakerAction::StopAndCancel,
            },
        ])
    }
}

impl From<Vec<BreakerRule>> for BreakerRules {
    /// 反序列化的入口。走 [`BreakerRules::new`]，所以強制規則一定被補回去。
    fn from(rules: Vec<BreakerRule>) -> BreakerRules {
        BreakerRules::new(rules)
    }
}

impl From<BreakerRules> for Vec<BreakerRule> {
    fn from(rules: BreakerRules) -> Vec<BreakerRule> {
        rules.rules
    }
}

/// 對帳結果。`Unknown` 和 `Mismatch` 一樣擋——不知道對不對得上，就是對不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reconciliation {
    /// 本地帳本和交易所對得上。
    Ok,
    /// 對不上。
    Mismatch,
    /// 還沒對帳、或對帳本身失敗。預設值，因為「沒說」只能當成「不知道」。
    #[default]
    Unknown,
}

/// 時間窗內的下單筆數。
///
/// 刻意用具名欄位而不是 `(u32, u32)`：在送單路徑上把「送出」和「被拒絕」兩個
/// 數字寫反，是一個型別檢查不出來、而且會讓熔斷器永遠不觸發的錯誤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OrderCounts {
    /// 窗內送出的筆數。
    pub sent: u32,
    /// 窗內被交易所拒絕的筆數。
    pub rejected: u32,
}

/// 評估熔斷用的一次觀測，對應**一個** session。全部由 App 層的累加器算好餵進來。
///
/// 每個欄位的「不知道」都有辦法表達，而且每一個「不知道」都會讓用到它的規則觸發。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerObservation {
    /// 現在的時間（毫秒）。
    ///
    /// 必須和 `last_market_event_ms` **同一個時鐘來源**（ADR 建議交易所時間）。
    /// 混用的話算出來的中斷時間會是錯的；算出負數時本評估會當成「不知道」而觸發。
    pub now_ms: i64,
    /// 最後一次收到行情事件的時間。`None` = 還沒收到過任何行情。
    pub last_market_event_ms: Option<i64>,
    /// 最近連續虧損的筆數。`None` = 算不出來（見 [`consecutive_losses`]）。
    pub consecutive_losses: Option<u32>,
    /// 時間窗內的下單筆數。窗長請用 [`BreakerTrigger::RejectRate`] 的 `window_ms`。
    pub recent_orders: OrderCounts,
    /// 相對權益高點的回撤比例（正數，0.1 = 回撤 10%）。`None` = 算不出來。
    pub drawdown: Option<Fixed>,
    /// 對帳結果。
    pub reconciliation: Reconciliation,
}

impl BreakerObservation {
    /// 一個「什麼都還不知道」的觀測：除了時間以外每一項都是不明。
    ///
    /// 這是填觀測值的正確起點——用 `..BreakerObservation::unknown(now)` 補其餘欄位，
    /// 漏填的那一項會讓規則觸發（擋下），而不是悄悄變成一個放行的值。
    pub fn unknown(now_ms: i64) -> BreakerObservation {
        BreakerObservation {
            now_ms,
            last_market_event_ms: None,
            consecutive_losses: None,
            // 沒送過單就沒有拒絕率可言，這一項的「零」不是樂觀假設。
            recent_orders: OrderCounts::default(),
            drawdown: None,
            reconciliation: Reconciliation::Unknown,
        }
    }
}

/// 一條規則觸發了。五條規則吐出同一個形狀，所以紀錄、顯示、套用動作只要寫一份。
///
/// `trigger` 帶著觸發當下的參數（門檻值），所以紀錄裡不需要另外存一份門檻。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerTrip {
    pub trigger: BreakerTrigger,
    pub action: BreakerAction,
}

impl fmt::Display for BreakerTrip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {}", self.trigger, self.action)
    }
}

/// 單一條件對單一觀測的判斷。**唯一**一處把條件換成 `true`／`false` 的地方。
fn trips(trigger: BreakerTrigger, obs: &BreakerObservation) -> bool {
    match trigger {
        // 算不出筆數 → 擋。
        BreakerTrigger::ConsecutiveLosses { count } => {
            obs.consecutive_losses.map_or(true, |n| n >= count)
        }
        BreakerTrigger::MarketStalled { ms } => match obs.last_market_event_ms {
            // 還沒收到過任何行情：在收到第一筆行情之前本來就不該交易。
            None => true,
            Some(last) => match obs.now_ms.checked_sub(last) {
                // 時間往回走（通常是把本機時鐘和交易所時間混用了）→ 算不準 → 擋。
                Some(elapsed) => elapsed < 0 || elapsed >= ms,
                None => true,
            },
        },
        BreakerTrigger::RejectRate { ratio, .. } => {
            let OrderCounts { sent, rejected } = obs.recent_orders;
            if rejected > sent {
                // 拒絕數比送出數還多：累加器自相矛盾，不猜。
                return true;
            }
            if sent == 0 {
                // 窗內一張單都沒送 → 沒有拒絕率。這不是「不知道」，是「沒有」。
                return false;
            }
            match (
                Fixed::from_int(i64::from(sent)),
                Fixed::from_int(i64::from(rejected)),
            ) {
                (Some(sent), Some(rejected)) => rejected
                    .checked_div(sent)
                    .map_or(true, |actual| actual >= ratio),
                _ => true,
            }
        }
        BreakerTrigger::ReconciliationMismatch => obs.reconciliation != Reconciliation::Ok,
        // 算不出回撤 → 擋。
        BreakerTrigger::StrategyDrawdown { ratio } => obs.drawdown.map_or(true, |d| d >= ratio),
    }
}

/// 權益序列的一個取樣點。
///
/// App 層每收到一則 `TestnetUpdate::Bar` 就產生一個；`position` 來自快照的部位，
/// `equity` 來自 `TestnetSnapshot::point.equity`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EquitySample {
    /// 取樣時間（毫秒）。目前只用來排序與除錯，判斷邏輯不看它。
    pub at_ms: i64,
    /// 帶號部位：正做多、負做空、**0 = 空手，也就是一筆交易的邊界**。
    pub position: Fixed,
    /// 當下權益。`None` = 算不出來。
    pub equity: Option<Fixed>,
}

/// 從權益序列數出「最近連續虧損幾筆」。`None` = 算不出來（呼叫端要原樣塞進
/// [`BreakerObservation::consecutive_losses`]，規則會擋）。
///
/// # 一「筆」是什麼：用「回到空手」當交易邊界
///
/// 現在的帳本（`at_testnet_trading::Trader`）只有 `cash` 與 `position`，沒有成本
/// 基礎，所以沒辦法對單一賣單說「這一筆賺還是虧」。這裡改用權益序列推邊界
/// （ADR 5.5 選項 a）：
///
/// - **一筆交易 = 一次從空手（`position == 0`）到空手的來回。**
/// - **這筆的損益 = 回到空手時的權益 − 上一次空手時的權益。**
///
/// 權益本身已經同時含已實現與未實現損益（和 `DailyPnl` 用權益差的理由一樣），
/// 所以對「單一交易對的現貨來回」這是精確值，含手續費與滑價，而且**完全不用動
/// 已審查過的帳本結構**。
///
/// # 限制（UI 要寫給使用者看）
///
/// 策略如果永遠不回到空手，就永遠不會累積「筆數」——這條熔斷規則對一直抱著部位
/// 的策略不會觸發。這是用零帳本改動換來的，不是 bug。
///
/// # 什麼情況回 `None`
///
/// 某一筆交易的兩個邊界權益有一個是 `None`、或相減溢位時，那一筆的損益不明，
/// 連續筆數就不明。序列開頭就已經有部位（例如 App 重啟接手）時，第一筆的起點
/// 權益不明，同樣回 `None`。
///
/// **起點不明是在建倉時就回 `None`，不是等平倉才回。** 抱著一個起點不明的部位
/// 期間，我們對「之前虧了幾筆」一無所知，這時候回 `Some(0)`（看起來沒虧過）
/// 等於放行，而且同一個不明狀態只因為觀測時機不同就從擋變成放行。
///
/// 不明是會恢復的：之後只要出現**一筆賺錢的交易**，連續虧損就重新從 0 算起。
/// 這是刻意的——「不知道」不該把策略永久鎖死，但在恢復之前一律擋。
pub fn consecutive_losses(samples: &[EquitySample]) -> Option<u32> {
    // 最近一次空手時的權益；序列從有部位開始時是 None（起點不明）。
    let mut flat_equity: Option<Fixed> = None;
    // 上次空手之後有沒有建過倉。沒有建倉就回到空手不算一筆交易。
    let mut opened = false;
    let mut run: Option<u32> = Some(0);

    for sample in samples {
        if !sample.position.is_zero() {
            if flat_equity.is_none() {
                // 抱著部位、而且這一筆的起點權益不明（序列從有部位開始，或上一次
                // 空手時的權益算不出來）→ 筆數立刻不明。不等它平倉才說不明：等的
                // 那段期間會回 Some(0)，也就是「看起來沒虧過」而放行。
                run = None;
            }
            opened = true;
            continue;
        }
        if opened {
            let pnl = flat_equity
                .zip(sample.equity)
                .and_then(|(before, after)| after.checked_sub(before));
            run = match pnl {
                // 這一筆算不出來 → 連續筆數不明。
                None => None,
                Some(pnl) if pnl.is_negative() => run.map(|n| n.saturating_add(1)),
                // 沒虧（含剛好打平）→ 連續虧損中斷，從 0 重新算。
                Some(_) => Some(0),
            };
            opened = false;
        }
        flat_equity = sample.equity;
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn f(n: i64) -> Fixed {
        Fixed::from_int(n).unwrap()
    }

    /// 一個每一項都「正常」的觀測：套用預設規則集時不該有任何觸發。
    fn healthy() -> BreakerObservation {
        BreakerObservation {
            now_ms: 1_000_000,
            last_market_event_ms: Some(1_000_000),
            consecutive_losses: Some(0),
            recent_orders: OrderCounts {
                sent: 10,
                rejected: 0,
            },
            drawdown: Some(Fixed::ZERO),
            reconciliation: Reconciliation::Ok,
        }
    }

    /// 只有一條規則的規則集（強制規則仍然會被加進去，所以評估時要篩掉它）。
    fn only(trigger: BreakerTrigger) -> BreakerRules {
        BreakerRules::new(vec![BreakerRule {
            trigger,
            switch: BreakerSwitch::Optional(true),
            action: BreakerAction::PauseStrategy,
        }])
    }

    /// 這個條件在這個觀測下有沒有觸發（忽略強制規則的觸發）。
    fn fires(trigger: BreakerTrigger, obs: &BreakerObservation) -> bool {
        only(trigger)
            .evaluate(obs)
            .iter()
            .any(|trip| trip.trigger == trigger)
    }

    fn sample(position: Fixed, equity: Option<Fixed>) -> EquitySample {
        EquitySample {
            at_ms: 0,
            position,
            equity,
        }
    }

    /// 空手 + 權益。
    fn flat(equity: i64) -> EquitySample {
        sample(Fixed::ZERO, Some(f(equity)))
    }

    /// 持有 1 顆 + 權益。
    fn holding(equity: i64) -> EquitySample {
        sample(Fixed::ONE, Some(f(equity)))
    }

    // ---------- 強制開啟：型別層 + 集合層 ----------

    #[test]
    fn mandatory_switch_is_always_armed() {
        // 型別層的保證：`Mandatory` 沒有 payload，沒有任何值能表達「被關閉」。
        assert!(BreakerSwitch::Mandatory.is_armed());
        assert!(BreakerSwitch::Optional(true).is_armed());
        assert!(!BreakerSwitch::Optional(false).is_armed());
    }

    #[test]
    fn an_empty_rule_set_still_contains_the_mandatory_rule() {
        let rules = BreakerRules::new(vec![]);
        assert_eq!(rules.rules(), MANDATORY_RULES);
    }

    #[test]
    fn a_mandatory_rule_passed_in_switched_off_comes_back_mandatory() {
        // 試圖用「傳一條關掉的對帳規則」把它關掉。
        let rules = BreakerRules::new(vec![BreakerRule {
            trigger: BreakerTrigger::ReconciliationMismatch,
            switch: BreakerSwitch::Optional(false),
            action: BreakerAction::PauseStrategy,
        }]);
        // 留下來的是正本：強制、而且動作是「暫停整個帳戶的下單」，不是傳進來的那個。
        assert_eq!(rules.rules(), MANDATORY_RULES);
        assert!(rules
            .rules()
            .iter()
            .all(|rule| rule.switch == BreakerSwitch::Mandatory));
    }

    #[test]
    fn deleting_the_mandatory_rule_from_the_json_file_does_not_work() {
        // 手改設定檔，把對帳那條整條刪掉，只留連續虧損。
        let json = r#"[
            {"trigger":{"ConsecutiveLosses":{"count":3}},
             "switch":{"Optional":true},
             "action":"PauseStrategy"}
        ]"#;
        let rules: BreakerRules = serde_json::from_str(json).unwrap();
        assert!(rules.rules().contains(&MANDATORY_RULES[0]));
        assert_eq!(rules.rules().len(), 2);
    }

    #[test]
    fn switching_the_mandatory_rule_off_in_the_json_file_does_not_work() {
        let json = r#"[
            {"trigger":"ReconciliationMismatch",
             "switch":{"Optional":false},
             "action":"PauseStrategy"}
        ]"#;
        let rules: BreakerRules = serde_json::from_str(json).unwrap();
        assert_eq!(rules.rules(), MANDATORY_RULES);
    }

    #[test]
    fn the_mandatory_rule_fires_on_mismatch_and_on_unknown() {
        let rules = BreakerRules::new(vec![]);
        let expected = vec![BreakerTrip {
            trigger: BreakerTrigger::ReconciliationMismatch,
            action: BreakerAction::PauseAllOrders,
        }];

        let mismatch = BreakerObservation {
            reconciliation: Reconciliation::Mismatch,
            ..healthy()
        };
        assert_eq!(rules.evaluate(&mismatch), expected);

        // 「還沒對帳」和「對不上」一樣擋。
        let unknown = BreakerObservation {
            reconciliation: Reconciliation::Unknown,
            ..healthy()
        };
        assert_eq!(rules.evaluate(&unknown), expected);

        assert_eq!(rules.evaluate(&healthy()), vec![]);
    }

    // ---------- 單一評估點 ----------

    #[test]
    fn a_healthy_observation_trips_nothing_in_the_default_rule_set() {
        assert_eq!(BreakerRules::default().evaluate(&healthy()), vec![]);
    }

    #[test]
    fn the_default_rule_set_is_the_five_rules_from_the_design() {
        let rules = BreakerRules::default();
        assert_eq!(rules.rules().len(), 5);
        let triggers: Vec<BreakerTrigger> = rules.rules().iter().map(|r| r.trigger).collect();
        assert_eq!(
            triggers,
            vec![
                DEFAULT_CONSECUTIVE_LOSSES,
                DEFAULT_MARKET_STALLED,
                DEFAULT_REJECT_RATE,
                DEFAULT_STRATEGY_DRAWDOWN,
                BreakerTrigger::ReconciliationMismatch,
            ]
        );
        // 可開關的四條預設都開著。
        assert!(rules.rules().iter().all(|r| r.switch.is_armed()));
    }

    #[test]
    fn several_rules_can_trip_at_once_and_each_carries_its_own_action() {
        // 完全失控的一刻：什麼都不知道。五條規則有四條會說話
        // （拒絕率不會：窗內沒送過單就沒有拒絕率，見
        // `an_unknown_observation_trips_every_rule_that_depends_on_it`）。
        let trips = BreakerRules::default().evaluate(&BreakerObservation::unknown(1_000));
        assert_eq!(trips.len(), 4);
        assert!(!trips
            .iter()
            .any(|t| matches!(t.trigger, BreakerTrigger::RejectRate { .. })));
        assert!(trips
            .iter()
            .any(|t| t.action == BreakerAction::PauseAllOrders));
        assert!(trips
            .iter()
            .any(|t| t.action == BreakerAction::PauseStrategy));
        assert!(trips
            .iter()
            .any(|t| t.action == BreakerAction::StopAndCancel));
    }

    #[test]
    fn a_switched_off_rule_is_not_evaluated_at_all() {
        let rules = BreakerRules::new(vec![BreakerRule {
            trigger: DEFAULT_CONSECUTIVE_LOSSES,
            switch: BreakerSwitch::Optional(false),
            action: BreakerAction::PauseStrategy,
        }]);
        // 連續虧損 99 筆，但規則關著 → 只有強制規則會說話（這裡對帳是 Ok，所以沒有）。
        let obs = BreakerObservation {
            consecutive_losses: Some(99),
            ..healthy()
        };
        assert_eq!(rules.evaluate(&obs), vec![]);
    }

    // ---------- 五條規則的門檻與 fail-closed ----------

    #[test]
    fn consecutive_losses_trips_at_exactly_the_configured_count() {
        let trigger = BreakerTrigger::ConsecutiveLosses { count: 5 };
        let with = |n: Option<u32>| BreakerObservation {
            consecutive_losses: n,
            ..healthy()
        };
        assert!(!fires(trigger, &with(Some(4))));
        // 剛好 5 筆就該停，不是「第 6 筆才停」。
        assert!(fires(trigger, &with(Some(5))));
        assert!(fires(trigger, &with(Some(6))));
        // 算不出來 → 擋。
        assert!(fires(trigger, &with(None)));
    }

    #[test]
    fn market_stalled_trips_on_a_gap_on_no_data_and_on_time_going_backwards() {
        let trigger = BreakerTrigger::MarketStalled { ms: 3_000 };
        let with = |last: Option<i64>| BreakerObservation {
            now_ms: 1_000_000,
            last_market_event_ms: last,
            ..healthy()
        };
        assert!(!fires(trigger, &with(Some(997_001)))); // 2999ms
        assert!(fires(trigger, &with(Some(997_000)))); // 剛好 3000ms
        assert!(fires(trigger, &with(Some(900_000)))); // 100 秒
                                                       // 還沒收到過任何行情。
        assert!(fires(trigger, &with(None)));
        // 行情時間比「現在」還新：兩個時鐘混用了，算不準。
        assert!(fires(trigger, &with(Some(1_000_001))));
        // 相減溢位。
        assert!(fires(trigger, &with(Some(i64::MIN))));
    }

    #[test]
    fn reject_rate_trips_at_the_ratio_and_on_contradictory_counts() {
        let trigger = BreakerTrigger::RejectRate {
            window_ms: 60_000,
            ratio: fx("0.2"),
        };
        let with = |sent: u32, rejected: u32| BreakerObservation {
            recent_orders: OrderCounts { sent, rejected },
            ..healthy()
        };
        assert!(!fires(trigger, &with(10, 1))); // 10%
        assert!(fires(trigger, &with(10, 2))); // 剛好 20%
        assert!(fires(trigger, &with(3, 1))); // 33%
                                              // 窗內一張單都沒送：沒有拒絕率，不是「不知道」——不然開機就熔斷。
        assert!(!fires(trigger, &with(0, 0)));
        // 拒絕數比送出數多：累加器自相矛盾，不猜。
        assert!(fires(trigger, &with(0, 1)));
        assert!(fires(trigger, &with(2, 3)));
    }

    #[test]
    fn strategy_drawdown_trips_at_the_ratio_and_when_it_cannot_be_computed() {
        let trigger = BreakerTrigger::StrategyDrawdown { ratio: fx("0.1") };
        let with = |drawdown: Option<Fixed>| BreakerObservation {
            drawdown,
            ..healthy()
        };
        assert!(!fires(trigger, &with(Some(fx("0.09")))));
        assert!(fires(trigger, &with(Some(fx("0.1"))))); // 剛好到上限
        assert!(fires(trigger, &with(Some(fx("0.5")))));
        assert!(fires(trigger, &with(None)));
    }

    #[test]
    fn an_unknown_observation_trips_every_rule_that_depends_on_it() {
        // `unknown()` 是填觀測值的起點：漏填任何一項都會擋，不會悄悄放行。
        let obs = BreakerObservation::unknown(1_000);
        assert!(fires(DEFAULT_CONSECUTIVE_LOSSES, &obs));
        assert!(fires(DEFAULT_MARKET_STALLED, &obs));
        assert!(fires(DEFAULT_STRATEGY_DRAWDOWN, &obs));
        assert!(fires(BreakerTrigger::ReconciliationMismatch, &obs));
        // 例外：沒送過單就是沒有拒絕率。
        assert!(!fires(DEFAULT_REJECT_RATE, &obs));
    }

    // ---------- 參數是資料：可調、可存、壞值會被換掉 ----------

    #[test]
    fn parameters_are_data_not_constants() {
        let strict = only(BreakerTrigger::ConsecutiveLosses { count: 2 });
        let obs = BreakerObservation {
            consecutive_losses: Some(3),
            ..healthy()
        };
        assert_eq!(strict.evaluate(&obs).len(), 1);
        let loose = only(BreakerTrigger::ConsecutiveLosses { count: 10 });
        assert_eq!(loose.evaluate(&obs), vec![]);
    }

    #[test]
    fn nonsense_parameters_are_replaced_by_the_defaults() {
        let bad = vec![
            BreakerTrigger::ConsecutiveLosses { count: 0 }, // 永遠觸發
            BreakerTrigger::MarketStalled { ms: 0 },
            BreakerTrigger::MarketStalled { ms: -1 },
            BreakerTrigger::RejectRate {
                window_ms: 0,
                ratio: fx("0.2"),
            },
            BreakerTrigger::RejectRate {
                window_ms: 60_000,
                ratio: f(5), // 500% 拒絕率 = 永遠不觸發，危險的那一邊
            },
            BreakerTrigger::StrategyDrawdown { ratio: Fixed::ZERO },
            BreakerTrigger::StrategyDrawdown { ratio: f(-1) },
        ];
        let defaults = [
            DEFAULT_CONSECUTIVE_LOSSES,
            DEFAULT_MARKET_STALLED,
            DEFAULT_REJECT_RATE,
            DEFAULT_STRATEGY_DRAWDOWN,
        ];
        for trigger in bad {
            let rules = only(trigger);
            let kept = rules.rules()[0].trigger;
            assert!(
                defaults.contains(&kept),
                "{trigger:?} 應該被換成預設值，實際是 {kept:?}"
            );
        }
    }

    #[test]
    fn a_sane_parameter_is_left_alone() {
        let trigger = BreakerTrigger::RejectRate {
            window_ms: 30_000,
            ratio: fx("0.05"),
        };
        assert_eq!(only(trigger).rules()[0].trigger, trigger);
    }

    #[test]
    fn rules_round_trip_through_json_as_decimal_strings() {
        let rules = BreakerRules::default();
        let json = serde_json::to_string_pretty(&rules).unwrap();
        // 比例存成人看得懂的十進位字串，不是內部整數。
        assert!(json.contains(r#""ratio": "0.2""#), "{json}");
        assert!(json.contains(r#""ratio": "0.1""#), "{json}");
        assert_eq!(serde_json::from_str::<BreakerRules>(&json).unwrap(), rules);
    }

    #[test]
    fn triggers_and_actions_render_text_for_the_ui() {
        assert_eq!(
            BreakerTrigger::ConsecutiveLosses { count: 5 }.to_string(),
            "連續虧損 5 筆"
        );
        assert_eq!(
            BreakerTrigger::MarketStalled { ms: 3_000 }.to_string(),
            "行情中斷超過 3000 毫秒"
        );
        assert_eq!(
            BreakerTrigger::RejectRate {
                window_ms: 60_000,
                ratio: fx("0.2")
            }
            .to_string(),
            "60000 毫秒內的拒絕率達到 20%"
        );
        assert_eq!(
            BreakerTrigger::ReconciliationMismatch.to_string(),
            "對帳不一致"
        );
        assert_eq!(
            BreakerTrip {
                trigger: BreakerTrigger::ReconciliationMismatch,
                action: BreakerAction::PauseAllOrders,
            }
            .to_string(),
            "對帳不一致 → 暫停整個帳戶的下單"
        );
    }

    // ---------- 「一筆交易」從權益序列推出來 ----------

    #[test]
    fn a_trade_is_one_round_trip_back_to_flat() {
        // 1000 → 建倉 → 回到空手 980：虧 20，一筆。
        let samples = [flat(1_000), holding(1_010), flat(980)];
        assert_eq!(consecutive_losses(&samples), Some(1));
    }

    #[test]
    fn five_losing_round_trips_trip_the_default_breaker() {
        let mut samples = vec![flat(1_000)];
        for equity in [990, 980, 970, 960, 950] {
            samples.push(holding(equity + 5));
            samples.push(flat(equity));
        }
        assert_eq!(consecutive_losses(&samples), Some(5));

        let obs = BreakerObservation {
            consecutive_losses: consecutive_losses(&samples),
            ..healthy()
        };
        let trips = BreakerRules::default().evaluate(&obs);
        assert_eq!(
            trips,
            vec![BreakerTrip {
                trigger: DEFAULT_CONSECUTIVE_LOSSES,
                action: BreakerAction::PauseStrategy,
            }]
        );
    }

    #[test]
    fn a_winning_trade_resets_the_run() {
        // 虧、虧、賺、虧 → 連續虧損是 1，不是 3。
        let samples = [
            flat(1_000),
            holding(995),
            flat(990),
            holding(985),
            flat(980),
            holding(1_000),
            flat(1_020),
            holding(1_010),
            flat(1_000),
        ];
        assert_eq!(consecutive_losses(&samples), Some(1));
    }

    #[test]
    fn breaking_even_also_resets_the_run() {
        // 先確認兩筆連續虧損真的數成 2（1000→990、990→980）。
        let two_losses = [
            flat(1_000),
            holding(995),
            flat(990),
            holding(985),
            flat(980),
        ];
        assert_eq!(consecutive_losses(&two_losses), Some(2));

        // 第三筆剛好打平（980 → 980）：不是虧損，所以連續虧損歸零。
        let then_even = [
            flat(1_000),
            holding(995),
            flat(990),
            holding(985),
            flat(980),
            holding(975),
            flat(980),
        ];
        assert_eq!(consecutive_losses(&then_even), Some(0));
    }

    #[test]
    fn an_unfinished_trade_is_not_counted_yet() {
        // 還抱著部位：這一筆還沒結束，不算。
        let samples = [flat(1_000), holding(900)];
        assert_eq!(consecutive_losses(&samples), Some(0));
        // 永遠不回到空手的策略永遠不會累積筆數——這是已知限制，不是 bug。
        // （前提是起點權益是知道的，也就是序列從空手開始。整條序列都抱著部位的
        // 情形見 `consecutive_losses_does_not_drift_open_while_holding_with_an_unknown_basis`。）
        let never_flat = [flat(1_000), holding(500), holding(100)];
        assert_eq!(consecutive_losses(&never_flat), Some(0));
    }

    #[test]
    fn an_empty_series_has_no_losses() {
        assert_eq!(consecutive_losses(&[]), Some(0));
    }

    #[test]
    fn repeated_flat_samples_do_not_invent_trades() {
        // 一直空手：沒有建倉就沒有交易。
        let samples = [flat(1_000), flat(1_000), flat(990), flat(990)];
        assert_eq!(consecutive_losses(&samples), Some(0));
    }

    #[test]
    fn an_unknown_equity_at_a_trade_boundary_makes_the_count_unknown() {
        // 收尾那一刻的權益算不出來。
        let at_close = [flat(1_000), holding(990), sample(Fixed::ZERO, None)];
        assert_eq!(consecutive_losses(&at_close), None);
        // 起點那一刻的權益算不出來。
        let at_open = [sample(Fixed::ZERO, None), holding(990), flat(980)];
        assert_eq!(consecutive_losses(&at_open), None);
    }

    #[test]
    fn an_unknown_equity_while_holding_does_not_matter() {
        // 持倉中的權益算不出來不影響這一筆的損益——只有兩個邊界要用到權益。
        let samples = [flat(1_000), sample(Fixed::ONE, None), flat(980)];
        assert_eq!(consecutive_losses(&samples), Some(1));
    }

    #[test]
    fn a_series_that_starts_mid_position_has_an_unknown_first_trade() {
        // App 重啟接手，序列開頭就已經有部位：第一筆的起點權益不明。
        let samples = [holding(1_000), flat(980)];
        assert_eq!(consecutive_losses(&samples), None);
    }

    #[test]
    fn the_count_recovers_from_unknown_after_a_winning_trade() {
        // 不明 → 虧（還是不明）→ 賺（重新從 0 算）→ 虧（1）。
        let samples = [
            holding(1_000),
            flat(980), // 不明
            holding(975),
            flat(970), // 虧，但前面不明 → 仍然不明
        ];
        assert_eq!(consecutive_losses(&samples), None);

        let recovered = [
            holding(1_000),
            flat(980),
            holding(1_000),
            flat(1_020), // 賺 → 重新從 0 算
            holding(1_010),
            flat(1_000), // 虧 1 筆
        ];
        assert_eq!(consecutive_losses(&recovered), Some(1));
    }

    #[test]
    fn an_overflowing_equity_difference_makes_the_count_unknown() {
        let samples = [
            sample(Fixed::ZERO, Some(Fixed::from_raw(i64::MIN))),
            holding(0),
            sample(Fixed::ZERO, Some(Fixed::from_raw(i64::MAX))),
        ];
        assert_eq!(consecutive_losses(&samples), None);
    }

    // ---------- fail-closed：基準不明的那段期間不可以看起來安全 ----------

    #[test]
    fn consecutive_losses_does_not_drift_open_while_holding_with_an_unknown_basis() {
        // App 重啟接手，序列開頭就已經有部位：這一筆的起點權益不明，而且在它平倉
        // 之前我們對「重啟之前虧了幾筆」一無所知。這段期間必須回不明（擋），不能回
        // Some(0)（放行）——否則同一個「不明」狀態只因為觀測時機不同就從擋變成放行。
        let still_holding = [holding(1_000), holding(500), holding(100)];
        assert_eq!(consecutive_losses(&still_holding), None);

        // 對照：同一段序列一旦平倉，本來就已經是不明。上面那個答案必須和這個一致。
        let then_closed = [holding(1_000), holding(500), holding(100), flat(90)];
        assert_eq!(consecutive_losses(&then_closed), None);
    }

    #[test]
    fn an_unknown_flat_equity_makes_the_next_open_trade_unknown_immediately() {
        // 空手期間的權益算不出來（不是交易邊界，所以不會立刻算損益），下一次建倉的
        // 起點權益就不明了。抱著這個部位的期間一樣要擋。
        let samples = [flat(1_000), sample(Fixed::ZERO, None), holding(900)];
        assert_eq!(consecutive_losses(&samples), None);

        // 不明是會恢復的：之後一筆算得出來的交易就能重新建立基準。
        let recovered = [
            flat(1_000),
            sample(Fixed::ZERO, None),
            holding(900),
            flat(800), // 起點不明 → 這一筆不明
            holding(810),
            flat(820), // 基準 800 → 賺 20 → 從 0 重算
        ];
        assert_eq!(consecutive_losses(&recovered), Some(0));
    }

    #[test]
    fn holding_after_a_known_flat_sample_is_still_just_an_unfinished_trade() {
        // 反面保證：基準是知道的，只是這一筆還沒結束 → 0，不是不明。
        // 「永遠不回到空手就永遠不累積筆數」這個已知限制在這裡維持不變。
        let samples = [flat(1_000), holding(900), holding(800)];
        assert_eq!(consecutive_losses(&samples), Some(0));
    }

    // ---------- 以下三個是探索性測試：證明這些情境已經被想過 ----------

    #[test]
    fn a_loss_while_the_count_is_unknown_cannot_be_turned_into_a_pass() {
        // 不明期間發生的虧損不會被加進計數（`run.map` 對 None 是 no-op），乍看像是
        // 「虧損被吃掉了」。但吃掉之後停在 None = 繼續擋，所以沒有放行。
        let unknown_then_losses = [
            holding(1_000),
            flat(980), // 起點不明 → 不明
            holding(975),
            flat(970), // 虧，但前面不明 → 仍然不明
            holding(965),
            flat(960), // 又虧 → 仍然不明
        ];
        assert_eq!(consecutive_losses(&unknown_then_losses), None);

        // 唯一的出口是一筆算得出來而且沒虧的交易。那一刻「最近連續虧損」確實是 0，
        // 因為最後那一筆沒虧——放行的依據是這一筆，不是把前面的虧損忘掉。
        let mut cleared = unknown_then_losses.to_vec();
        cleared.extend([holding(970), flat(980)]);
        assert_eq!(consecutive_losses(&cleared), Some(0));

        // 而且清零之後的虧損照樣從 1 開始數，不會少算。
        let mut after = cleared.clone();
        after.extend([holding(975), flat(970)]);
        assert_eq!(consecutive_losses(&after), Some(1));
    }

    #[test]
    fn a_reversal_without_a_flat_sample_collapses_two_legs_into_one_trade() {
        // 由多翻空、中間沒有任何 position == 0 的取樣：按「一筆 = 空手到空手」的
        // 定義，這整段只算一筆，損益用頭尾相減。
        // 多單這一段賺（1000 → 1020）、空單這一段虧（1020 → 1010），合起來是賺，
        // 所以連續虧損是 0；若改成逐段記帳會是 1。這是定義造成的少算，是已知限制
        // （和「永遠不回到空手就不累積筆數」同一個根源），不是這個函式算錯。
        let samples = [
            flat(1_000),
            sample(Fixed::ONE, Some(f(1_020))), // 做多，浮盈
            sample(fx("-1"), Some(f(1_015))),   // 直接反手做空
            flat(1_010),
        ];
        assert_eq!(consecutive_losses(&samples), Some(0));

        // 合起來虧的時候照樣算一筆虧損，所以規則不是完全失效，只是顆粒度較粗。
        let net_loss = [
            flat(1_000),
            sample(Fixed::ONE, Some(f(1_020))),
            sample(fx("-1"), Some(f(1_015))),
            flat(990),
        ];
        assert_eq!(consecutive_losses(&net_loss), Some(1));
    }

    #[test]
    fn equity_moving_while_flat_shifts_the_next_trades_basis_on_purpose() {
        // 空手期間權益自己掉了 10（例如前一筆的費用晚一拍才結算、或資金費入帳）：
        // 基準跟著移到 990，所以下一筆 990 → 995 算「賺 5」，連續虧損歸零，即使
        // 權益比 1000 還低。這是刻意的——非交易造成的權益變化不該記到某一筆交易頭上，
        // 整體權益下滑由回撤那條規則負責。
        let samples = [flat(1_000), flat(990), holding(992), flat(995)];
        assert_eq!(consecutive_losses(&samples), Some(0));

        // 同一段序列的回撤規則會說話：1000 的高點掉到 995 是 0.5% 回撤，門檻設 0.4%
        // 就會觸發。連續虧損筆數不是用來抓「權益下滑」的工具。
        let obs = BreakerObservation {
            consecutive_losses: consecutive_losses(&samples),
            drawdown: Some(fx("0.005")),
            ..healthy()
        };
        assert!(fires(
            BreakerTrigger::StrategyDrawdown { ratio: fx("0.004") },
            &obs
        ));
    }

    #[test]
    fn a_short_round_trip_counts_too() {
        // 做空也是一筆：邊界是「部位為 0」，和方向無關。
        let short = sample(fx("-1"), Some(f(995)));
        let samples = [flat(1_000), short, flat(970)];
        assert_eq!(consecutive_losses(&samples), Some(1));
    }
}
