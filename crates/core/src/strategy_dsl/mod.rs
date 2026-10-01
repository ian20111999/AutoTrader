//! 使用者自訂策略的中介表示（Strategy DSL）與執行引擎。
//!
//! 一份自訂策略是一棵**純資料**的條件樹：只能表示「指標、比較、AND/OR」，
//! 沒有迴圈、沒有函式呼叫、沒有 I/O。[`StrategyAst::compile`] 把它變成一個
//! [`CustomStrategy`]，而 `CustomStrategy` 直接實作
//! [`Strategy`](crate::strategy::Strategy)，所以回測、模擬交易、測試網交易
//! **跑的是同一份執行引擎**，一行都不用改。
//!
//! ```
//! use at_core::strategy_dsl::StrategyAst;
//! use at_core::Strategy;
//!
//! let json = r#"{
//!   "schemaVersion": 1,
//!   "direction": "long_only",
//!   "sizing": { "positionPct": "100", "leverage": "1" },
//!   "longEntry": {
//!     "kind": "gt",
//!     "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
//!     "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
//!   },
//!   "longExit": {
//!     "kind": "lte",
//!     "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
//!     "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
//!   }
//! }"#;
//! let ast: StrategyAst = serde_json::from_str(json).unwrap();
//! let strategy = ast.compile().unwrap();
//! assert_eq!(strategy.warmup_bars(), 50);
//! ```
//!
//! ## 安全邊界（不可違反）
//!
//! **這個模組唯一的輸出是 [`TargetPosition`](crate::strategy::TargetPosition)。**
//! 它沒有、也永遠不會有任何下單能力：
//!
//! 1. 求值函式的簽名是「一根 K 線 + 自己的狀態 → 一個數字或布林」，裡面沒有
//!    任何可以產生副作用的東西（沒有帳戶、沒有 client、沒有 channel、
//!    碰不到檔案系統、碰不到網路）。它連「現在幾點」都不知道。
//!    這一條由 `tests::the_dsl_cannot_reach_the_network_the_filesystem_or_an_exchange`
//!    掃原始碼鎖住。
//! 2. `Node` 不可以有任何帶「動作」語意的 variant（送單、撤單、通知…）。
//!    DSL 是一個算式求值器，不是腳本語言。使用者想要的任何「動作」都必須表達成
//!    「目標部位是多少」。
//! 3. 「目標部位 → 訂單」的轉換在別的 crate，而且要先過 `at-risk-control`。
//!    使用者在 `sizing` 裡填 125 倍槓桿不會變成 125 倍的實際下單——
//!    **這裡的驗證上限只是防手滑，風控層才是防線。**
//!
//! ## 數值一律用十進位字串
//!
//! `number.value`、`sizing.positionPct`、`sizing.leverage`、布林通道的 `mult`
//! 都是**字串**，不是 JSON number。JSON number 在 JS 端是 `f64`，`0.1` 會變成
//! `0.1000000000000000055…`，而這個數字會決定要不要下單。
//! 只有「根數」類的參數（`period`、`bars`、`offset`）用整數，因為它們不是價格。
//!
//! ## 命名風格（專案既有陷阱 #1）
//!
//! **欄位名用 camelCase**（`schemaVersion`、`longEntry`、`positionPct`），
//! **`kind` 的值用 snake_case**（`cross_above`）——因為它們是識別字不是欄位名。
//! 兩個規則不一樣，由 `serialization` 測試鎖住。

mod eval;
/// 指標狀態機。`pub(crate)` 是因為內建的 [`Rsi`](crate::strategies::Rsi) 策略
/// 和 DSL 的 `rsi` 節點共用同一份 RSI 計算核心。
pub(crate) mod indicators;

pub use eval::CustomStrategy;

use crate::fixed::Fixed;
use eval::{BbOutput, DonchianOutput, IndicatorKind, Node, Roots};
use indicators::MacdOutput;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 目前支援的 schema 版本。
pub const SCHEMA_VERSION: u16 = 1;

/// 攤平後的節點總數上限（防資源耗盡）。
pub const MAX_NODES: usize = 512;
/// 條件樹的深度上限（防遞迴走訪把堆疊吃光）。
pub const MAX_DEPTH: usize = 32;
/// 指標週期與 `sustained.bars` 的上限。
///
/// `period` 直接決定 rolling window 的長度：512 節點 × 2000 × 8 bytes ≈ 8 MB，
/// 這是記憶體用量的硬上限。
pub const MAX_PERIOD: u32 = 2000;
/// 往前位移的根數上限。
pub const MAX_OFFSET: u16 = 500;
/// 槓桿上限（只是防手滑，真正的上限由 `at-risk-control` 把關）。
pub const MAX_LEVERAGE: i64 = 125;

