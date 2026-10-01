//! `compile()` 的信任邊界驗證、三值邏輯、求值順序，以及安全邊界。

use super::*;
use crate::bar::Bar;
use crate::strategy::{Strategy, TargetPosition};

/// 2024-01-01 00:00 UTC
const T0: i64 = 1_704_067_200_000;

fn fx(s: &str) -> Fixed {
    s.parse().unwrap()
}

/// 開高低收都等於收盤價的 1 小時 K 線。
fn closes(values: &[&str]) -> Vec<Bar> {
    values
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let close = fx(close);
            Bar {
                open_time: T0 + i as i64 * 3_600_000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
            }
        })
        .collect()
}

/// 把訊號縮寫成一個字元：`.` 空手、`L` 做多、`S` 做空。
fn signals(strategy: &mut dyn Strategy, bars: &[Bar]) -> String {
    bars.iter()
        .map(|bar| {
            let target = strategy.on_bar(bar);
            if target.is_flat() {
                '.'
            } else if target.ratio().is_negative() {
                'S'
            } else {
                'L'
            }
        })
        .collect()
}

/// 包成一份只做多的策略 JSON。
fn long_only(entry: &str, exit: &str) -> String {
    format!(
        r#"{{"schemaVersion":1,"direction":"long_only",
            "sizing":{{"positionPct":"100","leverage":"1"}},
            "longEntry":{entry},"longExit":{exit}}}"#
    )
}

fn parse(json: &str) -> StrategyAst {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("JSON 應該要合法：{e}\n{json}"))
}

fn compile_long_only(entry: &str, exit: &str) -> Result<CustomStrategy, DslError> {
    parse(&long_only(entry, exit)).compile()
}

/// 永遠成立 / 永遠不成立的條件，用來把注意力集中在另一棵樹上。
const ALWAYS: &str = r#"{"kind":"gt","left":{"kind":"price","field":"close"},
                         "right":{"kind":"number","value":"0"}}"#;
const NEVER: &str = r#"{"kind":"lt","left":{"kind":"price","field":"close"},
                        "right":{"kind":"number","value":"0"}}"#;

fn expect_error(entry: &str, contains: &str) -> DslError {
    let error = compile_long_only(entry, NEVER).expect_err("這份定義應該要被擋下來");
    assert!(
        error.to_string().contains(contains),
        "錯誤訊息要提到 {contains:?}，實際是：{error}"
    );
    error
}

// ---------------------------------------------------------------------------
// 4.4 節的驗證表，逐項一個測試
// ---------------------------------------------------------------------------

#[test]
fn schema_version_must_match() {
    let json = long_only(ALWAYS, NEVER).replace("\"schemaVersion\":1", "\"schemaVersion\":2");
    let error = parse(&json).compile().expect_err("版本不同要報錯");
    assert!(error.to_string().contains("不同版本"), "{error}");
}

#[test]
fn too_many_nodes_is_rejected() {
    // 每個 gt 子條件會攤成 3 個節點（price、number、gt），所以 200 個子條件
    // 加上 any 本身就超過 512
    let children: Vec<String> = (0..200).map(|_| ALWAYS.to_string()).collect();
    let entry = format!(r#"{{"kind":"any","children":[{}]}}"#, children.join(","));
    expect_error(&entry, "積木太多");
}

#[test]
fn a_tree_that_is_too_deep_is_rejected_not_crashed() {
    // 40 層巢狀的 any：深度上限是 32，所以遞迴最多只會走 33 層就回錯誤
    let mut entry = ALWAYS.to_string();
    for _ in 0..40 {
        entry = format!(r#"{{"kind":"any","children":[{entry}]}}"#);
    }
    expect_error(&entry, "巢狀太深");
}

#[test]
fn deeply_nested_json_is_an_error_not_a_stack_overflow() {
    // serde_json 自己有遞迴上限（預設 128 層），所以 10000 層會在反序列化就被擋下來。
    // 這件事要實測，不要只是相信文件。
    let mut json = String::new();
    for _ in 0..10_000 {
        json.push_str(r#"{"kind":"any","children":[ "#);
    }
    json.push_str(ALWAYS);
    for _ in 0..10_000 {
        json.push_str(" ]}");
    }
    let parsed: Result<Cond, _> = serde_json::from_str(&json);
    assert!(parsed.is_err(), "10000 層巢狀的 JSON 必須被拒絕");
}

#[test]
fn indicator_period_has_bounds() {
    for (period, message) in [(0, "1 到 2000"), (2001, "1 到 2000")] {
        let entry = format!(
            r#"{{"kind":"gt","left":{{"kind":"indicator","name":"sma","params":{{"period":{period}}}}},
                 "right":{{"kind":"number","value":"0"}}}}"#
        );
        expect_error(&entry, message);
    }
    // 邊界內要過
    assert!(compile_long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":2000}},
            "right":{"kind":"number","value":"0"}}"#,
        NEVER
    )
    .is_ok());
}

