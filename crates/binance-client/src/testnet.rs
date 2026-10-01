//! 測試網（`testnet.binance.vision`）下單：市價單下單＋查詢訂單狀態＋撤單。
//!
//! ROADMAP 6.1（Keychain 憑證獨立存放）＋ 6.2（下單 client）。這是整個專案
//! 第一次出現「送出訂單」的程式碼路徑，所以刻意不共用 [`crate::BinanceClient`]
//! 的 `base_url` 欄位——那個欄位是可變的 `String`，日後只要有人改錯一個預設值
//! 或多開一個建構子，就有可能讓下單程式碼连到正式環境。這裡改用獨立的
//! [`BinanceTestnetClient`]，網址寫死在 [`TESTNET_BASE_URL`] 常數、不經過任何
//! 欄位或參數，型別上就不可能指向別的地方。
//!
//! Keychain 服務名稱也跟 4.1/4.6 正式環境的唯讀金鑰分開（[`TESTNET_SERVICE_API_KEY`]
//! / [`TESTNET_SERVICE_API_SECRET`]），避免兩組憑證互相覆蓋或讀錯。
//!
//! # 簽名機制
//!
//! 沿用 [`crate::sign`] / [`crate::build_signed_query`]：已經用 Binance 官方
//! 簽名範例驗證過。官方文件（`binance-spot-api-docs/rest-api.md`「SIGNED
//! (TRADE and USER_DATA) Endpoint Security」一節）說明 SIGNED 端點不論
//! GET/POST/DELETE，簽名payload都是「query string 接 body」；本模組的
//! POST/DELETE 把所有參數都放進 query string、body 留空，所以簽名方式跟既有
//! GET 完全一樣，不需要另外的簽名邏輯。
//!
//! # 回應解析
//!
//! 下單／查詢／撤單三個端點回應的共同欄位（`symbol`/`orderId`/`clientOrderId`/
//! `status`/`origQty`/`executedQty`/`cummulativeQuoteQty`）已經用官方文件
//! （`rest-api.md` 的 New order RESULT／FULL 範例、Query order／Cancel order
//! 範例）核對過。刻意不解析 `FULL` 回應才有的 `fills` 明細陣列——那是 6.4
//! 把成交回報寫進帳本時才需要的東西，現在沒有呼叫端會用到。

use crate::{build_signed_query, get_text, timestamp_ms, BinanceError};
use at_core::Fixed;
use at_secrets::{CredentialStore, KeychainStore, SecretValue};
use serde::Deserialize;
use std::fmt;

/// 測試網的網址，寫死在這裡、不開放呼叫端指定。
const TESTNET_BASE_URL: &str = "https://testnet.binance.vision";
const TESTNET_SERVICE_API_KEY: &str = "com.autotrader.app.binance-testnet-api-key";
const TESTNET_SERVICE_API_SECRET: &str = "com.autotrader.app.binance-testnet-api-secret";
const TESTNET_ACCOUNT: &str = "autotrader";
const ORDER_PATH: &str = "/api/v3/order";

/// 下單方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    fn as_str(self) -> &'static str {
        match self {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }
}

/// 訂單狀態。涵蓋 Binance 官方文件（`binance-spot-api-docs/enums.md`
/// 「Order status (status)」一節）列出的全部九種狀態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// 已被引擎接受。
    New,
    /// 訂單清單裡的從屬訂單，等主訂單完全成交前的中間狀態。
    PendingNew,
    /// 部分成交。
    PartiallyFilled,
    /// 完全成交。
    Filled,
    /// 使用者取消。
    Canceled,
    /// 官方文件標示「目前沒有用到」，保留只是為了涵蓋文件列出的全部狀態。
    PendingCancel,
    /// 引擎拒絕，完全沒有進入撮合。
    Rejected,
    /// 依訂單類型規則或交易所動作被取消（例如 FOK 沒成交、維護中強制取消）。
    Expired,
    /// 因自我成交防範（STP）被交易所取消。
    ExpiredInMatch,
}

