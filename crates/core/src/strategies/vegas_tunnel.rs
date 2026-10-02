//! 維加斯通道：EMA144／EMA169 夾出一條通道，EMA12 當過濾器確認突破。

use super::{non_zero, StrategyParamError};
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategy::{Strategy, TargetPosition};
use crate::strategy_dsl::indicators::Smoothed;

/// 維加斯通道（Vegas Tunnel，趨勢突破）。
///
/// EMA144 與 EMA169 兩條長均線靠得很近，中間那條帶子就是「通道」（tunnel）：
/// 價格在通道裡面代表沒有明確趨勢，穿出通道才算趨勢啟動。再用一條短週期
/// EMA12 當**過濾器**，避免只是價格瞬間噴出去、均線自己還沒跟上的假突破。
///
/// - 收盤價**和** EMA12 同時站上通道上下緣 → 滿倉做多
/// - 收盤價**和** EMA12 同時跌破通道上下緣 → 空手
/// - 其他情形（含「價格穿出去了但 EMA12 還在通道裡」）→ 維持上一根的部位
///
/// ## 四個刻意的選擇
///
/// 1. **「站上通道」是站上兩條長均線中較高的那一條**，不是只贏過其中一條。
///    EMA144 和 EMA169 誰在上面會隨行情翻轉（上漲段快線在上、下跌段反過來），
///    所以程式不假設順序，每根都取 `max`／`min` 當上下緣。這也是原始系統的
///    說法——穿越的是整條帶子，不是其中一條線。
/// 2. **過濾器不是「價格的另一個門檻」，是「均線有沒有跟上」。** 只有價格突破、
///    EMA12 還沒跟上時，訊號**完全忽略**（維持原部位），而不是反向或出場。
///    一根長紅 K 線可以讓收盤價跳到通道上方，但 EMA12 每根只吸收
///    `2/13 ≈ 15%` 的落差；長期在通道下方盤整之後的單根噴出，EMA12 不可能
///    一根就追上來。這正是這個策略最核心、也最容易寫錯的地方（有測試盯著）。
/// 3. **三條 EMA 都在這一根收盤後才比較。** 和唐奇安通道不同：唐奇安的通道是
///    「前 N 根」的最高價，把當根算進去的話收盤價永遠不可能高過含自己的最高價，
///    訊號會永遠不出現。EMA 是遞推的，當根只佔 α 的權重，
///    `新EMA = 舊EMA + α(收盤 − 舊EMA)`，收盤價高於舊 EMA 時新 EMA 一定還在
///    收盤價下方（差距縮為 `1−α` 倍），所以「含當根」不會讓條件失效，而且和
///    看圖的人在收盤那一刻看到的均線值一致。
/// 4. **跌破通道是出場，不是反手做空。** 內建策略一律不回傳負部位，理由見
///    [`crate::strategies`] 的模組說明（現貨不能做空，而「只做多」模式不會
///    幫忙夾掉負部位）。要反手就把回測／模擬頁的方向設成「多空」。
///
/// ## 週期無關：1 秒 K 線也能跑
///
/// 這個策略原本是給 1 小時／4 小時用的，但 EMA 的「期數」概念和實際時間長短
/// 無關——144 根就是 144 根，是 1 秒還是 4 小時由資料決定，策略邏輯裡沒有任何
/// 地方讀 [`Interval`](crate::bar::Interval)。所以它在 1 秒 K 線上同樣成立
/// （代表「過去 144 秒的指數平均」），**刻意不限制週期**。
///
/// ## 參數可以調，但 144／169／12 就是這個策略的定義
///
/// 三個週期開放給使用者調（視覺上更貼合某個市場的節奏是合理的微調），但
/// 144／169／12 是這個系統之所以叫「維加斯通道」的核心：144 與 169 是
/// 費波那契數列相鄰項的平方關係（12²＝144、13²＝169），兩條線天生靠得近才
/// 夾得出一條窄帶。改成 20／200 之類差距很大的組合，通道會變成一片很寬的區域，
/// 那就是另一個策略了，不是這一個。
#[derive(Debug)]
pub struct VegasTunnel {
    /// 通道的短週期長均線（經典 EMA144）。
    tunnel_fast: Smoothed,
    /// 通道的長週期長均線（經典 EMA169）。
    tunnel_slow: Smoothed,
    /// 過濾用的短均線（經典 EMA12）。
    filter: Smoothed,
    holding: bool,
}

