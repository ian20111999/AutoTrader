//! 帳戶設定同步：把「跟錢有關的帳戶設定」從 Binance 抓回本機、存起來，
//! 抓失敗時沿用上次的值。目前只有現貨手續費率（ROADMAP 4.3）。
//!
//! 三條規則：
//!
//! 1. **24 小時快取**：本機存的值不滿 24 小時就直接用，連 API 都不打
//!    （順便避免每次都去讀 Keychain，macOS 讀憑證可能會跳授權對話框）。
//! 2. **失敗沿用舊值**：API 失敗（沒網路、金鑰失效…）不讓整個流程掛掉，
//!    改用本機舊值，但回傳值會標成 [`Freshness::Stale`] 並附上這份資料多舊。
//!    費率算錯會直接讓回測與實盤的損益失真，所以寧可用舊值也不能靜默假裝新。
//! 3. **沒有舊值就誠實報錯**：第一次用、又剛好同步失敗時沒有東西可以沿用，
//!    回傳 [`FeeSyncError::NoCachedFees`]，讓呼叫端自己決定要中止還是先用
//!    1.4 的 `FeeModel::spot_vip0()` 預設值。

use at_binance::{BinanceClient, BinanceError};
use at_core::{CommissionRates, FeeModel, FeeSchedule, Fixed, SpotFees, Symbol};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本機快取在這段時間內直接沿用，不重打 API。
pub const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// 現貨手續費率端點（唯讀）。
const COMMISSION_PATH: &str = "/api/v3/account/commission";

/// 這份費率資料有多新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// 24 小時內同步成功：這次剛打完 API，或用的是還沒過期的本機快取。
    Fresh,
    /// 這次同步失敗，用的是本機舊值；距離上次「成功」同步已經 `age_ms` 毫秒。
    Stale { age_ms: i64 },
}

/// 同步結果：費率本身 + 這份資料新不新鮮。
///
/// 刻意不直接回傳 [`FeeSchedule`]：呼叫端必須看得到「這是不是舊資料」，
/// 才能決定要不要提醒使用者，而不是拿到一份看起來很正常的過期費率。
#[derive(Debug, Clone)]
pub struct SyncedFees {
    /// 只含這次同步的交易對。`schedule.synced_at` 是上次**成功**同步的時間。
    pub schedule: FeeSchedule,
    pub freshness: Freshness,
}

/// 同步費率失敗的原因。
#[derive(Debug)]
pub enum FeeSyncError {
    /// API 失敗，而且本機沒有可用的舊值（還沒成功同步過，或快取檔壞掉）。
    NoCachedFees(BinanceError),
    /// 回應（或快取檔）的內容不是預期的費率格式。
    BadResponse(String),
    /// 系統時間早於 1970 年，算不出時間戳（實務上不會發生）。
    SystemClock,
}

impl fmt::Display for FeeSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeeSyncError::NoCachedFees(err) => write!(
                f,
                "同步帳戶費率失敗，而且本機沒有可以沿用的舊費率：{err}。\
                 請先確認連線設定，或暫時改用內建的 VIP 0 預設費率"
            ),
            FeeSyncError::BadResponse(what) => {
                write!(f, "看不懂 Binance 回傳的費率內容：{what}")
            }
            FeeSyncError::SystemClock => write!(f, "無法取得系統時間"),
        }
    }
}

impl std::error::Error for FeeSyncError {}

/// `GET /api/v3/account/commission` 的回應結構，同時也是本機快取的存檔格式。
///
/// 費率在 JSON 裡是字串（例如 `"0.00100000"`），這裡照樣先收成 `String`，
/// 再自己 `parse` 成 `Fixed`：`Fixed` 住在零外部依賴的 `at-core`，不能掛
/// serde 的 derive，而且拿字串進來也比較好回報「哪一欄不是合法數字」。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RatesJson {
    maker: String,
    taker: String,
    buyer: String,
    seller: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscountJson {
    enabled_for_account: bool,
    enabled_for_symbol: bool,
    /// 開 BNB 抵扣時標準費率要乘上的倍數（`0.75` = 打 75 折、省 25%），
    /// 語意和 1.4 `SpotFees::bnb_discount` 一致。
    discount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommissionJson {
    symbol: String,
    standard_commission: RatesJson,
    /// 帳戶沒有這組費率時 Binance 可能整個不給這個欄位，當成 0 處理。
    tax_commission: Option<RatesJson>,
    special_commission: Option<RatesJson>,
    discount: Option<DiscountJson>,
}

/// 本機快取檔：費率 + 這份費率是什麼時候拿到的。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedFees {
    /// 上次成功從 Binance 拿到這份費率的時間（UTC 毫秒）。
    synced_at_ms: i64,
    commission: CommissionJson,
}

