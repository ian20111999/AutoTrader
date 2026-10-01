//! Binance REST API client：HMAC-SHA256 簽名機制 + 打 GET 請求。
//!
//! 兩種端點分開兩條路：
//!
//! - [`BinanceClient::signed_get`]：需要 API 金鑰與簽名的端點（例如帳戶費率）。
//! - [`public_get`]：不需要金鑰的公開端點（例如 `/api/v3/exchangeInfo`）。
//!
//! 只提供「送出 GET」這個骨架，不解析各端點的回應結構——那是後續串接步驟
//! （4.3 帳戶費率、4.4 下單規則）各自的工作，這裡只回傳原始 JSON 字串。
//!
//! [`testnet`] 子模組是 ROADMAP 6.2 的測試網下單（獨立型別，見該模組文件
//! 說明為什麼不跟這裡的 [`BinanceClient`] 共用 `base_url`）。
//!
//! # 安全設計
//!
//! - [`BinanceError`] 的所有變體都只包含固定的中文說明或 HTTP 狀態碼，絕對
//!   不包含 API Secret、算出來的簽名、或送出去的完整 URL（那串 URL 裡含有
//!   簽名過的 query string）。
//! - [`sign`] 只回傳簽名結果本身；呼叫端要自行負責不要把回傳值印出來。

pub mod market_data;
pub mod testnet;

use at_secrets::{CredentialStore, KeychainStore, SecretError, SecretValue};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const SERVICE_API_KEY: &str = "com.autotrader.app.binance-readonly-api-key";
const SERVICE_API_SECRET: &str = "com.autotrader.app.binance-readonly-api-secret";
const ACCOUNT: &str = "autotrader";
const DEFAULT_BASE_URL: &str = "https://api.binance.com";
/// Binance 官方文件的預設值；請求必須在伺服器收到時，`timestamp` 與伺服器
/// 時間的差距在這個範圍內，否則會被拒絕。
const DEFAULT_RECV_WINDOW_MS: u64 = 5000;

/// 呼叫 Binance API 各步驟失敗的原因。
#[derive(Debug)]
pub enum BinanceError {
    /// 讀取 Keychain 裡的憑證失敗。
    Credential(SecretError),
    /// Keychain 裡沒有存 API Key / Secret（還沒在 4.6 的連線設定畫面存過）。
    MissingCredential,
    /// 系統時間早於 1970 年，算不出時間戳（實務上不會發生）。
    SystemClock,
    /// HTTP 請求失敗（連線錯誤、逾時、DNS 失敗、4xx/5xx 狀態碼等）。訊息只
    /// 包含一般性說明，不包含送出去的 URL。
    Http(String),
    /// 讀取回應內容失敗。
    Response(String),
}

impl fmt::Display for BinanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // 刻意不把內層 SecretError 的內容原樣接進來：雖然 at-secrets 目前唯一
            // 的建構處只會塞固定訊息，但這裡不應該依賴「上游永遠安全」這個假設，
            // 固定訊息本身才是防線，不是信任鏈。
            BinanceError::Credential(_) => {
                write!(
                    f,
                    "讀取 API 憑證失敗，請確認系統安全儲存區存取權限，或回連線設定畫面重新輸入金鑰"
                )
            }
            BinanceError::MissingCredential => {
                write!(f, "尚未設定 Binance API 金鑰，請先在連線設定畫面輸入")
            }
            BinanceError::SystemClock => write!(f, "無法取得系統時間"),
            BinanceError::Http(msg) => write!(f, "呼叫 Binance API 失敗：{msg}"),
            BinanceError::Response(msg) => write!(f, "讀取 Binance API 回應失敗：{msg}"),
        }
    }
}

impl std::error::Error for BinanceError {}

/// 把 bytes 轉成小寫 hex 字串。
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 對一組 query string 用 API Secret 做 HMAC-SHA256 簽名，回傳小寫 hex 字串。
///
/// `query` 必須是最終要送出去的查詢字串本身（不含 `signature` 參數）；
/// Binance 用同一個 secret 對同一個字串重算一次來驗證簽名，所以這裡簽的
/// 內容必須跟實際送出去的一字不差。
pub fn sign(secret: &str, query: &str) -> String {
    // HMAC 的金鑰長度沒有限制（比 block size 長會先雜湊），這裡的 `expect`
    // 不會真的失敗。
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC 可以接受任意長度的金鑰");
    mac.update(query.as_bytes());
    to_hex(&mac.finalize().into_bytes())
}