#[test]
fn sustained_bars_has_bounds() {
    for bars in [0, 2001] {
        let entry = format!(r#"{{"kind":"sustained","bars":{bars},"inner":{ALWAYS}}}"#);
        expect_error(&entry, "1 到 2000");
    }
}

#[test]
fn offset_has_an_upper_bound() {
    let entry = r#"{"kind":"gt","left":{"kind":"price","field":"close","offset":501},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "500");
}

#[test]
fn macd_needs_fast_shorter_than_slow() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"macd","output":"line",
                    "params":{"fast":26,"slow":12,"signal":9}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "快線週期必須短於慢線週期");
}

#[test]
fn macd_rejects_a_missing_parameter() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"macd","output":"line",
                    "params":{"fast":12,"slow":26}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "缺少 signal 參數");
}

#[test]
fn multi_output_indicators_need_an_explicit_output() {
    // 不預設成 line／upper，避免使用者以為自己選了別的
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"macd",
                    "params":{"fast":12,"slow":26,"signal":9}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "output 必填");

    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"bb","output":"midle",
                    "params":{"period":20,"mult":"2"}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "不認得 output");
}

#[test]
fn single_output_indicators_reject_an_output() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","output":"line",
                    "params":{"period":20}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "只有一路輸出");
}

#[test]
fn an_empty_and_or_or_is_rejected() {
    // 空的 AND 在邏輯上是「真」，使用者絕對不是這個意思
    expect_error(r#"{"kind":"all","children":[]}"#, "至少要有一個子條件");
    expect_error(r#"{"kind":"any","children":[]}"#, "至少要有一個子條件");
}

#[test]
fn a_number_that_is_not_a_decimal_is_rejected_at_compile_time() {
    let entry = r#"{"kind":"gt","left":{"kind":"price","field":"close"},
                    "right":{"kind":"number","value":"三十"}}"#;
    expect_error(entry, "不是合法的十進位數字");
    // 小數超過 8 位也不行（不會默默捨去）
    let entry = r#"{"kind":"gt","left":{"kind":"price","field":"close"},
                    "right":{"kind":"number","value":"0.000000001"}}"#;
    expect_error(entry, "不是合法的十進位數字");
}

#[test]
fn atr_and_donchian_reject_a_source() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"atr","params":{"period":14},
                    "source":{"kind":"price","field":"high"}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "不能指定 source");
}

#[test]
fn an_indicator_rejects_parameters_it_does_not_understand() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"sma",
                    "params":{"period":20,"mult":"2"}},
                    "right":{"kind":"number","value":"0"}}"#;
    expect_error(entry, "只接受 period 參數");
}

#[test]
fn a_misspelled_parameter_is_rejected_by_serde() {
    let json = long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"periode":20}},
            "right":{"kind":"number","value":"0"}}"#,
        NEVER,
    );
    let parsed: Result<StrategyAst, _> = serde_json::from_str(&json);
    assert!(
        parsed.is_err(),
        "拼錯的參數名必須被擋下來，不是默默套預設值"
    );
}

