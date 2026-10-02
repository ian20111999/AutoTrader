//! JSON 形狀的契約測試。
//!
//! 專案既有陷阱 #1（前後端命名風格不一致）在這裡特別容易中招，因為 DSL 同時
//!用了兩套規則：
//!
//! - **欄位名 camelCase**：`schemaVersion`、`longEntry`、`shortExit`、`positionPct`
//! - **`kind` 的值 snake_case**：`cross_above`、`long_only`
//!
//! 它們不一樣是因為一個是欄位名、一個是識別字。兩邊都用測試鎖住，之後手寫
//! TypeScript 型別時才有對照。

use super::*;

#[test]
fn field_names_are_camel_case_and_kind_values_are_snake_case() {
    let json = r#"{
      "schemaVersion": 1,
      "direction": "long_short",
      "sizing": { "positionPct": "50", "leverage": "2" },
      "longEntry":  { "kind": "cross_above",
                      "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "longExit":   { "kind": "cross_below",
                      "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortEntry": { "kind": "cross_below",
                      "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } },
      "shortExit":  { "kind": "cross_above",
                      "left": { "kind": "price", "field": "close" },
                      "right": { "kind": "number", "value": "0" } }
    }"#;
    let ast: StrategyAst = serde_json::from_str(json).expect("要解析得出來");
    assert_eq!(ast.direction, Direction::LongShort);
    assert_eq!(ast.sizing.position_pct, "50");

    // 序列化回去的欄位名與 kind 值要和輸入一致
    let text = serde_json::to_string(&ast).unwrap();
    for expected in [
        "\"schemaVersion\":1",
        "\"direction\":\"long_short\"",
        "\"positionPct\":\"50\"",
        "\"longEntry\"",
        "\"longExit\"",
        "\"shortEntry\"",
        "\"shortExit\"",
        "\"kind\":\"cross_above\"",
        "\"kind\":\"cross_below\"",
    ] {
        assert!(text.contains(expected), "序列化結果少了 {expected}：{text}");
    }
    // 不可以洩漏 Rust 的命名風格
    for leaked in [
        "schema_version",
        "long_entry",
        "position_pct",
        "crossAbove",
        "LongShort",
    ] {
        assert!(!text.contains(leaked), "序列化結果洩漏了 {leaked}：{text}");
    }
}

#[test]
fn every_node_kind_round_trips() {
    // 每種節點各一個的範例（之後給前端的契約測試用同一份）
    let json = r#"{
      "schemaVersion": 1,
      "direction": "long_only",
      "sizing": { "positionPct": "100", "leverage": "1" },
      "longEntry": {
        "kind": "all",
        "children": [
          { "kind": "gt",  "left": { "kind": "price", "field": "open" },
                           "right": { "kind": "number", "value": "1" } },
          { "kind": "gte", "left": { "kind": "price", "field": "high", "offset": 2 },
                           "right": { "kind": "indicator", "name": "sma", "params": { "period": 5 } } },
          { "kind": "lt",  "left": { "kind": "price", "field": "low" },
                           "right": { "kind": "indicator", "name": "ema", "params": { "period": 5 } } },
          { "kind": "lte", "left": { "kind": "price", "field": "volume" },
                           "right": { "kind": "indicator", "name": "rsi", "params": { "period": 5 } } },
          { "kind": "cross_above",
            "left": { "kind": "indicator", "name": "macd", "output": "histogram",
                      "params": { "fast": 2, "slow": 4, "signal": 2 } },
            "right": { "kind": "number", "value": "0" } },
          { "kind": "cross_below",
            "left": { "kind": "indicator", "name": "bb", "output": "upper",
                      "params": { "period": 5, "mult": "1.5" } },
            "right": { "kind": "indicator", "name": "atr", "params": { "period": 5 } } },
          { "kind": "any", "children": [
            { "kind": "gt", "left": { "kind": "indicator", "name": "donchian", "output": "high",
                                      "params": { "period": 5 }, "offset": 1 },
                            "right": { "kind": "indicator", "name": "highest", "params": { "period": 5 } } },
            { "kind": "sustained", "bars": 2,
              "inner": { "kind": "lt",
                         "left": { "kind": "indicator", "name": "lowest", "params": { "period": 5 },
                                   "source": { "kind": "price", "field": "low" } },
                         "right": { "kind": "number", "value": "99999" } } }
          ] }
        ]
      },
      "longExit": { "kind": "lt", "left": { "kind": "price", "field": "close" },
                    "right": { "kind": "number", "value": "0" } }
    }"#;
    let ast: StrategyAst = serde_json::from_str(json).expect("要解析得出來");
    let text = serde_json::to_string(&ast).unwrap();
    let again: StrategyAst = serde_json::from_str(&text).expect("序列化出來的要解析得回去");
    assert_eq!(ast, again, "來回一趟不可以改變任何東西");
    // 這份範例也要真的編得起來
    assert!(ast.compile().is_ok());
}

