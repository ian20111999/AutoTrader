//! 執行引擎：攤平的後序節點陣列 + 每根 K 線由前往後掃一次。
//!
//! ## 為什麼不是天真的遞迴直譯器
//!
//! 遞迴直譯器有一個具體而危險的 bug：**短路會讓有狀態的節點漏掉 K 線。**
//!
//! ```text
//! any([ cross_above(sma10, sma50),   ← 需要記住前一根的比較結果
//!       close > sma200 ])
//! ```
//!
//! 如果用 `||` 短路求值而第二個子條件先成立，第一個 `cross_above` 節點這根就
//! 不會被求值，它記的「前一根比較結果」會停在更早的一根。下一根它再被求值時，
//! 會拿兩根之前的狀態來判斷穿越——**算出一個根本沒發生過的穿越訊號**。
//! 更糟的是這個差異會依求值順序而異，所以回測與實盤如果節點順序不同
//! （例如前端重新排列了積木），訊號會不一樣，正好打破本專案最在意的那條原則
//! 「同一份策略在四種模式下的決策完全一樣」。
//!
//! 防法有兩種：(a) 寫一個「保證不短路」的遞迴求值器，然後靠 code review 守住它；
//! (b) 讓「每個節點每根都算一次」成為**資料結構的性質**。這裡選 (b)：
//! 子節點的 index 一定小於自己，所以 `for i in 0..nodes.len()` 這一個迴圈
//! 同時保證了「用到的值都已經算好」與「沒有任何節點被跳過」。
//!
//! ## 為什麼是 `Vec<NodeState>` + index，不是 `HashMap<NodeId, State>`
//!
//! id 由攤平的後序走訪自己編，所以不可能重複、不可能漏送，也不需要驗證。
//! **引擎的狀態槽身分由樹的結構決定，不由前端的字串決定。**
//! 前端當然有自己的 block id（拖拉 UI 需要 key），但那是 UI 的事。

use super::{PriceField, MAX_NODES};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategies::Window;
use crate::strategy::{Strategy, TargetPosition};
use std::collections::VecDeque;

use super::indicators::{Atr, Macd, MacdOutput, RsiCore, Smoothed};

/// 布林通道的三路輸出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BbOutput {
    Upper,
    Middle,
    Lower,
}

/// 唐奇安通道的兩路輸出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DonchianOutput {
    High,
    Low,
}

/// 攤平後的指標節點。參數已經在 `compile()` 驗證過，這裡不再檢查。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum IndicatorKind {
    Sma {
        period: usize,
    },
    Ema {
        period: usize,
    },
    Rsi {
        period: usize,
    },
    Macd {
        fast: usize,
        slow: usize,
        signal: usize,
        output: MacdOutput,
    },
    Bb {
        period: usize,
        mult: Fixed,
        output: BbOutput,
    },
    Atr {
        period: usize,
    },
    Donchian {
        period: usize,
        output: DonchianOutput,
    },
    Highest {
        period: usize,
    },
    Lowest {
        period: usize,
    },
}

impl IndicatorKind {
    /// 這個指標本身要幾根 K 線才算得出第一個值（不含 `source` 與 `offset`）。
    fn bars(self) -> usize {
        match self {
            IndicatorKind::Sma { period }
            | IndicatorKind::Ema { period }
            | IndicatorKind::Bb { period, .. }
            | IndicatorKind::Donchian { period, .. }
            | IndicatorKind::Highest { period }
            | IndicatorKind::Lowest { period } => period,
            // 漲跌幅／真實波幅都要「和前一根比」，所以多一根
            IndicatorKind::Rsi { period } | IndicatorKind::Atr { period } => period + 1,
            // MACD 線從第 slow 根開始有值，訊號線是它的 EMA(signal)
            IndicatorKind::Macd { slow, signal, .. } => slow + signal - 1,
        }
    }
}

