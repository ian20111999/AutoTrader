//! 回測迴圈：訊號在收盤產生、下一根開盤成交，成交要付手續費與滑價；
//! 合約模式再加上做空、槓桿與資金費。
//!
//! 做的事只有四件：把 K 線一根一根餵給策略、按策略要的目標部位調整倉位、
//! 持倉期間收付資金費、每根收盤記一次帳戶總值。那串總值就是**權益曲線**，
//! 也是 [`crate::metrics::Metrics`] 四個指標唯一需要的輸入。
//!
//! 除了曲線，[`BacktestResult`] 還帶兩個曲線裡看不出來的數字：成交筆數與爆倉次數。
//!
//! ## 成交時點：決定與成交永遠隔一根（2.3）
//!
//! 每根 K 線的順序固定是：**先收付資金費 → 檢查有沒有被強制平倉 →
//! 成交上一根留下的目標（用這根的開盤價）→ 用這根的收盤價評價、記一點權益 →
//! 才問策略這根收盤想要什麼部位**。
//!
//! 這樣寫是因為現實就是這樣：收盤價要等收盤才知道，看到它才決定的單子，
//! 最快也只能送到下一根開盤。2.2 用同一根的收盤價成交，等於「看完收盤價才決定、
//! 卻還拿得到那個價格」——回測最典型的偷看未來（look-ahead bias）。
//!
//! 代價是決定與成交之間的跳空要自己承受：第 N 根收盤看到的價格和第 N+1 根
//! 開盤真正成交的價格可以差很多，而且不保證對自己有利。這正是回測要算進去的東西。
//!
//! 最後一根收盤產生的目標沒有下一根可以成交，直接作廢——它只是「還沒來得及執行」，
//! 不影響已經記完的權益曲線。
//!
//! ## 交易成本：滑價動價格、手續費動金額（2.4）
//!
//! 兩種成本進帳的方式不一樣，混在一起算會得到錯的數字：
//!
//! - **滑價改的是成交價**：買進成交在 `開盤價 × (1 + 滑價)`、賣出成交在
//!   `開盤價 × (1 − 滑價)`。兩邊都是買貴賣賤，因為滑價的方向不由你決定，
//!   回測要假設它永遠站在對你不利的那一邊。**評價用的收盤價不加滑價**——
//!   那是市價，不是你的成交價。
//! - **手續費改的是金額**：`成交金額 × 費率`，從現金扣。買進時要先算出
//!   「每一顆連手續費的總成本」才知道買得起多少，不然會買到手續費付不出來。
//!
//! 費率一律用 **taker（吃單）**：這一版的成交都是「下一根開盤直接成交」，
//! 也就是市價單。maker 費率要等有限價單與掛單簿模型才用得到。
//!
//! ## 合約：帶正負號的倉位（2.5）
//!
//! 帳本的形狀完全沒變，還是「現金 + 倉位數量」，只是兩個數字都可以是負的：
//!
//! | 狀態 | 現金 | 數量 | 權益 `現金 + 數量 × 價格` |
//! |---|---|---|---|
//! | 空手 | 10000 | 0 | 10000 |
//! | 1 倍做多 | 0 | +100 @100 | 10000 |
//! | 1 倍做空 | 20000 | −100 @100 | 10000 |
//! | 2 倍做多 | −10000 | +200 @100 | 10000 |
//!
//! **做空**就是數量變負（先賣後買，現金先變多）；**槓桿**就是現金變負
//! （名目金額大於權益，差額等於跟交易所借的錢）。權益永遠是同一條式子
//! `現金 + 數量 × 價格`，它等價於「錢包餘額 + 未實現損益」，只是不用另外記進場均價。
//!
//! 目標數量的算法：
//!
//! ```text
//! 目標數量 = 目標比例 × 成交前權益 ÷ (成交價 × (1 + 費率))
//! 這一筆要成交的量 = 目標數量 − 目前數量      // 正數就是買、負數就是賣
//! ```
//!
//! 三件事要注意：
//!
//! 1. **買賣方向看的是「這一筆是買還是賣」，不是「目標是多還是空」。**
//!    開空是賣（賣賤、付賣方費率）、平空是買（買貴、付買方費率）。搞反了滑價會
//!    站到對你有利的那一邊，回測就開始憑空生錢。
//! 2. **槓桿放大的是名目金額，費率不用改。**手續費一直是「名目金額 × 費率」，
//!    2 倍槓桿的名目金額是 2 倍，手續費自然也是 2 倍。
//! 3. **目標是「名目金額」而不是「數量」，所以價格一動就可能要調倉。**
//!    1 倍做多剛好不用調（名目金額永遠等於權益）；2 倍做多在價格上漲後
//!    槓桿會掉到 2 倍以下，下一根開盤會再買一點補回去——這就是固定槓桿產品的
//!    每日再平衡，來回震盪會磨損權益（volatility decay），不是程式寫錯。
//!
//! ## 資金費（2.5）
//!
//! U 本位永續合約沒有到期日，靠資金費把合約價格拉回現貨價格：每 8 小時
//! （[`FUNDING_INTERVAL_MS`]，UTC 00:00、08:00、16:00）結算一次，
//!
//! ```text
//! 付出的錢 = 帶正負號的名目金額 × 資金費率 = (數量 × 價格) × 費率
//! ```
//!
//! 直接加減現金，不碰價格。費率為**正**時多頭付錢給空頭（數量為正 → 付出為正 →
//! 現金變少），費率為**負**時反過來（數量為負 → 付出為負 → 現金變多）。
//! 一條式子同時處理多空兩邊，不用分支。
//!
//! 這一版的簡化（都是刻意的）：
//!
//! - 全程用**一個固定費率**，不抓歷史資金費率。真實費率每 8 小時不同，
//!   極端行情可以到 ±0.75%；要做參數敏感度就換幾個費率各跑一次。
//! - 結算價用**那根 K 線的開盤價**，不是結算當下的標記價格（我們沒有那筆資料）。
//! - 跨過幾個結算點就收幾次：K 線週期大於 8 小時（例如日線）時，一根會一次收好幾次。
//!
//! ## 強制平倉（2.5）
//!
//! 有槓桿就會爆倉。權益掉到 `名目金額 × 維持保證金率` 以下時，倉位會被整個平掉
//! （付滑價與吃單費），現金若被扣成負數就歸零——保證金虧光就是虧光，
//! 交易所的保險基金吃掉剩下的，帳戶不會倒欠。**權益因此永遠不會是負的。**
//!
//! 已知的簡化：**只在開盤與收盤檢查，不看盤中的最高最低價**。真實世界是標記價格
//! 一碰到強平價就立刻被平掉，可能發生在 K 線中間。所以高槓桿策略在這個回測裡
//! **偏樂觀**：一根長下影線該爆的倉，這裡可能因為收盤又拉回來而活下來。
//! 詳細取捨寫在 `docs/steps/2.5-合約.md`。
//!
//! 把 [`BacktestConfig::maintenance_margin_rate`] 設成 `None` 可以關掉這個檢查，
//! 變成純記帳模式；這時候權益一旦變負就回 [`BacktestError::NegativeEquity`]，
//! 絕不會拿一條負權益的曲線繼續往下跑。
//!
//! ## 想確認記帳沒壞
//!
//! 用 [`BacktestConfig::frictionless`] 跑同一份資料：它應該完全複製 2.3 的數字。
//! 槓桿 1 倍、只做多、資金費率 0 時，這一版的算式會退化成 2.4 的算式，
//! 數字也必須一模一樣（見測試 `unit_leverage_long_only_reproduces_the_2_4_numbers`）。

use crate::bar::Bar;
use crate::fees::FeeModel;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};
use crate::types::{Liquidity, Side};
use std::fmt;

/// 資金費結算間隔：8 小時（毫秒）。
///
/// Binance U 本位永續合約在 UTC 00:00、08:00、16:00 結算，剛好都是
/// 「毫秒時間戳是 8 小時的倍數」的時刻，所以判斷跨過幾次只要除法。
/// 少數交易對是 4 小時一次，這一版不處理。
pub const FUNDING_INTERVAL_MS: i64 = 8 * 60 * 60 * 1000;

/// 預設維持保證金率 0.5%。
///
/// Binance 依倉位大小分級（BTCUSDT 最小級距約 0.4%，倉位愈大要求愈高），
/// 這一版不分級，用一個保守的固定值。
pub const DEFAULT_MAINTENANCE_MARGIN_RATE: Fixed = Fixed::from_raw(500_000);

