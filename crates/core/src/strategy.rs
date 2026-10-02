//! 策略介面：策略看 K 線，回答「我現在想持有多少部位」。
//!
//! 回測（2.2）、成交模擬（2.3）、模擬交易、實盤都透過這一個介面呼叫策略，
//! 所以同一份策略程式在四種模式下的決策完全一樣。
//!
//! 三個刻意的設計：
//!
//! 1. **回傳目標部位，不是訂單。** 策略只說「我想要半倉做多」，至於要送幾張單、
//!    成交價是多少、數量怎麼取整到交易所的級距（`SymbolRules`），是成交模擬的事。
//!    策略不碰下單，就不會因為換了市場或換了交易所規則而要改。
//! 2. **歷史由策略自己記。** 均線交叉、布林通道、唐奇安突破、RSI 都要回顧一段歷史，
//!    但呼叫端不需要知道誰要看幾根：K 線一根一根餵進來，策略在 `on_bar` 裡自己
//!    維護需要的 rolling window 或累加值。想知道要暖機幾根的人看 `warmup_bars()`。
//! 3. **部位是有正負的比例，不是開關。** 正數做多、負數做空、`0` 空手。
//!    現貨目前只會用到 `0` 與正數；合約（2.5）直接用負數做空，用大於 1 的數字
//!    表示槓桿，介面不用改。

use crate::bar::Bar;
use crate::fixed::Fixed;

/// 目標部位：策略「想要」的倉位，以策略配置資金的比例計。
///
/// - `0` 是空手，正數做多，負數做空。
/// - `1` 表示資金滿倉做多，`-0.5` 表示用一半資金做空，`2` 表示兩倍槓桿做多。
///
/// 用比例而不是數量，是因為策略不該知道帳戶有多少錢、也不該知道交易所的
/// 數量級距。換算成實際下單數量是成交模擬與 `SymbolRules` 的工作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TargetPosition(Fixed);

impl TargetPosition {
    /// 空手。
    pub const FLAT: TargetPosition = TargetPosition(Fixed::ZERO);
    /// 資金滿倉做多。
    pub const FULL_LONG: TargetPosition = TargetPosition(Fixed::ONE);

    /// 直接用帶正負號的比例建立。
    pub const fn new(ratio: Fixed) -> TargetPosition {
        TargetPosition(ratio)
    }

    /// 做多，大小取絕對值（傳負數也是做多，避免寫錯符號變成做空）。
    pub fn long(ratio: Fixed) -> TargetPosition {
        TargetPosition(ratio.abs())
    }

    /// 做空，大小取絕對值後轉負。
    pub fn short(ratio: Fixed) -> TargetPosition {
        TargetPosition(Fixed::from_raw(-ratio.abs().raw()))
    }

    /// 帶正負號的比例。
    pub const fn ratio(self) -> Fixed {
        self.0
    }

    pub fn is_flat(self) -> bool {
        self.0.is_zero()
    }
}

/// 一個交易策略。
///
/// 實作只要回答「看完這根 K 線，我想持有多少部位」；要怎麼從現在的部位變成
/// 目標部位，是呼叫端的事。
pub trait Strategy {
    /// 這根 K 線收盤了，回傳收盤後想持有的目標部位。
    ///
    /// 呼叫端保證：同一個交易對與週期、按開盤時間遞增、每根只餵一次。
    /// 實作端保證：不 panic；暖機不足或指標算不出來時回傳 [`TargetPosition::FLAT`]，
    /// 也就是寧可空手也不亂猜。
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition;

    /// 至少要先餵幾根 K 線，訊號才有意義（例如 20 日均線要 20 根）。
    ///
    /// 這只是宣告，不是保護：餵不夠時 `on_bar` 仍然要能安全回傳空手。
    /// 回測迴圈可以用它把暖機期排除在績效統計外，介面可以用它提醒「資料不夠」。
    fn warmup_bars(&self) -> usize {
        0
    }

    /// 這支策略會不會讀 K 線的訂單流欄位（成交筆數、主動買盤佔比）。
    ///
    /// 和 `warmup_bars()` 一樣只是宣告：回 `true` 的策略餵到沒有訂單流的
    /// K 線時仍然必須安全地回傳空手。呼叫端用它在**跑之前**就擋下
    /// 「策略要訂單流、資料沒有」這種會靜默產生零交易回測的組合。
    fn needs_order_flow(&self) -> bool {
        false
    }
}