#[test]
fn direction_and_the_short_trees_must_agree() {
    // long_only 但給了做空條件 → 報錯，不是靜默忽略
    let json = format!(
        r#"{{"schemaVersion":1,"direction":"long_only",
             "sizing":{{"positionPct":"100","leverage":"1"}},
             "longEntry":{ALWAYS},"longExit":{NEVER},"shortEntry":{ALWAYS}}}"#
    );
    let error = parse(&json).compile().expect_err("要報錯");
    assert!(error.to_string().contains("必須留空"), "{error}");

    // long_short 但缺一棵 → 報錯
    let json = format!(
        r#"{{"schemaVersion":1,"direction":"long_short",
             "sizing":{{"positionPct":"100","leverage":"1"}},
             "longEntry":{ALWAYS},"longExit":{NEVER},"shortEntry":{ALWAYS}}}"#
    );
    let error = parse(&json).compile().expect_err("要報錯");
    assert!(error.to_string().contains("兩棵都必填"), "{error}");
}

#[test]
fn sizing_has_bounds() {
    for (pct, leverage, message) in [
        ("0", "1", "大於 0 且不超過 100"),
        ("100.1", "1", "大於 0 且不超過 100"),
        ("-10", "1", "大於 0 且不超過 100"),
        ("100", "0.5", "1 到 125"),
        ("100", "126", "1 到 125"),
        ("100", "三", "不是合法的十進位數字"),
    ] {
        let json = format!(
            r#"{{"schemaVersion":1,"direction":"long_only",
                 "sizing":{{"positionPct":"{pct}","leverage":"{leverage}"}},
                 "longEntry":{ALWAYS},"longExit":{NEVER}}}"#
        );
        let error = parse(&json).compile().expect_err("要報錯");
        assert!(
            error.to_string().contains(message),
            "pct={pct} leverage={leverage} 的訊息要提到 {message:?}，實際是：{error}"
        );
    }
}

#[test]
fn a_position_size_that_rounds_to_zero_is_rejected() {
    // 0.00000001% × 1 倍 = 1e-10，在 8 位小數下是 0 → 永遠不會進場
    let json = format!(
        r#"{{"schemaVersion":1,"direction":"long_only",
             "sizing":{{"positionPct":"0.00000001","leverage":"1"}},
             "longEntry":{ALWAYS},"longExit":{NEVER}}}"#
    );
    let error = parse(&json).compile().expect_err("要報錯");
    assert!(error.to_string().contains("算出來是 0"), "{error}");
}

#[test]
fn errors_carry_a_path_so_the_user_knows_which_block_to_fix() {
    let entry = format!(
        r#"{{"kind":"all","children":[{ALWAYS},
             {{"kind":"gt","left":{{"kind":"indicator","name":"sma","params":{{"period":0}}}},
               "right":{{"kind":"number","value":"0"}}}}]}}"#
    );
    let error = expect_error(&entry, "1 到 2000");
    assert_eq!(error.path, "做多進場條件 → 第 2 個子條件 → 左側");
    assert!(
        error
            .to_string()
            .starts_with("做多進場條件 → 第 2 個子條件 → 左側 ："),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// 攤平與暖機
// ---------------------------------------------------------------------------

#[test]
fn flattening_produces_one_node_per_block_without_deduplicating() {
    // sma10 > sma50 / sma10 <= sma50：兩棵樹各 5 個節點
    // （收盤價、sma、收盤價、sma、比較），重複的 sma 不去重
    let strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":10}},
            "right":{"kind":"indicator","name":"sma","params":{"period":50}}}"#,
        r#"{"kind":"lte","left":{"kind":"indicator","name":"sma","params":{"period":10}},
            "right":{"kind":"indicator","name":"sma","params":{"period":50}}}"#,
    )
    .unwrap();
    assert_eq!(strategy.node_count(), 10);
}