/// 一次回測的設定：起始資金、交易成本、合約參數。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BacktestConfig {
    /// 起始資金（報價幣，例如 USDT），必須大於 0。
    pub initial_capital: Fixed,
    /// 手續費模型。`None` 表示零費率，只當對照組用。
    pub fees: Option<FeeModel>,
    /// 滑價比例：`0.0005` 就是 0.05%。買貴賣賤，必須在 0（含）與 1（不含）之間。
    pub slippage: Fixed,
    /// 每 8 小時結算一次的資金費率：`0.0001` 就是 0.01%。
    ///
    /// 正數表示多頭付給空頭，負數反過來；`0` 等於關掉。全程固定，
    /// 絕對值必須小於 1（`0.75` 這種數字已經是史上最極端的行情，
    /// 再大一定是把百分比寫成了整數）。
    pub funding_rate: Fixed,
    /// 維持保證金率：`0.005` 就是 0.5%。權益低於「名目金額 × 這個比例」就強制平倉。
    ///
    /// `None` 表示不模擬強制平倉（純記帳模式），這時權益變負會直接回錯誤。
    /// 必須在 0（含）與 1（不含）之間。
    pub maintenance_margin_rate: Option<Fixed>,
}

impl BacktestConfig {
    /// 零手續費、零滑價、零資金費的理想化設定（強制平倉仍然會檢查）。
    ///
    /// 只有兩個用途：拿來和含成本的結果對照，以及當記帳邏輯的迴歸基準
    /// （它跑出來的數字必須和 2.3 完全一樣）。**不要拿它評估策略好不好**——
    /// 沒有成本的回測會把一堆高頻進出的爛策略美化成金雞母。
    ///
    /// 強制平倉不算「成本」而是「帳戶還在不在」，所以這裡不關掉：
    /// 一條爆倉後還繼續往上漲的權益曲線是假的，沒有對照價值。
    pub fn frictionless(initial_capital: Fixed) -> BacktestConfig {
        BacktestConfig {
            initial_capital,
            fees: None,
            slippage: Fixed::ZERO,
            funding_rate: Fixed::ZERO,
            maintenance_margin_rate: Some(DEFAULT_MAINTENANCE_MARGIN_RATE),
        }
    }
}

/// 權益曲線上的一點。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquityPoint {
    /// 對應 K 線的開盤時間（UTC 毫秒），和 [`Bar::open_time`] 一致。
    pub open_time: i64,
    /// 這根 K 線**收盤時**的帳戶總值 = 現金 + 持倉市值（以收盤價計，可為負數量）。
    pub equity: Fixed,
}

/// 一次回測的完整結果。
///
/// 為什麼不只回權益曲線：**成交筆數與爆倉次數是曲線裡看不出來的資訊**。
/// 「年化 30%、中途爆倉一次」和「年化 30%、全程沒爆倉」在曲線上可以長得很像
/// （爆倉後拿殘值重開的固定槓桿策略，和完全沒爆倉只是回撤深一點的策略，
/// 權益序列可以幾乎一樣），但在報表上是完全不同的兩件事。
///
/// 只有回測迴圈知道這兩個數字，在外面重跑一次策略去猜是錯的做法。
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestResult {
    /// 每根 K 線收盤時的帳戶總值。[`crate::metrics::Metrics`] 只吃這個欄位。
    pub curve: Vec<EquityPoint>,
    /// 實際送出去的成交筆數。
    ///
    /// **強制平倉也算一筆**：它是真的市價單，付了滑價與吃單費。想知道「有幾次
    /// 是被迫的」看 `liquidations`。同一根 K 線最多算一筆調倉，
    /// 「目標和現況只差一點成本、放棄調倉」的那種不算（因為真的沒送單）。
    pub trades: usize,
    /// 被強制平倉的次數。0 以外的任何數字都該在報表上標出來。
    pub liquidations: usize,
}

/// 回測跑不下去的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacktestError {
    /// 起始資金必須大於 0。
    NonPositiveCapital,
    /// 滑價比例不在 0（含）與 1（不含）之間。
    InvalidSlippage,
    /// 吃單費率算不出來，或不在 0（含）與 1（不含）之間。
    InvalidFeeRate,
    /// 資金費率的絕對值必須小於 1。
    InvalidFundingRate,
    /// 維持保證金率不在 0（含）與 1（不含）之間。
    InvalidMaintenanceMargin,
    /// 第 `index` 根 K 線的開盤時間沒有比前一根晚：資金費是按時間戳算的，
    /// 時間倒退會把資金費反過來收，等於憑空生錢。
    NonMonotonicTime { index: usize },
    /// 第 `index` 根 K 線的價格不是正數：成交用的開盤價或評價用的收盤價。
    NonPositivePrice { index: usize },
    /// 第 `index` 根 K 線算出負的權益：帳戶早該被強制平倉了，再跑下去只會是假數字。
    /// 只有在關掉強制平倉（`maintenance_margin_rate: None`）時才可能出現。
    NegativeEquity { index: usize },
    /// 第 `index` 根 K 線的金額計算超出 `Fixed` 可表示的範圍。
    Overflow { index: usize },
}

impl fmt::Display for BacktestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BacktestError::NonPositiveCapital => write!(f, "起始資金必須大於 0"),
            BacktestError::InvalidSlippage => {
                write!(f, "滑價比例必須大於等於 0 且小於 1")
            }
            BacktestError::InvalidFeeRate => {
                write!(f, "吃單費率必須大於等於 0 且小於 1")
            }
            BacktestError::InvalidFundingRate => {
                write!(f, "資金費率的絕對值必須小於 1")
            }
            BacktestError::InvalidMaintenanceMargin => {
                write!(f, "維持保證金率必須大於等於 0 且小於 1")
            }
            BacktestError::NonMonotonicTime { index } => {
                write!(f, "第 {index} 根 K 線的開盤時間沒有比前一根晚")
            }
            BacktestError::NonPositivePrice { index } => {
                write!(f, "第 {index} 根 K 線的價格不是正數，不能拿來成交或評價")
            }
            BacktestError::NegativeEquity { index } => {
                write!(f, "第 {index} 根 K 線的權益變成負數，帳戶早該被強制平倉")
            }
            BacktestError::Overflow { index } => {
                write!(f, "第 {index} 根 K 線的金額計算超出可表示範圍")
            }
        }
    }
}

impl std::error::Error for BacktestError {}

/// 迴圈外先算好的成交成本：滑價乘數與吃單費率，買賣各一組。
struct Costs {
    buy_price: Fixed,
    sell_price: Fixed,
    buy_rate: Fixed,
    sell_rate: Fixed,
}

impl Costs {
    /// 這一筆成交的（成交價, 費率）。
    ///
    /// `buying` 說的是「這一筆是買還是賣」，不是「目標是多還是空」：
    /// 開空是賣、平空是買，方向看的永遠是實際送出去的那張單。
    fn quote(&self, price: Fixed, buying: bool) -> Option<(Fixed, Fixed)> {
        let mult = if buying {
            self.buy_price
        } else {
            self.sell_price
        };
        let rate = if buying {
            self.buy_rate
        } else {
            self.sell_rate
        };
        Some((price.checked_mul(mult)?, rate))
    }
}

/// 以 `price` 為市價成交 `delta`（正數買、負數賣），回傳成交後的現金。
///
/// 買進現金變少、賣出現金變多，兩邊都再扣手續費（`名目金額 × 費率`）。
fn settle(cash: Fixed, delta: Fixed, price: Fixed, costs: &Costs) -> Option<Fixed> {
    let buying = !delta.is_negative();
    let (fill, rate) = costs.quote(price, buying)?;
    let notional = delta.abs().checked_mul(fill)?;
    let fee = notional.checked_mul(rate)?;
    let after = if buying {
        cash.checked_sub(notional)?
    } else {
        cash.checked_add(notional)?
    };
    after.checked_sub(fee)
}