/// 目前時間的毫秒數時間戳，Binance 的 SIGNED 端點必填。
fn timestamp_ms() -> Result<u64, BinanceError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|_| BinanceError::SystemClock)
}

/// 把業務參數接成 `a=1&b=2`；沒有參數時回傳空字串。
///
/// ponytail: 參數值假設已經是 URL-safe（Binance 的參數目前都是幣種代碼、
/// 數字、BUY/SELL 這類英數字），沒有做通用的百分比編碼；真的需要傳非
/// ASCII 值時再補。
fn join_params(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// 組出「業務參數 + recvWindow + timestamp」的查詢字串、簽名，回傳附上
/// `signature` 的完整查詢字串。
fn build_signed_query(secret: &str, params: &[(&str, &str)], timestamp_ms: u64) -> String {
    let mut query = join_params(params);
    if !query.is_empty() {
        query.push('&');
    }
    query.push_str(&format!(
        "recvWindow={DEFAULT_RECV_WINDOW_MS}&timestamp={timestamp_ms}"
    ));

    let signature = sign(secret, &query);
    format!("{query}&signature={signature}")
}

/// 送出 GET 並讀回內文。`api_key` 傳 `None` 代表公開端點：不帶
/// `X-MBX-APIKEY`，錯誤訊息也不會誤導使用者去檢查根本沒用到的金鑰。
fn get_text(url: &str, api_key: Option<&str>) -> Result<String, BinanceError> {
    let mut request = ureq::get(url);
    if let Some(key) = api_key {
        request = request.header("X-MBX-APIKEY", key);
    }

    match request.call() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| BinanceError::Response(e.to_string())),
        Err(ureq::Error::StatusCode(code)) => Err(BinanceError::Http(if api_key.is_some() {
            format!("HTTP 狀態碼 {code}，請確認金鑰與權限是否正確")
        } else {
            format!("HTTP 狀態碼 {code}")
        })),
        Err(e) => Err(BinanceError::Http(e.to_string())),
    }
}

/// 打一個**公開**端點（Binance 文件標示安全等級 `NONE` 的端點，例如
/// `/api/v3/exchangeInfo`），回傳原始 JSON 回應字串。
///
/// 刻意寫成自由函式而不是 [`BinanceClient`] 的方法：公開端點不需要金鑰，
/// 那就不該讓它有機會碰到金鑰。呼叫端連 Keychain 都不用讀，也就不會為了查
/// 一份公開資料而跳出 macOS 的授權對話框。
pub fn public_get(path: &str, params: &[(&str, &str)]) -> Result<String, BinanceError> {
    let query = join_params(params);
    let url = if query.is_empty() {
        format!("{DEFAULT_BASE_URL}{path}")
    } else {
        format!("{DEFAULT_BASE_URL}{path}?{query}")
    };
    get_text(&url, None)
}

/// Binance REST client。只提供打「已簽名 GET」的骨架，不解析特定端點的回應。
pub struct BinanceClient {
    api_key: SecretValue,
    api_secret: SecretValue,
    base_url: String,
}

