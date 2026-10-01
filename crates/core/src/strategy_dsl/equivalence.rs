//! **驗收核心：DSL 版與既有 Rust 版的四個內建策略，逐根目標部位必須完全相等。**
//!
//! 這是整份 DSL 設計存在的理由。它同時驗證了指標值、三值邏輯、出場優先順序、
//! 暖機行為與 hysteresis——任何一項寫錯，這四個測試就會紅。
//!
//! 每個測試餵兩串 K 線：
//!
//! 1. [`market`]：一段確定性的「鋸齒 + 趨勢」行情，保證每個策略都會真的進出場
//!    好幾次（測試會斷言訊號變化次數，否則「兩邊都永遠空手」會假通過）。
//! 2. `test_util::extreme_bars`：極端價格，確認兩邊**同時**退化成空手而不是
//!    其中一邊 panic。

use super::StrategyAst;
use crate::bar::Bar;
use crate::fixed::Fixed;
use crate::strategies::test_util::extreme_bars;
use crate::strategies::{Bollinger, Donchian, Rsi, SmaCross};
use crate::strategy::{Strategy, TargetPosition};

/// 2024-01-01 00:00 UTC
const T0: i64 = 1_704_067_200_000;

/// 一段確定性的 1 小時行情：隨機步伐 + 每 40 根換一次方向的趨勢。
///
/// 用 xorshift 自己產生（不加依賴），所以每次執行完全一樣——回測的可重現性
/// 也是這個專案的基本要求。
fn market(count: usize) -> Vec<Bar> {
    let mut seed: u64 = 0x2026_1001_5EED;
    let mut close: i64 = 100 * Fixed::SCALE;
    let mut bars = Vec::with_capacity(count);
    for i in 0..count {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let step = (seed % 401) as i64 - 200; // ±2.00
        let trend = if (i / 40) % 2 == 0 { 40 } else { -40 }; // ±0.40
        let open = close;
        close = (close + (step + trend) * 1_000_000).clamp(20 * Fixed::SCALE, 400 * Fixed::SCALE);
        let spread = ((seed >> 24) % 120) as i64 * 1_000_000;
        let bar = Bar {
            open_time: T0 + i as i64 * 3_600_000,
            open: Fixed::from_raw(open),
            high: Fixed::from_raw(open.max(close) + spread),
            low: Fixed::from_raw((open.min(close) - spread).max(1)),
            close: Fixed::from_raw(close),
            volume: 1_000.0 + (seed % 5_000) as f64,
        };
        assert_eq!(bar.validate(), Ok(()), "測試資料本身要是合理的 K 線");
        bars.push(bar);
    }
    bars
}

/// 逐根比對 DSL 版與內建版的目標部位。
///
/// `min_changes` 是「訊號至少要變這麼多次」——沒有這個斷言，一組讓兩邊都永遠
/// 空手的測資會讓測試毫無意義地通過。
fn assert_identical(
    label: &str,
    make_builtin: impl Fn() -> Box<dyn Strategy>,
    json: &str,
    min_changes: usize,
) {
    let ast: StrategyAst = serde_json::from_str(json).expect("範例 JSON 必須合法");
    let mut dsl = ast.compile().expect("範例 JSON 必須編得起來");
    let mut builtin = make_builtin();
    assert_eq!(
        dsl.warmup_bars(),
        builtin.warmup_bars(),
        "{label}：宣告的暖機根數也要一樣"
    );

    let bars = market(400);
    let mut changes = 0usize;
    let mut longs = 0usize;
    let mut prev = TargetPosition::FLAT;
    for (index, bar) in bars.iter().enumerate() {
        let want = builtin.on_bar(bar);
        let got = dsl.on_bar(bar);
        assert_eq!(got, want, "{label}：第 {} 根的目標部位不同", index + 1);
        if got != prev {
            changes += 1;
            prev = got;
        }
        if !got.is_flat() {
            longs += 1;
        }
    }
    assert!(
        changes >= min_changes,
        "{label}：訊號只變了 {changes} 次（期望至少 {min_changes}），測資沒有真的驗到進出場"
    );
    assert!(longs > 0, "{label}：全程都空手，這個比對沒有意義");

    // 極端價格：兩邊要同時退化成空手，而不是其中一邊 panic
    let mut builtin_extreme = make_builtin();
    let mut dsl_extreme = serde_json::from_str::<StrategyAst>(json)
        .unwrap()
        .compile()
        .unwrap();
    for (index, bar) in extreme_bars().iter().enumerate() {
        assert_eq!(
            dsl_extreme.on_bar(bar),
            builtin_extreme.on_bar(bar),
            "{label}：極端價格第 {} 根的目標部位不同",
            index + 1
        );
    }
}