impl OrderStatus {
    fn parse(raw: &str) -> Result<Self, BinanceError> {
        match raw {
            "NEW" => Ok(OrderStatus::New),
            "PENDING_NEW" => Ok(OrderStatus::PendingNew),
            "PARTIALLY_FILLED" => Ok(OrderStatus::PartiallyFilled),
            "FILLED" => Ok(OrderStatus::Filled),
            "CANCELED" => Ok(OrderStatus::Canceled),
            "PENDING_CANCEL" => Ok(OrderStatus::PendingCancel),
            "REJECTED" => Ok(OrderStatus::Rejected),
            "EXPIRED" => Ok(OrderStatus::Expired),
            "EXPIRED_IN_MATCH" => Ok(OrderStatus::ExpiredInMatch),
            other => Err(BinanceError::Response(format!("不認得的訂單狀態：{other}"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            OrderStatus::New => "NEW",
            OrderStatus::PendingNew => "PENDING_NEW",
            OrderStatus::PartiallyFilled => "PARTIALLY_FILLED",
            OrderStatus::Filled => "FILLED",
            OrderStatus::Canceled => "CANCELED",
            OrderStatus::PendingCancel => "PENDING_CANCEL",
            OrderStatus::Rejected => "REJECTED",
            OrderStatus::Expired => "EXPIRED",
            OrderStatus::ExpiredInMatch => "EXPIRED_IN_MATCH",
        }
    }
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 下單／查詢／撤單回應裡三個端點共用的欄位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderResponse {
    pub symbol: String,
    pub order_id: u64,
    pub client_order_id: String,
    pub status: OrderStatus,
    pub orig_qty: Fixed,
    pub executed_qty: Fixed,
    pub cummulative_quote_qty: Fixed,
}

/// 原始 JSON 形狀。數字欄位 Binance 給的是字串（例如 `"1.00000000"`），
/// 跟 4.3 `at-account-sync` 的 `RatesJson` 同一個理由：先收成 `String`，
/// 自己 `parse` 成 [`Fixed`]——`Fixed` 住在零外部依賴的 `at-core`，不能掛
/// serde 的 derive。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderResponseJson {
    symbol: String,
    order_id: u64,
    client_order_id: String,
    status: String,
    orig_qty: String,
    executed_qty: String,
    cummulative_quote_qty: String,
}

fn parse_fixed_field(value: &str, field: &str) -> Result<Fixed, BinanceError> {
    value
        .parse::<Fixed>()
        .map_err(|_| BinanceError::Response(format!("下單回應的 {field} 不是合法的數字：{value}")))
}

impl OrderResponse {
    fn from_json(json: OrderResponseJson) -> Result<Self, BinanceError> {
        Ok(OrderResponse {
            symbol: json.symbol,
            order_id: json.order_id,
            client_order_id: json.client_order_id,
            status: OrderStatus::parse(&json.status)?,
            orig_qty: parse_fixed_field(&json.orig_qty, "origQty")?,
            executed_qty: parse_fixed_field(&json.executed_qty, "executedQty")?,
            cummulative_quote_qty: parse_fixed_field(
                &json.cummulative_quote_qty,
                "cummulativeQuoteQty",
            )?,
        })
    }

    fn parse(body: &str) -> Result<Self, BinanceError> {
        let json: OrderResponseJson = serde_json::from_str(body)
            .map_err(|e| BinanceError::Response(format!("看不懂下單回應內容：{e}")))?;
        Self::from_json(json)
    }
}

/// 打一個 SIGNED POST，不帶 body（參數都在 query string），回傳原始內文。
fn post_text(url: &str, api_key: &str) -> Result<String, BinanceError> {
    match ureq::post(url).header("X-MBX-APIKEY", api_key).send_empty() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| BinanceError::Response(e.to_string())),
        Err(ureq::Error::StatusCode(code)) => Err(BinanceError::Http(format!(
            "HTTP 狀態碼 {code}，請確認金鑰、權限或下單參數是否正確"
        ))),
        Err(e) => Err(BinanceError::Http(e.to_string())),
    }
}

/// 打一個 SIGNED DELETE，回傳原始內文。
fn delete_text(url: &str, api_key: &str) -> Result<String, BinanceError> {
    match ureq::delete(url).header("X-MBX-APIKEY", api_key).call() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| BinanceError::Response(e.to_string())),
        Err(ureq::Error::StatusCode(code)) => Err(BinanceError::Http(format!(
            "HTTP 狀態碼 {code}，請確認訂單是否存在或已經結束"
        ))),
        Err(e) => Err(BinanceError::Http(e.to_string())),
    }
}

/// 測試網下單 client。只連 [`TESTNET_BASE_URL`]，沒有任何方法可以改掉這個網址。
pub struct BinanceTestnetClient {
    api_key: SecretValue,
    api_secret: SecretValue,
}

impl BinanceTestnetClient {
    pub fn new(api_key: SecretValue, api_secret: SecretValue) -> Self {
        Self {
            api_key,
            api_secret,
        }
    }