#[test]
fn warmup_follows_the_per_node_table() {
    let cases: [(&str, usize); 8] = [
        // price：1 + offset
        (
            r#"{"kind":"gt","left":{"kind":"price","field":"close","offset":3},
                "right":{"kind":"number","value":"0"}}"#,
            4,
        ),
        // sma(n)：n
        (
            r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":20}},
                "right":{"kind":"number","value":"0"}}"#,
            20,
        ),
        // rsi(n)：n + 1
        (
            r#"{"kind":"gt","left":{"kind":"indicator","name":"rsi","params":{"period":14}},
                "right":{"kind":"number","value":"0"}}"#,
            15,
        ),
        // atr(n)：n + 1
        (
            r#"{"kind":"gt","left":{"kind":"indicator","name":"atr","params":{"period":14}},
                "right":{"kind":"number","value":"0"}}"#,
            15,
        ),
        // macd(f, s, sig)：s + sig − 1
        (
            r#"{"kind":"gt","left":{"kind":"indicator","name":"macd","output":"signal",
                "params":{"fast":12,"slow":26,"signal":9}},
                "right":{"kind":"number","value":"0"}}"#,
            34,
        ),
        // donchian(n) offset 1：n + offset
        (
            r#"{"kind":"gt","left":{"kind":"price","field":"close"},
                "right":{"kind":"indicator","name":"donchian","output":"high",
                         "params":{"period":20},"offset":1}}"#,
            21,
        ),
        // cross_above：max(左, 右) + 1
        (
            r#"{"kind":"cross_above","left":{"kind":"indicator","name":"sma","params":{"period":20}},
                "right":{"kind":"number","value":"0"}}"#,
            21,
        ),
        // sustained(n)：inner + n − 1
        (
            r#"{"kind":"sustained","bars":3,
                "inner":{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":20}},
                         "right":{"kind":"number","value":"0"}}}"#,
            22,
        ),
    ];
    for (entry, expected) in cases {
        let strategy = compile_long_only(entry, NEVER).unwrap();
        assert_eq!(strategy.warmup_bars(), expected, "entry = {entry}");
    }
}

#[test]
fn an_indicator_fed_by_another_indicator_stacks_its_warmup() {
    // sma(10) 吃 ema(20) → 20 + 10 − 1 = 29
    let strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":10},
             "source":{"kind":"indicator","name":"ema","params":{"period":20}}},
            "right":{"kind":"number","value":"0"}}"#,
        NEVER,
    )
    .unwrap();
    assert_eq!(strategy.warmup_bars(), 29);
}

#[test]
fn the_root_warmup_is_the_max_of_all_trees() {
    let strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":5}},
            "right":{"kind":"number","value":"0"}}"#,
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":60}},
            "right":{"kind":"number","value":"0"}}"#,
    )
    .unwrap();
    assert_eq!(strategy.warmup_bars(), 60);
}

// ---------------------------------------------------------------------------
// 三值邏輯與求值順序
// ---------------------------------------------------------------------------

#[test]
fn either_tree_being_unknown_means_flat_and_clears_the_state() {
    // 進場用 sma(2)（第 2 根就有值）、出場用 sma(5)。第 2～4 根進場成立但出場
    // 還算不出來 → 必須空手；第 5 根起才可能進場。
    let mut strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":2}},
            "right":{"kind":"number","value":"0"}}"#,
        r#"{"kind":"lt","left":{"kind":"indicator","name":"sma","params":{"period":5}},
            "right":{"kind":"number","value":"0"}}"#,
    )
    .unwrap();
    let bars = closes(&["10", "10", "10", "10", "10", "10"]);
    assert_eq!(signals(&mut strategy, &bars), "....LL");
}

#[test]
fn any_concludes_as_soon_as_one_child_is_true_even_while_others_warm_up() {
    // 「資訊足夠就下結論」：第一個子條件已經成立，第二個還在暖機也不影響答案
    let entry = format!(
        r#"{{"kind":"any","children":[{ALWAYS},
             {{"kind":"gt","left":{{"kind":"indicator","name":"sma","params":{{"period":50}}}},
               "right":{{"kind":"number","value":"999999"}}}}]}}"#
    );
    let mut strategy = compile_long_only(&entry, NEVER).unwrap();
    assert_eq!(signals(&mut strategy, &closes(&["10", "10", "10"])), "LLL");
}