// ---------------------------------------------------------------------------
// AST（JSON 可序列化的純資料）
// ---------------------------------------------------------------------------

/// 一份自訂策略：四棵條件樹 + 部位設定。
///
/// 這是 `at-core` 看得到的全部。策略名稱、版本、交易對清單、保證金模式、
/// 停損設定都是 App 層的事（它們包在外層的 `StrategyDoc.ast` 裡），
/// 這條界線要守住，否則 `at-core` 會開始依賴 UI 的概念。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StrategyAst {
    /// 目前固定是 [`SCHEMA_VERSION`]。不相等時 `compile()` 會報錯，不會猜。
    pub schema_version: u16,
    pub direction: Direction,
    pub sizing: Sizing,
    /// 做多進場條件。
    pub long_entry: Cond,
    /// 做多出場條件。
    pub long_exit: Cond,
    /// 做空進場條件。`long_only` 時必須是 `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_entry: Option<Cond>,
    /// 做空出場條件。`long_only` 時必須是 `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_exit: Option<Cond>,
}

/// 方向模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// 只做多：做空的兩棵樹必須是 `null`。
    LongOnly,
    /// 多空都做：做空的兩棵樹都必填。
    LongShort,
}

/// 部位大小設定。目標部位 = `positionPct / 100 × leverage`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sizing {
    /// 每次進場投入的權益百分比，`"100"` = 滿倉。範圍 `(0, 100]`。
    pub position_pct: String,
    /// 槓桿倍數，範圍 `[1, 125]`。現貨必須是 `"1"`（由 App 層把關）。
    pub leverage: String,
}

/// 數值節點：求值結果是 `Option<Fixed>`，`None` 表示暖機不足或算式溢位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    /// 價格或成交量。
    Price {
        field: PriceField,
        /// 往前位移幾根。`0` = 這根（已收盤），`1` = 前一根。
        #[serde(default, skip_serializing_if = "is_zero_u16")]
        offset: u16,
    },
    /// 常數（十進位字串）。
    Number { value: String },
    /// 指標。
    Indicator {
        name: IndicatorName,
        /// 餵給指標的數列。省略 = 收盤價。`atr`／`donchian` 不接受這個欄位。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<Box<Expr>>,
        params: IndicatorParams,
        /// 多輸出指標要選哪一路。單輸出指標不可以填。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// 取這個指標**幾根之前**的值。
        #[serde(default, skip_serializing_if = "is_zero_u16")]
        offset: u16,
    },
}

/// `Bar` 上可以讀的欄位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceField {
    Open,
    High,
    Low,
    Close,
    /// 成交量。`Bar::volume` 是 `f64`，進 DSL 時做一次確定性轉換
    /// （詳見 `eval::volume_to_fixed`）。
    Volume,
}

/// 支援的指標。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndicatorName {
    /// 簡單移動平均。參數：`period`。
    Sma,
    /// 指數移動平均（α = 2/(period+1)，SMA 種子）。參數：`period`。
    Ema,
    /// Wilder RSI，0～100。參數：`period`。
    Rsi,
    /// MACD。參數：`fast`、`slow`、`signal`；`output` 必填。
    Macd,
    /// 布林通道。參數：`period`、`mult`；`output` 必填。
    Bb,
    /// Wilder ATR，吃整根 K 線。參數：`period`。
    Atr,
    /// 唐奇安通道，吃整根 K 線的高低價。參數：`period`；`output` 必填。
    Donchian,
    /// 視窗最大值。參數：`period`。
    Highest,
    /// 視窗最小值。參數：`period`。
    Lowest,
}