    /// 從 OS 安全儲存區讀取測試網專用金鑰建立 client（6.1 存進去的
    /// service/account，跟 4.1/4.6 正式環境的唯讀金鑰是分開的兩組）。
    pub fn from_keychain() -> Result<Self, BinanceError> {
        let store = KeychainStore::new();
        let api_key = store
            .get(TESTNET_SERVICE_API_KEY, TESTNET_ACCOUNT)
            .map_err(BinanceError::Credential)?
            .ok_or(BinanceError::MissingCredential)?;
        let api_secret = store
            .get(TESTNET_SERVICE_API_SECRET, TESTNET_ACCOUNT)
            .map_err(BinanceError::Credential)?
            .ok_or(BinanceError::MissingCredential)?;
        Ok(Self::new(api_key, api_secret))
    }

    fn signed_url(&self, params: &[(&str, &str)]) -> Result<String, BinanceError> {
        let timestamp = timestamp_ms()?;
        let query = build_signed_query(self.api_secret.expose(), params, timestamp);
        Ok(format!("{TESTNET_BASE_URL}{ORDER_PATH}?{query}"))
    }

    /// 下市價單。`quantity` 是基礎資產數量（例如 `BTCUSDT` 的 BTC 數量），
    /// 只支援市價單——限價單是之後需要時再做的範圍外工作。
    pub fn place_market_order(
        &self,
        symbol: &str,
        side: OrderSide,
        quantity: Fixed,
    ) -> Result<OrderResponse, BinanceError> {
        let quantity = quantity.to_string();
        let params = [
            ("symbol", symbol),
            ("side", side.as_str()),
            ("type", "MARKET"),
            ("quantity", quantity.as_str()),
        ];
        let url = self.signed_url(&params)?;
        let body = post_text(&url, self.api_key.expose())?;
        OrderResponse::parse(&body)
    }

    /// 查詢一筆訂單目前的狀態。
    pub fn query_order(&self, symbol: &str, order_id: u64) -> Result<OrderResponse, BinanceError> {
        let order_id = order_id.to_string();
        let params = [("symbol", symbol), ("orderId", order_id.as_str())];
        let url = self.signed_url(&params)?;
        let body = get_text(&url, Some(self.api_key.expose()))?;
        OrderResponse::parse(&body)
    }

    /// 撤銷一筆還沒結束的訂單。
    ///
    /// 市價單在 Binance 撮合引擎裡是立刻成交的，實務上送出去的瞬間就已經
    /// 結束，不會真的有「市價單被撤銷」的情境——這個方法主要是給之後萬一
    /// 支援限價單時用；對已經結束的訂單呼叫這個方法會收到 Binance 的
    /// `Unknown order sent` 錯誤（[`BinanceError::Http`]）。
    pub fn cancel_order(&self, symbol: &str, order_id: u64) -> Result<OrderResponse, BinanceError> {
        let order_id = order_id.to_string();
        let params = [("symbol", symbol), ("orderId", order_id.as_str())];
        let url = self.signed_url(&params)?;
        let body = delete_text(&url, self.api_key.expose())?;
        OrderResponse::parse(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_side_maps_to_binance_enum_strings() {
        assert_eq!(OrderSide::Buy.as_str(), "BUY");
        assert_eq!(OrderSide::Sell.as_str(), "SELL");
    }

    #[test]
    fn order_status_parses_every_documented_value() {
        let cases = [
            ("NEW", OrderStatus::New),
            ("PENDING_NEW", OrderStatus::PendingNew),
            ("PARTIALLY_FILLED", OrderStatus::PartiallyFilled),
            ("FILLED", OrderStatus::Filled),
            ("CANCELED", OrderStatus::Canceled),
            ("PENDING_CANCEL", OrderStatus::PendingCancel),
            ("REJECTED", OrderStatus::Rejected),
            ("EXPIRED", OrderStatus::Expired),
            ("EXPIRED_IN_MATCH", OrderStatus::ExpiredInMatch),
        ];
        for (raw, expected) in cases {
            assert_eq!(OrderStatus::parse(raw).unwrap(), expected, "raw={raw}");
            assert_eq!(expected.as_str(), raw);
        }
    }

    #[test]
    fn order_status_rejects_unknown_value_instead_of_guessing() {
        let err = OrderStatus::parse("SOME_NEW_STATUS_BINANCE_ADDED_LATER").unwrap_err();
        assert!(err.to_string().contains("不認得的訂單狀態"));
    }

    /// Binance 官方文件 New order 的 RESULT 範例
    /// （`binance-spot-api-docs/rest-api.md`「New order (TRADE)」一節）。
    const OFFICIAL_NEW_ORDER_RESULT: &str = r#"{
        "symbol": "BTCUSDT",
        "orderId": 28,
        "orderListId": -1,
        "clientOrderId": "6gCrw2kRUAF9CvJDGP16IP",
        "transactTime": 1507725176595,
        "price": "0.00000000",
        "origQty": "10.00000000",
        "executedQty": "10.00000000",
        "origQuoteOrderQty": "0.000000",
        "cummulativeQuoteQty": "10.00000000",
        "status": "FILLED",
        "timeInForce": "GTC",
        "type": "MARKET",
        "side": "SELL",
        "workingTime": 1507725176595,
        "selfTradePreventionMode": "NONE"
    }"#;

    /// 同一份文件的 FULL 範例（RESULT 的欄位之外多一個 `fills` 陣列），用來
    /// 確認多出來的欄位會被忽略、不會讓解析失敗。
    const OFFICIAL_NEW_ORDER_FULL: &str = r#"{
        "symbol": "BTCUSDT",
        "orderId": 28,
        "orderListId": -1,
        "clientOrderId": "6gCrw2kRUAF9CvJDGP16IP",
        "transactTime": 1507725176595,
        "price": "0.00000000",
        "origQty": "10.00000000",
        "executedQty": "10.00000000",
        "origQuoteOrderQty": "0.000000",
        "cummulativeQuoteQty": "10.00000000",
        "status": "FILLED",
        "timeInForce": "GTC",
        "type": "MARKET",
        "side": "SELL",
        "workingTime": 1507725176595,
        "selfTradePreventionMode": "NONE",
        "fills": [
            {
                "price": "4000.00000000",
                "qty": "1.00000000",
                "commission": "4.00000000",
                "commissionAsset": "USDT",
                "tradeId": 56
            }
        ]
    }"#;