#[test]
fn all_is_unknown_while_any_child_is_still_warming_up() {
    let entry = format!(
        r#"{{"kind":"all","children":[{ALWAYS},
             {{"kind":"gt","left":{{"kind":"indicator","name":"sma","params":{{"period":3}}}},
               "right":{{"kind":"number","value":"0"}}}}]}}"#
    );
    let mut strategy = compile_long_only(&entry, NEVER).unwrap();
    assert_eq!(
        signals(&mut strategy, &closes(&["10", "10", "10", "10"])),
        "..LL"
    );
}

#[test]
fn all_is_false_as_soon_as_one_child_is_false_even_while_others_warm_up() {
    let entry = format!(
        r#"{{"kind":"all","children":[{NEVER},
             {{"kind":"gt","left":{{"kind":"indicator","name":"sma","params":{{"period":50}}}},
               "right":{{"kind":"number","value":"0"}}}}]}}"#
    );
    // 第一個子條件永遠為假 → all 為假（不是「無法判定」），所以出場樹也能判定
    let mut strategy = compile_long_only(ALWAYS, &entry).unwrap();
    assert_eq!(signals(&mut strategy, &closes(&["10", "10"])), "LL");
}

#[test]
fn exit_wins_when_entry_and_exit_are_both_true() {
    // 這重現 bollinger.rs 的既有語意：標準差為 0 時三線重疊，收盤同時滿足
    // 「≤ 下軌」與「≥ 中軌」，正確答案是空手，不是憑零波動開倉。
    let mut strategy = compile_long_only(ALWAYS, ALWAYS).unwrap();
    assert_eq!(signals(&mut strategy, &closes(&["10", "10", "10"])), "...");

    // 零波動的真正布林通道，和內建 Bollinger 的既有測試對照
    let mut bands = compile_long_only(
        r#"{"kind":"lte","left":{"kind":"price","field":"close"},
            "right":{"kind":"indicator","name":"bb","output":"lower",
                     "params":{"period":3,"mult":"2"}}}"#,
        r#"{"kind":"gte","left":{"kind":"price","field":"close"},
            "right":{"kind":"indicator","name":"bb","output":"middle",
                     "params":{"period":3,"mult":"2"}}}"#,
    )
    .unwrap();
    assert_eq!(
        signals(&mut bands, &closes(&["50", "50", "50", "50"])),
        "...."
    );
}

#[test]
fn hysteresis_holds_the_position_while_neither_tree_fires() {
    // 進場：收盤 ≤ 10；出場：收盤 ≥ 20；中間維持上一根
    let mut strategy = compile_long_only(
        r#"{"kind":"lte","left":{"kind":"price","field":"close"},
            "right":{"kind":"number","value":"10"}}"#,
        r#"{"kind":"gte","left":{"kind":"price","field":"close"},
            "right":{"kind":"number","value":"20"}}"#,
    )
    .unwrap();
    let bars = closes(&["30", "10", "15", "15", "20", "15"]);
    assert_eq!(signals(&mut strategy, &bars), ".LLL..");
}

#[test]
fn cross_above_needs_the_previous_bar_to_be_at_or_below() {
    // 進場只在「穿越的那一根」成立，出場永不成立 → 第一次穿越後鎖住做多
    let mut strategy = compile_long_only(
        r#"{"kind":"cross_above","left":{"kind":"price","field":"close"},
            "right":{"kind":"number","value":"10"}}"#,
        NEVER,
    )
    .unwrap();
    //  9 → 第一根沒有「前一根」，無法判定 → 空手
    // 11 → 前一根 9 ≤ 10、這根 11 > 10 → 穿越 → 做多
    // 12 → 前一根也在上面 → 不是穿越，但出場不成立 → 續抱
    let bars = closes(&["9", "11", "12", "8"]);
    assert_eq!(signals(&mut strategy, &bars), ".LLL");
}