impl VegasTunnel {
    /// 預設通道短均線週期（根）。
    pub const DEFAULT_TUNNEL_FAST: usize = 144;
    /// 預設通道長均線週期（根）。
    pub const DEFAULT_TUNNEL_SLOW: usize = 169;
    /// 預設過濾均線週期（根）。
    pub const DEFAULT_FILTER: usize = 12;

    /// 三個週期都只要求大於 0，**不要求 `filter < tunnel_fast < tunnel_slow`**：
    /// 把快慢設反只是變成另一種（通常沒用的）策略，不是會算錯的參數，
    /// 而通道上下緣本來就每根重新取 `max`／`min`，不依賴誰比較大。
    pub fn new(
        tunnel_fast: usize,
        tunnel_slow: usize,
        filter: usize,
    ) -> Result<VegasTunnel, StrategyParamError> {
        Ok(VegasTunnel {
            tunnel_fast: Smoothed::ema(non_zero(tunnel_fast)?),
            tunnel_slow: Smoothed::ema(non_zero(tunnel_slow)?),
            filter: Smoothed::ema(non_zero(filter)?),
            holding: false,
        })
    }

    /// 目前的通道 `(上緣, 下緣)`——兩條長均線裡較高與較低的那一個值。
    /// 任一條還在暖機時回傳 `None`。
    pub fn tunnel(&self) -> Option<(Fixed, Fixed)> {
        let fast = self.tunnel_fast.value()?;
        let slow = self.tunnel_slow.value()?;
        Some((fast.max(slow), fast.min(slow)))
    }

    /// 目前的過濾均線值。暖機不足時回傳 `None`。
    pub fn filter_ema(&self) -> Option<Fixed> {
        self.filter.value()
    }
}

impl Default for VegasTunnel {
    /// 144 / 169 / 12：經典的維加斯通道設定。
    fn default() -> VegasTunnel {
        // 三個常數寫死且合法，不是外部輸入
        VegasTunnel::new(
            VegasTunnel::DEFAULT_TUNNEL_FAST,
            VegasTunnel::DEFAULT_TUNNEL_SLOW,
            VegasTunnel::DEFAULT_FILTER,
        )
        .expect("內建預設參數必須合法")
    }
}

impl Strategy for VegasTunnel {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition {
        self.tunnel_fast.push(bar.close);
        self.tunnel_slow.push(bar.close);
        self.filter.push(bar.close);

        let (Some((upper, lower)), Some(filter)) = (self.tunnel(), self.filter_ema()) else {
            self.holding = false;
            return TargetPosition::FLAT;
        };
        // 嚴格大於／小於：收盤價剛好貼在均線上不算穿越（和唐奇安的「創新高」一致）。
        // 兩個條件互斥（upper ≥ lower），順序不影響結果。
        if bar.close > upper && filter > upper {
            self.holding = true;
        } else if bar.close < lower && filter < lower {
            self.holding = false;
        }
        if self.holding {
            TargetPosition::FULL_LONG
        } else {
            TargetPosition::FLAT
        }
    }