/// 攤平後的節點。子節點用**比自己小的 index** 表示。
///
/// **不可以有任何帶「動作」語意的 variant**（送單、撤單、通知…）。
/// 這是一個算式求值器，不是腳本語言；使用者想要的任何「動作」
/// 都必須表達成「目標部位是多少」。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Node {
    Price {
        field: PriceField,
        offset: u16,
    },
    Number {
        value: Fixed,
    },
    Indicator {
        kind: IndicatorKind,
        /// 餵給指標的數列節點。`None` = 吃整根 K 線（`atr`／`donchian`）。
        source: Option<usize>,
        offset: u16,
    },
    Gt {
        left: usize,
        right: usize,
    },
    Gte {
        left: usize,
        right: usize,
    },
    Lt {
        left: usize,
        right: usize,
    },
    Lte {
        left: usize,
        right: usize,
    },
    Cross {
        /// `true` = 向上穿越，`false` = 向下穿越。
        above: bool,
        left: usize,
        right: usize,
    },
    Logic {
        /// `true` = AND，`false` = OR。
        all: bool,
        children: Vec<usize>,
    },
    Sustained {
        inner: usize,
        bars: u16,
    },
}

/// 四棵樹的根 index。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Roots {
    pub(super) long_entry: usize,
    pub(super) long_exit: usize,
    /// `(進場, 出場)`。`None` = 只做多。
    pub(super) short: Option<(usize, usize)>,
}

/// 一根 K 線上某個節點的求值結果。
///
/// 數值節點與條件節點用不同的 variant，所以「把一個數字當成條件用」在
/// `compile()` 的型別上就不可能發生；真的拿錯也只會得到 `None`（空手），
/// 不會算出一個假訊號。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Num(Option<Fixed>),
    Flag(Option<bool>),
}

impl Value {
    fn num(self) -> Option<Fixed> {
        match self {
            Value::Num(value) => value,
            Value::Flag(_) => None,
        }
    }

    fn flag(self) -> Option<bool> {
        match self {
            Value::Flag(value) => value,
            Value::Num(_) => None,
        }
    }
}

/// 固定延遲的環狀緩衝：實作 `offset`（取幾根之前的值）。
///
/// 讀寫順序和既有 [`Donchian`](crate::strategies::Donchian) 的
/// 「先讀舊值、再推入新值」一致——`offset` 讀的是推入前的歷史。
#[derive(Debug)]
struct Delay {
    cap: usize,
    buf: VecDeque<Option<Fixed>>,
}

impl Delay {
    fn new(offset: u16) -> Delay {
        Delay {
            cap: offset as usize,
            buf: VecDeque::new(),
        }
    }

    /// 回傳 `cap` 根之前的值，然後把這根推進去。
    fn step(&mut self, current: Option<Fixed>) -> Option<Fixed> {
        if self.cap == 0 {
            return current;
        }
        let out = if self.buf.len() == self.cap {
            self.buf.front().copied().flatten()
        } else {
            None
        };
        self.buf.push_back(current);
        if self.buf.len() > self.cap {
            self.buf.pop_front();
        }
        out
    }
}

/// 指標的狀態機。和 `IndicatorKind` 一起建立，所以兩者永遠配對。
#[derive(Debug)]
enum IndicatorState {
    /// `sma`／`bb`／`highest`／`lowest` 共用的 rolling window。
    Window(Window),
    /// `ema`。
    Smoothed(Smoothed),
    Rsi(RsiCore),
    Macd(Macd),
    Atr(Atr),
    Donchian {
        highs: Window,
        lows: Window,
    },
}

/// 每個節點的狀態槽，和節點陣列等長、用 index 存取。
#[derive(Debug)]
enum NodeState {
    /// `number`、比較節點、`all`／`any`、以及 `offset == 0` 的 `price`。
    Stateless,
    /// `offset > 0` 的 `price`。
    Delay(Delay),
    /// 指標狀態裝箱：這樣 `Vec<NodeState>` 的每一格都很小，無狀態節點不必為了
    /// MACD 的三個平滑平均付空間。裝箱只發生在 `compile()`，不在熱路徑上。
    Indicator {
        inner: Box<IndicatorState>,
        delay: Delay,
    },
    /// `cross_above`／`cross_below`：前一根的比較結果。
    Cross { prev: Option<bool> },
    /// `sustained(n)`：最近 n 根的結果。
    Sustained(VecDeque<Option<bool>>),
}

