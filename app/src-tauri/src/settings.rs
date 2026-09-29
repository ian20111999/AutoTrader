//! 4.6 連線設定畫面的 Tauri command：把 4.1 的 [`at_secrets::KeychainStore`]
//! 包成前端能呼叫的「存 / 查狀態 / 清除」三個 command。完全不連網路，
//! 只操作本機 OS 安全儲存區。
//!
//! # 安全設計
//!
//! - `save_binance_credentials`/`clear_binance_credentials` 的參數與回傳值都
//!   不含憑證內容；[`at_secrets::SecretError`] 本來就只回固定中文訊息，這裡
//!   直接 `.to_string()` 轉成 command 的 `Err`，不會夾帶輸入值。
//! - `binance_credentials_status` 只回答「有沒有設定」的布林值，刻意不回傳
//!   `SecretValue`，就算呼叫端想印出回傳值也印不出金鑰內容。
//! - 這個模組完全不呼叫任何 `log`/`println!`/`eprintln!`，也沒有用到會記錄
//!   command 參數的 Tauri plugin（本專案目前唯一裝的是 `tauri-plugin-opener`，
//!   不記錄 command 參數；`invoke_handler` 本身也不會把參數寫進任何 log）。

use at_secrets::{CredentialStore, KeychainStore, SecretValue};
use serde::Serialize;

const SERVICE_API_KEY: &str = "com.autotrader.app.binance-readonly-api-key";
const SERVICE_API_SECRET: &str = "com.autotrader.app.binance-readonly-api-secret";
const ACCOUNT: &str = "autotrader";

/// 「有沒有設定」的狀態，刻意不含實際憑證內容。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub api_key_set: bool,
    pub api_secret_set: bool,
}

fn status_with(store: &dyn CredentialStore) -> Result<CredentialStatus, String> {
    let api_key_set = store
        .get(SERVICE_API_KEY, ACCOUNT)
        .map_err(|e| e.to_string())?
        .is_some();
    let api_secret_set = store
        .get(SERVICE_API_SECRET, ACCOUNT)
        .map_err(|e| e.to_string())?
        .is_some();
    Ok(CredentialStatus {
        api_key_set,
        api_secret_set,
    })
}

/// 兩個欄位一起驗證、一起存，避免「secret 空白被拒絕，但 key 已經先存進去」的
/// 半套狀態——驗證放在任何 `store.set` 之前。存進去的是 `trim()` 過的值：
/// 複製貼上常見的前後空白/換行如果原樣存進去，4.2 拿去做 HMAC 簽名時會直接
/// 簽名失敗，而且錯誤訊息完全看不出來是這個原因。
fn save_with(store: &dyn CredentialStore, api_key: &str, api_secret: &str) -> Result<(), String> {
    let api_key = api_key.trim();
    let api_secret = api_secret.trim();
    if api_key.is_empty() {
        return Err("API Key 不能是空白".to_string());
    }
    if api_secret.is_empty() {
        return Err("API Secret 不能是空白".to_string());
    }
    store
        .set(SERVICE_API_KEY, ACCOUNT, &SecretValue::new(api_key))
        .map_err(|e| e.to_string())?;
    store
        .set(SERVICE_API_SECRET, ACCOUNT, &SecretValue::new(api_secret))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn clear_with(store: &dyn CredentialStore) -> Result<(), String> {
    store
        .delete(SERVICE_API_KEY, ACCOUNT)
        .map_err(|e| e.to_string())?;
    store
        .delete(SERVICE_API_SECRET, ACCOUNT)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn save_binance_credentials(api_key: String, api_secret: String) -> Result<(), String> {
    save_with(&KeychainStore::new(), &api_key, &api_secret)
}

#[tauri::command]
pub fn binance_credentials_status() -> Result<CredentialStatus, String> {
    status_with(&KeychainStore::new())
}

#[tauri::command]
pub fn clear_binance_credentials() -> Result<(), String> {
    clear_with(&KeychainStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_secrets::InMemoryStore;

    const NOTHING_SET: CredentialStatus = CredentialStatus {
        api_key_set: false,
        api_secret_set: false,
    };

    #[test]
    fn status_is_false_false_when_nothing_saved() {
        let store = InMemoryStore::new();
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn save_then_status_reports_both_set() {
        let store = InMemoryStore::new();
        save_with(&store, "fake-test-key-12345", "fake-test-secret-67890").unwrap();
        assert_eq!(
            status_with(&store).unwrap(),
            CredentialStatus {
                api_key_set: true,
                api_secret_set: true,
            }
        );
    }

    #[test]
    fn empty_api_key_is_rejected_without_saving_anything() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "", "fake-test-secret-67890").unwrap_err();
        assert_eq!(err, "API Key 不能是空白");
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn blank_api_key_is_rejected_same_as_empty() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "   ", "fake-test-secret-67890").unwrap_err();
        assert_eq!(err, "API Key 不能是空白");
    }

    #[test]
    fn empty_api_secret_is_rejected_without_leaving_the_key_saved() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "fake-test-key-12345", "").unwrap_err();
        assert_eq!(err, "API Secret 不能是空白");
        // 驗證順序保證：secret 沒過，key 也不該留下半套狀態。
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn save_trims_surrounding_whitespace_before_storing() {
        let store = InMemoryStore::new();
        save_with(
            &store,
            "  fake-test-key-12345\n",
            "\tfake-test-secret-67890 ",
        )
        .unwrap();
        assert_eq!(
            store
                .get(SERVICE_API_KEY, ACCOUNT)
                .unwrap()
                .unwrap()
                .expose(),
            "fake-test-key-12345"
        );
        assert_eq!(
            store
                .get(SERVICE_API_SECRET, ACCOUNT)
                .unwrap()
                .unwrap()
                .expose(),
            "fake-test-secret-67890"
        );
    }

    #[test]
    fn save_overwrites_the_previous_value() {
        let store = InMemoryStore::new();
        save_with(&store, "fake-test-key-old", "fake-test-secret-old").unwrap();
        save_with(&store, "fake-test-key-new", "fake-test-secret-new").unwrap();
        assert_eq!(
            store
                .get(SERVICE_API_KEY, ACCOUNT)
                .unwrap()
                .unwrap()
                .expose(),
            "fake-test-key-new"
        );
    }

    #[test]
    fn clear_removes_both_and_status_goes_back_to_false() {
        let store = InMemoryStore::new();
        save_with(&store, "fake-test-key-12345", "fake-test-secret-67890").unwrap();
        clear_with(&store).unwrap();
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn clear_on_nothing_saved_is_ok_not_err() {
        let store = InMemoryStore::new();
        assert!(clear_with(&store).is_ok());
    }

    #[test]
    fn error_messages_never_contain_the_credential_value() {
        let store = InMemoryStore::new();
        let secret_marker = "totally-secret-value-should-not-leak";
        let err = save_with(&store, secret_marker, "").unwrap_err();
        assert!(
            !err.contains(secret_marker),
            "錯誤訊息洩漏了憑證內容：{err}"
        );
    }
}
