//! 設定頁面「測試連線」按鈕：實際打一次 `/api/v3/account`（SIGNED、唯讀），
//! 讀回這組正式環境金鑰目前真正有的權限（交易/提領/入金），同時也當作
//! 連線是否成功的依據——呼叫失敗就是連線失敗。
//!
//! 不走 4.3 `at-account-sync` 的 24 小時快取：使用者按這顆按鈕就是想知道
//!「現在」通不通、權限是不是這樣，快取掉的話按鈕會變成假的。
//!
//! # 安全設計
//!
//! 回應裡還有 `balances` 等帳戶資產欄位，這裡刻意只取三個布林權限欄位，
//! 其餘一律丟棄，不讓資產內容流到前端或任何錯誤訊息裡。

use at_binance::BinanceClient;
use serde::{Deserialize, Serialize};

const ACCOUNT_PATH: &str = "/api/v3/account";

/// `GET /api/v3/account` 回應裡跟這顆按鈕有關的欄位，其餘（餘額等）不解析。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountInfoJson {
    can_trade: bool,
    can_withdraw: bool,
    can_deposit: bool,
}

/// 回給前端的權限清單，刻意只含三個布林欄位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPermissions {
    pub can_trade: bool,
    pub can_withdraw: bool,
    pub can_deposit: bool,
}

fn parse_permissions(body: &str) -> Result<AccountPermissions, String> {
    let parsed: AccountInfoJson =
        serde_json::from_str(body).map_err(|_| "看不懂 Binance 回傳的帳戶權限內容".to_string())?;
    Ok(AccountPermissions {
        can_trade: parsed.can_trade,
        can_withdraw: parsed.can_withdraw,
        can_deposit: parsed.can_deposit,
    })
}

#[tauri::command]
pub fn check_account_permissions() -> Result<AccountPermissions, String> {
    let client = BinanceClient::from_keychain().map_err(|e| e.to_string())?;
    let body = client
        .signed_get(ACCOUNT_PATH, &[])
        .map_err(|e| e.to_string())?;
    parse_permissions(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFICIAL_EXAMPLE: &str = r#"{
      "makerCommission": 15,
      "canTrade": true,
      "canWithdraw": false,
      "canDeposit": true,
      "balances": [
        {"asset":"BTC","free":"4723846.89208129","locked":"0.00000000"}
      ]
    }"#;

    #[test]
    fn parses_the_three_permission_flags() {
        assert_eq!(
            parse_permissions(OFFICIAL_EXAMPLE).unwrap(),
            AccountPermissions {
                can_trade: true,
                can_withdraw: false,
                can_deposit: true,
            }
        );
    }

    #[test]
    fn ignores_unrelated_fields_like_balances() {
        // balances 刻意不解析：上面那個測試已經用含 balances 的回應證明不會
        // 因為多餘欄位而解析失敗；這裡再確認錯誤路徑一樣不會夾帶內容。
        let err = parse_permissions("{ 這不是合法的 JSON").unwrap_err();
        assert!(!err.contains("這不是合法的 JSON"));
    }

    #[test]
    fn bad_json_gives_a_fixed_chinese_message() {
        let err = parse_permissions("not json").unwrap_err();
        assert_eq!(err, "看不懂 Binance 回傳的帳戶權限內容");
    }
}