impl NodeState {
    fn for_node(node: &Node) -> NodeState {
        match node {
            Node::Price { offset, .. } => {
                if *offset == 0 {
                    NodeState::Stateless
                } else {
                    NodeState::Delay(Delay::new(*offset))
                }
            }
            Node::Number { .. }
            | Node::Gt { .. }
            | Node::Gte { .. }
            | Node::Lt { .. }
            | Node::Lte { .. }
            | Node::Logic { .. } => NodeState::Stateless,
            Node::Cross { .. } => NodeState::Cross { prev: None },
            Node::Sustained { bars, .. } => {
                NodeState::Sustained(VecDeque::with_capacity(*bars as usize))
            }
            Node::Indicator { kind, offset, .. } => NodeState::Indicator {
                inner: Box::new(IndicatorState::for_kind(*kind)),
                delay: Delay::new(*offset),
            },
        }
    }
}

impl IndicatorState {
    fn for_kind(kind: IndicatorKind) -> IndicatorState {
        match kind {
            IndicatorKind::Sma { period }
            | IndicatorKind::Bb { period, .. }
            | IndicatorKind::Highest { period }
            | IndicatorKind::Lowest { period } => IndicatorState::Window(Window::new(period)),
            IndicatorKind::Ema { period } => IndicatorState::Smoothed(Smoothed::ema(period)),
            IndicatorKind::Rsi { period } => IndicatorState::Rsi(RsiCore::new(period)),
            IndicatorKind::Macd {
                fast, slow, signal, ..
            } => IndicatorState::Macd(Macd::new(fast, slow, signal)),
            IndicatorKind::Atr { period } => IndicatorState::Atr(Atr::new(period)),
            IndicatorKind::Donchian { period, .. } => IndicatorState::Donchian {
                highs: Window::new(period),
                lows: Window::new(period),
            },
        }
    }
}

/// 一個由 DSL 定義的策略。
///
/// 它就是一個 [`Strategy`]：沒有新的執行路徑、沒有新的介面、
/// 沒有任何下單能力。`run_backtest`／`PaperEngine`／測試網的 `Trader`
/// 看到的和四個內建策略完全一樣。
#[derive(Debug)]
pub struct CustomStrategy {
    /// `compile()` 產生，之後唯讀。
    nodes: Vec<Node>,
    /// 和 `nodes` 等長，`on_bar` 會改。
    states: Vec<NodeState>,
    /// 這一根的求值結果暫存，和 `nodes` 等長。
    values: Vec<Value>,
    roots: Roots,
    /// `positionPct / 100 × leverage`，`compile()` 算好。
    size: Fixed,
    long_held: bool,
    short_held: bool,
    warmup: usize,
    needs_order_flow: bool,
}

impl CustomStrategy {
    pub(super) fn new(nodes: Vec<Node>, roots: Roots, size: Fixed) -> CustomStrategy {
        debug_assert!(nodes.len() <= MAX_NODES);
        let states = nodes.iter().map(NodeState::for_node).collect();
        let values = vec![Value::Num(None); nodes.len()];
        let warmup = root_warmup(&nodes, &roots);
        let needs_order_flow = nodes.iter().any(|node| {
            matches!(
                node,
                Node::Price {
                    field: PriceField::Trades | PriceField::TakerBuyRatio,
                    ..
                }
            )
        });
        CustomStrategy {
            nodes,
            states,
            values,
            roots,
            size,
            long_held: false,
            short_held: false,
            warmup,
            needs_order_flow,
        }
    }

    /// 攤平後的節點總數。給 UI 顯示策略複雜度、也讓測試能驗證攤平結果。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 目標部位的大小（`positionPct / 100 × leverage`）。
    pub fn position_size(&self) -> Fixed {
        self.size
    }
}

impl Strategy for CustomStrategy {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        // 1. 無短路，每個節點各算一次。子節點的 index 一定比自己小，
        //    所以一次由前往後的掃描就夠。
        let nodes = &self.nodes;
        let states = &mut self.states;
        let values = &mut self.values;
        for i in 0..nodes.len() {
            let (done, rest) = values.split_at_mut(i);
            rest[0] = eval(&nodes[i], &mut states[i], done, bar);
        }

        // 2. 取出各棵樹的結論；3. 先出場、後進場
        step_held(
            &mut self.long_held,
            values[self.roots.long_entry].flag(),
            values[self.roots.long_exit].flag(),
        );
        if let Some((entry, exit)) = self.roots.short {
            step_held(
                &mut self.short_held,
                values[entry].flag(),
                values[exit].flag(),
            );
        }