/// 指標參數。
///
/// 用具名欄位而不是 `HashMap<String, _>`，這樣拼錯的參數名
/// （`periode: 14`）會在反序列化就被擋下來，不會默默套用預設值。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndicatorParams {
    /// 週期（根數）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<u32>,
    /// MACD 的快線週期。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<u32>,
    /// MACD 的慢線週期。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow: Option<u32>,
    /// MACD 的訊號線週期。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<u32>,
    /// 布林通道的標準差倍數，**十進位字串**（`"2"`、`"1.5"`）。
    ///
    /// 用字串 + [`Fixed::checked_mul`] 而不是「分子/分母兩個整數」，是為了走
    /// 和內建 [`Bollinger`](crate::strategies::Bollinger) **完全相同**的乘法
    /// 路徑（它收一個 `Fixed` 倍數）。自己另算一套先乘後除會在非整數倍數時
    /// 和內建策略差一個最小單位，而「DSL 版和 Rust 版逐根相等」是這份設計的
    /// 驗收核心。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mult: Option<String>,
}

/// 條件節點：求值結果是 `Option<bool>`，`None` 表示無法判定。
///
/// 三值邏輯（Kleene）的規則：
///
/// - 比較節點：任一邊 `None` → `None`。
/// - `cross_above`／`cross_below`：這根或前一根的比較結果是 `None` → `None`。
/// - `all`：任一子節點 `Some(false)` → `Some(false)`；否則有 `None` → `None`；否則真。
/// - `any`：任一子節點 `Some(true)` → `Some(true)`；否則有 `None` → `None`；否則假。
/// - `sustained`：視窗內任一根 `None` → `None`；全部真 → 真；否則假。
///
/// `all`／`any` 的規則是「資訊足夠就下結論」：`any` 裡只要有一個已經成立，
/// 其他還在暖機也不影響答案。
///
/// **刻意沒有 `not`**：做空條件是 UI 生成 JSON 時把 `gt` 換成 `lt`、
/// `cross_above` 換成 `cross_below`，不是在樹上包一層 NOT。
/// 少一個節點種類就少一組三值邏輯的邊界案例要測。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Cond {
    /// `left > right`
    Gt { left: Expr, right: Expr },
    /// `left >= right`
    Gte { left: Expr, right: Expr },
    /// `left < right`
    Lt { left: Expr, right: Expr },
    /// `left <= right`
    Lte { left: Expr, right: Expr },
    /// 向上穿越：**前一根** `left <= right` **且這根** `left > right`。
    CrossAbove { left: Expr, right: Expr },
    /// 向下穿越：前一根 `left >= right` 且這根 `left < right`。
    CrossBelow { left: Expr, right: Expr },
    /// AND。`children` 不可為空（空的 AND 在邏輯上是「真」，使用者絕對不是這個意思）。
    All { children: Vec<Cond> },
    /// OR。`children` 不可為空。
    Any { children: Vec<Cond> },
    /// `inner` 連續成立 `bars` 根（含這根）。
    Sustained { inner: Box<Cond>, bars: u16 },
}

fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

// ---------------------------------------------------------------------------
// 錯誤
// ---------------------------------------------------------------------------

/// 策略定義錯誤。
///
/// **帶路徑**是刻意的：使用者在一棵 30 個積木的樹裡收到「週期錯誤」會不知道
/// 要改哪一顆，所以訊息長這樣：
/// 「做多進場條件 → 第 2 個子條件 → 左側：週期必須在 1 到 2000 之間，收到 0」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DslError {
    /// 出錯的位置，從策略根節點往下描述。可能是空字串（錯在根上）。
    pub path: String,
    /// 繁體中文的錯誤說明。
    pub message: String,
}

impl fmt::Display for DslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{} ：{}", self.path, self.message)
        }
    }
}

impl std::error::Error for DslError {}

fn err(path: &str, message: impl Into<String>) -> DslError {
    DslError {
        path: path.to_string(),
        message: message.into(),
    }
}

/// 把一段路徑接在後面：`"做多進場條件" + "左側"`。
fn seg(path: &str, child: &str) -> String {
    if path.is_empty() {
        child.to_string()
    } else {
        format!("{path} → {child}")
    }
}

// ---------------------------------------------------------------------------
// compile()：信任邊界
// ---------------------------------------------------------------------------