/// 方向模式：UI 上的「只做多／多空」選項。
///
/// 內建的四個策略（均線交叉、布林通道、唐奇安、RSI）目前只會回傳
/// [`TargetPosition::FLAT`] 或 [`TargetPosition::FULL_LONG`]，從來不會自己做空——
/// 做不做空、開多少槓桿是帳戶層的風險設定，不是策略邏輯本身，所以用
/// [`LeveragedStrategy`] 包一層，而不是修改這四個策略或 [`TargetPosition`] 的定義。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionMode {
    /// 只做多：策略空手就維持空手。
    LongOnly,
    /// 多空都做：策略空手時鏡像成反向（做空），讓只會做多/空手的策略變成
    /// 「永遠在場」。策略如果自己回傳負的目標部位（未來的空頭策略），
    /// 方向維持不變，只套槓桿。
    LongShort,
}

/// 把內層策略的原始訊號套上槓桿倍數與方向模式，轉成實際要送進
/// [`crate::backtest::run_backtest`] 的目標部位。
///
/// 這是刻意選的設計：不改 [`TargetPosition`]（已經是穩定介面，語意是「策略自己
/// 想要的部位比例，可以帶槓桿」）、也不改四個內建策略（它們就是只想表達
/// 「做多」或「空手」）。UI 的槓桿拉桿、多空切換是帳戶層的設定，用 adapter
/// 包一層最乾淨：內層策略專心判斷方向，外層只管把比例放大、把「空手」
/// 轉成「反向」。
pub struct LeveragedStrategy {
    inner: Box<dyn Strategy + Send>,
    leverage: Fixed,
    direction: DirectionMode,
}

impl LeveragedStrategy {
    /// `leverage` 必須大於 0（呼叫端負責驗證；這裡不重複檢查，無法用的槓桿
    /// 會在算出目標部位時變成 `0`，退化成空手，不會 panic）。
    pub fn new(
        inner: Box<dyn Strategy + Send>,
        leverage: Fixed,
        direction: DirectionMode,
    ) -> LeveragedStrategy {
        LeveragedStrategy {
            inner,
            leverage,
            direction,
        }
    }
}