fn parse_rate(value: &str, group: &str, field: &str) -> Result<Fixed, FeeSyncError> {
    value
        .parse::<Fixed>()
        .map_err(|_| FeeSyncError::BadResponse(format!("{group}.{field} 不是合法的費率數字")))
}

impl RatesJson {
    fn to_rates(&self, group: &str) -> Result<CommissionRates, FeeSyncError> {
        Ok(CommissionRates {
            maker: parse_rate(&self.maker, group, "maker")?,
            taker: parse_rate(&self.taker, group, "taker")?,
            buyer: parse_rate(&self.buyer, group, "buyer")?,
            seller: parse_rate(&self.seller, group, "seller")?,
        })
    }
}

fn rates_or_zero(rates: Option<&RatesJson>, group: &str) -> Result<CommissionRates, FeeSyncError> {
    match rates {
        Some(r) => r.to_rates(group),
        None => Ok(CommissionRates::default()),
    }
}

impl CommissionJson {
    fn to_fee_model(&self) -> Result<FeeModel, FeeSyncError> {
        // 只有帳戶和這個交易對都開了 BNB 抵扣才真的會折；少判一個就會把費率算太低。
        let bnb_discount = match &self.discount {
            Some(d) if d.enabled_for_account && d.enabled_for_symbol => {
                Some(parse_rate(&d.discount, "discount", "discount")?)
            }
            _ => None,
        };
        Ok(FeeModel::Spot(SpotFees {
            standard: self.standard_commission.to_rates("standardCommission")?,
            tax: rates_or_zero(self.tax_commission.as_ref(), "taxCommission")?,
            special: rates_or_zero(self.special_commission.as_ref(), "specialCommission")?,
            bnb_discount,
        }))
    }
}

/// 把一份回應（或快取）換成只含這個交易對的 [`FeeSchedule`]。
fn schedule_of(
    symbol: &Symbol,
    commission: &CommissionJson,
    synced_at_ms: i64,
) -> Result<FeeSchedule, FeeSyncError> {
    if commission.symbol != symbol.as_str() {
        return Err(FeeSyncError::BadResponse(format!(
            "拿到的是 {} 的費率，不是 {symbol} 的",
            commission.symbol
        )));
    }
    let mut schedule = FeeSchedule::new();
    schedule.insert(symbol.clone(), commission.to_fee_model()?);
    schedule.synced_at = Some(synced_at_ms);
    Ok(schedule)
}

/// 快取檔路徑：`<base_dir>/fee_schedule_<SYMBOL>.json`，一個交易對一個檔。
///
/// `base_dir` 由呼叫端決定（跟 1.7 `at_downloader::local_path` 同一個慣例）：
/// 桌面 App 傳 Tauri 的 `app_data_dir()`，命令列工具傳 `data`。這個 crate 不
/// 自己寫死相對路徑——相對路徑會跟著行程的工作目錄跑，同一份快取在不同啟動
/// 方式下會存到不同地方，24 小時快取形同虛設。
///
/// `Symbol` 只允許大寫英數字（見 `at_core::Symbol`），組不出跳出 `base_dir`
/// 的路徑。
pub fn cache_path(base_dir: impl AsRef<Path>, symbol: &Symbol) -> PathBuf {
    base_dir
        .as_ref()
        .join(format!("fee_schedule_{symbol}.json"))
}

/// 讀本機快取。檔案不存在、讀不到、內容壞掉、或存的是別的交易對，一律當成
/// 「沒有快取」——這幾種情況呼叫端能做的事一樣（重打 API，失敗才報錯）。
fn read_cache(path: &Path, symbol: &Symbol) -> Option<CachedFees> {
    let content = fs::read_to_string(path).ok()?;
    let cached: CachedFees = serde_json::from_str(&content).ok()?;
    // 先確認真的換得出費率，後面的流程才不用再處理「快取壞掉」這種分支。
    schedule_of(symbol, &cached.commission, cached.synced_at_ms).ok()?;
    Some(cached)
}

