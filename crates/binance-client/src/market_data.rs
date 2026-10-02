//! 公開行情端點：補抓「最近 N 根已收盤」的歷史 K 線，給暖機回放用
//! （[`at_core::WarmupBars`]）。
//!
//! # 為什麼不用 `at-downloader`
//!
//! 這個專案已經有一條歷史 K 線的路（1.7 的 `at-downloader`：從
//! `data.binance.vision` 抓**月檔** zip），但它解不了暖機這個題目：月檔要等
//! 一個月結束之後才發布。要在「現在」啟動一個 session，最新的月檔可能是上個
//! 月的——1 分鐘週期拿到一個月前的 50 根 K 線，接著即時行情直接跳到現在，
//! 中間缺幾萬根。那不是暖機，那是把一段無關的歷史灌進指標裡，比冷啟動更糟
//! （SMA 的視窗會是 49 根上個月的價格 + 1 根現在的價格）。
//!
//! `/api/v3/klines` 給的是「一直到現在這一根」的連續 K 線，正好是暖機要的東西。
//! 所以這裡不是發明新的資料來源，是補上 `at-downloader` 結構上做不到的那一段：
//! 月檔管「跑一段歷史回測」，這裡管「接上現在」。
//!
//! # 這是正式環境的公開端點，和「下單」無關
//!
//! `/api/v3/klines` 是 Binance 文件標示安全等級 `NONE` 的公開端點，不帶金鑰
//! （走 [`public_get`]，連 Keychain 都不會讀）。4.5 的即時行情串流、4.4 的下單
//! 規則同步本來就是打正式環境的公開資料，這裡一致。
//!
//! 測試網交易的暖機也該用**正式環境**的 K 線：測試網自己的成交稀疏、價格不真實，
//! 用它暖機等於讓指標收斂到一段假的市場結構上。送單走測試網、看行情看正式環境，
//! 這是 6.4／6.5 既有的分工。

use crate::{public_get, BinanceError};
use at_core::{Bar, Fixed, Interval, OrderFlow, Symbol, WarmupBars};
use serde_json::Value;

const KLINES_PATH: &str = "/api/v3/klines";

/// Binance `/api/v3/klines` 單次請求的根數上限（官方文件的 `limit` 最大值）。
pub const MAX_KLINES_PER_REQUEST: usize = 1000;

/// 抓「最近 `count` 根**已收盤**」的 K 線，回傳可以直接拿去回放的
/// [`WarmupBars`]。
///
/// `count` 用 [`at_core::warmup_fetch_count`] 算；傳 `0` 時**不發任何網路請求**，
/// 直接回 [`WarmupBars::none`]（不需要暖機的策略不該因為網路不通而不能啟動）。
///
/// # 怎麼確定「已收盤」
///
/// Binance 回傳的最後一筆是**還在形成中**的那根 K 線。這裡多要一根、
/// 無條件丟掉最後一筆，不讀本機時鐘去跟 `closeTime` 比對——本機時鐘和交易所
/// 差幾秒就會判斷錯，而「少暖機一根」沒有代價（那一根稍後會由即時串流送來）。
///
/// # 拿不到那麼多根
///
/// 新上市的交易對、或 `count` 接近 [`MAX_KLINES_PER_REQUEST`] 時，回傳的根數會
/// 少於要求。這裡**不當成錯誤**，而是如實回傳拿到的根數：呼叫端比對
/// `warmup.len()` 與要求的 `count`，自己決定要不要警告使用者。這個函式不會
/// 假造 K 線，也不會默默當成「暖機完成」。
pub fn recent_closed_bars(
    symbol: &Symbol,
    interval: Interval,
    count: usize,
) -> Result<WarmupBars, BinanceError> {
    if count == 0 {
        return Ok(WarmupBars::none());
    }
    // 多要一根來丟：最後一筆是還在形成中的 K 線。
    let limit = count.saturating_add(1).min(MAX_KLINES_PER_REQUEST);
    let body = public_get(
        KLINES_PATH,
        &[
            ("symbol", symbol.as_str()),
            ("interval", interval.as_str()),
            ("limit", &limit.to_string()),
        ],
    )?;

    let mut bars = parse_klines_json(&body)?;
    // 丟掉還在形成中的那一根。空回應（新上市的交易對）不是錯誤。
    bars.pop();
    if bars.len() > count {
        bars.drain(..bars.len() - count);
    }

    WarmupBars::new(bars, interval).map_err(|e| {
        BinanceError::Response(format!(
            "交易所回傳的歷史 K 線順序有問題（{e}），無法用來暖機"
        ))
    })
}