#[test]
fn equalling_the_threshold_is_not_yet_a_cross_above() {
    let mut strategy = compile_long_only(
        r#"{"kind":"cross_above","left":{"kind":"price","field":"close"},
            "right":{"kind":"number","value":"10"}}"#,
        ALWAYS,
    )
    .unwrap();
    // 出場永遠成立，所以只有「真的穿越」那根會是做多…但出場先判斷 → 永遠空手。
    // 換成永不出場才看得到穿越那一根。
    assert_eq!(signals(&mut strategy, &closes(&["9", "10", "11"])), "...");

    let mut strategy = compile_long_only(
        r#"{"kind":"cross_above","left":{"kind":"price","field":"close"},
            "right":{"kind":"number","value":"10"}}"#,
        NEVER,
    )
    .unwrap();
    // 9 → 無前一根；10 → 不大於 10，不是穿越；11 → 前一根 10 ≤ 10 → 穿越
    assert_eq!(signals(&mut strategy, &closes(&["9", "10", "11"])), "..L");
}

#[test]
fn sustained_needs_the_whole_window_to_be_true() {
    let entry = r#"{"kind":"sustained","bars":3,
                    "inner":{"kind":"gt","left":{"kind":"price","field":"close"},
                             "right":{"kind":"number","value":"10"}}}"#;
    let mut strategy = compile_long_only(entry, NEVER).unwrap();
    assert_eq!(strategy.warmup_bars(), 3);
    // 11、11 → 視窗未滿 → 無法判定 → 空手
    // 11 → 連續 3 根都 > 10 → 進場（出場永不成立所以之後都續抱）
    assert_eq!(signals(&mut strategy, &closes(&["11", "11", "11"])), "..L");

    // 視窗內有一根不成立 → 整段不算連續
    let mut strategy = compile_long_only(entry, NEVER).unwrap();
    assert_eq!(
        signals(&mut strategy, &closes(&["11", "9", "11", "11"])),
        "...."
    );
}

#[test]
fn sustained_restarts_after_the_streak_breaks() {
    let entry = r#"{"kind":"sustained","bars":3,
                    "inner":{"kind":"gt","left":{"kind":"price","field":"close"},
                             "right":{"kind":"number","value":"10"}}}"#;
    // 出場 = 進場的反面，這樣每一根都看得到 sustained 的真假
    let exit = r#"{"kind":"lte","left":{"kind":"price","field":"close"},
                   "right":{"kind":"number","value":"10"}}"#;
    let mut strategy = compile_long_only(entry, exit).unwrap();
    let bars = closes(&["11", "11", "11", "9", "11", "11", "11", "11"]);
    //                   .    .    L    .    .    .    L    L
    assert_eq!(signals(&mut strategy, &bars), "..L...LL");
}

#[test]
fn an_offset_price_reads_the_bar_before_pushing_this_one() {
    // 前一根收盤價 > 10 才進場
    let mut strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"price","field":"close","offset":1},
            "right":{"kind":"number","value":"10"}}"#,
        r#"{"kind":"lte","left":{"kind":"price","field":"close","offset":1},
            "right":{"kind":"number","value":"10"}}"#,
    )
    .unwrap();
    assert_eq!(strategy.warmup_bars(), 2);
    // 第一根沒有「前一根」→ 空手；第二根看的是 11 → 做多；第三根看的是 9 → 空手
    assert_eq!(signals(&mut strategy, &closes(&["11", "9", "20"])), ".L.");
}

