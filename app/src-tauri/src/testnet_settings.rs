//! 6.5 測試網交易頁面的測試網專用 Keychain 憑證管理：存 / 查狀態 / 清除。
//!
//! 架構完全比照 `settings.rs`（4.6 正式環境唯讀金鑰的連線設定頁面）——
//! 差別只有這裡用的是 `at_binance::testnet` 公開出來的測試網專用
//! service/account 常數，跟正式環境的唯讀金鑰是分開兩組，不會互相覆蓋。
//! 安全設計（錯誤訊息不含憑證內容、狀態只回布林值、不印 log）理由同
//! `settings.rs`，不重複寫一次。

use at_binance::testnet::{TESTNET_ACCOUNT, TESTNET_SERVICE_API_KEY, TESTNET_SERVICE_API_SECRET};
use at_secrets::{CredentialStore, KeychainStore, SecretValue};
use serde::Serialize;

/// 「有沒有設定」的狀態，刻意不含實際憑證內容。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestnetCredentialStatus {
    pub api_key_set: bool,
    pub api_secret_set: bool,
}

fn status_with(store: &dyn CredentialStore) -> Result<TestnetCredentialStatus, String> {
    let api_key_set = store
        .get(TESTNET_SERVICE_API_KEY, TESTNET_ACCOUNT)
        .map_err(|e| e.to_string())?
        .is_some();
    let api_secret_set = store
        .get(TESTNET_SERVICE_API_SECRET, TESTNET_ACCOUNT)
        .map_err(|e| e.to_string())?
        .is_some();
    Ok(TestnetCredentialStatus {
        api_key_set,
        api_secret_set,
    })
}

fn save_with(store: &dyn CredentialStore, api_key: &str, api_secret: &str) -> Result<(), String> {
    let api_key = api_key.trim();
    let api_secret = api_secret.trim();
    if api_key.is_empty() {
        return Err("測試網 API Key 不能是空白".to_string());
    }
    if api_secret.is_empty() {
        return Err("測試網 API Secret 不能是空白".to_string());
    }
    store
        .set(
            TESTNET_SERVICE_API_KEY,
            TESTNET_ACCOUNT,
            &SecretValue::new(api_key),
        )
        .map_err(|e| e.to_string())?;
    store
        .set(
            TESTNET_SERVICE_API_SECRET,
            TESTNET_ACCOUNT,
            &SecretValue::new(api_secret),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn clear_with(store: &dyn CredentialStore) -> Result<(), String> {
    store
        .delete(TESTNET_SERVICE_API_KEY, TESTNET_ACCOUNT)
        .map_err(|e| e.to_string())?;
    store
        .delete(TESTNET_SERVICE_API_SECRET, TESTNET_ACCOUNT)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn save_testnet_credentials(api_key: String, api_secret: String) -> Result<(), String> {
    save_with(&KeychainStore::new(), &api_key, &api_secret)
}

#[tauri::command]
pub fn testnet_credentials_status() -> Result<TestnetCredentialStatus, String> {
    status_with(&KeychainStore::new())
}

#[tauri::command]
pub fn clear_testnet_credentials() -> Result<(), String> {
    clear_with(&KeychainStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_secrets::InMemoryStore;

    const NOTHING_SET: TestnetCredentialStatus = TestnetCredentialStatus {
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
        save_with(
            &store,
            "fake-testnet-key-12345",
            "fake-testnet-secret-67890",
        )
        .unwrap();
        assert_eq!(
            status_with(&store).unwrap(),
            TestnetCredentialStatus {
                api_key_set: true,
                api_secret_set: true,
            }
        );
    }

    #[test]
    fn empty_api_key_is_rejected_without_saving_anything() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "", "fake-testnet-secret-67890").unwrap_err();
        assert_eq!(err, "測試網 API Key 不能是空白");
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn blank_api_key_is_rejected_same_as_empty() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "   ", "fake-testnet-secret-67890").unwrap_err();
        assert_eq!(err, "測試網 API Key 不能是空白");
    }

    #[test]
    fn empty_api_secret_is_rejected_without_leaving_the_key_saved() {
        let store = InMemoryStore::new();
        let err = save_with(&store, "fake-testnet-key-12345", "").unwrap_err();
        assert_eq!(err, "測試網 API Secret 不能是空白");
        assert_eq!(status_with(&store).unwrap(), NOTHING_SET);
    }

    #[test]
    fn save_trims_surrounding_whitespace_before_storing() {
        let store = InMemoryStore::new();
        save_with(
            &store,
            "  fake-testnet-key-12345\n",
            "\tfake-testnet-secret-67890 ",
        )
        .unwrap();
        assert_eq!(
            store
                .get(TESTNET_SERVICE_API_KEY, TESTNET_ACCOUNT)
                .unwrap()
                .unwrap()
                .expose(),
            "fake-testnet-key-12345"
        );
    }

    #[test]
    fn save_overwrites_the_previous_value() {
        let store = InMemoryStore::new();
        save_with(&store, "fake-testnet-key-old", "fake-testnet-secret-old").unwrap();
        save_with(&store, "fake-testnet-key-new", "fake-testnet-secret-new").unwrap();
        assert_eq!(
            store
                .get(TESTNET_SERVICE_API_KEY, TESTNET_ACCOUNT)
                .unwrap()
                .unwrap()
                .expose(),
            "fake-testnet-key-new"
        );
    }

    #[test]
    fn clear_removes_both_and_status_goes_back_to_false() {
        let store = InMemoryStore::new();
        save_with(
            &store,
            "fake-testnet-key-12345",
            "fake-testnet-secret-67890",
        )
        .unwrap();
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

    #[test]
    fn testnet_credentials_are_independent_of_production_ones() {
        // 兩組憑證用不同的 service name，同一個 InMemoryStore 裡存測試網的值
        // 不該被正式環境的 service name 讀到（InMemoryStore 以 (service, account) 為鍵）。
        let store = InMemoryStore::new();
        save_with(&store, "fake-testnet-key", "fake-testnet-secret").unwrap();
        let prod = store
            .get(
                "com.autotrader.app.binance-readonly-api-key",
                TESTNET_ACCOUNT,
            )
            .unwrap();
        assert!(prod.is_none(), "測試網金鑰不該污染正式環境的 service name");
    }
}