/// 解析 `/api/v3/klines` 的回應：JSON 陣列的陣列，每筆前 6 個欄位是
/// `[開盤時間, 開, 高, 低, 收, 量]`（和 1.5 的官方 CSV 同一組欄位，
/// 見 [`at_core::parse_klines_csv`]），第 9、10 欄（索引 `[8]`／`[9]`）
/// 是成交筆數與主動買方成交量，組成 `OrderFlow`。
///
/// 欄位數 `>= 10` 才讀訂單流，否則 `order_flow: None`（由呼叫端的
/// `needs_order_flow()` 檢查擋下，不會靜默變成零交易）。交易所之後在
/// 後面加欄位也不會讓解析失敗。
///
/// 外部輸入一律回 [`Result`]，沒有任何 `unwrap`。
fn parse_klines_json(body: &str) -> Result<Vec<Bar>, BinanceError> {
    let rows: Vec<Vec<Value>> = serde_json::from_str(body)
        .map_err(|e| BinanceError::Response(format!("歷史 K 線不是預期的 JSON 格式：{e}")))?;

    rows.iter().enumerate().map(parse_row).collect()
}

fn parse_row((index, row): (usize, &Vec<Value>)) -> Result<Bar, BinanceError> {
    // 第幾筆，給人看的從 1 起算。
    let no = index + 1;
    let bad = |field: &str| BinanceError::Response(format!("歷史 K 線第 {no} 筆的{field}不合法"));

    if row.len() < 6 {
        return Err(bad("欄位數"));
    }
    let price = |at: usize, field: &'static str| -> Result<Fixed, BinanceError> {
        row[at]
            .as_str()
            .ok_or_else(|| bad(field))?
            .parse::<Fixed>()
            .map_err(|_| bad(field))
    };

    let order_flow = if row.len() >= 10 {
        let trades = row[8].as_u64().ok_or_else(|| bad("成交筆數"))?;
        let taker_buy_volume = row[9]
            .as_str()
            .ok_or_else(|| bad("主動買方成交量"))?
            .parse::<f64>()
            .map_err(|_| bad("主動買方成交量"))?;
        Some(OrderFlow {
            trades,
            taker_buy_volume,
        })
    } else {
        None
    };

    let bar = Bar {
        open_time: row[0].as_i64().ok_or_else(|| bad("開盤時間"))?,
        open: price(1, "開盤價")?,
        high: price(2, "最高價")?,
        low: price(3, "最低價")?,
        close: price(4, "收盤價")?,
        // 成交量只用在統計與畫圖，`Bar` 本來就是 f64（見 at_core::Bar）。
        volume: row[5]
            .as_str()
            .ok_or_else(|| bad("成交量"))?
            .parse::<f64>()
            .map_err(|_| bad("成交量"))?,
        order_flow,
    };
    bar.validate()
        .map_err(|e| BinanceError::Response(format!("歷史 K 線第 {no} 筆不合理：{e}")))?;
    Ok(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 兩筆真實格式的 1 分鐘 K 線（欄位照 Binance 官方文件逐字複製的形狀）。
    const TWO_ROWS: &str = r#"[
      [1704067200000,"42000.00","42050.00","41980.00","42020.00","12.34500000",1704067259999,"518000.00",308,"6.00000000","252000.00","0"],
      [1704067260000,"42020.00","42100.00","42000.00","42080.00","15.67800000",1704067319999,"659000.00",412,"8.00000000","336000.00","0"]
    ]"#;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    #[test]
    fn parses_the_first_six_fields_of_each_row() {
        let bars = parse_klines_json(TWO_ROWS).expect("官方格式應該解析得出來");
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open_time, 1_704_067_200_000);
        assert_eq!(bars[0].open, fx("42000"));
        assert_eq!(bars[0].high, fx("42050"));
        assert_eq!(bars[0].low, fx("41980"));
        assert_eq!(bars[0].close, fx("42020"));
        assert!((bars[0].volume - 12.345).abs() < 1e-9);
        assert_eq!(bars[1].open_time, 1_704_067_260_000);
        assert_eq!(
            bars[0].order_flow,
            Some(OrderFlow {
                trades: 308,
                taker_buy_volume: 6.0,
            })
        );
    }

    #[test]
    fn extra_trailing_fields_do_not_break_parsing() {
        // 交易所之後在後面加欄位時不該整個 session 起不來。
        let body = r#"[[1704067200000,"1","1","1","1","0",2,"0",0,"0","0","0","新欄位"]]"#;
        assert_eq!(parse_klines_json(body).expect("多餘欄位要忽略").len(), 1);
    }

    #[test]
    fn an_empty_response_is_not_an_error() {
        assert!(parse_klines_json("[]").expect("空陣列合法").is_empty());
    }

    #[test]
    fn a_non_numeric_price_is_a_clear_error_not_a_panic() {
        let body =
            r#"[[1704067200000,"42000","not-a-number","41980","42020","1.0",2,"0",0,"0","0","0"]]"#;
        let error = parse_klines_json(body).expect_err("壞掉的價格要回錯誤");
        let message = error.to_string();
        assert!(message.contains("第 1 筆"), "錯誤要指出是第幾筆：{message}");
        assert!(message.contains("最高價"), "錯誤要指出是哪一欄：{message}");
    }

    #[test]
    fn a_row_with_too_few_fields_is_an_error() {
        let body = r#"[[1704067200000,"42000","42050"]]"#;
        assert!(parse_klines_json(body).is_err());
    }

    #[test]
    fn a_bar_that_cannot_exist_is_rejected() {
        // 最高價低於最低價：至少有一邊是錯的，不可以拿去餵策略。
        let body = r#"[[1704067200000,"42000","100","41980","42020","1.0",2,"0",0,"0","0","0"]]"#;
        let error = parse_klines_json(body).expect_err("不合理的 K 線要回錯誤");
        assert!(error.to_string().contains("不合理"), "{error}");
    }

    #[test]
    fn html_or_garbage_is_a_clear_error() {
        let error =
            parse_klines_json("<html>502 Bad Gateway</html>").expect_err("非 JSON 的回應要回錯誤");
        assert!(error.to_string().contains("JSON"), "{error}");
    }

    #[test]
    fn asking_for_no_warmup_makes_no_request() {
        // 這個測試本身就是證據：沒有網路也會通過。
        let warmup = recent_closed_bars(&Symbol::new("BTCUSDT").unwrap(), Interval::M1, 0)
            .expect("不需要暖機時不該失敗");
        assert_eq!(warmup, WarmupBars::none());
    }

    /// 真的打一次 `data` 端點，驗證「拿回來的根數、順序、丟掉形成中那根」都對。
    /// 公開端點，不需要金鑰、沒有任何送單路徑。
    ///
    /// 手動驗證：`cargo test -p at-binance -- --ignored --nocapture market_data`
    #[test]
    #[ignore]
    fn fetches_real_closed_bars_from_binance() {
        let symbol = Symbol::new("BTCUSDT").unwrap();
        let mut strategy = at_core::SmaCross::new(10, 30).unwrap();
        let count = at_core::warmup_fetch_count(&strategy);
        assert_eq!(count, 150);

        let warmup =
            recent_closed_bars(&symbol, Interval::M1, count).expect("公開端點應該抓得到 K 線");
        assert_eq!(warmup.len(), count, "BTCUSDT 1 分鐘線不該缺根");
        println!("抓到 {} 根已收盤的 1 分鐘 K 線", warmup.len());

        // 丟掉形成中那根的證據：最後一根的收盤時間已經過去了。
        let through = warmup.replay(&mut strategy).expect("應該有最後一根");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系統時間應該晚於 1970")
            .as_millis() as i64;
        assert!(
            through + Interval::M1.millis() <= now_ms,
            "最後一根必須已經收盤：開盤 {through}、現在 {now_ms}"
        );
    }
}
