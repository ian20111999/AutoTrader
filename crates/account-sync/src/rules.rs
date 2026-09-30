//! 下單規則同步：把 Binance 對某個交易對的真實下單限制（價格跳動、數量級距、
//! 數量上下限、最小金額）抓回本機，換成 1.3 的 [`SymbolRules`]，抓失敗時沿用
//! 上次的值。
//!
//! 行為規則和同 crate 的手續費同步（4.3，見 crate 根模組）一模一樣：24 小時
//! 快取、失敗沿用舊值並標成 [`Freshness::Stale`]、完全沒有舊值才報錯。
//! 規則算錯的後果和費率算錯一樣直接：回測會出現真實世界下不了的單。
//!
//! 和費率同步的兩點差別：
//!
//! 1. `GET /api/v3/exchangeInfo` 是**公開端點**，不用簽名、不用讀 Keychain。
//! 2. 規則藏在回應的 `filters` 陣列裡，要按 `filterType` 挑出需要的那幾個，
//!    不能看到欄位名對就拿（見 [`SymbolInfoJson::to_rules_json`]）。

use crate::{Freshness, CACHE_TTL_MS};
use at_binance::BinanceError;
use at_core::{Fixed, Symbol, SymbolRules};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 交易規則端點（公開、唯讀）。
const EXCHANGE_INFO_PATH: &str = "/api/v3/exchangeInfo";

/// 同步結果：規則本身 + 這份資料新不新鮮。
///
/// 和 4.3 的 `SyncedFees` 一樣刻意不直接回傳 [`SymbolRules`]：呼叫端必須看得到
/// 「這是不是舊資料」。時間戳放在這層 wrapper，不動 1.3 的 [`SymbolRules`]——
/// 那個型別是回測引擎的核心，不該為了同步機制多長一個欄位。
#[derive(Debug, Clone)]
pub struct SyncedRules {
    pub rules: SymbolRules,
    /// 上次**成功**從 Binance 拿到這份規則的時間（UTC 毫秒）。
    pub synced_at_ms: i64,
    pub freshness: Freshness,
}

/// 同步下單規則失敗的原因。
#[derive(Debug)]
pub enum RulesSyncError {
    /// API 失敗，而且本機沒有可用的舊值（還沒成功同步過，或快取檔壞掉）。
    NoCachedRules(BinanceError),
    /// 回應（或快取檔）的內容不是預期的規則格式。
    BadResponse(String),
    /// 系統時間早於 1970 年，算不出時間戳（實務上不會發生）。
    SystemClock,
}

impl fmt::Display for RulesSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RulesSyncError::NoCachedRules(err) => write!(
                f,
                "同步下單規則失敗，而且本機沒有可以沿用的舊規則：{err}。\
                 請確認網路連線後再試一次"
            ),
            RulesSyncError::BadResponse(what) => {
                write!(f, "看不懂 Binance 回傳的下單規則內容：{what}")
            }
            RulesSyncError::SystemClock => write!(f, "無法取得系統時間"),
        }
    }
}

impl std::error::Error for RulesSyncError {}

/// `exchangeInfo` 回應裡 `filters` 陣列的一個元素。
///
/// 不同 `filterType` 的欄位不一樣，所以除了 `filterType` 全部收成 `Option`，
/// 由 [`SymbolInfoJson::to_rules_json`] 按 `filterType` 挑。這裡沒收的欄位
/// （`minPrice`/`maxPrice`/`maxNotional`、各種張數上限…）1.3 的 [`SymbolRules`]
/// 沒有對應位置，serde 會直接忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterJson {
    filter_type: String,
    tick_size: Option<String>,
    min_qty: Option<String>,
    max_qty: Option<String>,
    step_size: Option<String>,
    min_notional: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SymbolInfoJson {
    symbol: String,
    filters: Vec<FilterJson>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExchangeInfoJson {
    symbols: Vec<SymbolInfoJson>,
}

/// 本機快取存的規則。
///
/// 只留 [`SymbolRules`] 真的會用到的五個數字，不存整包 `exchangeInfo`：不帶
/// `symbol` 參數時那包是上千個交易對、好幾 MB，其中絕大多數欄位這個工具永遠
/// 用不到。數字照 Binance 的原樣存成字串再自己 `parse`——`Fixed` 住在零外部
/// 依賴的 `at-core`，不能掛 serde 的 derive，收字串也比較好回報是哪一欄壞掉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RulesJson {
    symbol: String,
    tick_size: String,
    step_size: String,
    min_qty: String,
    max_qty: String,
    min_notional: String,
}