impl BinanceClient {
    pub fn new(api_key: SecretValue, api_secret: SecretValue) -> Self {
        Self {
            api_key,
            api_secret,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// 從 OS 安全儲存區讀取金鑰建立 client（4.1 的 [`KeychainStore`]，
    /// 4.6 連線設定畫面存進去的同一組 service/account）。
    pub fn from_keychain() -> Result<Self, BinanceError> {
        let store = KeychainStore::new();
        let api_key = store
            .get(SERVICE_API_KEY, ACCOUNT)
            .map_err(BinanceError::Credential)?
            .ok_or(BinanceError::MissingCredential)?;
        let api_secret = store
            .get(SERVICE_API_SECRET, ACCOUNT)
            .map_err(BinanceError::Credential)?
            .ok_or(BinanceError::MissingCredential)?;
        Ok(Self::new(api_key, api_secret))
    }

    /// 打一個 SIGNED GET 端點，自動附加 `recvWindow`/`timestamp`/`signature`，
    /// 回傳原始 JSON 回應字串。`params` 是端點本身的業務參數（不含這三個，
    /// 這個函式會自動附加）。
    pub fn signed_get(&self, path: &str, params: &[(&str, &str)]) -> Result<String, BinanceError> {
        let timestamp = timestamp_ms()?;
        let query = build_signed_query(self.api_secret.expose(), params, timestamp);
        let url = format!("{}{}?{}", self.base_url, path, query);

        get_text(&url, Some(self.api_key.expose()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binance 官方文件給的簽名範例（HMAC 密鑰），用來當作已知答案：
    /// https://developers.binance.com/docs/binance-spot-api-docs/rest-api/general-api-information
    /// 已經用 Python `hmac.new(secret, query, hashlib.sha256).hexdigest()`
    /// 離線覆算過，結果一致。
    const OFFICIAL_SECRET: &str =
        "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j";
    const OFFICIAL_QUERY: &str = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";

    #[test]
    fn sign_matches_binance_official_example() {
        let expected = "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71";
        assert_eq!(expected.len(), 64, "官方範例的簽名長度就是 64 個 hex 字元");
        assert_eq!(sign(OFFICIAL_SECRET, OFFICIAL_QUERY), expected);
    }

    #[test]
    fn build_signed_query_reproduces_the_official_example_end_to_end() {
        let params = [
            ("symbol", "LTCBTC"),
            ("side", "BUY"),
            ("type", "LIMIT"),
            ("timeInForce", "GTC"),
            ("quantity", "1"),
            ("price", "0.1"),
        ];
        let query = build_signed_query(OFFICIAL_SECRET, &params, 1_499_827_319_559);
        let expected = format!(
            "{OFFICIAL_QUERY}&signature=c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
        assert_eq!(query, expected);
    }

    #[test]
    fn sign_is_deterministic() {
        assert_eq!(
            sign("secret", "a=1&b=2"),
            sign("secret", "a=1&b=2"),
            "同樣的輸入必須算出同樣的簽名"
        );
    }

    #[test]
    fn different_query_changes_the_signature() {
        assert_ne!(sign("secret", "a=1"), sign("secret", "a=2"));
    }

    #[test]
    fn different_secret_changes_the_signature() {
        assert_ne!(sign("secret-a", "a=1"), sign("secret-b", "a=1"));
    }

    #[test]
    fn join_params_builds_a_plain_query_string() {
        assert_eq!(
            join_params(&[("symbol", "BTCUSDT"), ("limit", "1")]),
            "symbol=BTCUSDT&limit=1"
        );
        assert_eq!(join_params(&[]), "", "沒有參數就不該生出一個空的 `?`");
    }

    #[test]
    fn build_signed_query_with_no_extra_params_still_has_recv_window_and_timestamp() {
        let query = build_signed_query("secret", &[], 123);
        assert!(query.starts_with("recvWindow=5000&timestamp=123&signature="));
    }

    #[test]
    fn error_display_never_contains_the_secret() {
        let secret_marker = "totally-secret-value-should-not-leak";
        let err = BinanceError::Credential(SecretError::Store(secret_marker.to_string()));
        // Store 變體本身在 at-secrets 裡永遠是固定訊息，但這裡不依賴那個假設：
        // 即使塞進一個模擬洩漏內容的字串，BinanceError 的 Display 也絕對不能
        // 把它原樣接出去。這是真的檢查 marker 本身，不是檢查無關的欄位名稱。
        assert!(!err.to_string().contains(secret_marker));
    }

    /// 實際打 Binance 正式環境，用 4.1/4.6 存在 Keychain 裡的真實唯讀金鑰驗證
    /// 整條簽名 + HTTP 的路徑是通的。不在 `cargo test` 預設跑：
    /// - 需要真實網路連線
    /// - headless 環境第一次讀取既有 Keychain 憑證會卡在系統的互動授權對話框
    ///
    /// 手動驗證：`cargo test -p at-binance -- --ignored`
    #[test]
    #[ignore]
    fn signed_get_account_commission_against_real_binance() {
        let client = BinanceClient::from_keychain()
            .expect("讀取 Keychain 憑證失敗，請先確認 4.6 連線設定畫面已經存過金鑰");
        let body = client
            .signed_get("/api/v3/account/commission", &[("symbol", "BTCUSDT")])
            .expect("呼叫 Binance API 失敗");
        assert!(
            body.contains("standardCommission"),
            "回應內容看起來不像手續費資料：{body}"
        );
    }

    /// 實際打公開端點 `/api/v3/exchangeInfo`。和上面那個不同：完全不讀
    /// Keychain、不需要金鑰，所以不會跳出授權對話框。仍然標成 `#[ignore]`，
    /// 因為需要真實網路連線。
    ///
    /// 手動驗證：`cargo test -p at-binance -- --ignored`
    #[test]
    #[ignore]
    fn public_get_exchange_info_against_real_binance() {
        let body =
            public_get("/api/v3/exchangeInfo", &[("symbol", "BTCUSDT")]).expect("呼叫公開端點失敗");
        assert!(
            body.contains("PRICE_FILTER"),
            "回應內容看起來不像交易規則：{body}"
        );
    }
}