/// 維持保證金檢查：權益掉到「名目金額 × 維持保證金率」以下就被整個平掉。
///
/// 回傳檢查後的（現金, 數量, 這次有沒有被平倉）。`rate` 是 `None` 就完全不檢查。
fn margin_call(
    cash: Fixed,
    qty: Fixed,
    mark: Fixed,
    rate: Option<Fixed>,
    costs: &Costs,
) -> Option<(Fixed, Fixed, bool)> {
    let Some(rate) = rate else {
        return Some((cash, qty, false));
    };
    if qty.is_zero() {
        return Some((cash, qty, false));
    }
    let value = qty.checked_mul(mark)?;
    let equity = cash.checked_add(value)?;
    let threshold = value.abs().checked_mul(rate)?;
    if equity > threshold {
        return Some((cash, qty, false));
    }
    // 平掉整個倉位：多頭賣出、空頭買回，一樣要付滑價與吃單費。
    let after = settle(cash, Fixed::ZERO.checked_sub(qty)?, mark, costs)?;
    // 保證金虧光就是虧光：交易所的保險基金吃掉剩下的，帳戶不會倒欠，權益也不會變負。
    let after = if after.is_negative() {
        Fixed::ZERO
    } else {
        after
    };
    Some((after, Fixed::ZERO, true))
}

/// 從 `prev`（不含）到 `now`（含）之間跨過幾個資金費結算點。
///
/// 結算點是「毫秒時間戳剛好是 8 小時倍數」的時刻，也就是 UTC 00:00、08:00、16:00。
fn funding_periods(prev: i64, now: i64) -> i64 {
    now.div_euclid(FUNDING_INTERVAL_MS) - prev.div_euclid(FUNDING_INTERVAL_MS)
}