/// 寫本機快取。
///
/// ponytail: 寫失敗直接吞掉。這次的費率已經拿到手、照樣回傳，寫不進去的代價
/// 只是下次呼叫要多打一次 API；為了磁碟寫入失敗就讓整個同步失敗更糟。真的需要
/// 在畫面上提示「快取寫不進去」時再改成回傳錯誤。
fn write_cache(path: &Path, cached: &CachedFees) {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(cached) {
        let _ = fs::write(path, json);
    }
}

/// 距離上次同步過了多久。系統時鐘被調回過去（`now_ms` 早於同步時間）時回傳
/// `None`，呼叫端把它當成「這份快取不可信」，重打一次 API。
fn age_ms(now_ms: i64, synced_at_ms: i64) -> Option<i64> {
    now_ms.checked_sub(synced_at_ms).filter(|age| *age >= 0)
}

fn now_ms() -> Result<i64, FeeSyncError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or(FeeSyncError::SystemClock)
}

/// 同步某個交易對的現貨手續費率，快取存在 `base_dir`（見 [`cache_path`]）。
///
/// 24 小時內用本機快取，過期才打 Binance；API 失敗就沿用本機舊值並標成
/// [`Freshness::Stale`]。只有「快取過期 + API 也失敗 + 本機沒有舊值」才會
/// 回傳 [`FeeSyncError::NoCachedFees`]。
pub fn sync_spot_fees(
    base_dir: impl AsRef<Path>,
    symbol: &Symbol,
) -> Result<SyncedFees, FeeSyncError> {
    sync_spot_fees_with(symbol, &cache_path(base_dir, symbol), now_ms()?, || {
        // 放在 closure 裡：快取還新的時候連 Keychain 都不會去讀。
        let client = BinanceClient::from_keychain()?;
        client.signed_get(COMMISSION_PATH, &[("symbol", symbol.as_str())])
    })
}