impl Strategy for LeveragedStrategy {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        let raw = self.inner.on_bar(bar).ratio();
        // 只做多模式下，內層策略自己回傳的負部位（目前只有 DSL 自訂策略
        // 才可能發生，四個內建策略永遠不會）一律視為空手，不能讓它穿透成
        // 實際送出去的空單——現貨的「只做多」帳戶沒有合法做空這件事，
        // CLAUDE.md 明文禁止。下面 `!raw.is_zero()` 那個分支本來完全沒看過
        // `direction`，非零的負部位直接套槓桿送出去，是這裡要補的洞。
        if self.direction == DirectionMode::LongOnly && raw.is_negative() {
            return TargetPosition::FLAT;
        }
        // 內層策略想做多/做空：維持方向、套槓桿。溢位（槓桿設得離譜）寧可空手。
        if !raw.is_zero() {
            return match raw.checked_mul(self.leverage) {
                Some(scaled) => TargetPosition::new(scaled),
                None => TargetPosition::FLAT,
            };
        }
        // 內層策略空手：只做多模式維持空手；多空模式鏡像成槓桿做空。
        match self.direction {
            DirectionMode::LongOnly => TargetPosition::FLAT,
            DirectionMode::LongShort => TargetPosition::short(self.leverage),
        }
    }

    fn warmup_bars(&self) -> usize {
        self.inner.warmup_bars()
    }

    fn needs_order_flow(&self) -> bool {
        self.inner.needs_order_flow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;

    fn bars(closes: &[&str]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let close = fx(c);
                Bar {
                    open_time: T0 + i as i64 * 3_600_000,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                    order_flow: None,
                }
            })
            .collect()
    }

    /// 把一串 K 線餵給策略，收集每一根的目標部位。呼叫端只做這件事。
    fn run(strategy: &mut dyn Strategy, bars: &[Bar]) -> Vec<TargetPosition> {
        bars.iter().map(|b| strategy.on_bar(b)).collect()
    }

    /// 永遠滿倉做多。
    struct AlwaysLong;
    impl Strategy for AlwaysLong {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FULL_LONG
        }
    }

    /// 永遠空手。
    struct AlwaysFlat;
    impl Strategy for AlwaysFlat {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            TargetPosition::FLAT
        }
    }

    /// 每 `period` 根切換一次多空，用來驗證策略可以保存自己的狀態。
    struct Alternating {
        period: usize,
        seen: usize,
    }
    impl Strategy for Alternating {
        fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
            let phase = self.seen / self.period;
            self.seen += 1;
            if phase % 2 == 0 {
                TargetPosition::FULL_LONG
            } else {
                TargetPosition::short(Fixed::ONE)
            }
        }
    }

    /// 需要回顧 `window` 根才會出訊號：收盤價高於視窗內第一根就做多。
    /// 這是 2.7 那些指標策略的縮小版：歷史自己記，暖機期回傳空手。
    struct HigherThanWindowStart {
        window: usize,
        history: Vec<Fixed>,
    }
    impl Strategy for HigherThanWindowStart {
        fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
            self.history.push(bar.close);
            if self.history.len() > self.window {
                self.history.remove(0);
            }
            if self.history.len() < self.window {
                return TargetPosition::FLAT;
            }
            if bar.close > self.history[0] {
                TargetPosition::FULL_LONG
            } else {
                TargetPosition::FLAT
            }
        }

        fn warmup_bars(&self) -> usize {
            self.window
        }
    }

    #[test]
    fn flat_long_short_are_distinguishable() {
        assert!(TargetPosition::FLAT.is_flat());
        assert_eq!(TargetPosition::FULL_LONG.ratio(), Fixed::ONE);
        assert!(!TargetPosition::FULL_LONG.is_flat());
        assert!(TargetPosition::short(Fixed::ONE).ratio().is_negative());
        assert!(!TargetPosition::FULL_LONG.ratio().is_negative());
    }

    #[test]
    fn position_size_is_not_just_on_off() {
        // 介面要能表示「半倉」與「兩倍槓桿」，不是只有 0 與 1
        assert_eq!(TargetPosition::long(fx("0.5")).ratio(), fx("0.5"));
        assert_eq!(TargetPosition::new(fx("2")).ratio(), fx("2"));
        assert_eq!(TargetPosition::short(fx("0.25")).ratio(), fx("-0.25"));
    }

    #[test]
    fn long_and_short_ignore_the_given_sign() {
        assert_eq!(
            TargetPosition::long(fx("-0.5")),
            TargetPosition::long(fx("0.5"))
        );
        assert_eq!(
            TargetPosition::short(fx("-0.5")),
            TargetPosition::short(fx("0.5"))
        );
        assert_eq!(TargetPosition::short(Fixed::ZERO), TargetPosition::FLAT);
    }

    #[test]
    fn default_target_is_flat() {
        // 忘記設定不會變成不小心開倉
        assert_eq!(TargetPosition::default(), TargetPosition::FLAT);
    }

    #[test]
    fn always_long_returns_long_every_bar() {
        let bars = bars(&["100", "101", "99"]);
        assert_eq!(
            run(&mut AlwaysLong, &bars),
            vec![TargetPosition::FULL_LONG; 3]
        );
        assert_eq!(AlwaysLong.warmup_bars(), 0);
    }

    #[test]
    fn always_flat_returns_flat_every_bar() {
        let bars = bars(&["100", "101", "99"]);
        assert_eq!(run(&mut AlwaysFlat, &bars), vec![TargetPosition::FLAT; 3]);
    }

    #[test]
    fn alternating_strategy_keeps_its_own_state() {
        let bars = bars(&["100", "101", "102", "103", "104", "105"]);
        let long = TargetPosition::FULL_LONG;
        let short = TargetPosition::short(Fixed::ONE);
        assert_eq!(
            run(&mut Alternating { period: 2, seen: 0 }, &bars),
            vec![long, long, short, short, long, long]
        );
    }

    #[test]
    fn strategy_needing_history_stays_flat_until_warmed_up() {
        let mut s = HigherThanWindowStart {
            window: 3,
            history: Vec::new(),
        };
        assert_eq!(s.warmup_bars(), 3);
        // 前兩根資料不足 → 空手；第三根起才有訊號
        let bars = bars(&["100", "101", "102", "99", "103"]);
        assert_eq!(
            run(&mut s, &bars),
            vec![
                TargetPosition::FLAT,      // 只有 1 根
                TargetPosition::FLAT,      // 只有 2 根
                TargetPosition::FULL_LONG, // 102 > 100
                TargetPosition::FLAT,      // 99 < 101
                TargetPosition::FULL_LONG, // 103 > 102
            ]
        );
    }

    #[test]
    fn leveraged_long_only_scales_long_and_keeps_flat_flat() {
        let bars = bars(&["100", "101", "99"]);
        let mut s = LeveragedStrategy::new(Box::new(AlwaysLong), fx("3"), DirectionMode::LongOnly);
        assert_eq!(run(&mut s, &bars), vec![TargetPosition::new(fx("3")); 3]);

        let mut flat =
            LeveragedStrategy::new(Box::new(AlwaysFlat), fx("3"), DirectionMode::LongOnly);
        assert_eq!(run(&mut flat, &bars), vec![TargetPosition::FLAT; 3]);
    }

    /// 回歸測試：內層策略（目前只有 DSL 自訂策略可能這樣）自己回傳負的
    /// 目標部位時，只做多模式必須把它當空手，不能讓它穿透成實際的空單——
    /// 現貨的「只做多」帳戶沒有合法做空這件事。`Alternating` 每一根都在
    /// 滿倉做多跟滿倉做空之間切換，用它確認*每一根*的輸出都不是負的，
    /// 不是只看第一根。
    #[test]
    fn leveraged_long_only_never_lets_an_inner_short_signal_through() {
        let bars = bars(&["100", "101", "99", "102"]);
        let mut s = LeveragedStrategy::new(
            Box::new(Alternating { period: 1, seen: 0 }),
            fx("2"),
            DirectionMode::LongOnly,
        );
        let positions = run(&mut s, &bars);
        assert!(
            positions.iter().all(|p| !p.ratio().is_negative()),
            "只做多模式下不該有任何一根是負部位：{positions:?}"
        );
        // 偶數根（0、2…）原本就是做多訊號，應該正常套槓桿；奇數根被夾成空手。
        assert_eq!(
            positions,
            vec![
                TargetPosition::new(fx("2")),
                TargetPosition::FLAT,
                TargetPosition::new(fx("2")),
                TargetPosition::FLAT,
            ]
        );
    }

    #[test]
    fn leveraged_long_short_mirrors_flat_into_a_leveraged_short() {
        let bars = bars(&["100", "101", "99"]);
        let mut s = LeveragedStrategy::new(Box::new(AlwaysFlat), fx("2"), DirectionMode::LongShort);
        assert_eq!(run(&mut s, &bars), vec![TargetPosition::short(fx("2")); 3]);

        let mut long =
            LeveragedStrategy::new(Box::new(AlwaysLong), fx("2"), DirectionMode::LongShort);
        assert_eq!(run(&mut long, &bars), vec![TargetPosition::new(fx("2")); 3]);
    }

    #[test]
    fn leveraged_strategy_keeps_inner_warmup() {
        let s = LeveragedStrategy::new(
            Box::new(HigherThanWindowStart {
                window: 5,
                history: Vec::new(),
            }),
            fx("1"),
            DirectionMode::LongOnly,
        );
        assert_eq!(s.warmup_bars(), 5);
    }

    #[test]
    fn leveraged_strategy_overflow_degrades_to_flat_not_panic() {
        // 槓桿離譜大時，ratio 相乘會溢位；要寧可空手也不 panic。
        struct HugeLong;
        impl Strategy for HugeLong {
            fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
                TargetPosition::new(Fixed::from_raw(i64::MAX))
            }
        }
        let mut s = LeveragedStrategy::new(
            Box::new(HugeLong),
            Fixed::from_raw(i64::MAX),
            DirectionMode::LongOnly,
        );
        assert_eq!(s.on_bar(&bars(&["100"])[0]), TargetPosition::FLAT);
    }

    #[test]
    fn strategies_can_be_stored_as_trait_objects() {
        // 2.2 的回測迴圈與之後的介面會拿一串不同策略跑同一份資料
        let bars = bars(&["100", "101"]);
        let mut all: Vec<Box<dyn Strategy>> = vec![
            Box::new(AlwaysLong),
            Box::new(AlwaysFlat),
            Box::new(Alternating { period: 1, seen: 0 }),
        ];
        let last: Vec<TargetPosition> = all
            .iter_mut()
            .map(|s| *run(s.as_mut(), &bars).last().unwrap())
            .collect();
        assert_eq!(
            last,
            vec![
                TargetPosition::FULL_LONG,
                TargetPosition::FLAT,
                TargetPosition::short(Fixed::ONE),
            ]
        );
    }
}