/// 跑一次回測，回傳權益曲線與成交統計。
///
/// `bars` 要是同一個交易對與週期、時間遞增的連續 K 線（用 [`crate::find_gaps`] 先檢查）。
/// 空的 `bars` 不是錯誤，回傳空曲線與 0 筆成交。
pub fn run_backtest(
    bars: &[Bar],
    strategy: &mut dyn Strategy,
    config: &BacktestConfig,
) -> Result<BacktestResult, BacktestError> {
    if config.initial_capital <= Fixed::ZERO {
        return Err(BacktestError::NonPositiveCapital);
    }
    if config.slippage.is_negative() || config.slippage >= Fixed::ONE {
        return Err(BacktestError::InvalidSlippage);
    }
    if config.funding_rate.abs() >= Fixed::ONE {
        return Err(BacktestError::InvalidFundingRate);
    }
    if let Some(rate) = config.maintenance_margin_rate {
        if rate.is_negative() || rate >= Fixed::ONE {
            return Err(BacktestError::InvalidMaintenanceMargin);
        }
    }
    // 成交價的滑價倍數：買貴（>1）、賣賤（<1）。滑價在 [0, 1) 之間，兩個乘數都算得出來。
    let buy_price = Fixed::ONE
        .checked_add(config.slippage)
        .ok_or(BacktestError::InvalidSlippage)?;
    let sell_price = Fixed::ONE
        .checked_sub(config.slippage)
        .ok_or(BacktestError::InvalidSlippage)?;

    // 這一版的成交都是市價單，所以只用 taker 費率；買方、賣方費率可能不同，各取一次。
    // 費率整場不變，先算出來也順便讓不合理的費率在第一筆成交之前就被擋掉。
    let (buy_rate, sell_rate) = match &config.fees {
        Some(model) => (
            model
                .rate(Liquidity::Taker, Side::Buy)
                .ok_or(BacktestError::InvalidFeeRate)?,
            model
                .rate(Liquidity::Taker, Side::Sell)
                .ok_or(BacktestError::InvalidFeeRate)?,
        ),
        None => (Fixed::ZERO, Fixed::ZERO),
    };
    if buy_rate.is_negative()
        || buy_rate >= Fixed::ONE
        || sell_rate.is_negative()
        || sell_rate >= Fixed::ONE
    {
        return Err(BacktestError::InvalidFeeRate);
    }
    let costs = Costs {
        buy_price,
        sell_price,
        buy_rate,
        sell_rate,
    };

    let mut cash = config.initial_capital;
    // 帶正負號的持倉數量（基礎幣，例如 BTC）：正做多、負做空。
    let mut qty = Fixed::ZERO;
    // 上一根收盤算出、還沒成交的目標部位。第一根之前沒有目標，所以是 None。
    let mut pending: Option<TargetPosition> = None;
    let mut curve = Vec::with_capacity(bars.len());
    let mut trades = 0usize;
    let mut liquidations = 0usize;

    for (index, bar) in bars.iter().enumerate() {
        if index > 0 && bar.open_time <= bars[index - 1].open_time {
            return Err(BacktestError::NonMonotonicTime { index });
        }
        // 開盤價只有在「要成交」或「手上有倉位」時才用得到；第一根永遠不成交，
        // 它的開盤價用不到，所以不檢查也不會拿它算出錯誤的成交。
        if (pending.is_some() || !qty.is_zero()) && bar.open <= Fixed::ZERO {
            return Err(BacktestError::NonPositivePrice { index });
        }

        // ① 資金費：上一根開盤到這一根開盤之間跨過幾個結算點就收付幾次。
        //    正費率、做多（數量為正）→ 付出為正 → 現金變少；做空反過來。
        if !qty.is_zero() && !config.funding_rate.is_zero() && index > 0 {
            let periods = funding_periods(bars[index - 1].open_time, bar.open_time);
            if periods > 0 {
                let times = Fixed::from_int(periods).ok_or(BacktestError::Overflow { index })?;
                let payment = qty
                    .checked_mul(bar.open)
                    .and_then(|notional| notional.checked_mul(config.funding_rate))
                    .and_then(|per_period| per_period.checked_mul(times))
                    .ok_or(BacktestError::Overflow { index })?;
                cash = cash
                    .checked_sub(payment)
                    .ok_or(BacktestError::Overflow { index })?;
            }
        }

        // ② 強制平倉檢查（開盤價）：先付完資金費才算得準。
        let hit;
        (cash, qty, hit) = margin_call(cash, qty, bar.open, config.maintenance_margin_rate, &costs)
            .ok_or(BacktestError::Overflow { index })?;
        if hit {
            liquidations += 1;
            trades += 1;
        }

        // ③ 成交上一根留下的目標：用**這根的開盤價**，不是上一根的收盤價。
        let equity_at_open = qty
            .checked_mul(bar.open)
            .and_then(|value| cash.checked_add(value))
            .ok_or(BacktestError::Overflow { index })?;
        if equity_at_open.is_negative() {
            return Err(BacktestError::NegativeEquity { index });
        }
        if let Some(target) = pending.take() {
            // 目標名目金額 = 目標比例 × 成交前權益（帶正負號：正做多、負做空、>1 是槓桿）。
            let budget = target
                .ratio()
                .checked_mul(equity_at_open)
                .ok_or(BacktestError::Overflow { index })?;
            // 先用沒加滑價的開盤價估一次，只為了知道這一筆是買還是賣。
            let provisional = budget
                .checked_div(bar.open)
                .ok_or(BacktestError::Overflow { index })?;
            if provisional != qty {
                let buying = provisional > qty;
                let (fill, rate) = costs
                    .quote(bar.open, buying)
                    .ok_or(BacktestError::Overflow { index })?;
                if fill <= Fixed::ZERO {
                    return Err(BacktestError::NonPositivePrice { index });
                }
                // 「每一顆連手續費的總成本」：先算它才知道這筆預算吃得下多少，
                // 順序反了會買到手續費付不出來。槓桿 1 倍、只做多時這一段
                // 會退化成 2.4 的 `現金 ÷ (成交價 × (1 + 費率))`。
                let unit_cost = Fixed::ONE
                    .checked_add(rate)
                    .and_then(|mult| fill.checked_mul(mult))
                    .ok_or(BacktestError::Overflow { index })?;
                let want = budget
                    .checked_div(unit_cost)
                    .ok_or(BacktestError::Overflow { index })?;
                let delta = want
                    .checked_sub(qty)
                    .ok_or(BacktestError::Overflow { index })?;
                // 成本讓目標數量比估算的小一點，所以「目標和現況只差一點成本」的時候，
                // 估出來的方向會和真正要成交的方向相反。這時候這一根完全不動：
                // 要成交的量比一趟成本還小，硬送一張反方向的單只是白付手續費，
                // 而且那張單的數量還是用錯邊的費率算出來的。
                if buying == (delta > Fixed::ZERO) {
                    cash = settle(cash, delta, bar.open, &costs)
                        .ok_or(BacktestError::Overflow { index })?;
                    qty = want;
                    trades += 1;
                }
            }
        }

        // ④ 評價：收盤價是市價，不加滑價。
        let price = bar.close;
        if price <= Fixed::ZERO {
            return Err(BacktestError::NonPositivePrice { index });
        }
        let hit;
        (cash, qty, hit) = margin_call(cash, qty, price, config.maintenance_margin_rate, &costs)
            .ok_or(BacktestError::Overflow { index })?;
        if hit {
            liquidations += 1;
            trades += 1;
        }
        let equity = qty
            .checked_mul(price)
            .and_then(|value| cash.checked_add(value))
            .ok_or(BacktestError::Overflow { index })?;
        if equity.is_negative() {
            return Err(BacktestError::NegativeEquity { index });
        }
        curve.push(EquityPoint {
            open_time: bar.open_time,
            equity,
        });

        // ⑤ 這根收盤才問策略，答案留到下一根開盤成交。
        // 最後一根的目標沒有下一根可以成交，迴圈結束時直接連同 `pending` 作廢。
        pending = Some(strategy.on_bar(bar));
    }

    Ok(BacktestResult {
        curve,
        trades,
        liquidations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FuturesFees;
    use crate::strategy::TargetPosition;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC，剛好是一個資金費結算點（8 小時的整數倍）。
    const T0: i64 = 1_704_067_200_000;
    const HOUR: i64 = 3_600_000;

    /// 一串只有收盤價有意義、間隔 `step` 毫秒的 K 線。
    fn bars_every(step: i64, closes: &[&str]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * step,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                }
            })
            .collect()
    }

    /// 一串只有收盤價有意義的 1 小時 K 線。
    fn bars(closes: &[&str]) -> Vec<Bar> {
        bars_every(HOUR, closes)
    }

    /// 一串 `open != close` 的 K 線：每個元素是 `(開盤價, 收盤價)`。
    ///
    /// 成交價來自**下一根的開盤**、評價價來自**這根的收盤**，兩者不同才分得出
    /// 成交時點有沒有做對。
    fn oc_bars(pairs: &[(&str, &str)]) -> Vec<Bar> {
        pairs
            .iter()
            .enumerate()
            .map(|(i, (o, c))| {
                let open = fx(o);
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * HOUR,
                    open,
                    high: open.max(close),
                    low: open.min(close),
                    close,
                    volume: 1.0,
                }
            })
            .collect()
    }

    fn equities(curve: &[EquityPoint]) -> Vec<Fixed> {
        curve.iter().map(|p| p.equity).collect()
    }

    /// 零費率、零滑價、零資金費：2.3 留下來的行為測試全部用這個設定。
    fn frictionless(capital: &str) -> BacktestConfig {
        BacktestConfig::frictionless(fx(capital))
    }

    /// Binance 現貨 VIP 0（吃單 0.1%）+ 指定滑價。
    fn with_costs(capital: &str, slippage: &str) -> BacktestConfig {
        BacktestConfig {
            fees: Some(FeeModel::spot_vip0()),
            slippage: fx(slippage),
            ..BacktestConfig::frictionless(fx(capital))
        }
    }

    /// Binance U 本位合約 VIP 0（吃單 0.05%）+ 指定滑價與資金費率。
    fn futures(capital: &str, slippage: &str, funding: &str) -> BacktestConfig {
        BacktestConfig {
            fees: Some(FeeModel::futures_vip0()),
            slippage: fx(slippage),
            funding_rate: fx(funding),
            ..BacktestConfig::frictionless(fx(capital))
        }
    }

    struct AlwaysLong;
    impl Strategy for AlwaysLong {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FULL_LONG
        }
    }

    struct AlwaysFlat;
    impl Strategy for AlwaysFlat {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FLAT
        }
    }

    /// 永遠持有同一個目標比例（負數做空、大於 1 是槓桿）。
    struct Hold(Fixed);
    impl Strategy for Hold {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::new(self.0)
        }
    }

    /// 前 `hold_bars` 根持有 `ratio`，之後空手。
    struct HoldThenFlat {
        ratio: Fixed,
        hold_bars: usize,
        seen: usize,
    }
    impl HoldThenFlat {
        fn new(ratio: &str, hold_bars: usize) -> HoldThenFlat {
            HoldThenFlat {
                ratio: fx(ratio),
                hold_bars,
                seen: 0,
            }
        }
    }
    impl Strategy for HoldThenFlat {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            self.seen += 1;
            if self.seen <= self.hold_bars {
                TargetPosition::new(self.ratio)
            } else {
                TargetPosition::FLAT
            }
        }
    }

    #[test]
    fn empty_bars_give_an_empty_curve() {
        // 沒資料不是錯誤，也不能 panic
        assert_eq!(
            run_backtest(&[], &mut AlwaysLong, &frictionless("10000")),
            Ok(BacktestResult {
                curve: vec![],
                trades: 0,
                liquidations: 0,
            })
        );
    }

    #[test]
    fn flat_strategy_keeps_equity_flat() {
        let curve = run_backtest(
            &bars(&["100", "110", "121"]),
            &mut AlwaysFlat,
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(equities(&curve), vec![fx("10000"); 3]);
    }

    #[test]
    fn entry_fills_at_the_next_open_not_at_this_close() {
        // 第 0 根：開 100 收 125，策略在收盤說做多 → 不成交，只記帳。
        // 第 1 根：開 100 → 用 100 把 10000 換成 100 顆；收 200 → 權益 100 × 200 = 20000。
        let curve = run_backtest(
            &oc_bars(&[("100", "125"), ("100", "200")]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("20000")]);
        // 舊模型（本根收盤 125 成交）會買到 10000 ÷ 125 = 80 顆，最後是 80 × 200 = 16000。
        // 兩個答案不同，這條測試才有意義。
        assert_ne!(curve[1].equity, fx("16000"));
        // 時間戳要跟著 K 線走，2.6 算年化要用
        assert_eq!(curve[1].open_time, T0 + HOUR);
    }

    #[test]
    fn exit_fills_at_the_next_open_so_the_gap_counts() {
        // 第 0 根（開 100 收 100）：說做多 → 待成交。
        // 第 1 根（開 100 收 200）：開盤 100 買 100 顆，收盤權益 20000；收盤說空手 → 待成交。
        // 第 2 根（開 150 收 300）：開盤 150 賣掉 → 現金 15000，之後漲到 300 都與我無關。
        //
        // 賣在 150 不是賣在第 1 根的收盤 200：決定與成交之間隔著一次跳空，
        // 這 5000 的價差就是「訊號價不等於成交價」的實測。
        let curve = run_backtest(
            &oc_bars(&[("100", "100"), ("100", "200"), ("150", "300")]),
            &mut HoldThenFlat::new("1", 1),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("20000"), fx("15000")]
        );
    }

    #[test]
    fn a_single_bar_never_trades() {
        // 只有一根：收盤算出的目標沒有下一根可以成交，直接作廢。
        // 所以即使策略喊做多、價格從 100 衝到 500，權益仍然是起始資金（而且不 panic）。
        let result = run_backtest(
            &oc_bars(&[("100", "500")]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap();
        assert_eq!(
            result.curve,
            vec![EquityPoint {
                open_time: T0,
                equity: fx("10000"),
            }]
        );
        assert_eq!(result.trades, 0);
    }

    #[test]
    fn all_in_long_compounds_price_moves() {
        // 第 0 根只觀察，第 1 根開盤 100 買進，之後每根漲 10%：
        // 10000 → 10000 → 11000 → 12100
        let curve = run_backtest(
            &bars(&["100", "100", "110", "121"]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("11000"), fx("12100")]
        );
        // 手算對照：10000 × 1.1 × 1.1
        let by_hand = fx("10000")
            .checked_mul(fx("1.1"))
            .unwrap()
            .checked_mul(fx("1.1"))
            .unwrap();
        assert_eq!(curve.last().unwrap().equity, by_hand);
    }

    #[test]
    fn selling_locks_in_the_gain() {
        // 第 1 根開盤 100 買進、第 3 根開盤 110 賣出（決定是在第 2 根收盤下的），
        // 之後崩到 50 也不再影響權益
        let curve = run_backtest(
            &bars(&["100", "100", "110", "110", "50"]),
            &mut HoldThenFlat::new("1", 2),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![
                fx("10000"),
                fx("10000"),
                fx("11000"),
                fx("11000"),
                fx("11000")
            ]
        );
    }

    #[test]
    fn rounding_leftover_stays_in_cash() {
        // 第 1 根開盤 3 買進：10000 ÷ 3 = 3333.33333333（8 位），
        // 買不完的 0.00000001 留在現金，所以成交當根的權益仍然剛好是起始資金
        let curve = run_backtest(
            &bars(&["3", "3", "6"]),
            &mut AlwaysLong,
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(curve[1].equity, fx("10000"));
        // 價格翻倍：3333.33333333 × 6 + 0.00000001 = 19999.99999999
        assert_eq!(curve[2].equity, fx("19999.99999999"));
    }

    #[test]
    fn non_positive_capital_is_rejected() {
        assert_eq!(
            run_backtest(
                &bars(&["100"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(Fixed::ZERO)
            ),
            Err(BacktestError::NonPositiveCapital)
        );
        assert_eq!(
            run_backtest(
                &bars(&["100"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(fx("-1"))
            ),
            Err(BacktestError::NonPositiveCapital)
        );
    }

    #[test]
    fn non_positive_price_is_rejected() {
        // 評價用的收盤價
        let mut bad_close = bars(&["100", "110"]);
        bad_close[1].close = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_close, &mut AlwaysLong, &frictionless("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 成交用的開盤價（第 0 根收盤的目標要在這裡成交）
        let mut bad_open = bars(&["100", "110"]);
        bad_open[1].open = Fixed::ZERO;
        assert_eq!(
            run_backtest(&bad_open, &mut AlwaysLong, &frictionless("10000")),
            Err(BacktestError::NonPositivePrice { index: 1 })
        );
        // 第一根永遠不成交，它的開盤價用不到，所以不檢查也不會拿它算出錯誤的成交
        let mut zero_first_open = bars(&["100", "110"]);
        zero_first_open[0].open = Fixed::ZERO;
        assert!(run_backtest(&zero_first_open, &mut AlwaysLong, &frictionless("10000")).is_ok());
    }

    #[test]
    fn extreme_numbers_return_an_error_instead_of_panicking() {
        // 天價資金 ÷ 極小價格 = 數量爆掉，要回錯誤而不是溢位 panic。
        // 成交發生在第 1 根的開盤，所以出錯的是 index 1（2.2 是 index 0）。
        assert_eq!(
            run_backtest(
                &bars(&["0.00000001", "0.00000001"]),
                &mut AlwaysLong,
                &BacktestConfig::frictionless(Fixed::from_raw(i64::MAX))
            ),
            Err(BacktestError::Overflow { index: 1 })
        );
    }

    /// 迴歸基準：零費率 + 零滑價必須完整複製 2.3 的數字。
    ///
    /// 成本一打開，「成交當根的權益 = 起始資金」就不再成立，光看數字變了沒辦法
    /// 分辨是真的扣了成本、還是記帳邏輯壞了。所以這條測試要一直留著：它固定住
    /// 「成本歸零時的行為」，任何讓它變色的改動都是記帳邏輯本身出了問題。
    #[test]
    fn zero_cost_reproduces_the_2_3_numbers() {
        let data = oc_bars(&[("100", "100"), ("100", "200"), ("150", "300")]);
        // 和 exit_fills_at_the_next_open_so_the_gap_counts 同一組資料、同一組答案
        let curve = run_backtest(
            &data,
            &mut HoldThenFlat::new("1", 1),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("20000"), fx("15000")]
        );

        // 同一份資料加上成本，答案一定變差；兩者相同就表示成本根本沒被算進去
        let with_cost = run_backtest(
            &data,
            &mut HoldThenFlat::new("1", 1),
            &with_costs("10000", "0.0005"),
        )
        .unwrap()
        .curve;
        assert!(
            with_cost.last().unwrap().equity < fx("15000"),
            "含成本的權益 {} 應該比無成本的 15000 低",
            with_cost.last().unwrap().equity
        );
    }

    #[test]
    fn slippage_alone_makes_you_buy_dearer() {
        // 滑價 0.05%：開盤價 100 的買單成交在 100 × 1.0005 = 100.05。
        // 起始資金剛好 10005 = 100.05 × 100，所以買到 100 顆、現金歸零。
        // 收盤市價還是 100（評價不加滑價），權益 100 × 100 = 10000，
        // 也就是一買進就先少了 5 = 10000 × 0.05%。
        let cfg = BacktestConfig {
            slippage: fx("0.0005"),
            ..BacktestConfig::frictionless(fx("10005"))
        };
        let curve = run_backtest(&bars(&["100", "100"]), &mut AlwaysLong, &cfg)
            .unwrap()
            .curve;
        assert_eq!(equities(&curve), vec![fx("10005"), fx("10000")]);
    }

    #[test]
    fn fee_alone_is_charged_on_the_notional() {
        // 吃單 0.1%、無滑價：每一顆的總成本是 100 × 1.001 = 100.1，
        // 起始資金 10010 剛好買 100 顆 → 貨款 10000、手續費 10、現金歸零。
        // 收盤權益 10000，少掉的 10 就是 10000 × 0.1%。
        let cfg = BacktestConfig {
            fees: Some(FeeModel::spot_vip0()),
            ..BacktestConfig::frictionless(fx("10010"))
        };
        let curve = run_backtest(&bars(&["100", "100"]), &mut AlwaysLong, &cfg)
            .unwrap()
            .curve;
        assert_eq!(equities(&curve), vec![fx("10010"), fx("10000")]);
    }

    /// 手算一趟完整的來回：價格完全不動，成本自己就能把帳戶吃掉 30。
    ///
    /// 吃單 0.1%、滑價 0.05%、起始資金 10015.005（挑這個數字是為了讓數量剛好 100 顆，
    /// 不用跟四捨五入的零頭糾纏），四根 K 線的開盤與收盤都是 100。
    ///
    /// | K 線 | 這根做的事 | 手算 | 權益 |
    /// |---|---|---|---|
    /// | 0 | 收盤說做多 → 記著 | — | 10015.005 |
    /// | 1 | 買：成交價 100 × 1.0005 = 100.05；每顆總成本 100.05 × 1.001 = 100.15005；數量 10015.005 ÷ 100.15005 = 100；貨款 10005、手續費 10.005 → 現金 0 | 0 + 100 × 100 | 10000 |
    /// | 2 | 賣：成交價 100 × 0.9995 = 99.95；收入 9995、手續費 9.995 → 現金 9985.005 | — | 9985.005 |
    /// | 3 | 空手 | — | 9985.005 |
    ///
    /// 來回總成本 10015.005 − 9985.005 = 30，四筆拆開剛好對得起來：
    /// 買滑價 5 + 買手續費 10.005 + 賣滑價 5 + 賣手續費 9.995 = 30。
    #[test]
    fn a_round_trip_on_a_flat_market_costs_exactly_the_four_charges() {
        let curve = run_backtest(
            &bars(&["100", "100", "100", "100"]),
            &mut HoldThenFlat::new("1", 1),
            &with_costs("10015.005", "0.0005"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10015.005"), fx("10000"), fx("9985.005"), fx("9985.005")]
        );
        // 四筆費用各自對得起來
        let buy_slip = fx("10000").checked_mul(fx("0.0005")).unwrap(); // 5
        let buy_fee = fx("10005").checked_mul(fx("0.001")).unwrap(); // 10.005
        let sell_slip = fx("10000").checked_mul(fx("0.0005")).unwrap(); // 5
        let sell_fee = fx("9995").checked_mul(fx("0.001")).unwrap(); // 9.995
        let total = [buy_slip, buy_fee, sell_slip, sell_fee]
            .into_iter()
            .fold(Fixed::ZERO, |a, b| a.checked_add(b).unwrap());
        assert_eq!(total, fx("30"));
        assert_eq!(fx("10015.005").checked_sub(curve[2].equity), Some(fx("30")));
    }

    #[test]
    fn awkward_prices_still_cost_only_slippage_and_fee() {
        // 價格難整除（3）、費率與滑價都不是好數字：現金不可以被扣成負數，
        // 否則就是「買了付不出手續費」，帳上會多出一筆不存在的錢。
        //
        // 順便固定住「1 倍滿倉不會每根都調倉」：全部資金都換成部位之後現金只剩
        // 四捨五入的零頭，名目金額永遠等於權益，所以第 2 根算出來的目標和現況
        // 一模一樣、完全不成交——權益因此和第 1 根相等。
        let cfg = BacktestConfig {
            fees: Some(FeeModel::spot_vip0()),
            slippage: fx("0.00037"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let curve = run_backtest(&bars(&["3", "3", "3"]), &mut AlwaysLong, &cfg)
            .unwrap()
            .curve;
        // 買在 3 × 1.00037 = 3.00111，收盤評價回 3，所以權益一定比起始資金低
        assert!(curve[1].equity < fx("10000"));
        // 低的幅度不該超過滑價 + 手續費（10000 × (0.00037 + 0.001) 約 13.7）
        assert!(curve[1].equity > fx("9986"));
        assert_eq!(curve[1].equity, curve[2].equity);
    }

    /// 要成交的量比一趟成本還小時，這一根完全不動。
    ///
    /// 策略先要 0.999 倍、下一根改成 1 倍：目標名目金額只多了 0.1%，
    /// 但一趟成本（滑價 0.05% + 吃單 0.1%）是 0.15%，含成本的目標數量
    /// 反而比手上的還少。這種「估出來要買、算完卻要賣」的情況只能放棄調倉，
    /// 不然就是白付一次手續費去買一個用錯邊費率算出來的數量。
    ///
    /// 價格全程 100，所以只要權益在第 2、3 根之間沒動，就證明真的沒成交。
    #[test]
    fn a_trade_smaller_than_the_round_trip_cost_is_skipped() {
        struct NudgeUp(usize);
        impl Strategy for NudgeUp {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                self.0 += 1;
                TargetPosition::new(if self.0 <= 1 { fx("0.999") } else { fx("1") })
            }
        }
        let curve = run_backtest(
            &bars(&["100", "100", "100", "100"]),
            &mut NudgeUp(0),
            &with_costs("10000", "0.0005"),
        )
        .unwrap()
        .curve;
        // 第 1 根建了 0.999 倍的倉，第 2、3 根都放棄調倉 → 權益完全不動
        assert_eq!(curve[1].equity, curve[2].equity);
        assert_eq!(curve[2].equity, curve[3].equity);
        // 而且確實建了倉（不是整場空手）：0.999 倍的倉位付了一次進場成本
        assert!(curve[1].equity < fx("10000"), "{}", curve[1].equity);
    }

    #[test]
    fn invalid_slippage_is_rejected() {
        let bad = |s: &str| BacktestConfig {
            slippage: fx(s),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        // 負滑價等於「每筆都買便宜賣貴」，回測會憑空生錢
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("-0.0001")),
            Err(BacktestError::InvalidSlippage)
        );
        // 滑價 100% 會讓賣出價變成 0
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("1")),
            Err(BacktestError::InvalidSlippage)
        );
    }

    #[test]
    fn invalid_taker_rate_is_rejected() {
        let with_taker = |taker: &str| BacktestConfig {
            fees: Some(FeeModel::Futures(FuturesFees {
                maker: fx("-0.00005"), // 掛單返佣是真的存在，不該被這個檢查擋到
                taker: fx(taker),
                bnb_discount: None,
            })),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        // 吃單返佣不存在：負費率會讓回測以為交易本身就能賺錢
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("-0.0001")),
            Err(BacktestError::InvalidFeeRate)
        );
        // 費率 100% 表示一筆成交把本金全部吃掉，一定是填錯（例如把 0.1% 寫成 100）
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("1")),
            Err(BacktestError::InvalidFeeRate)
        );
        // 正常的吃單費率照跑
        assert!(run_backtest(&bars(&["100"]), &mut AlwaysLong, &with_taker("0.0005")).is_ok());
    }

    #[test]
    fn works_through_a_boxed_strategy() {
        // 之後的策略庫會拿 Box<dyn Strategy> 跑同一份資料
        let mut boxed: Box<dyn Strategy> = Box::new(AlwaysLong);
        let curve = run_backtest(
            &bars(&["100", "100", "200"]),
            boxed.as_mut(),
            &frictionless("1000"),
        )
        .unwrap()
        .curve;
        assert_eq!(equities(&curve), vec![fx("1000"), fx("1000"), fx("2000")]);
    }

    // ── 2.5 做空 ──────────────────────────────────────────────────────

    /// 做空：先賣後買，價格跌就賺。
    ///
    /// | K 線 | 開盤做的事 | 現金 | 數量 | 收盤權益 |
    /// |---|---|---|---|---|
    /// | 0 (100) | 收盤說做空 1 倍 | 10000 | 0 | 10000 |
    /// | 1 (100) | 賣 100 顆 @100 → 現金 +10000 | 20000 | −100 | 20000 − 10000 = 10000 |
    /// | 2 (50) | 價格腰斬，權益 15000；1 倍做空的名目金額要跟著變成 15000 → 再賣 200 顆 @50 | 30000 | −300 | 30000 − 15000 = 15000 |
    #[test]
    fn a_short_profits_when_the_price_falls() {
        let curve = run_backtest(
            &bars(&["100", "100", "50"]),
            &mut Hold(fx("-1")),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("15000")]
        );
        // 2.4 把做空訊號當成空手，會停在 10000；現在是真的反向獲利
        assert_ne!(curve[2].equity, fx("10000"));
    }

    #[test]
    fn a_short_loses_when_the_price_rises() {
        // 賣 100 顆 @100（現金 20000），漲到 110 → 權益 20000 − 11000 = 9000
        let curve = run_backtest(
            &bars(&["100", "100", "110"]),
            &mut Hold(fx("-1")),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000"), fx("9000")]);
    }

    /// 平掉空頭是**買**，所以要買貴、付買方費率——不是賣賤。
    ///
    /// 滑價 0.05%、零手續費、起始資金 9995（挑這個數字讓數量剛好 100 顆）：
    ///
    /// | K 線 | 開盤做的事 | 現金 | 數量 | 收盤權益 |
    /// |---|---|---|---|---|
    /// | 0 (100) | 收盤說做空 | 9995 | 0 | 9995 |
    /// | 1 (100) | 開空＝賣：成交價 100 × 0.9995 = 99.95，數量 9995 ÷ 99.95 = 100 | 19990 | −100 | 19990 − 10000 = 9990 |
    /// | 2 (100) | 平空＝買：成交價 100 × 1.0005 = 100.05，付 10005 | 9985 | 0 | 9985 |
    ///
    /// 價格從頭到尾是 100，一趟來回被滑價吃掉 10 = 兩次 10000 × 0.05%。
    /// 方向寫反（平空也用賣價）會變成 19990 − 9995 = 9995，等於憑空多賺 10。
    #[test]
    fn closing_a_short_pays_the_buy_side_slippage() {
        let cfg = BacktestConfig {
            slippage: fx("0.0005"),
            ..BacktestConfig::frictionless(fx("9995"))
        };
        let curve = run_backtest(
            &bars(&["100", "100", "100"]),
            &mut HoldThenFlat::new("-1", 1),
            &cfg,
        )
        .unwrap()
        .curve;
        assert_eq!(equities(&curve), vec![fx("9995"), fx("9990"), fx("9985")]);
        assert_ne!(curve[2].equity, fx("9995"));
    }

    #[test]
    fn flipping_from_long_to_short_is_one_trade() {
        // 做多 100 顆後直接翻空 100 顆：一筆賣出 200 顆，手續費照 200 顆的名目金額算。
        // 零成本時：第 1 根買 100 顆 @100（現金 0），第 2 根翻空 → 賣 200 顆 @100
        // → 現金 20000、數量 −100 → 權益 20000 − 10000 = 10000。
        struct LongThenShort(usize);
        impl Strategy for LongThenShort {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                self.0 += 1;
                if self.0 <= 1 {
                    TargetPosition::FULL_LONG
                } else {
                    TargetPosition::short(Fixed::ONE)
                }
            }
        }
        let curve = run_backtest(
            &bars(&["100", "100", "100", "50"]),
            &mut LongThenShort(0),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        // 第 3 根價格腰斬：空頭賺 5000（第 2 根翻空後的名目金額是 10000）
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("10000"), fx("15000")]
        );
    }

    // ── 2.5 槓桿 ──────────────────────────────────────────────────────

    /// 2 倍槓桿：價格漲 10%，權益漲 20%。
    ///
    /// 第 1 根開盤 100 買 200 顆 → 現金 10000 − 20000 = −10000（負現金就是跟交易所借的錢），
    /// 權益 −10000 + 200 × 100 = 10000。第 2 根開盤仍然是 100，名目金額還是 2 倍權益，
    /// 所以不用調倉；收盤 110 → 權益 −10000 + 200 × 110 = 12000。
    #[test]
    fn two_times_leverage_doubles_the_move() {
        let data = oc_bars(&[("100", "100"), ("100", "100"), ("100", "110")]);
        let levered = run_backtest(&data, &mut Hold(fx("2")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(
            equities(&levered),
            vec![fx("10000"), fx("10000"), fx("12000")]
        );
        // 同一份資料、1 倍槓桿只漲 10%
        let plain = run_backtest(&data, &mut AlwaysLong, &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(plain.last().unwrap().equity, fx("11000"));
    }

    /// 槓桿放大的是名目金額，手續費跟著名目金額走：2 倍槓桿的手續費剛好是 2 倍。
    ///
    /// 合約吃單 0.05%、無滑價、起始資金 10000、價格全程 100：
    /// 1 倍買 10000 的名目，手續費 4.99750125；2 倍買 20000 的名目，手續費 9.9950025。
    /// 兩者相除剛好是 2——費率一個字都沒改。
    #[test]
    fn leverage_multiplies_the_fee_not_the_rate() {
        let cfg = |capital: &str| BacktestConfig {
            fees: Some(FeeModel::futures_vip0()),
            ..BacktestConfig::frictionless(fx(capital))
        };
        let data = bars(&["100", "100"]);
        let one = run_backtest(&data, &mut AlwaysLong, &cfg("10000"))
            .unwrap()
            .curve;
        let two = run_backtest(&data, &mut Hold(fx("2")), &cfg("10000"))
            .unwrap()
            .curve;
        let cost_one = fx("10000").checked_sub(one[1].equity).unwrap();
        let cost_two = fx("10000").checked_sub(two[1].equity).unwrap();
        assert_eq!(cost_one, fx("4.99750125"));
        assert_eq!(cost_two, fx("9.9950025"));
        assert_eq!(cost_one.checked_mul(fx("2")), Some(cost_two));
    }

    #[test]
    fn a_levered_long_rebalances_back_to_the_target_leverage() {
        // 2 倍做多，第 2 根開盤漲到 110：權益 12000、名目金額只剩 22000（1.83 倍），
        // 所以要再買到 2 × 12000 ÷ 110 = 218.18181818 顆才回到 2 倍。
        // 零成本時調倉不影響權益，只是把槓桿補回去。
        let data = oc_bars(&[("100", "100"), ("100", "100"), ("110", "110")]);
        let curve = run_backtest(&data, &mut Hold(fx("2")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("12000")]
        );
    }

    // ── 2.5 資金費 ────────────────────────────────────────────────────

    /// 資金費率為正：多頭付錢、空頭收錢。
    ///
    /// 8 小時 K 線、價格全程 100、費率 0.01%、零交易成本。
    /// 第 1 根開盤建倉（名目 10000），第 2 根開盤跨過一個結算點：
    /// 多頭付 10000 × 0.0001 = 1，空頭收 1。
    #[test]
    fn positive_funding_moves_money_from_longs_to_shorts() {
        let data = bars_every(8 * HOUR, &["100", "100", "100"]);
        let cfg = BacktestConfig {
            funding_rate: fx("0.0001"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let long = run_backtest(&data, &mut AlwaysLong, &cfg).unwrap().curve;
        let short = run_backtest(&data, &mut Hold(fx("-1")), &cfg)
            .unwrap()
            .curve;
        assert_eq!(equities(&long), vec![fx("10000"), fx("10000"), fx("9999")]);
        assert_eq!(
            equities(&short),
            vec![fx("10000"), fx("10000"), fx("10001")]
        );
        // 多頭付的剛好等於空頭收的
        assert_eq!(
            fx("10000").checked_sub(long[2].equity),
            short[2].equity.checked_sub(fx("10000"))
        );
    }

    #[test]
    fn negative_funding_pays_the_longs() {
        // 費率為負時反過來：多頭收錢
        let data = bars_every(8 * HOUR, &["100", "100", "100"]);
        let cfg = BacktestConfig {
            funding_rate: fx("-0.0001"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let curve = run_backtest(&data, &mut AlwaysLong, &cfg).unwrap().curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("10000"), fx("10001")]
        );
    }

    #[test]
    fn funding_is_charged_per_eight_hours_not_per_bar() {
        // 1 小時 K 線、費率誇張地放大到 1%：從 T0 開始的 9 根裡只有第 8 根
        // （開盤時間 T0 + 8 小時）跨過結算點，所以只收一次 10000 × 1% = 100。
        // 每根都收的話會變成 10000 × (1 − 1%)^7，差超過 600。
        let data = bars(&["100"; 9]);
        let cfg = BacktestConfig {
            funding_rate: fx("0.01"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let curve = run_backtest(&data, &mut AlwaysLong, &cfg).unwrap().curve;
        let expected: Vec<Fixed> = (0..9)
            .map(|i| if i < 8 { fx("10000") } else { fx("9900") })
            .collect();
        assert_eq!(equities(&curve), expected);
    }

    #[test]
    fn a_daily_bar_pays_three_funding_periods() {
        // 日線：一根 K 線跨過 00:00、08:00、16:00 三個結算點，一次收三筆。
        // 第 1 根建倉（名目 10000），第 2 根開盤收 3 × 10000 × 0.0001 = 3。
        let data = bars_every(24 * HOUR, &["100", "100", "100"]);
        let cfg = BacktestConfig {
            funding_rate: fx("0.0001"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let curve = run_backtest(&data, &mut AlwaysLong, &cfg).unwrap().curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000"), fx("9997")]);
    }

    #[test]
    fn funding_needs_a_position() {
        // 空手的帳戶不收資金費（數量 0 → 名目金額 0）
        let data = bars_every(8 * HOUR, &["100", "100", "100"]);
        let cfg = BacktestConfig {
            funding_rate: fx("0.01"),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let curve = run_backtest(&data, &mut AlwaysFlat, &cfg).unwrap().curve;
        assert_eq!(equities(&curve), vec![fx("10000"); 3]);
    }

    // ── 2.5 強制平倉與防呆 ────────────────────────────────────────────

    /// 5 倍做多、價格跌 19.6%：權益只剩 200 就被強制平倉，之後不再跟著價格動。
    ///
    /// 第 1 根開盤 100 買 500 顆（名目 50000、現金 −40000）。
    /// 第 2 根開盤 80.4：權益 −40000 + 40200 = 200，維持保證金門檻
    /// 40200 × 0.5% = 201 → 200 ≤ 201，倉位被平掉，現金 −40000 + 40200 = 200。
    ///
    /// 第 1 根**收盤**跌到 80.4 就被平倉，第 2 根價格拉回 100 也回不來：爆倉是不可逆的。
    ///
    /// | K 線 | 開 / 收 | 這根做的事 | 現金 | 數量 | 權益 |
    /// |---|---|---|---|---|---|
    /// | 0 | 100 / 100 | 收盤說 5 倍做多 | 10000 | 0 | 10000 |
    /// | 1 | 100 / 80.4 | 開盤買 500 顆（名目 50000）；收盤權益 200 ≤ 40200 × 0.5% = 201 → 整個倉位被平掉 | 200 | 0 | 200 |
    /// | 2 | 100 / 100 | 價格回到 100，但只剩 200 可以再開 5 倍（名目 1000 = 10 顆） | −800 | 10 | 200 |
    ///
    /// 沒有強制平倉模型的話，500 顆會一路抱回 100，第 2 根的權益會完整復活成
    /// 10000——那是回測最危險的假訊號：實際上你在 80.4 就已經被交易所請出場了。
    #[test]
    fn a_levered_long_gets_liquidated_and_cannot_recover() {
        let data = oc_bars(&[("100", "100"), ("100", "80.4"), ("100", "100")]);
        let curve = run_backtest(&data, &mut Hold(fx("5")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("200"), fx("200")]);
        // 沒被平倉的話這裡會是 10000（價格回到原點）
        assert_ne!(curve[2].equity, fx("10000"));
    }

    #[test]
    fn a_levered_short_gets_liquidated_and_cannot_recover() {
        // 5 倍做空：賣 500 顆 @100（現金 60000）。收盤漲到 119.6：
        // 權益 60000 − 59800 = 200 ≤ 59800 × 0.5% = 299 → 買回 500 顆 @119.6，現金 200。
        // 第 2 根價格跌回 100（本來會大賺），但只剩 200 可以再開 5 倍空。
        let data = oc_bars(&[("100", "100"), ("100", "119.6"), ("100", "100")]);
        let curve = run_backtest(&data, &mut Hold(fx("-5")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("200"), fx("200")]);
        assert_ne!(curve[2].equity, fx("10000"));
    }

    #[test]
    fn a_gap_through_the_liquidation_price_leaves_zero_not_a_negative_equity() {
        // 5 倍做多，價格直接跳空 30%：帳面權益會是 −40000 + 35000 = −5000。
        // 真實世界不會讓你倒欠交易所（保險基金吃掉），所以現金歸零、權益是 0。
        let data = oc_bars(&[("100", "100"), ("100", "100"), ("70", "70")]);
        let curve = run_backtest(&data, &mut Hold(fx("5")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(equities(&curve), vec![fx("10000"), fx("10000"), fx("0")]);
        // 爆倉後帳戶剩 0，之後怎麼漲都回不來
        let longer = oc_bars(&[("100", "100"), ("100", "100"), ("70", "70"), ("200", "200")]);
        let curve = run_backtest(&longer, &mut Hold(fx("5")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert_eq!(curve.last().unwrap().equity, Fixed::ZERO);
    }

    #[test]
    fn without_a_margin_model_a_negative_equity_is_an_error_not_a_number() {
        // 關掉強制平倉的純記帳模式：同一份資料會算出 −5000 的權益。
        // 那是假的（真實世界早就被平倉了），所以回錯誤，不准繼續往下跑。
        let cfg = BacktestConfig {
            maintenance_margin_rate: None,
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let data = oc_bars(&[("100", "100"), ("100", "100"), ("70", "70")]);
        assert_eq!(
            run_backtest(&data, &mut Hold(fx("5")), &cfg),
            Err(BacktestError::NegativeEquity { index: 2 })
        );
    }

    #[test]
    fn liquidation_pays_slippage_and_fee_like_any_other_fill() {
        // 強制平倉是市價單，一樣要付滑價與吃單費，所以剩下的錢比零成本時少。
        let data = oc_bars(&[("100", "100"), ("100", "80.4"), ("100", "100")]);
        let cfg = futures("10000", "0.0005", "0");
        let with_cost = run_backtest(&data, &mut Hold(fx("5")), &cfg).unwrap().curve;
        let free = run_backtest(&data, &mut Hold(fx("5")), &frictionless("10000"))
            .unwrap()
            .curve;
        assert!(
            with_cost[1].equity < free[1].equity,
            "含成本的爆倉殘值 {} 應該比零成本的 {} 少",
            with_cost[1].equity,
            free[1].equity
        );
        assert!(!with_cost[1].equity.is_negative());
    }

    #[test]
    fn invalid_funding_rate_is_rejected() {
        let bad = |r: &str| BacktestConfig {
            funding_rate: fx(r),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        // 把 0.75% 寫成 75 這種錯誤要當場擋下來
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("75")),
            Err(BacktestError::InvalidFundingRate)
        );
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("-1")),
            Err(BacktestError::InvalidFundingRate)
        );
        // 負費率是真的存在（空頭付錢給多頭），不該被擋
        assert!(run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("-0.0075")).is_ok());
    }

    #[test]
    fn invalid_maintenance_margin_is_rejected() {
        let bad = |r: &str| BacktestConfig {
            maintenance_margin_rate: Some(fx(r)),
            ..BacktestConfig::frictionless(fx("10000"))
        };
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("-0.001")),
            Err(BacktestError::InvalidMaintenanceMargin)
        );
        // 100% 表示「權益低於名目金額就平倉」，1 倍做多也會當場被平掉，一定是填錯
        assert_eq!(
            run_backtest(&bars(&["100"]), &mut AlwaysLong, &bad("1")),
            Err(BacktestError::InvalidMaintenanceMargin)
        );
    }

    #[test]
    fn non_monotonic_time_is_rejected() {
        // 資金費是按時間戳算的：時間倒退會算出負的結算次數，等於反過來收錢
        let mut data = bars(&["100", "100", "100"]);
        data[2].open_time = data[1].open_time;
        assert_eq!(
            run_backtest(&data, &mut AlwaysLong, &frictionless("10000")),
            Err(BacktestError::NonMonotonicTime { index: 2 })
        );
    }

    /// 迴歸基準（2.4 作者指定）：槓桿 1 倍、只做多、資金費率 0 時，
    /// 合約的記帳必須退化成 2.4 的現貨記帳，數字一個字都不能差。
    ///
    /// 兩組數字都直接抄自 `docs/steps/2.4-手續費與滑價.md`：
    /// 零成本的 `[10000, 20000, 15000]`，含成本的 `[10015.005, 10000, 9985.005, 9985.005]`。
    #[test]
    fn unit_leverage_long_only_reproduces_the_2_4_numbers() {
        let gapped = oc_bars(&[("100", "100"), ("100", "200"), ("150", "300")]);
        let curve = run_backtest(
            &gapped,
            &mut HoldThenFlat::new("1", 1),
            &frictionless("10000"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10000"), fx("20000"), fx("15000")]
        );

        let flat = bars(&["100", "100", "100", "100"]);
        let curve = run_backtest(
            &flat,
            &mut HoldThenFlat::new("1", 1),
            &with_costs("10015.005", "0.0005"),
        )
        .unwrap()
        .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10015.005"), fx("10000"), fx("9985.005"), fx("9985.005")]
        );

        // 資金費率 0 的合約設定跑同一份資料，答案只差在費率本身（合約吃單比現貨便宜），
        // 記帳路徑完全一樣：把費率調成現貨的 0.1% 就會回到同一組數字。
        let as_futures = BacktestConfig {
            fees: Some(FeeModel::Futures(FuturesFees {
                maker: fx("0.001"),
                taker: fx("0.001"),
                bnb_discount: None,
            })),
            slippage: fx("0.0005"),
            ..BacktestConfig::frictionless(fx("10015.005"))
        };
        let curve = run_backtest(&flat, &mut HoldThenFlat::new("1", 1), &as_futures)
            .unwrap()
            .curve;
        assert_eq!(
            equities(&curve),
            vec![fx("10015.005"), fx("10000"), fx("9985.005"), fx("9985.005")]
        );
    }

    // ── 2.6 成交統計 ──────────────────────────────────────────────────

    #[test]
    fn a_round_trip_counts_two_trades() {
        // 第 1 根開盤買、第 2 根開盤賣，之後空手不再送單
        let result = run_backtest(
            &bars(&["100", "100", "100", "100"]),
            &mut HoldThenFlat::new("1", 1),
            &with_costs("10000", "0.0005"),
        )
        .unwrap();
        assert_eq!(result.trades, 2);
        assert_eq!(result.liquidations, 0);
    }

    #[test]
    fn a_flat_strategy_never_sends_an_order() {
        let result = run_backtest(
            &bars(&["100", "110", "121"]),
            &mut AlwaysFlat,
            &frictionless("10000"),
        )
        .unwrap();
        assert_eq!(result.trades, 0);
    }

    /// 放棄調倉的那一根不算成交——沒送單就是沒送單。
    ///
    /// 和 `a_trade_smaller_than_the_round_trip_cost_is_skipped` 同一組設定：
    /// 第 1 根建 0.999 倍的倉（一筆），第 2、3 根的調倉都比一趟成本還小而被放棄。
    #[test]
    fn a_skipped_rebalance_is_not_counted_as_a_trade() {
        struct NudgeUp(usize);
        impl Strategy for NudgeUp {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                self.0 += 1;
                TargetPosition::new(if self.0 <= 1 { fx("0.999") } else { fx("1") })
            }
        }
        let result = run_backtest(
            &bars(&["100", "100", "100", "100"]),
            &mut NudgeUp(0),
            &with_costs("10000", "0.0005"),
        )
        .unwrap();
        assert_eq!(result.trades, 1);
    }

    /// 強制平倉算一筆成交，也單獨記一次爆倉。
    ///
    /// 同 `a_levered_long_gets_liquidated_and_cannot_recover` 的三根 K 線：
    /// 第 1 根開盤建倉（第 1 筆）、第 1 根收盤被平掉（第 2 筆，同時是第 1 次爆倉）、
    /// 第 2 根開盤拿殘值重開 5 倍（第 3 筆）。
    ///
    /// 這就是為什麼 `liquidations` 要單獨記：`trades` 是 3、權益從 10000 掉到 200，
    /// 但只有 `liquidations == 1` 說得出「其中一筆不是策略決定的」。
    #[test]
    fn a_liquidation_is_both_a_trade_and_a_liquidation() {
        let data = oc_bars(&[("100", "100"), ("100", "80.4"), ("100", "100")]);
        let result = run_backtest(&data, &mut Hold(fx("5")), &frictionless("10000")).unwrap();
        assert_eq!(result.liquidations, 1);
        assert_eq!(result.trades, 3);
    }
}