#[test]
fn an_omitted_offset_defaults_to_zero_and_is_not_serialized() {
    let expr: Expr = serde_json::from_str(r#"{"kind":"price","field":"close"}"#).unwrap();
    assert_eq!(
        expr,
        Expr::Price {
            field: PriceField::Close,
            offset: 0
        }
    );
    let text = serde_json::to_string(&expr).unwrap();
    assert_eq!(text, r#"{"kind":"price","field":"close"}"#);
}

#[test]
fn the_short_trees_are_omitted_when_null() {
    let json = r#"{
      "schemaVersion": 1,
      "direction": "long_only",
      "sizing": { "positionPct": "100", "leverage": "1" },
      "longEntry": { "kind": "lt", "left": { "kind": "price", "field": "close" },
                     "right": { "kind": "number", "value": "0" } },
      "longExit":  { "kind": "lt", "left": { "kind": "price", "field": "close" },
                     "right": { "kind": "number", "value": "0" } },
      "shortEntry": null,
      "shortExit": null
    }"#;
    let ast: StrategyAst = serde_json::from_str(json).expect("明寫 null 也要收");
    assert_eq!(ast.short_entry, None);
    let text = serde_json::to_string(&ast).unwrap();
    assert!(
        !text.contains("shortEntry"),
        "null 的做空樹不必序列化出來：{text}"
    );
}

#[test]
fn an_unknown_node_kind_is_an_error_not_a_silent_fallback() {
    // 舊引擎讀到未來才有的節點種類（例如算術節點）要報「不支援」，不是誤解
    let parsed: Result<Expr, _> =
        serde_json::from_str(r#"{"kind":"arith","op":"mul","left":null,"right":null}"#);
    assert!(parsed.is_err(), "不認得的 kind 必須是錯誤");
}

#[test]
fn an_unknown_field_is_rejected() {
    // 使用者手打的 JSON 多一個欄位，通常是拼錯了而不是多給資訊
    for json in [
        r#"{"kind":"price","field":"close","offsett":1}"#,
        r#"{"kind":"number","value":"1","scale":"2"}"#,
    ] {
        let parsed: Result<Expr, _> = serde_json::from_str(json);
        assert!(parsed.is_err(), "{json} 應該要被拒絕");
    }
}

#[test]
fn order_flow_price_field_names_are_locked() {
    for (field, json_name) in [
        (PriceField::Trades, "trades"),
        (PriceField::TakerBuyRatio, "taker_buy_ratio"),
    ] {
        let expr = Expr::Price { field, offset: 0 };
        let text = serde_json::to_string(&expr).unwrap();
        assert_eq!(text, format!(r#"{{"kind":"price","field":"{json_name}"}}"#));
        let back: Expr = serde_json::from_str(&text).unwrap();
        assert_eq!(back, expr);
    }
}

#[test]
fn a_bad_price_field_name_is_rejected() {
    let parsed: Result<Expr, _> = serde_json::from_str(r#"{"kind":"price","field":"Close"}"#);
    assert!(parsed.is_err(), "欄位名大小寫不同就是不同的東西");
}