        // 4. 多空同時成立 = 互相抵銷 → 空手。理由和 Bollinger 處理「標準差為 0」
        //    一樣：矛盾的訊號代表沒有資訊，不該憑矛盾開倉。
        match (self.long_held, self.short_held) {
            (true, false) => TargetPosition::long(self.size),
            (false, true) => TargetPosition::short(self.size),
            _ => TargetPosition::FLAT,
        }
    }

    fn warmup_bars(&self) -> usize {
        self.warmup
    }

    fn needs_order_flow(&self) -> bool {
        self.needs_order_flow
    }
}

/// 更新一個方向的持有狀態。
///
/// - **任一棵樹無法判定（暖機不足／指標算不出來）→ 空手並清掉狀態**，
///   精確對應三個內建有狀態策略的
///   `let (Some(..)) = .. else { self.holding = false; return FLAT; }`。
///   暖機期間不只是不開倉，而是連狀態都不留。
/// - **出場條件先判斷**：兩邊同時成立時答案是空手。這重現 `bollinger.rs` 的
///   既有語意（標準差為 0 時三線重疊，收盤同時滿足「≤ 下軌」與「≥ 中軌」，
///   正確答案是空手，不是憑零波動開倉）。
/// - 兩邊都不成立 → 維持上一根（hysteresis，`Rsi`／`Bollinger`／`Donchian`
///   的 `holding: bool` 就是這個）。
///
/// **和 ADR 第 4.3 節的虛擬碼有一處出入，是刻意的。** 虛擬碼寫的是兩個獨立的
/// `if`（先 `if exit { false }` 再 `if entry { true }`），那會讓「兩邊同時成立」
/// 變成做多——和同一份 ADR 第 1.3 節第 2 點、第 3.5 節的驗證說明、以及
/// `bollinger.rs` 的既有行為都矛盾，也會讓「DSL 版與 Rust 版逐根相等」的驗收
/// 測試在零波動那根紅掉。這裡照散文與既有程式碼的語意寫成 `else if`。
fn step_held(held: &mut bool, entry: Option<bool>, exit: Option<bool>) {
    let (Some(exit), Some(entry)) = (exit, entry) else {
        *held = false;
        return;
    };
    if exit {
        *held = false;
    } else if entry {
        *held = true;
    }
}