/// 本機快取檔：規則 + 這份規則是什麼時候拿到的。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedRules {
    /// 上次成功從 Binance 拿到這份規則的時間（UTC 毫秒）。
    synced_at_ms: i64,
    rules: RulesJson,
}

fn required(
    value: &Option<String>,
    filter_type: &str,
    field: &str,
) -> Result<String, RulesSyncError> {
    value
        .clone()
        .ok_or_else(|| RulesSyncError::BadResponse(format!("{filter_type} 少了 {field} 欄位")))
}

impl SymbolInfoJson {
    fn filter(&self, filter_type: &str) -> Option<&FilterJson> {
        self.filters.iter().find(|f| f.filter_type == filter_type)
    }

    fn require_filter(&self, filter_type: &str) -> Result<&FilterJson, RulesSyncError> {
        self.filter(filter_type).ok_or_else(|| {
            RulesSyncError::BadResponse(format!("{} 的規則裡沒有 {filter_type}", self.symbol))
        })
    }

    /// 按 `filterType` 挑出 1.3 [`SymbolRules`] 要的五個數字。
    ///
    /// 一定要認 `filterType`、不能只看欄位名：`MARKET_LOT_SIZE` 也有
    /// `minQty`/`maxQty`/`stepSize`，但它講的是市價單的限制，而且 BTCUSDT 現貨
    /// 的 `stepSize` 是 `0`（= 這條規則停用）。拿錯會讓所有限價單的數量取整失效。
    fn to_rules_json(&self) -> Result<RulesJson, RulesSyncError> {
        let price = self.require_filter("PRICE_FILTER")?;
        let lot = self.require_filter("LOT_SIZE")?;
        // `NOTIONAL` 是現行的 filter（同時給最低與最高金額）；`MIN_NOTIONAL` 是
        // 它的舊版，只有最低金額。兩者的 `minNotional` 語意相同，部分交易對與
        // 測試網還在給舊的，所以兩個都認、現行的優先。
        let notional = self
            .filter("NOTIONAL")
            .or_else(|| self.filter("MIN_NOTIONAL"))
            .ok_or_else(|| {
                RulesSyncError::BadResponse(format!(
                    "{} 的規則裡沒有 NOTIONAL 也沒有 MIN_NOTIONAL",
                    self.symbol
                ))
            })?;

        Ok(RulesJson {
            symbol: self.symbol.clone(),
            tick_size: required(&price.tick_size, "PRICE_FILTER", "tickSize")?,
            step_size: required(&lot.step_size, "LOT_SIZE", "stepSize")?,
            min_qty: required(&lot.min_qty, "LOT_SIZE", "minQty")?,
            max_qty: required(&lot.max_qty, "LOT_SIZE", "maxQty")?,
            min_notional: required(&notional.min_notional, &notional.filter_type, "minNotional")?,
        })
    }
}

fn parse_num(value: &str, field: &str) -> Result<Fixed, RulesSyncError> {
    value
        .parse::<Fixed>()
        .map_err(|_| RulesSyncError::BadResponse(format!("{field} 不是合法的數字：{value}")))
}