fn fx(s: &str) -> Fixed {
    s.parse().unwrap()
}

/// 10/50 均線交叉（ADR 第 3.5 節的範例 JSON）。
///
/// `SmaCross` 是四個策略裡唯一無狀態的（純比大小），所以出場條件就是進場條件
/// 的邏輯反面。兩線相等時 `lte` 成立 → 出場先判斷 → 空手，和內建版一致。
const SMA_CROSS: &str = r#"{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "gt",
    "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
    "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
  },
  "longExit": {
    "kind": "lte",
    "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
    "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
  }
}"#;

/// 布林通道 20 根 / 2 倍：收盤 ≤ 下軌進場、收盤 ≥ 中軌出場。
///
/// 兩棵樹不是互補的（中間維持上一根），所以這個測試同時驗證了 hysteresis。
const BOLLINGER: &str = r#"{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "lte",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "bb", "output": "lower",
               "params": { "period": 20, "mult": "2" } }
  },
  "longExit": {
    "kind": "gte",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "bb", "output": "middle",
               "params": { "period": 20, "mult": "2" } }
  }
}"#;

/// 唐奇安 20/10（ADR 第 3.7 節的範例）。
///
/// `offset: 1` 是重現「通道用**前** N 根算」的關鍵：`donchian(20)` 本身是
/// 「含這根的 20 根」，往前位移一根就等於「前 20 根」。
const DONCHIAN: &str = r#"{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "gt",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "donchian", "output": "high",
               "params": { "period": 20 }, "offset": 1 }
  },
  "longExit": {
    "kind": "lt",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "donchian", "output": "low",
               "params": { "period": 10 }, "offset": 1 }
  }
}"#;

/// RSI 14 / 30 / 70（ADR 第 3.6 節的範例）。
///
/// 兩棵樹各自有一個 `rsi(14)` 節點，是兩個獨立的狀態槽；餵的是同一串收盤價，
/// 所以算出的值永遠相同（指標是輸入序列的純函數）——前提是執行引擎每根 K 線
/// 都餵每一個指標，而這正是攤平陣列保證的事。
const RSI: &str = r#"{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "lte",
    "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
    "right": { "kind": "number", "value": "30" }
  },
  "longExit": {
    "kind": "gte",
    "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
    "right": { "kind": "number", "value": "70" }
  }
}"#;

#[test]
fn dsl_reproduces_sma_cross_bar_for_bar() {
    assert_identical(
        "均線交叉",
        || Box::new(SmaCross::new(10, 50).unwrap()),
        SMA_CROSS,
        6,
    );
}

#[test]
fn dsl_reproduces_bollinger_bar_for_bar() {
    assert_identical(
        "布林通道",
        || Box::new(Bollinger::new(20, fx("2")).unwrap()),
        BOLLINGER,
        6,
    );
}

#[test]
fn dsl_reproduces_donchian_bar_for_bar() {
    assert_identical(
        "唐奇安",
        || Box::new(Donchian::new(20, 10).unwrap()),
        DONCHIAN,
        6,
    );
}

#[test]
fn dsl_reproduces_rsi_bar_for_bar() {
    assert_identical(
        "RSI",
        || Box::new(Rsi::new(14, fx("30"), fx("70")).unwrap()),
        RSI,
        6,
    );
}

/// 布林通道的非整數倍數也要逐根相等。
///
/// 這是 ADR 第 11.2 節的未決問題：倍數用「十進位字串 + `Fixed::checked_mul`」
/// 而不是「分子/分母兩個整數先乘後除」，就是為了走和內建 `Bollinger`
/// **完全相同**的乘法路徑（`checked_mul` 是四捨五入，不是截尾）。
#[test]
fn dsl_reproduces_bollinger_with_a_fractional_multiplier() {
    let json = BOLLINGER.replace("\"mult\": \"2\"", "\"mult\": \"1.5\"");
    assert_identical(
        "布林通道 1.5 倍",
        || Box::new(Bollinger::new(20, fx("1.5")).unwrap()),
        &json,
        6,
    );
}