    /// 三個週期裡最長的那一個：[`Smoothed`] 的種子期是 `period` 筆算術平均，
    /// 湊滿才算得出第一個數值。預設參數下是 169 根。
    ///
    /// 這是「**算得出**數值」的最低根數，不是「已經收斂」的根數——EMA 理論上
    /// 記得所有歷史。收斂要靠 [`WARMUP_SAFETY_FACTOR`](crate::WARMUP_SAFETY_FACTOR)
    /// 那個 ×5：169 × 5 = 845 根，仍然在 Binance 單次 1000 根的上限內，
    /// 模擬／測試網交易啟動時一個請求就抓得完。
    fn warmup_bars(&self) -> usize {
        self.tunnel_fast
            .period()
            .max(self.tunnel_slow.period())
            .max(self.filter.period())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_util::{closes, extreme_bars, fx, last_signal, signals};

    /// 測試用的小週期版本：EMA6 / EMA7 / EMA5，平滑係數分別是
    /// `2/7`、`2/8 = 0.25`、`2/6 = 1/3`。
    ///
    /// 用小週期不是偷懶：EMA 的邏輯和週期長短無關（見型別說明的「週期無關」），
    /// 小週期才能把每一根的均線值手算出來。注意**過濾均線（5）比通道（6／7）短**，
    /// 和經典的 12 比 144／169 短是同一個關係。
    fn vegas() -> VegasTunnel {
        VegasTunnel::new(6, 7, 5).unwrap()
    }

    /// 8 根 100 元的平盤 K 線：常數序列的 EMA 就是那個常數，所以餵完之後
    /// 三條均線都**剛好**等於 100，後面每一根都能手算。
    const FLAT_START: [&str; 8] = ["100"; 8];

    fn with_flat_start(rest: &[&str]) -> Vec<Bar> {
        let mut all: Vec<&str> = FLAT_START.to_vec();
        all.extend_from_slice(rest);
        closes(&all)
    }

    #[test]
    fn rejects_zero_periods() {
        assert_eq!(
            VegasTunnel::new(0, 169, 12).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            VegasTunnel::new(144, 0, 12).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert_eq!(
            VegasTunnel::new(144, 169, 0).unwrap_err(),
            StrategyParamError::ZeroPeriod
        );
        assert!(VegasTunnel::new(1, 1, 1).is_ok());
    }

    #[test]
    fn default_is_the_classic_144_169_12() {
        let s = VegasTunnel::default();
        assert_eq!(s.tunnel_fast.period(), 144);
        assert_eq!(s.tunnel_slow.period(), 169);
        assert_eq!(s.filter.period(), 12);
        // 最長的週期要先湊滿種子期才算得出通道
        assert_eq!(s.warmup_bars(), 169);
        // ×5 的安全餘裕仍在 Binance 單次 1000 根的上限內
        assert_eq!(crate::warmup_fetch_count(&s), 845);
    }

    #[test]
    fn tunnel_is_none_before_warmup_and_stays_flat() {
        let mut s = vegas();
        assert_eq!(s.warmup_bars(), 7);
        assert_eq!(s.tunnel(), None);
        // 前 6 根：EMA7 的種子期還沒湊滿，整條通道算不出來 → 一律空手
        assert_eq!(signals(&mut s, &closes(&["100"; 6])), "......");
        assert_eq!(s.tunnel(), None);
        assert_eq!(s.filter_ema(), Some(fx("100"))); // EMA5 已經有值，但不夠出訊號
    }

    #[test]
    fn a_flat_market_never_opens_a_position() {
        // 收盤價剛好等於通道上緣（三條線都是 100），不算穿越
        let mut s = vegas();
        assert_eq!(signals(&mut s, &with_flat_start(&[])), "........");
        assert_eq!(s.tunnel(), Some((fx("100"), fx("100"))));
    }

    #[test]
    fn breaks_out_with_the_filter_then_exits_on_a_breakdown() {
        // 平盤 8 根後三條線都是 100，接著手算：
        //   第 9 根收 200：EMA6 = 100 + (2/7)×100 = 128.57142857
        //                 EMA7 = 100 + 0.25×100   = 125
        //                 EMA5 = 100 + (1/3)×100  = 133.33333333
        //     通道上緣 128.57142857；收盤 200 > 上緣，EMA5 133.33 > 上緣 → 進場
        //   第 10 根收 50：EMA6 = 128.57142857 + (2/7)×(50−128.57142857) = 106.12244898
        //                 EMA7 = 125 + 0.25×(50−125)                    = 106.25
        //                 EMA5 = 133.33333333 + (1/3)×(50−133.33333333) = 105.55555556
        //     通道下緣 106.12244898；收盤 50 < 下緣，EMA5 105.56 < 下緣 → 出場
        let mut s = vegas();
        assert_eq!(
            signals(&mut s, &with_flat_start(&["200", "50"])),
            "........L."
        );
    }

    #[test]
    fn a_price_breakout_the_filter_ema_has_not_followed_is_not_a_signal() {
        // 這個策略最容易寫錯的地方：價格穿出通道，但過濾均線還沒跟上 → 不算訊號。
        //
        // 先用一根 60 元的長黑把通道壓到價格上方（第 9 根）：
        //   EMA6 = 100 + (2/7)×(60−100) = 88.57142858（截尾往零，所以尾數 8）
        //   EMA7 = 100 + 0.25×(60−100)  = 90
        //   EMA5 = 100 + (1/3)×(60−100) = 86.66666667
        //   → 通道 (90, 88.57142858)，收盤 60 < 下緣且 EMA5 86.67 < 下緣 → 出場
        //     （本來就空手，所以訊號還是空手）
        //
        // 第 10 根收 95：價格已經穿到通道上方，但 EMA5 還在通道裡
        //   EMA6 = 88.57142858 + (2/7)×(95−88.57142858) = 90.40816327
        //   EMA7 = 90 + 0.25×(95−90)                    = 91.25
        //   EMA5 = 86.66666667 + (1/3)×(95−86.66666667) = 89.44444444
        //   → 通道上緣 91.25，收盤 95 > 91.25 ✔，但 EMA5 89.44 < 91.25 ✘ → 忽略
        let mut s = vegas();
        let bars = with_flat_start(&["60", "95"]);
        assert_eq!(signals(&mut s, &bars), ".........."); // 10 根全部空手

        // 把上面的手算結果釘住，證明「不進場」真的是過濾均線擋下來的，
        // 而不是價格其實沒突破
        let (upper, _lower) = s.tunnel().unwrap();
        assert_eq!(upper, fx("91.25"));
        assert_eq!(bars.last().unwrap().close, fx("95"));
        assert!(bars.last().unwrap().close > upper, "價格確實已經突破通道");
        assert_eq!(s.filter_ema(), Some(fx("89.44444444")));
        assert!(
            s.filter_ema().unwrap() < upper,
            "過濾均線還沒跟上，所以不算訊號"
        );
    }

    #[test]
    fn the_filter_only_delays_the_entry_it_does_not_block_it_forever() {
        // 接上一個案例：價格停在 95 不動，過濾均線一根一根追上來。
        // 第 5 根 95（整串的第 14 根）EMA5 終於超過通道上緣 → 這時才進場。
        // 價格一直沒變，差別純粹來自均線的收斂速度——這就是過濾器的作用：
        // 延後到「均線也同意」才進場，不是永遠不進場。
        let mut s = vegas();
        let bars = with_flat_start(&["60", "95", "95", "95", "95", "95"]);
        assert_eq!(signals(&mut s, &bars), ".............L");
        let (upper, _) = s.tunnel().unwrap();
        assert!(s.filter_ema().unwrap() > upper);
    }

    #[test]
    fn holds_the_position_while_the_price_sits_inside_the_tunnel() {
        // 進場後價格回到通道裡（既沒站上上緣、也沒跌破下緣）→ 續抱，
        // 趨勢策略不能一回檔就跑掉。
        let mut s = vegas();
        // 第 9 根 200 進場（同 breaks_out 的手算），第 10 根收 127：
        //   EMA6 = 128.57142857 + (2/7)×(127−128.57142857) = 128.12244898
        //   EMA7 = 125 + 0.25×(127−125)                    = 125.5
        //   → 通道 (128.12244898, 125.5)，收盤 127 落在上下緣之間 → 續抱
        let bars = with_flat_start(&["200", "127"]);
        assert_eq!(signals(&mut s, &bars), "........LL");
        let (upper, lower) = s.tunnel().unwrap();
        assert_eq!((upper, lower), (fx("128.12244898"), fx("125.5")));
        let close = bars.last().unwrap().close;
        assert!(close < upper && close > lower, "收盤價落在通道裡面");
    }

    #[test]
    fn extreme_prices_do_not_panic() {
        let mut s = vegas();
        let _ = signals(&mut s, &extreme_bars());
    }

    #[test]
    fn does_not_need_order_flow() {
        // 只看收盤價，舊格式（6 欄）的 K 線檔也能跑
        assert!(!VegasTunnel::default().needs_order_flow());
        let mut s = vegas();
        let signal = last_signal(&mut s, &with_flat_start(&["200"]));
        assert_eq!(signal, TargetPosition::FULL_LONG);
    }
}