/// **每個節點每根 K 線都算一次**——這是攤平陣列最重要的性質。
///
/// 做法：把一個有狀態的 `cross_above` 放進 `any` 的**第二個**位置，第一個子條件
/// 是「收盤 > sma(50)」（會時真時假）。天真的短路求值器在第一個子條件成立的那些
/// 根會跳過 `cross_above`，它記的「前一根比較結果」就會停在更早的一根，之後算出
/// 一個根本沒發生過的穿越。
///
/// 驗法：巢狀版的結果必須剛好等於「兩個子條件各自單獨跑」的邏輯或，而且要確認
/// 真的有「第一個子條件成立、同時第二個子條件是穿越」的重疊根數——否則這個測試
/// 根本沒踩到會出問題的那條路徑。
#[test]
fn no_node_is_ever_skipped_even_when_a_sibling_already_decided_the_answer() {
    const TREND: &str = r#"{"kind":"gt","left":{"kind":"price","field":"close"},
                            "right":{"kind":"indicator","name":"sma","params":{"period":50}}}"#;
    const CROSS: &str = r#"{"kind":"cross_above","left":{"kind":"price","field":"close"},
                            "right":{"kind":"indicator","name":"sma","params":{"period":3}}}"#;

    let bars = wavy(300);
    // 出場樹是觀察窗：進場永遠成立，所以「空手」就代表出場條件這根成立
    let exits_of = |exit: &str| -> Vec<bool> {
        let mut strategy = compile_long_only(ALWAYS, exit).unwrap();
        bars.iter()
            .map(|bar| strategy.on_bar(bar).is_flat())
            .collect()
    };
    let trend = exits_of(TREND);
    let cross = exits_of(CROSS);
    let nested = exits_of(&format!(r#"{{"kind":"any","children":[{TREND},{CROSS}]}}"#));

    // 暖機期（sma(50) 之前）三者都是「無法判定 → 空手」，從第 50 根起才有意義
    let mut overlap = 0;
    for i in 50..bars.len() {
        assert_eq!(
            nested[i],
            trend[i] || cross[i],
            "第 {} 根：巢狀版和兩個子條件的邏輯或不一致",
            i + 1
        );
        if trend[i] && cross[i] {
            overlap += 1;
        }
    }
    assert!(
        overlap > 0,
        "測資裡「第一個子條件成立、同時第二個子條件是穿越」的根只有 {overlap} 根，\
         這個測試沒踩到短路求值會出錯的那條路徑"
    );
}

/// 一段會上下震盪的確定性行情（夠讓 cross_above 觸發很多次）。
fn wavy(count: usize) -> Vec<Bar> {
    let mut seed: u64 = 0x1234_5678_9abc;
    let mut close: i64 = 100 * Fixed::SCALE;
    (0..count)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            close = (close + ((seed % 601) as i64 - 300) * 1_000_000)
                .clamp(20 * Fixed::SCALE, 400 * Fixed::SCALE);
            let price = Fixed::from_raw(close);
            Bar {
                open_time: T0 + i as i64 * 3_600_000,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 1.0,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 指標節點（MACD／ATR 的數值由 indicators.rs 的測試向量驗證）
// ---------------------------------------------------------------------------

#[test]
fn every_indicator_compiles_and_never_panics() {
    let specs = [
        r#"{"kind":"indicator","name":"sma","params":{"period":5}}"#,
        r#"{"kind":"indicator","name":"ema","params":{"period":5}}"#,
        r#"{"kind":"indicator","name":"rsi","params":{"period":5}}"#,
        r#"{"kind":"indicator","name":"macd","output":"histogram","params":{"fast":2,"slow":4,"signal":2}}"#,
        r#"{"kind":"indicator","name":"bb","output":"upper","params":{"period":5,"mult":"2"}}"#,
        r#"{"kind":"indicator","name":"atr","params":{"period":5}}"#,
        r#"{"kind":"indicator","name":"donchian","output":"low","params":{"period":5}}"#,
        r#"{"kind":"indicator","name":"highest","params":{"period":5},"source":{"kind":"price","field":"high"}}"#,
        r#"{"kind":"indicator","name":"lowest","params":{"period":5},"source":{"kind":"price","field":"low"}}"#,
    ];
    let extreme = crate::strategies::test_util::extreme_bars();
    for spec in specs {
        let entry =
            format!(r#"{{"kind":"gt","left":{spec},"right":{{"kind":"number","value":"0"}}}}"#);
        let mut strategy = compile_long_only(&entry, NEVER)
            .unwrap_or_else(|e| panic!("{spec} 應該要編得起來：{e}"));
        // 不 panic 就算通過
        let _ = signals(&mut strategy, &extreme);
    }
}

#[test]
fn volume_can_be_compared_in_the_fixed_domain() {
    let mut strategy = compile_long_only(
        r#"{"kind":"gt","left":{"kind":"price","field":"volume"},
            "right":{"kind":"number","value":"100"}}"#,
        r#"{"kind":"lte","left":{"kind":"price","field":"volume"},
            "right":{"kind":"number","value":"100"}}"#,
    )
    .unwrap();
    let mut bars = closes(&["10", "10", "10"]);
    bars[0].volume = 50.0;
    bars[1].volume = 150.0;
    // 超過 Fixed 上限（約 922 億）→ None → 空手，不做飽和
    bars[2].volume = 1e20;
    assert_eq!(signals(&mut strategy, &bars), ".L.");
}

// ---------------------------------------------------------------------------
// 安全邊界
// ---------------------------------------------------------------------------

/// `at-core` 的依賴清單只能有 serde——DSL 連「有一個交易所」這件事都不該知道。
#[test]
fn at_core_depends_on_nothing_that_could_place_an_order() {
    let manifest = include_str!("../../Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .expect("Cargo.toml 要有 [dependencies] 區段");
    let deps = deps.split("[dev-dependencies]").next().unwrap();
    let names: Vec<&str> = deps
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('['))
        .map(|line| line.split('=').next().unwrap_or("").trim())
        .collect();
    assert_eq!(
        names,
        vec!["serde"],
        "at-core 的 runtime 依賴只能有 serde（加任何能下單／連網的東西就破壞了這條界線）"
    );
}

/// DSL 模組碰不到交易所、網路、檔案系統，也不能有「動作」節點。
#[test]
fn the_dsl_cannot_reach_the_network_the_filesystem_or_an_exchange() {
    let sources = [
        ("mod.rs", include_str!("mod.rs")),
        ("eval.rs", include_str!("eval.rs")),
        ("indicators.rs", include_str!("indicators.rs")),
    ];
    let forbidden = [
        "std::fs",
        "std::net",
        "std::process",
        "at_binance_client",
        "at_testnet_trading",
        "at_paper_trading",
        "at_risk_control",
        "reqwest",
        "tokio",
        "OrderGateway",
        "place_order",
    ];
    for (name, source) in sources {
        for token in forbidden {
            assert!(
                !source.contains(token),
                "{name} 出現了 {token}：DSL 是純計算，不可以有任何送單或 I/O 的路徑"
            );
        }
    }
}

/// `CustomStrategy` 必須是 `Send`，否則 `at_paper_trading::spawn` 與
/// `at_testnet_trading::spawn`（都要 `Box<dyn Strategy + Send>`）就得改。
#[test]
fn a_custom_strategy_is_a_sendable_strategy_object() {
    fn assert_send<T: Send>() {}
    assert_send::<CustomStrategy>();

    let strategy = compile_long_only(ALWAYS, NEVER).unwrap();
    let boxed: Box<dyn Strategy + Send> = Box::new(strategy);
    assert_eq!(boxed.warmup_bars(), 1);
}

/// 荒謬但「合法」的上限組合要編得起來並跑完，不會吃爆記憶體或掛掉。
#[test]
fn the_worst_legal_strategy_still_runs() {
    let entry = r#"{"kind":"gt","left":{"kind":"indicator","name":"sma","params":{"period":2000},
                    "offset":500},"right":{"kind":"number","value":"0"}}"#;
    let mut strategy = compile_long_only(entry, NEVER).unwrap();
    assert_eq!(strategy.warmup_bars(), 2500);
    // 還在暖機，所以必須是空手——而且不會 panic
    assert_eq!(strategy.on_bar(&closes(&["10"])[0]), TargetPosition::FLAT);
}