impl RulesJson {
    fn to_symbol_rules(&self) -> Result<SymbolRules, RulesSyncError> {
        SymbolRules::new(
            parse_num(&self.tick_size, "tickSize")?,
            parse_num(&self.step_size, "stepSize")?,
            parse_num(&self.min_qty, "minQty")?,
            parse_num(&self.max_qty, "maxQty")?,
            parse_num(&self.min_notional, "minNotional")?,
        )
        // Binance 把 `tickSize`/`stepSize` 設成 0 代表「這條規則停用」，但 1.3 的
        // SymbolRules 拿它們當除數，沒有「停用」這個狀態。與其偷偷代一個假值進去，
        // 不如直接報錯：正在交易中的現貨交易對不會是 0，真的是 0 就代表拿錯 filter。
        .map_err(|err| RulesSyncError::BadResponse(format!("Binance 給的規則不合理：{err}")))
    }
}

/// 把一份回應（或快取）換成這個交易對的 [`SymbolRules`]。
fn rules_of(symbol: &Symbol, rules: &RulesJson) -> Result<SymbolRules, RulesSyncError> {
    if rules.symbol != symbol.as_str() {
        return Err(RulesSyncError::BadResponse(format!(
            "拿到的是 {} 的下單規則，不是 {symbol} 的",
            rules.symbol
        )));
    }
    rules.to_symbol_rules()
}

/// 從 `exchangeInfo` 的回應裡挑出這個交易對的規則。
///
/// 回應是一個 `symbols` 陣列（帶 `symbol` 參數時只有一個元素，不帶時是全部
/// 交易對），這裡只取需要的那一個，其餘丟掉、不會進快取。
fn parse_exchange_info(body: &str, symbol: &Symbol) -> Result<RulesJson, RulesSyncError> {
    let info: ExchangeInfoJson = serde_json::from_str(body).map_err(|_| {
        RulesSyncError::BadResponse("回應不是預期的 exchangeInfo JSON 結構".to_string())
    })?;
    info.symbols
        .iter()
        .find(|s| s.symbol == symbol.as_str())
        .ok_or_else(|| RulesSyncError::BadResponse(format!("回應裡沒有 {symbol} 的下單規則")))?
        .to_rules_json()
}

/// 快取檔路徑：`<base_dir>/symbol_rules_<SYMBOL>.json`，一個交易對一個檔。
///
/// `base_dir` 由呼叫端決定，慣例和 4.3 的 [`crate::cache_path`] 相同（桌面 App
/// 傳 Tauri 的 `app_data_dir()`，命令列傳 `data`）。`Symbol` 只允許大寫英數字，
/// 組不出跳出 `base_dir` 的路徑。
pub fn cache_path(base_dir: impl AsRef<Path>, symbol: &Symbol) -> PathBuf {
    base_dir
        .as_ref()
        .join(format!("symbol_rules_{symbol}.json"))
}

/// 讀本機快取。檔案不存在、讀不到、內容壞掉、或存的是別的交易對，一律當成
/// 「沒有快取」。
fn read_cache(path: &Path, symbol: &Symbol) -> Option<CachedRules> {
    let content = fs::read_to_string(path).ok()?;
    let cached: CachedRules = serde_json::from_str(&content).ok()?;
    // 先確認真的換得出規則，後面的流程才不用再處理「快取壞掉」這種分支。
    rules_of(symbol, &cached.rules).ok()?;
    Some(cached)
}

/// 寫本機快取。
///
/// ponytail: 寫失敗直接吞掉，理由同 4.3——規則這次已經拿到手、照樣回傳，
/// 寫不進去的代價只是下次要多打一次 API。
fn write_cache(path: &Path, cached: &CachedRules) {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(cached) {
        let _ = fs::write(path, json);
    }
}

// 下面兩個時間工具和 crate 根模組（4.3）的同名函式重複。沒有抽成共用的原因是
// 兩邊的錯誤型別不同（`FeeSyncError` / `RulesSyncError`），為了 8 行共用而讓
// 費率同步反過來依賴這個模組並不划算。

/// 距離上次同步過了多久。系統時鐘被調回過去時回傳 `None`，呼叫端把它當成
/// 「這份快取不可信」，重打一次 API。
fn age_ms(now_ms: i64, synced_at_ms: i64) -> Option<i64> {
    now_ms.checked_sub(synced_at_ms).filter(|age| *age >= 0)
}