/// 求值一個節點。
///
/// **簽名裡沒有任何可以產生副作用的東西**：輸入是一根 K 線、自己的狀態、
/// 以及前面節點算好的值；輸出是一個數字或布林。沒有帳戶、沒有 client、
/// 沒有 channel、碰不到檔案系統、碰不到網路。它連「現在幾點」都不知道
/// （只有 `bar.open_time`，而那是資料的一部分）。
///
/// 絕不 panic：所有算術走 `checked_*`，任何溢位 → 這個節點 `None`
/// → 傳播成「無法判定」→ 空手。
fn eval(node: &Node, state: &mut NodeState, values: &[Value], bar: &Bar) -> Value {
    match node {
        Node::Price { field, .. } => {
            let current = price_field(bar, *field);
            match state {
                NodeState::Delay(delay) => Value::Num(delay.step(current)),
                _ => Value::Num(current),
            }
        }
        Node::Number { value } => Value::Num(Some(*value)),
        Node::Indicator { kind, source, .. } => {
            let NodeState::Indicator { inner, delay } = state else {
                return Value::Num(None);
            };
            let current = match source {
                // 吃整根 K 線（atr／donchian）
                None => step_indicator(inner, *kind, None, bar),
                // 來源還沒有值時**不餵**指標（餵 0 會汙染種子，算出一條假的線）
                Some(index) => match values[*index].num() {
                    Some(value) => step_indicator(inner, *kind, Some(value), bar),
                    None => None,
                },
            };
            Value::Num(delay.step(current))
        }
        Node::Gt { left, right } => Value::Flag(compare(values, *left, *right, |a, b| a > b)),
        Node::Gte { left, right } => Value::Flag(compare(values, *left, *right, |a, b| a >= b)),
        Node::Lt { left, right } => Value::Flag(compare(values, *left, *right, |a, b| a < b)),
        Node::Lte { left, right } => Value::Flag(compare(values, *left, *right, |a, b| a <= b)),
        Node::Cross { above, left, right } => {
            let NodeState::Cross { prev } = state else {
                return Value::Flag(None);
            };
            // 向上穿越 = 前一根 left <= right 且這根 left > right
            let now = if *above {
                compare(values, *left, *right, |a, b| a > b)
            } else {
                compare(values, *left, *right, |a, b| a < b)
            };
            // 這根判不出來時 prev 也記成 None，下一根就不會拿一個過期的
            // 「前一根」來判斷穿越（這正是攤平陣列要防的那個 bug 的反面）
            let was = *prev;
            *prev = now;
            match (was, now) {
                // 前一根不知道（第一根，或前一根暖機不足）→ 無法判定
                (Some(was), Some(now)) => Value::Flag(Some(now && !was)),
                _ => Value::Flag(None),
            }
        }
        Node::Logic { all, children } => {
            let mut unknown = false;
            for index in children {
                match values[*index].flag() {
                    // all：有一個假就是假；any：有一個真就是真（資訊足夠就下結論）
                    Some(value) if value != *all => return Value::Flag(Some(value)),
                    Some(_) => {}
                    None => unknown = true,
                }
            }
            if unknown {
                Value::Flag(None)
            } else {
                Value::Flag(Some(*all))
            }
        }
        Node::Sustained { inner, bars } => {
            let NodeState::Sustained(buf) = state else {
                return Value::Flag(None);
            };
            let current = values[*inner].flag();
            buf.push_back(current);
            if buf.len() > *bars as usize {
                buf.pop_front();
            }
            if buf.len() < *bars as usize {
                // 視窗還沒滿 → 還不知道有沒有連續成立
                return Value::Flag(None);
            }
            if buf.iter().any(|value| value.is_none()) {
                Value::Flag(None)
            } else {
                Value::Flag(Some(buf.iter().all(|value| *value == Some(true))))
            }
        }
    }
}

/// 比較兩個數值節點。任一邊 `None` → `None`（三值邏輯）。
fn compare(
    values: &[Value],
    left: usize,
    right: usize,
    op: fn(Fixed, Fixed) -> bool,
) -> Option<bool> {
    Some(op(values[left].num()?, values[right].num()?))
}

fn step_indicator(
    state: &mut IndicatorState,
    kind: IndicatorKind,
    source: Option<Fixed>,
    bar: &Bar,
) -> Option<Fixed> {
    match (state, kind) {
        (IndicatorState::Window(window), IndicatorKind::Sma { .. }) => {
            window.push(source?);
            window.mean()
        }
        (IndicatorState::Window(window), IndicatorKind::Highest { .. }) => {
            window.push(source?);
            window.highest()
        }
        (IndicatorState::Window(window), IndicatorKind::Lowest { .. }) => {
            window.push(source?);
            window.lowest()
        }
        (IndicatorState::Window(window), IndicatorKind::Bb { mult, output, .. }) => {
            window.push(source?);
            // 走和內建 Bollinger 完全相同的路徑：middle = mean，
            // 單邊寬度 = stddev × 倍數（Fixed::checked_mul）
            let middle = window.mean()?;
            match output {
                BbOutput::Middle => Some(middle),
                BbOutput::Upper => middle.checked_add(window.stddev()?.checked_mul(mult)?),
                BbOutput::Lower => middle.checked_sub(window.stddev()?.checked_mul(mult)?),
            }
        }
        (IndicatorState::Smoothed(ema), IndicatorKind::Ema { .. }) => {
            ema.push(source?);
            ema.value()
        }
        (IndicatorState::Rsi(core), IndicatorKind::Rsi { .. }) => {
            core.push_close(source?);
            core.value()
        }
        (IndicatorState::Macd(macd), IndicatorKind::Macd { output, .. }) => {
            macd.push(source?);
            macd.value(output)
        }
        (IndicatorState::Atr(atr), IndicatorKind::Atr { .. }) => {
            atr.push(bar);
            atr.value()
        }
        (IndicatorState::Donchian { highs, lows }, IndicatorKind::Donchian { output, .. }) => {
            highs.push(bar.high);
            lows.push(bar.low);
            match output {
                DonchianOutput::High => highs.highest(),
                DonchianOutput::Low => lows.lowest(),
            }
        }
        // 狀態與節點種類是一起建立的，不可能不配對；真的不配對就空手
        _ => None,
    }
}