    /// Binance 官方文件 Query order 的範例
    /// （`binance-spot-api-docs/rest-api.md`「Query order (USER_DATA)」一節）。
    const OFFICIAL_QUERY_ORDER: &str = r#"{
        "symbol": "LTCBTC",
        "orderId": 1,
        "orderListId": -1,
        "clientOrderId": "myOrder1",
        "price": "0.1",
        "origQty": "2",
        "executedQty": "0",
        "cummulativeQuoteQty": "0",
        "status": "NEW",
        "timeInForce": "GTC",
        "type": "LIMIT",
        "side": "BUY",
        "stopPrice": "0.0",
        "icebergQty": "0.0",
        "time": 1565245656089,
        "updateTime": 1565245656089,
        "isWorking": true,
        "workingTime": 1565245656089,
        "selfTradePreventionMode": "NONE"
    }"#;

    /// Binance 官方文件 Cancel order 的範例
    /// （`binance-spot-api-docs/rest-api.md`「Cancel order (TRADE)」一節）。
    const OFFICIAL_CANCEL_ORDER: &str = r#"{
        "symbol": "LTCBTC",
        "origClientOrderId": "myOrder1",
        "orderId": 4,
        "orderListId": -1,
        "clientOrderId": "cancelMyOrder1",
        "transactTime": 1684804350068,
        "price": "2.00000000",
        "origQty": "1.00000000",
        "executedQty": "0.00000000",
        "cummulativeQuoteQty": "0.00000000",
        "status": "CANCELED",
        "timeInForce": "GTC",
        "type": "LIMIT",
        "side": "BUY",
        "selfTradePreventionMode": "NONE"
    }"#;

    #[test]
    fn parses_official_new_order_result_example() {
        let order = OrderResponse::parse(OFFICIAL_NEW_ORDER_RESULT).unwrap();
        assert_eq!(order.symbol, "BTCUSDT");
        assert_eq!(order.order_id, 28);
        assert_eq!(order.client_order_id, "6gCrw2kRUAF9CvJDGP16IP");
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.orig_qty, "10".parse::<Fixed>().unwrap());
        assert_eq!(order.executed_qty, "10".parse::<Fixed>().unwrap());
        assert_eq!(order.cummulative_quote_qty, "10".parse::<Fixed>().unwrap());
    }

    #[test]
    fn parses_official_new_order_full_example_ignoring_fills() {
        let order = OrderResponse::parse(OFFICIAL_NEW_ORDER_FULL).unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.executed_qty, "10".parse::<Fixed>().unwrap());
    }

    #[test]
    fn parses_official_query_order_example() {
        let order = OrderResponse::parse(OFFICIAL_QUERY_ORDER).unwrap();
        assert_eq!(order.symbol, "LTCBTC");
        assert_eq!(order.order_id, 1);
        assert_eq!(order.status, OrderStatus::New);
        assert_eq!(order.orig_qty, "2".parse::<Fixed>().unwrap());
        assert_eq!(order.executed_qty, "0".parse::<Fixed>().unwrap());
    }

    #[test]
    fn parses_official_cancel_order_example() {
        let order = OrderResponse::parse(OFFICIAL_CANCEL_ORDER).unwrap();
        assert_eq!(order.symbol, "LTCBTC");
        assert_eq!(order.order_id, 4);
        assert_eq!(order.status, OrderStatus::Canceled);
    }

    #[test]
    fn malformed_json_is_a_response_error_not_a_panic() {
        let err = OrderResponse::parse("not json").unwrap_err();
        assert!(matches!(err, BinanceError::Response(_)));
    }

    #[test]
    fn non_numeric_quantity_field_is_a_response_error() {
        let bad = OFFICIAL_QUERY_ORDER.replace(r#""origQty": "2""#, r#""origQty": "not-a-number""#);
        let err = OrderResponse::parse(&bad).unwrap_err();
        assert!(err.to_string().contains("origQty"));
    }

    #[test]
    fn signed_url_points_only_at_the_testnet_host() {
        let client = BinanceTestnetClient::new(
            SecretValue::new("fake-test-key"),
            SecretValue::new("fake-test-secret"),
        );
        let url = client
            .signed_url(&[("symbol", "BTCUSDT")])
            .expect("系統時間必須算得出來");
        assert!(
            url.starts_with("https://testnet.binance.vision/api/v3/order?"),
            "網址必須是測試網的下單端點：{url}"
        );
        assert!(
            !url.contains("api.binance.com"),
            "絕對不能連到正式環境：{url}"
        );
        assert!(url.contains("signature="), "必須附上簽名：{url}");
    }

    /// 實際對測試網（`testnet.binance.vision`）下一張市價單、再查詢一次，
    /// 驗證簽名＋下單＋解析整條路徑真的是通的。
    ///
    /// 不在 `cargo test` 預設跑：
    /// - 需要 6.1 的測試網專用 Keychain 憑證（`com.autotrader.app.binance-testnet-api-key`
    ///   / `-api-secret`，account `autotrader`），這個開發環境裡沒有，必須由
    ///   使用者自己去 <https://testnet.binance.vision/> 申請後存進 Keychain
    ///   才能跑。
    /// - 下單數量 `0.001` BTC 是抓一個大致合理的下限，測試網當下的
    ///   `LOT_SIZE`/`MIN_NOTIONAL` 規則不同時可能需要調整才會成功——這條
    ///   還沒有實際跑過，數量只是起點，不保證直接過。
    ///
    /// 市價單在撮合引擎裡會立刻成交，所以這裡沒有另外測 `cancel_order`：
    /// 對一張已經成交的市價單撤單，Binance 一定回 `Unknown order sent`，
    /// 不是這個函式本身的 bug。`cancel_order` 的「打對端點」已經由
    /// `parses_official_cancel_order_example` 跟 `signed_url_points_only_at_the_testnet_host`
    /// 間接涵蓋；要真的驗證撤單成功，之後支援限價單時再補一個會先掛在簿上、
    /// 不會立刻成交的訂單。
    ///
    /// 手動驗證：`cargo test -p at-binance -- --ignored`
    #[test]
    #[ignore]
    fn place_and_query_market_order_against_real_testnet() {
        let client = BinanceTestnetClient::from_keychain()
            .expect("讀取測試網 Keychain 憑證失敗，請先手動存入 6.1 的測試網金鑰");

        let placed = client
            .place_market_order("BTCUSDT", OrderSide::Buy, "0.001".parse::<Fixed>().unwrap())
            .expect("下市價單失敗");
        assert!(
            matches!(
                placed.status,
                OrderStatus::Filled | OrderStatus::PartiallyFilled
            ),
            "市價單應該立刻成交或部分成交，實際狀態：{}",
            placed.status
        );

        let queried = client
            .query_order("BTCUSDT", placed.order_id)
            .expect("查詢訂單狀態失敗");
        assert_eq!(queried.order_id, placed.order_id);
        assert_eq!(queried.symbol, "BTCUSDT");
    }
}