/// [`sync_spot_fees`] 的本體，把「現在幾點」和「怎麼打 API」都變成參數，
/// 讓測試不必依賴真實時間與真實網路。
fn sync_spot_fees_with(
    symbol: &Symbol,
    cache_path: &Path,
    now_ms: i64,
    fetch: impl FnOnce() -> Result<String, BinanceError>,
) -> Result<SyncedFees, FeeSyncError> {
    let cached = read_cache(cache_path, symbol);

    if let Some(c) = &cached {
        if age_ms(now_ms, c.synced_at_ms).is_some_and(|age| age < CACHE_TTL_MS) {
            return Ok(SyncedFees {
                schedule: schedule_of(symbol, &c.commission, c.synced_at_ms)?,
                freshness: Freshness::Fresh,
            });
        }
    }

    match fetch() {
        Ok(body) => {
            let commission: CommissionJson = serde_json::from_str(&body).map_err(|_| {
                FeeSyncError::BadResponse("回應不是預期的手續費 JSON 結構".to_string())
            })?;
            let schedule = schedule_of(symbol, &commission, now_ms)?;
            write_cache(
                cache_path,
                &CachedFees {
                    synced_at_ms: now_ms,
                    commission,
                },
            );
            Ok(SyncedFees {
                schedule,
                freshness: Freshness::Fresh,
            })
        }
        Err(err) => match cached {
            Some(c) => Ok(SyncedFees {
                schedule: schedule_of(symbol, &c.commission, c.synced_at_ms)?,
                freshness: Freshness::Stale {
                    age_ms: age_ms(now_ms, c.synced_at_ms).unwrap_or(0),
                },
            }),
            None => Err(FeeSyncError::NoCachedFees(err)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::{Liquidity, Side};
    use std::cell::Cell;
    use std::env;

    const HOUR_MS: i64 = 60 * 60 * 1000;
    /// 2026-09-29T00:00:00Z 附近，測試裡當「現在」用。
    const NOW: i64 = 1_790_000_000_000;

    /// Binance 官方文件給的回應範例，欄位齊全、四組數字互不相同，
    /// 用來確認每一欄都對到 1.4 `SpotFees` 的正確位置（對錯了不會互相掩蓋）。
    const OFFICIAL_EXAMPLE: &str = r#"{
      "symbol": "BTCUSDT",
      "standardCommission": {"maker":"0.00000010","taker":"0.00000020","buyer":"0.00000030","seller":"0.00000040"},
      "specialCommission": {"maker":"0.01000000","taker":"0.02000000","buyer":"0.03000000","seller":"0.04000000"},
      "taxCommission": {"maker":"0.00000112","taker":"0.00000114","buyer":"0.00000118","seller":"0.00000116"},
      "discount": {"enabledForAccount":true,"enabledForSymbol":true,"discountAsset":"BNB","discount":"0.75000000"}
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
            "at_account_sync_test_{}_{name}.json",
            std::process::id()
        ))
    }

    /// 掛單、吃單都是 `rate`，其餘為 0、沒開 BNB 抵扣的回應。
    fn body(symbol: &str, rate: &str) -> String {
        format!(
            r#"{{"symbol":"{symbol}",
                "standardCommission":{{"maker":"{rate}","taker":"{rate}","buyer":"0","seller":"0"}},
                "taxCommission":{{"maker":"0","taker":"0","buyer":"0","seller":"0"}},
                "specialCommission":{{"maker":"0","taker":"0","buyer":"0","seller":"0"}},
                "discount":{{"enabledForAccount":false,"enabledForSymbol":false,"discountAsset":"BNB","discount":"0.75"}}}}"#
        )
    }

    /// 直接寫一份「上次在 `synced_at_ms` 同步到 `rate`」的快取檔。
    fn seed_cache(path: &Path, synced_at_ms: i64, rate: &str) {
        let commission = body("BTCUSDT", rate);
        fs::write(
            path,
            format!(r#"{{"syncedAtMs":{synced_at_ms},"commission":{commission}}}"#),
        )
        .unwrap();
    }

    /// 這份結果裡吃單（買方）的實際費率。
    fn taker_rate(synced: &SyncedFees) -> Fixed {
        synced
            .schedule
            .get(&sym())
            .unwrap()
            .rate(Liquidity::Taker, Side::Buy)
            .unwrap()
    }

    /// 永遠失敗的 API（模擬沒網路／金鑰失效）。
    fn api_down() -> Result<String, BinanceError> {
        Err(BinanceError::Http("測試用的假失敗".to_string()))
    }

    #[test]
    fn official_example_maps_every_field_to_the_right_slot() {
        let commission: CommissionJson = serde_json::from_str(OFFICIAL_EXAMPLE).unwrap();
        let model = commission.to_fee_model().unwrap();
        let FeeModel::Spot(fees) = model else {
            panic!("現貨端點必須產生 FeeModel::Spot");
        };
        assert_eq!(
            fees.standard,
            CommissionRates {
                maker: fx("0.0000001"),
                taker: fx("0.0000002"),
                buyer: fx("0.0000003"),
                seller: fx("0.0000004"),
            }
        );
        assert_eq!(
            fees.tax,
            CommissionRates {
                maker: fx("0.00000112"),
                taker: fx("0.00000114"),
                buyer: fx("0.00000118"),
                seller: fx("0.00000116"),
            }
        );
        assert_eq!(
            fees.special,
            CommissionRates {
                maker: fx("0.01"),
                taker: fx("0.02"),
                buyer: fx("0.03"),
                seller: fx("0.04"),
            }
        );
        assert_eq!(fees.bnb_discount, Some(fx("0.75")));
    }

    #[test]
    fn bnb_discount_needs_both_switches_on() {
        for (account, symbol_on) in [(true, false), (false, true), (false, false)] {
            let json = format!(
                r#"{{"symbol":"BTCUSDT",
                     "standardCommission":{{"maker":"0.001","taker":"0.001","buyer":"0","seller":"0"}},
                     "discount":{{"enabledForAccount":{account},"enabledForSymbol":{symbol_on},"discountAsset":"BNB","discount":"0.75"}}}}"#
            );
            let commission: CommissionJson = serde_json::from_str(&json).unwrap();
            let FeeModel::Spot(fees) = commission.to_fee_model().unwrap() else {
                panic!("現貨端點必須產生 FeeModel::Spot");
            };
            assert_eq!(
                fees.bnb_discount, None,
                "帳戶或交易對其中一邊沒開，就不該打折（account={account}, symbol={symbol_on}）"
            );
        }
    }

    #[test]
    fn missing_optional_groups_count_as_zero() {
        // 只有 standardCommission 的回應也要能用：缺的那幾組是「不收」，不是壞資料。
        let json = r#"{"symbol":"BTCUSDT","standardCommission":{"maker":"0.001","taker":"0.001","buyer":"0","seller":"0"}}"#;
        let commission: CommissionJson = serde_json::from_str(json).unwrap();
        let model = commission.to_fee_model().unwrap();
        assert_eq!(model.rate(Liquidity::Taker, Side::Buy), Some(fx("0.001")));
    }

    #[test]
    fn non_numeric_rate_is_rejected_with_the_field_name() {
        let json = r#"{"symbol":"BTCUSDT","standardCommission":{"maker":"abc","taker":"0.001","buyer":"0","seller":"0"}}"#;
        let commission: CommissionJson = serde_json::from_str(json).unwrap();
        let err = commission.to_fee_model().unwrap_err();
        assert!(
            err.to_string().contains("standardCommission.maker"),
            "錯誤訊息要指出是哪一欄：{err}"
        );
    }

    #[test]
    fn response_for_another_symbol_is_rejected() {
        let path = temp_path("wrong_symbol");
        let err =
            sync_spot_fees_with(&sym(), &path, NOW, || Ok(body("ETHUSDT", "0.001"))).unwrap_err();
        assert!(matches!(err, FeeSyncError::BadResponse(_)), "{err}");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn fresh_cache_is_used_without_calling_the_api() {
        let path = temp_path("fresh_cache");
        // 差一毫秒就滿 24 小時：還算新鮮。
        seed_cache(&path, NOW - CACHE_TTL_MS + 1, "0.001");

        let called = Cell::new(false);
        let synced = sync_spot_fees_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.002"))
        })
        .unwrap();

        assert!(!called.get(), "快取還沒過期就不應該打 API");
        assert_eq!(synced.freshness, Freshness::Fresh);
        assert_eq!(taker_rate(&synced), fx("0.001"), "用的是快取裡的值");
        assert_eq!(synced.schedule.synced_at, Some(NOW - CACHE_TTL_MS + 1));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_exactly_24h_old_is_refetched() {
        let path = temp_path("expired_cache");
        seed_cache(&path, NOW - CACHE_TTL_MS, "0.001");

        let called = Cell::new(false);
        let synced = sync_spot_fees_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.002"))
        })
        .unwrap();

        assert!(called.get(), "滿 24 小時就該重打 API");
        assert_eq!(synced.freshness, Freshness::Fresh);
        assert_eq!(taker_rate(&synced), fx("0.002"), "用的是 API 的新值");
        assert_eq!(synced.schedule.synced_at, Some(NOW));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn clock_moving_backwards_forces_a_refetch() {
        let path = temp_path("clock_backwards");
        // 快取的時間戳比「現在」還晚：時鐘被調過，不能相信這份快取還新鮮。
        seed_cache(&path, NOW + HOUR_MS, "0.001");

        let called = Cell::new(false);
        let synced = sync_spot_fees_with(&sym(), &path, NOW, || {
            called.set(true);
            Ok(body("BTCUSDT", "0.002"))
        })
        .unwrap();

        assert!(called.get());
        assert_eq!(taker_rate(&synced), fx("0.002"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn fetched_fees_round_trip_through_a_real_file() {
        let path = temp_path("round_trip");
        fs::remove_file(&path).ok();

        let first =
            sync_spot_fees_with(&sym(), &path, NOW, || Ok(OFFICIAL_EXAMPLE.to_string())).unwrap();
        // 第二次同一個時間點呼叫：一定走快取（上面已經確認過快取不會打 API）。
        let second = sync_spot_fees_with(&sym(), &path, NOW, || Ok(body("BTCUSDT", "9"))).unwrap();

        assert_eq!(
            first.schedule.get(&sym()),
            second.schedule.get(&sym()),
            "存進去和讀回來的費率必須一模一樣"
        );
        assert_eq!(first.schedule.synced_at, second.schedule.synced_at);
        assert_eq!(second.freshness, Freshness::Fresh);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn api_failure_falls_back_to_the_cached_value_and_says_how_old_it_is() {
        let path = temp_path("fallback");
        seed_cache(&path, NOW - 30 * HOUR_MS, "0.001");

        let synced = sync_spot_fees_with(&sym(), &path, NOW, api_down).unwrap();

        assert_eq!(
            synced.freshness,
            Freshness::Stale {
                age_ms: 30 * HOUR_MS
            },
            "舊資料必須被標成過期，不能假裝是新的"
        );
        assert_eq!(taker_rate(&synced), fx("0.001"));
        assert_eq!(synced.schedule.synced_at, Some(NOW - 30 * HOUR_MS));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn api_failure_without_any_cache_is_an_error() {
        let path = temp_path("no_cache");
        fs::remove_file(&path).ok();

        let err = sync_spot_fees_with(&sym(), &path, NOW, api_down).unwrap_err();

        assert!(matches!(err, FeeSyncError::NoCachedFees(_)), "{err}");
        assert!(
            err.to_string().contains("沒有可以沿用的舊費率"),
            "訊息要說清楚是「從來沒有資料」而不是普通的連線失敗：{err}"
        );
    }

    #[test]
    fn corrupt_cache_is_treated_as_no_cache() {
        let path = temp_path("corrupt");
        fs::write(&path, "{ 這不是合法的 JSON").unwrap();

        // 壞掉的快取不能被拿來當舊值沿用（可能是手改壞或寫到一半當機）。
        let err = sync_spot_fees_with(&sym(), &path, NOW, api_down).unwrap_err();
        assert!(matches!(err, FeeSyncError::NoCachedFees(_)), "{err}");

        // 但 API 通的時候要能直接覆蓋掉它，不會卡住。
        let synced =
            sync_spot_fees_with(&sym(), &path, NOW, || Ok(body("BTCUSDT", "0.002"))).unwrap();
        assert_eq!(taker_rate(&synced), fx("0.002"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_of_another_symbol_is_not_reused() {
        let path = temp_path("other_symbol_cache");
        fs::write(
            &path,
            format!(
                r#"{{"syncedAtMs":{NOW},"commission":{}}}"#,
                body("ETHUSDT", "0.001")
            ),
        )
        .unwrap();

        let err = sync_spot_fees_with(&sym(), &path, NOW, api_down).unwrap_err();
        assert!(matches!(err, FeeSyncError::NoCachedFees(_)), "{err}");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cache_path_is_one_file_per_symbol_under_the_given_dir() {
        assert_eq!(
            cache_path("data", &sym()),
            Path::new("data").join("fee_schedule_BTCUSDT.json")
        );
    }

    /// 實際打 Binance 正式環境，用 4.1/4.6 存在 Keychain 裡的真實唯讀金鑰，
    /// 確認整條「讀憑證 → 簽名 → 打 API → 解析 → 存快取」真的通。
    /// 不在 `cargo test` 預設跑（需要網路、可能跳 Keychain 授權對話框）。
    ///
    /// 手動驗證：`cargo test -p at-account-sync -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sync_spot_fees_against_real_binance() {
        let symbol = sym();
        // 寫到暫存目錄，不要在 repo 裡留下檔案。每次執行用不同目錄（帶 process
        // id）：這個測試的重點是「現在連線還通不通」，如果沿用上次的快取，就算
        // 網路壞掉、金鑰失效也會照樣通過，變成假的安心。
        let base_dir = env::temp_dir().join(format!("at_account_sync_live_{}", std::process::id()));
        let synced = sync_spot_fees(&base_dir, &symbol).expect("同步帳戶費率失敗");
        let rate = synced
            .schedule
            .get(&symbol)
            .and_then(|m| m.rate(Liquidity::Taker, Side::Buy))
            .expect("拿不到吃單費率");
        println!(
            "吃單費率 {rate}，新鮮度 {:?}，快取寫到 {}",
            synced.freshness,
            cache_path(&base_dir, &symbol).display()
        );
        assert!(
            rate >= Fixed::ZERO && rate <= fx("0.01"),
            "現貨吃單費率應該在 0%~1% 之間，拿到 {rate} 代表對錯欄位了"
        );
    }
}