impl StrategyAst {
    /// 把條件樹攤平成可以執行的策略。
    ///
    /// **這是信任邊界。** 策略 JSON 來自使用者的輸入框，也可能來自使用者從別處
    /// 拿到的檔案，所以所有驗證都在這裡做完（節點數、深度、週期、位移、
    /// 多輸出指標的 `output`、方向一致性、部位大小…）。
    /// `on_bar` 裡沒有任何驗證——它跑在熱路徑上，而且錯誤無處可回報。
    pub fn compile(&self) -> Result<CustomStrategy, DslError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(err(
                "",
                format!(
                    "這份策略來自不同版本（schemaVersion {}，本引擎支援 {}）",
                    self.schema_version, SCHEMA_VERSION
                ),
            ));
        }
        let size = self.sizing.compile()?;

        let mut c = Compiler { nodes: Vec::new() };
        let long_entry = c.cond(&self.long_entry, "做多進場條件", 1)?;
        let long_exit = c.cond(&self.long_exit, "做多出場條件", 1)?;
        let short = match self.direction {
            Direction::LongOnly => {
                if self.short_entry.is_some() || self.short_exit.is_some() {
                    return Err(err(
                        "",
                        "方向是「只做多」，做空的進出場條件必須留空（null）",
                    ));
                }
                None
            }
            Direction::LongShort => {
                let (Some(entry), Some(exit)) = (&self.short_entry, &self.short_exit) else {
                    return Err(err("", "方向是「多空」，做空的進場與出場條件兩棵都必填"));
                };
                let entry = c.cond(entry, "做空進場條件", 1)?;
                let exit = c.cond(exit, "做空出場條件", 1)?;
                Some((entry, exit))
            }
        };

        Ok(CustomStrategy::new(
            c.nodes,
            Roots {
                long_entry,
                long_exit,
                short,
            },
            size,
        ))
    }
}

impl Sizing {
    /// 算出目標部位的大小（`positionPct / 100 × leverage`）。
    fn compile(&self) -> Result<Fixed, DslError> {
        let pct = parse_fixed(&self.position_pct, "", "每幣部位 % 權益")?;
        let leverage = parse_fixed(&self.leverage, "", "槓桿倍數")?;
        let hundred = Fixed::from_raw(100 * Fixed::SCALE);
        if pct <= Fixed::ZERO || pct > hundred {
            return Err(err(
                "",
                format!("每幣部位 % 權益必須大於 0 且不超過 100，收到 {pct}"),
            ));
        }
        let max_leverage = Fixed::from_raw(MAX_LEVERAGE * Fixed::SCALE);
        if leverage < Fixed::ONE || leverage > max_leverage {
            return Err(err(
                "",
                format!("槓桿倍數必須在 1 到 {MAX_LEVERAGE} 之間，收到 {leverage}"),
            ));
        }
        let size = pct
            .checked_div(hundred)
            .and_then(|ratio| ratio.checked_mul(leverage))
            .ok_or_else(|| err("", "部位大小算不出來（數值超出可表示範圍）"))?;
        if size <= Fixed::ZERO {
            return Err(err(
                "",
                "部位大小算出來是 0，這份策略永遠不會進場；請調高每幣部位 % 權益",
            ));
        }
        Ok(size)
    }
}

/// 後序走訪的攤平器。子節點一定先被推進陣列，所以每個節點的子 index
/// 都比自己小——求值時單一次由前往後的掃描就保證「用到的值都已經算好」。
struct Compiler {
    nodes: Vec<Node>,
}