fn price_field(bar: &Bar, field: PriceField) -> Option<Fixed> {
    match field {
        PriceField::Open => Some(bar.open),
        PriceField::High => Some(bar.high),
        PriceField::Low => Some(bar.low),
        PriceField::Close => Some(bar.close),
        PriceField::Volume => volume_to_fixed(bar.volume),
        PriceField::Trades => {
            let flow = bar.order_flow?;
            let trades = i64::try_from(flow.trades).ok()?;
            Fixed::from_int(trades)
        }
        PriceField::TakerBuyRatio => {
            let flow = bar.order_flow?;
            if bar.volume <= 0.0 || bar.volume.is_nan() {
                // `volume == 0`（或 NaN，理論上不會發生，`Bar::validate` 已擋）：
                // 分母為 0，「主動買盤佔比」無法判定，不是 0。
                return None;
            }
            volume_to_fixed(flow.taker_buy_volume / bar.volume)
        }
    }
}

/// 成交量的確定性轉換：`Bar::volume` 是 `f64`（只用在統計），但 DSL 的比較
/// 一律在 `Fixed` 域裡做。
///
/// 同一個 `f64` 輸入永遠得到同一個 `Fixed`（截尾，不是四捨五入），所以回測與
/// 實盤的訊號一致性不受影響。**超過 `Fixed` 上限時回傳 `None`（視為指標算不
/// 出來，策略空手），不做飽和**——飽和會讓兩個不同的成交量變成同一個值。
///
/// ponytail：天花板 ≈ 922 億（基礎幣計）。SHIB 這類高供給量幣的日線成交量可能
/// 撞到；撞到時策略空手（安全方向）。若之後真要支援就在這裡先除一個固定比例
/// 並在 UI 標示單位，**不要**改成 `f64` 比較（會破壞確定性）。
fn volume_to_fixed(volume: f64) -> Option<Fixed> {
    if !volume.is_finite() || volume < 0.0 {
        return None;
    }
    let raw = volume * Fixed::SCALE as f64;
    if raw >= i64::MAX as f64 {
        return None;
    }
    Some(Fixed::from_raw(raw as i64))
}

/// 各節點的暖機根數，由前往後算（子節點的 index 一定比自己小）。
///
/// 不會溢位：`period ≤ 2000`、節點 ≤ 512、深度 ≤ 32，而
/// `IndicatorKind::bars()` 與 `Sustained::bars` 都 ≥ 1，所以 `− 1` 不會下溢。
fn warmups(nodes: &[Node]) -> Vec<usize> {
    let mut warmup = vec![0usize; nodes.len()];
    for i in 0..nodes.len() {
        warmup[i] = match &nodes[i] {
            Node::Price { offset, .. } => 1 + *offset as usize,
            Node::Number { .. } => 0,
            Node::Indicator {
                kind,
                source,
                offset,
            } => {
                // 有 source 的指標要把來源的暖機疊上去：sma(10) 吃 ema(20) → 29。
                // 吃整根 K 線的指標（atr／donchian）與常數來源都當成 1 根。
                let base = source.map_or(1, |index| warmup[index]).max(1);
                base + kind.bars() - 1 + *offset as usize
            }
            Node::Gt { left, right }
            | Node::Gte { left, right }
            | Node::Lt { left, right }
            | Node::Lte { left, right } => warmup[*left].max(warmup[*right]),
            Node::Cross { left, right, .. } => warmup[*left].max(warmup[*right]) + 1,
            Node::Logic { children, .. } => children
                .iter()
                .map(|index| warmup[*index])
                .max()
                .unwrap_or(0),
            Node::Sustained { inner, bars } => warmup[*inner] + *bars as usize - 1,
        };
    }
    warmup
}

fn root_warmup(nodes: &[Node], roots: &Roots) -> usize {
    let warmup = warmups(nodes);
    let mut max = warmup[roots.long_entry].max(warmup[roots.long_exit]);
    if let Some((entry, exit)) = roots.short {
        max = max.max(warmup[entry]).max(warmup[exit]);
    }
    max
}