/// ADR 第 3.8 節那份手刻的 JSON（多空 + 巢狀 + 持續 N 根 + MACD），
/// 真的跑一次完整回測。
///
/// 這是 Phase 1 的最後一步：使用者可以用手寫 JSON 跑自訂策略回測，沒有 UI。
#[test]
fn a_hand_written_nested_long_short_strategy_runs_a_real_backtest() {
    const JSON: &str = r#"{
      "schemaVersion": 1,
      "direction": "long_short",
      "sizing": { "positionPct": "50", "leverage": "2" },
      "longEntry": {
        "kind": "all",
        "children": [
          { "kind": "cross_above",
            "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
                       "params": { "fast": 12, "slow": 26, "signal": 9 } },
            "right": { "kind": "number", "value": "0" } },
          { "kind": "any",
            "children": [
              { "kind": "gt",
                "left":  { "kind": "price", "field": "close" },
                "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } } },
              { "kind": "sustained", "bars": 3,
                "inner": { "kind": "gt",
                           "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
                           "right": { "kind": "number", "value": "55" } } }
            ] }
        ]
      },
      "longExit": {
        "kind": "cross_below",
        "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
                   "params": { "fast": 12, "slow": 26, "signal": 9 } },
        "right": { "kind": "number", "value": "0" }
      },
      "shortEntry": {
        "kind": "all",
        "children": [
          { "kind": "cross_below",
            "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
                       "params": { "fast": 12, "slow": 26, "signal": 9 } },
            "right": { "kind": "number", "value": "0" } },
          { "kind": "any",
            "children": [
              { "kind": "lt",
                "left":  { "kind": "price", "field": "close" },
                "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } } },
              { "kind": "sustained", "bars": 3,
                "inner": { "kind": "lt",
                           "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
                           "right": { "kind": "number", "value": "45" } } }
            ] }
        ]
      },
      "shortExit": {
        "kind": "cross_above",
        "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
                   "params": { "fast": 12, "slow": 26, "signal": 9 } },
        "right": { "kind": "number", "value": "0" }
      }
    }"#;

    let ast: StrategyAst = serde_json::from_str(JSON).expect("JSON 必須合法");
    let mut strategy = ast.compile().expect("必須編得起來");
    // 50% × 2 倍 = 1.0
    assert_eq!(strategy.position_size(), Fixed::ONE);
    // 暖機 = max(四棵樹)：cross_above(macd) 的 34 + 1，以及 sma(50) 的 50
    assert_eq!(strategy.warmup_bars(), 50);

    let bars = market(500);
    let config = crate::backtest::BacktestConfig::frictionless(fx("10000"));
    let result = crate::backtest::run_backtest(&bars, &mut strategy, &config).expect("回測要跑完");

    let first = result.curve.first().expect("要有權益曲線").equity;
    let last = result.curve.last().expect("要有權益曲線").equity;
    println!(
        "手刻 JSON 回測：節點 {} 個、暖機 {} 根、K 線 {} 根、成交 {} 筆、強平 {} 次、\
         起始權益 {} → 期末權益 {}",
        strategy.node_count(),
        strategy.warmup_bars(),
        bars.len(),
        result.trades,
        result.liquidations,
        first,
        last
    );
    assert_eq!(result.curve.len(), bars.len());
    assert!(
        result.trades > 0,
        "這份策略在這段行情裡要真的有進出場，否則回測沒驗到什麼"
    );
}

/// 多空同時成立 → 空手（不是「誰先誰贏」）。
///
/// 理由和 `Bollinger` 處理「標準差為 0」一樣：矛盾的訊號代表沒有資訊，
/// 不該憑矛盾開倉。使用者完全有可能拖出矛盾的條件，所以要寫成測試鎖住。
#[test]
fn simultaneous_long_and_short_cancel_out_into_flat() {
    const JSON: &str = r#"{
      "schemaVersion": 1,
      "direction": "long_short",
      "sizing": { "positionPct": "100", "leverage": "1" },
      "longEntry":  { "kind": "gt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "longExit":   { "kind": "lt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortEntry": { "kind": "gt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortExit":  { "kind": "lt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } }
    }"#;
    let mut strategy = serde_json::from_str::<StrategyAst>(JSON)
        .unwrap()
        .compile()
        .unwrap();
    // 進場兩邊都永遠成立、出場兩邊都永遠不成立 → 多空同時持有 → 空手
    for bar in market(5) {
        assert_eq!(strategy.on_bar(&bar), TargetPosition::FLAT);
    }
}

/// 做空的目標部位是負的，大小和做多一樣。
#[test]
fn a_short_only_signal_is_a_negative_target_position() {
    const JSON: &str = r#"{
      "schemaVersion": 1,
      "direction": "long_short",
      "sizing": { "positionPct": "50", "leverage": "3" },
      "longEntry":  { "kind": "lt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "longExit":   { "kind": "lt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortEntry": { "kind": "gt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortExit":  { "kind": "lt", "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } }
    }"#;
    let mut strategy = serde_json::from_str::<StrategyAst>(JSON)
        .unwrap()
        .compile()
        .unwrap();
    assert_eq!(strategy.position_size(), fx("1.5"));
    assert_eq!(
        strategy.on_bar(&market(1)[0]),
        TargetPosition::short(fx("1.5"))
    );
}