fn now_ms() -> Result<i64, RulesSyncError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or(RulesSyncError::SystemClock)
}

/// 同步某個交易對的下單規則，快取存在 `base_dir`（見 [`cache_path`]）。
///
/// 24 小時內用本機快取，過期才打 Binance；API 失敗就沿用本機舊值並標成
/// [`Freshness::Stale`]。只有「快取過期 + API 也失敗 + 本機沒有舊值」才會
/// 回傳 [`RulesSyncError::NoCachedRules`]。
pub fn sync_symbol_rules(
    base_dir: impl AsRef<Path>,
    symbol: &Symbol,
) -> Result<SyncedRules, RulesSyncError> {
    sync_symbol_rules_with(symbol, &cache_path(base_dir, symbol), now_ms()?, || {
        // 帶上 symbol 參數：不帶的話 Binance 會回傳上千個交易對、好幾 MB 的
        // JSON，而這裡只要其中一個。
        at_binance::public_get(EXCHANGE_INFO_PATH, &[("symbol", symbol.as_str())])
    })
}

/// [`sync_symbol_rules`] 的本體，把「現在幾點」和「怎麼拿資料」都變成參數，
/// 讓測試不必依賴真實時間與真實網路。
fn sync_symbol_rules_with(
    symbol: &Symbol,
    cache_path: &Path,
    now_ms: i64,
    fetch: impl FnOnce() -> Result<String, BinanceError>,
) -> Result<SyncedRules, RulesSyncError> {
    let cached = read_cache(cache_path, symbol);

    if let Some(c) = &cached {
        if age_ms(now_ms, c.synced_at_ms).is_some_and(|age| age < CACHE_TTL_MS) {
            return Ok(SyncedRules {
                rules: rules_of(symbol, &c.rules)?,
                synced_at_ms: c.synced_at_ms,
                freshness: Freshness::Fresh,
            });
        }
    }

    match fetch() {
        Ok(body) => {
            let rules_json = parse_exchange_info(&body, symbol)?;
            let rules = rules_of(symbol, &rules_json)?;
            write_cache(
                cache_path,
                &CachedRules {
                    synced_at_ms: now_ms,
                    rules: rules_json,
                },
            );
            Ok(SyncedRules {
                rules,
                synced_at_ms: now_ms,
                freshness: Freshness::Fresh,
            })
        }
        Err(err) => match cached {
            Some(c) => Ok(SyncedRules {
                rules: rules_of(symbol, &c.rules)?,
                synced_at_ms: c.synced_at_ms,
                freshness: Freshness::Stale {
                    age_ms: age_ms(now_ms, c.synced_at_ms).unwrap_or(0),
                },
            }),
            None => Err(RulesSyncError::NoCachedRules(err)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::Side;
    use std::cell::Cell;
    use std::env;

    const HOUR_MS: i64 = 60 * 60 * 1000;
    /// 2026-09-29T00:00:00Z 附近，測試裡當「現在」用。
    const NOW: i64 = 1_790_000_000_000;

    /// 2026-09-29 從 `GET /api/v3/exchangeInfo?symbol=BTCUSDT` 抓回來的真實回應
    /// （只留 `symbols`，其餘 rateLimits 之類的欄位對這一步沒用）。
    const LIVE_BTCUSDT: &str = r#"{
      "timezone":"UTC","serverTime":1790672484497,"exchangeFilters":[],
      "symbols":[{
        "symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT",
        "filters":[
          {"filterType":"PRICE_FILTER","minPrice":"0.01000000","maxPrice":"1000000.00000000","tickSize":"0.01000000"},
          {"filterType":"LOT_SIZE","minQty":"0.00001000","maxQty":"9000.00000000","stepSize":"0.00001000"},
          {"filterType":"ICEBERG_PARTS","limit":100},
          {"filterType":"MARKET_LOT_SIZE","minQty":"0.00000000","maxQty":"104.34142329","stepSize":"0.00000000"},
          {"filterType":"TRAILING_DELTA","minTrailingAboveDelta":10,"maxTrailingAboveDelta":2000},
          {"filterType":"PERCENT_PRICE_BY_SIDE","bidMultiplierUp":"1.2","bidMultiplierDown":"0.5","avgPriceMins":5},
          {"filterType":"NOTIONAL","minNotional":"5.00000000","applyMinToMarket":true,"maxNotional":"9000000.00000000","applyMaxToMarket":false,"avgPriceMins":5},
          {"filterType":"MAX_NUM_ORDERS","maxNumOrders":200}
        ]
      }]
    }"#;

    /// 真實格式、但**每個數字都不一樣**的回應，用來確認五個欄位各自對到正確的
    /// 位置——真實回應裡 `minQty` 和 `stepSize` 剛好都是 `0.00001`，對調了也看
    /// 不出來。`MARKET_LOT_SIZE` 故意排在 `LOT_SIZE` 前面：只看欄位名不看
    /// `filterType` 的寫法會先撿到它。
    const DISTINCT: &str = r#"{
      "symbols":[{
        "symbol":"BTCUSDT",
        "filters":[
          {"filterType":"MARKET_LOT_SIZE","minQty":"0.07000000","maxQty":"104.08000000","stepSize":"0.09000000"},
          {"filterType":"PRICE_FILTER","minPrice":"0.02000000","maxPrice":"1000000.00000000","tickSize":"0.03000000"},
          {"filterType":"LOT_SIZE","minQty":"0.04000000","maxQty":"9000.05000000","stepSize":"0.06000000"},
          {"filterType":"NOTIONAL","minNotional":"5.10000000","maxNotional":"9000000.00000000"}
        ]
      }]
    }"#;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn sym() -> Symbol {
        Symbol::new("BTCUSDT").unwrap()
    }

    /// 每個測試用不同檔名，避免平行跑測試時互相覆寫。
    fn temp_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "at_rules_sync_test_{}_{name}.json",
            std::process::id()
        ))
    }

    /// 一份最小但合法的回應：價格跳動 `tick`、其餘固定。
    fn body(symbol: &str, tick: &str) -> String {
        format!(
            r#"{{"symbols":[{{"symbol":"{symbol}","filters":[
                {{"filterType":"PRICE_FILTER","tickSize":"{tick}"}},
                {{"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"}},
                {{"filterType":"NOTIONAL","minNotional":"5"}}]}}]}}"#
        )
    }

    /// 直接寫一份「上次在 `synced_at_ms` 同步到 `tick` 價格跳動」的快取檔。
    fn seed_cache(path: &Path, synced_at_ms: i64, tick: &str) {
        let rules = format!(
            r#"{{"symbol":"BTCUSDT","tickSize":"{tick}","stepSize":"0.00001",
                 "minQty":"0.00001","maxQty":"9000","minNotional":"5"}}"#
        );
        fs::write(
            path,
            format!(r#"{{"syncedAtMs":{synced_at_ms},"rules":{rules}}}"#),
        )
        .unwrap();
    }

    /// 永遠失敗的 API（模擬沒網路／Binance 維護中）。
    fn api_down() -> Result<String, BinanceError> {
        Err(BinanceError::Http("測試用的假失敗".to_string()))
    }

    fn parse(body: &str) -> Result<SymbolRules, RulesSyncError> {
        parse_exchange_info(body, &sym())?.to_symbol_rules()
    }

    #[test]
    fn every_filter_field_maps_to_the_right_slot() {
        let rules = parse(DISTINCT).unwrap();
        assert_eq!(rules.tick_size, fx("0.03"), "tickSize ← PRICE_FILTER");
        assert_eq!(rules.step_size, fx("0.06"), "stepSize ← LOT_SIZE");
        assert_eq!(rules.min_qty, fx("0.04"), "minQty ← LOT_SIZE");
        assert_eq!(rules.max_qty, fx("9000.05"), "maxQty ← LOT_SIZE");
        assert_eq!(rules.min_notional, fx("5.1"), "minNotional ← NOTIONAL");
    }

    #[test]
    fn market_lot_size_is_never_used_as_lot_size() {
        // DISTINCT 裡 MARKET_LOT_SIZE 排在 LOT_SIZE 前面，值也完全不同。
        let rules = parse(DISTINCT).unwrap();
        for wrong in ["0.07", "104.08", "0.09"] {
            let wrong = fx(wrong);
            assert!(
                rules.step_size != wrong && rules.min_qty != wrong && rules.max_qty != wrong,
                "撿到 MARKET_LOT_SIZE 的 {wrong} 了"
            );
        }
    }

    #[test]
    fn real_btcusdt_response_gives_sane_rules() {
        let rules = parse(LIVE_BTCUSDT).unwrap();
        assert_eq!(rules.tick_size, fx("0.01"));
        assert_eq!(rules.step_size, fx("0.00001"));
        assert_eq!(rules.min_qty, fx("0.00001"));
        assert_eq!(rules.max_qty, fx("9000"));
        assert_eq!(rules.min_notional, fx("5"));
        // 拿真實規則走一次 1.3 的取整 + 檢查，確認它們真的能合作。
        let price = rules.round_price(Side::Buy, fx("63880.129")).unwrap();
        let qty = rules.qty_for_quote(fx("1000"), price).unwrap();
        assert_eq!(rules.check(price, qty), Ok(()));
    }

    #[test]
    fn legacy_min_notional_filter_is_also_accepted() {
        // 舊版 filter：欄位名是 minNotional/applyToMarket，沒有 maxNotional。
        let json = r#"{"symbols":[{"symbol":"BTCUSDT","filters":[
            {"filterType":"PRICE_FILTER","tickSize":"0.01"},
            {"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"},
            {"filterType":"MIN_NOTIONAL","minNotional":"10","applyToMarket":true,"avgPriceMins":5}]}]}"#;
        assert_eq!(parse(json).unwrap().min_notional, fx("10"));
    }

    #[test]
    fn missing_filter_is_rejected_by_name() {
        let json = r#"{"symbols":[{"symbol":"BTCUSDT","filters":[
            {"filterType":"PRICE_FILTER","tickSize":"0.01"},
            {"filterType":"NOTIONAL","minNotional":"5"}]}]}"#;
        let err = parse(json).unwrap_err();
        assert!(err.to_string().contains("LOT_SIZE"), "{err}");
    }

    #[test]
    fn missing_notional_filter_is_rejected() {
        let json = r#"{"symbols":[{"symbol":"BTCUSDT","filters":[
            {"filterType":"PRICE_FILTER","tickSize":"0.01"},
            {"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"}]}]}"#;
        let err = parse(json).unwrap_err();
        assert!(err.to_string().contains("MIN_NOTIONAL"), "{err}");
    }

    #[test]
    fn missing_field_inside_a_filter_is_rejected_with_the_field_name() {
        let json = r#"{"symbols":[{"symbol":"BTCUSDT","filters":[
            {"filterType":"PRICE_FILTER","minPrice":"0.01"},
            {"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"},
            {"filterType":"NOTIONAL","minNotional":"5"}]}]}"#;
        let err = parse(json).unwrap_err();
        assert!(err.to_string().contains("tickSize"), "{err}");
    }

    #[test]
    fn disabled_tick_size_is_rejected_instead_of_silently_used() {
        // Binance 用 0 表示「這條規則停用」；1.3 的 SymbolRules 沒有這個狀態，
        // 拿 0 當除數會壞掉，所以必須明確報錯而不是硬吞。
        let err = parse(&body("BTCUSDT", "0")).unwrap_err();
        assert!(err.to_string().contains("必須大於 0"), "{err}");
    }

    #[test]
    fn non_numeric_value_is_rejected_with_the_field_name() {
        let err = parse(&body("BTCUSDT", "abc")).unwrap_err();
        assert!(err.to_string().contains("tickSize"), "{err}");
    }

    #[test]
    fn response_without_the_wanted_symbol_is_rejected() {
        let path = temp_path("wrong_symbol");
        let err =
            sync_symbol_rules_with(&sym(), &path, NOW, || Ok(body("ETHUSDT", "0.01"))).unwrap_err();
        assert!(matches!(err, RulesSyncError::BadResponse(_)), "{err}");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn only_the_wanted_symbol_is_taken_from_a_multi_symbol_response() {
        let json = format!(
            r#"{{"symbols":[{},{},{}]}}"#,
            symbol_entry("ETHUSDT", "0.99"),
            symbol_entry("BTCUSDT", "0.01"),
            symbol_entry("BNBUSDT", "0.77")
        );
        assert_eq!(parse(&json).unwrap().tick_size, fx("0.01"));
    }

    fn symbol_entry(symbol: &str, tick: &str) -> String {
        format!(
            r#"{{"symbol":"{symbol}","filters":[
                {{"filterType":"PRICE_FILTER","tickSize":"{tick}"}},
                {{"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"}},
                {{"filterType":"NOTIONAL","minNotional":"5"}}]}}"#
        )
    }

    #[test]
    fn fresh_cache_is_used_without_calling_the_api() {
        let path = temp_path("fresh_cache");
        // 差一毫秒就滿 24 小時：還算新鮮。
        seed_cache(&path, NOW - CACHE_TTL_MS + 1, "0.01");

        let called = Cell::new(false);
        let synced = sync_symbol_rules_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.02"))
        })
        .unwrap();

        assert!(!called.get(), "快取還沒過期就不應該打 API");
        assert_eq!(synced.freshness, Freshness::Fresh);
        assert_eq!(synced.rules.tick_size, fx("0.01"), "用的是快取裡的值");
        assert_eq!(synced.synced_at_ms, NOW - CACHE_TTL_MS + 1);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_exactly_24h_old_is_refetched() {
        let path = temp_path("expired_cache");
        seed_cache(&path, NOW - CACHE_TTL_MS, "0.01");

        let called = Cell::new(false);
        let synced = sync_symbol_rules_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.02"))
        })
        .unwrap();

        assert!(called.get(), "滿 24 小時就該重打 API");
        assert_eq!(synced.freshness, Freshness::Fresh);
        assert_eq!(synced.rules.tick_size, fx("0.02"), "用的是 API 的新值");
        assert_eq!(synced.synced_at_ms, NOW);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn clock_moving_backwards_forces_a_refetch() {
        let path = temp_path("clock_backwards");
        // 快取的時間戳比「現在」還晚：時鐘被調過，不能相信這份快取還新鮮。
        seed_cache(&path, NOW + HOUR_MS, "0.01");

        let called = Cell::new(false);
        let synced = sync_symbol_rules_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.02"))
        })
        .unwrap();

        assert!(called.get());
        assert_eq!(synced.rules.tick_size, fx("0.02"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn fetched_rules_round_trip_through_a_real_file() {
        let path = temp_path("round_trip");
        fs::remove_file(&path).ok();

        let first =
            sync_symbol_rules_with(&sym(), &path, NOW, || Ok(LIVE_BTCUSDT.to_string())).unwrap();
        // 第二次同一個時間點呼叫：一定走快取（上面已經確認過快取不會打 API）。
        let second =
            sync_symbol_rules_with(&sym(), &path, NOW, || Ok(body("BTCUSDT", "9"))).unwrap();

        assert_eq!(
            first.rules, second.rules,
            "存進去和讀回來的規則必須一模一樣"
        );
        assert_eq!(first.synced_at_ms, second.synced_at_ms);
        assert_eq!(second.freshness, Freshness::Fresh);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn api_failure_falls_back_to_the_cached_value_and_says_how_old_it_is() {
        let path = temp_path("fallback");
        seed_cache(&path, NOW - 30 * HOUR_MS, "0.01");

        let synced = sync_symbol_rules_with(&sym(), &path, NOW, api_down).unwrap();

        assert_eq!(
            synced.freshness,
            Freshness::Stale {
                age_ms: 30 * HOUR_MS
            },
            "舊資料必須被標成過期，不能假裝是新的"
        );
        assert_eq!(synced.rules.tick_size, fx("0.01"));
        assert_eq!(synced.synced_at_ms, NOW - 30 * HOUR_MS);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn api_failure_without_any_cache_is_an_error() {
        let path = temp_path("no_cache");
        fs::remove_file(&path).ok();

        let err = sync_symbol_rules_with(&sym(), &path, NOW, api_down).unwrap_err();

        assert!(matches!(err, RulesSyncError::NoCachedRules(_)), "{err}");
        assert!(
            err.to_string().contains("沒有可以沿用的舊規則"),
            "訊息要說清楚是「從來沒有資料」而不是普通的連線失敗：{err}"
        );
    }

    #[test]
    fn corrupt_cache_is_treated_as_no_cache() {
        let path = temp_path("corrupt");
        fs::write(&path, "{ 這不是合法的 JSON").unwrap();

        // 壞掉的快取不能被拿來當舊值沿用（可能是手改壞或寫到一半當機）。
        let err = sync_symbol_rules_with(&sym(), &path, NOW, api_down).unwrap_err();
        assert!(matches!(err, RulesSyncError::NoCachedRules(_)), "{err}");

        // 但 API 通的時候要能直接覆蓋掉它，不會卡住。
        let synced =
            sync_symbol_rules_with(&sym(), &path, NOW, || Ok(body("BTCUSDT", "0.02"))).unwrap();
        assert_eq!(synced.rules.tick_size, fx("0.02"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_of_another_symbol_is_not_reused() {
        let path = temp_path("other_symbol_cache");
        fs::write(
            &path,
            format!(
                r#"{{"syncedAtMs":{NOW},"rules":{{"symbol":"ETHUSDT","tickSize":"0.01",
                   "stepSize":"0.0001","minQty":"0.0001","maxQty":"9000","minNotional":"5"}}}}"#
            ),
        )
        .unwrap();

        let err = sync_symbol_rules_with(&sym(), &path, NOW, api_down).unwrap_err();
        assert!(matches!(err, RulesSyncError::NoCachedRules(_)), "{err}");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_path_is_one_file_per_symbol_under_the_given_dir() {
        assert_eq!(
            cache_path("data", &sym()),
            Path::new("data").join("symbol_rules_BTCUSDT.json")
        );
    }

    /// 實際打 Binance 正式環境的公開端點，確認整條「打 API → 挑 filter →
    /// 換成 SymbolRules → 存快取」真的通。不需要 API 金鑰、不讀 Keychain，
    /// 所以不會跳出 macOS 的授權對話框；標成 `#[ignore]` 只因為需要網路。
    ///
    /// 手動驗證：`cargo test -p at-account-sync -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sync_symbol_rules_against_real_binance() {
        let symbol = sym();
        // 每次執行用不同目錄：沿用上次的快取的話，就算網路壞掉也會照樣通過。
        let base_dir = env::temp_dir().join(format!("at_rules_sync_live_{}", std::process::id()));
        let synced = sync_symbol_rules(&base_dir, &symbol).expect("同步下單規則失敗");
        println!(
            "規則 {:?}，新鮮度 {:?}，快取寫到 {}",
            synced.rules,
            synced.freshness,
            cache_path(&base_dir, &symbol).display()
        );
        // BTCUSDT 現貨的價格跳動是 0.01 USDT、最小金額 5 USDT。數字本身會變，
        // 這裡只確認落在合理範圍——對錯欄位的話會差好幾個數量級。
        assert!(
            synced.rules.tick_size > Fixed::ZERO && synced.rules.tick_size <= fx("1"),
            "價格跳動 {} 不合理，可能對錯欄位了",
            synced.rules.tick_size
        );
        assert!(
            synced.rules.min_notional > Fixed::ZERO && synced.rules.min_notional <= fx("100"),
            "最小金額 {} 不合理，可能對錯欄位了",
            synced.rules.min_notional
        );
    }
}