impl Compiler {
    fn push(&mut self, node: Node, path: &str) -> Result<usize, DslError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(err(
                path,
                format!("這份策略的積木太多了（上限 {MAX_NODES} 個節點）"),
            ));
        }
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    fn depth(&self, depth: usize, path: &str) -> Result<(), DslError> {
        if depth > MAX_DEPTH {
            return Err(err(path, format!("條件巢狀太深了（上限 {MAX_DEPTH} 層）")));
        }
        Ok(())
    }

    fn cond(&mut self, cond: &Cond, path: &str, depth: usize) -> Result<usize, DslError> {
        self.depth(depth, path)?;
        match cond {
            Cond::Gt { left, right }
            | Cond::Gte { left, right }
            | Cond::Lt { left, right }
            | Cond::Lte { left, right }
            | Cond::CrossAbove { left, right }
            | Cond::CrossBelow { left, right } => {
                let left = self.expr(left, &seg(path, "左側"), depth + 1)?;
                let right = self.expr(right, &seg(path, "右側"), depth + 1)?;
                let node = match cond {
                    Cond::Gt { .. } => Node::Gt { left, right },
                    Cond::Gte { .. } => Node::Gte { left, right },
                    Cond::Lt { .. } => Node::Lt { left, right },
                    Cond::Lte { .. } => Node::Lte { left, right },
                    Cond::CrossAbove { .. } => Node::Cross {
                        above: true,
                        left,
                        right,
                    },
                    _ => Node::Cross {
                        above: false,
                        left,
                        right,
                    },
                };
                self.push(node, path)
            }
            Cond::All { children } | Cond::Any { children } => {
                let all = matches!(cond, Cond::All { .. });
                let label = if all { "且" } else { "或" };
                if children.is_empty() {
                    return Err(err(path, format!("「{label}」至少要有一個子條件")));
                }
                let mut indices = Vec::with_capacity(children.len());
                for (i, child) in children.iter().enumerate() {
                    let child_path = seg(path, &format!("第 {} 個子條件", i + 1));
                    indices.push(self.cond(child, &child_path, depth + 1)?);
                }
                self.push(
                    Node::Logic {
                        all,
                        children: indices,
                    },
                    path,
                )
            }
            Cond::Sustained { inner, bars } => {
                let bars = check_period(u32::from(*bars), path, "持續根數")? as u16;
                let inner = self.cond(inner, &seg(path, "內層條件"), depth + 1)?;
                self.push(Node::Sustained { inner, bars }, path)
            }
        }
    }

    fn expr(&mut self, expr: &Expr, path: &str, depth: usize) -> Result<usize, DslError> {
        self.depth(depth, path)?;
        match expr {
            Expr::Price { field, offset } => {
                check_offset(*offset, path)?;
                self.push(
                    Node::Price {
                        field: *field,
                        offset: *offset,
                    },
                    path,
                )
            }
            Expr::Number { value } => {
                let value = parse_fixed(value, path, "數值")?;
                self.push(Node::Number { value }, path)
            }
            Expr::Indicator {
                name,
                source,
                params,
                output,
                offset,
            } => {
                check_offset(*offset, path)?;
                // atr／donchian 吃整根 K 線，給 source 是使用者誤解了這個指標
                let takes_whole_bar = matches!(name, IndicatorName::Atr | IndicatorName::Donchian);
                if takes_whole_bar && source.is_some() {
                    return Err(err(
                        path,
                        "atr 與 donchian 吃的是整根 K 線的高低收，不能指定 source",
                    ));
                }
                let kind = indicator_kind(*name, params, output.as_deref(), path)?;
                // 先推來源節點（或預設的收盤價節點），後序才成立
                let source_index = if takes_whole_bar {
                    None
                } else {
                    Some(match source {
                        Some(inner) => self.expr(inner, &seg(path, "來源"), depth + 1)?,
                        None => self.push(
                            Node::Price {
                                field: PriceField::Close,
                                offset: 0,
                            },
                            path,
                        )?,
                    })
                };
                self.push(
                    Node::Indicator {
                        kind,
                        source: source_index,
                        offset: *offset,
                    },
                    path,
                )
            }
        }
    }
}

fn indicator_kind(
    name: IndicatorName,
    params: &IndicatorParams,
    output: Option<&str>,
    path: &str,
) -> Result<IndicatorKind, DslError> {
    match name {
        IndicatorName::Sma
        | IndicatorName::Ema
        | IndicatorName::Rsi
        | IndicatorName::Atr
        | IndicatorName::Highest
        | IndicatorName::Lowest => {
            let period = only_period(params, path)?;
            no_output(output, path)?;
            Ok(match name {
                IndicatorName::Sma => IndicatorKind::Sma { period },
                IndicatorName::Ema => IndicatorKind::Ema { period },
                IndicatorName::Rsi => IndicatorKind::Rsi { period },
                IndicatorName::Atr => IndicatorKind::Atr { period },
                IndicatorName::Highest => IndicatorKind::Highest { period },
                _ => IndicatorKind::Lowest { period },
            })
        }
        IndicatorName::Donchian => {
            let period = only_period(params, path)?;
            let output = pick_output(
                output,
                path,
                "donchian",
                &[("high", DonchianOutput::High), ("low", DonchianOutput::Low)],
            )?;
            Ok(IndicatorKind::Donchian { period, output })
        }
        IndicatorName::Bb => {
            if params.fast.is_some() || params.slow.is_some() || params.signal.is_some() {
                return Err(err(path, "bb 只接受 period 與 mult 參數"));
            }
            let period = check_period(need(params.period, path, "period")?, path, "週期")?;
            let mult_text = params
                .mult
                .as_deref()
                .ok_or_else(|| err(path, "bb 缺少 mult 參數（標準差倍數，十進位字串）"))?;
            let mult = parse_fixed(mult_text, path, "標準差倍數")?;
            if mult <= Fixed::ZERO {
                return Err(err(path, "標準差倍數必須大於 0"));
            }
            let output = pick_output(
                output,
                path,
                "bb",
                &[
                    ("upper", BbOutput::Upper),
                    ("middle", BbOutput::Middle),
                    ("lower", BbOutput::Lower),
                ],
            )?;
            Ok(IndicatorKind::Bb {
                period,
                mult,
                output,
            })
        }
        IndicatorName::Macd => {
            if params.period.is_some() || params.mult.is_some() {
                return Err(err(path, "macd 只接受 fast、slow、signal 三個參數"));
            }
            let fast = check_period(need(params.fast, path, "fast")?, path, "快線週期")?;
            let slow = check_period(need(params.slow, path, "slow")?, path, "慢線週期")?;
            let signal = check_period(need(params.signal, path, "signal")?, path, "訊號線週期")?;
            if fast >= slow {
                return Err(err(
                    path,
                    format!("macd 的快線週期必須短於慢線週期，收到 fast={fast}、slow={slow}"),
                ));
            }
            let output = pick_output(
                output,
                path,
                "macd",
                &[
                    ("line", MacdOutput::Line),
                    ("signal", MacdOutput::Signal),
                    ("histogram", MacdOutput::Histogram),
                ],
            )?;
            Ok(IndicatorKind::Macd {
                fast,
                slow,
                signal,
                output,
            })
        }
    }
}

/// 只吃 `period` 的指標：其他參數出現就是使用者誤解了。
fn only_period(params: &IndicatorParams, path: &str) -> Result<usize, DslError> {
    if params.fast.is_some()
        || params.slow.is_some()
        || params.signal.is_some()
        || params.mult.is_some()
    {
        return Err(err(path, "這個指標只接受 period 參數"));
    }
    check_period(need(params.period, path, "period")?, path, "週期")
}

fn need(value: Option<u32>, path: &str, field: &str) -> Result<u32, DslError> {
    value.ok_or_else(|| err(path, format!("缺少 {field} 參數")))
}

fn check_period(value: u32, path: &str, label: &str) -> Result<usize, DslError> {
    if value == 0 || value > MAX_PERIOD {
        return Err(err(
            path,
            format!("{label}必須在 1 到 {MAX_PERIOD} 之間，收到 {value}"),
        ));
    }
    Ok(value as usize)
}

fn check_offset(offset: u16, path: &str) -> Result<(), DslError> {
    if offset > MAX_OFFSET {
        return Err(err(
            path,
            format!("往前位移的根數不可超過 {MAX_OFFSET}，收到 {offset}"),
        ));
    }
    Ok(())
}

/// 單輸出指標不接受 `output`：填了代表使用者以為自己選了別的東西。
fn no_output(output: Option<&str>, path: &str) -> Result<(), DslError> {
    match output {
        None => Ok(()),
        Some(name) => Err(err(
            path,
            format!("這個指標只有一路輸出，不該指定 output（收到 {name:?}）"),
        )),
    }
}

/// 多輸出指標的 `output` 必填且必須合法——**不預設成第一個**，
/// 避免使用者以為自己選了別的。
fn pick_output<T: Copy>(
    output: Option<&str>,
    path: &str,
    indicator: &str,
    options: &[(&str, T)],
) -> Result<T, DslError> {
    let names: Vec<&str> = options.iter().map(|(name, _)| *name).collect();
    let listed = names.join("、");
    let Some(output) = output else {
        return Err(err(
            path,
            format!("{indicator} 有多路輸出，output 必填（可選：{listed}）"),
        ));
    };
    options
        .iter()
        .find(|(name, _)| *name == output)
        .map(|(_, value)| *value)
        .ok_or_else(|| {
            err(
                path,
                format!("{indicator} 不認得 output {output:?}（可選：{listed}）"),
            )
        })
}

fn parse_fixed(text: &str, path: &str, label: &str) -> Result<Fixed, DslError> {
    text.parse()
        .map_err(|e| err(path, format!("{label} {text:?} 不是合法的十進位數字：{e}")))
}

#[cfg(test)]
mod equivalence;
#[cfg(test)]
mod serialization;
#[cfg(test)]
mod tests;
