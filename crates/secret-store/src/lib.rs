//! 讀寫作業系統安全儲存區（macOS 鑰匙圈、Windows 認證管理員）。
//!
//! 用 [`CredentialStore`] 這個 trait 把「存 / 讀 / 刪一組憑證」抽象出來：
//! 正式環境用 [`KeychainStore`]（底層是 `keyring` crate），測試用
//! [`InMemoryStore`]（純 Rust `HashMap`，不碰真實系統，避免每次 `cargo test`
//! 都在使用者的登入鑰匙圈裡留下測試用的憑證）。
//!
//! `keyring` 4.2 的預設 feature（`v1`）已經涵蓋 macOS 鑰匙圈與 Windows
//! 認證管理員兩個平台，不需要額外開 feature flag。
//!
//! # 安全設計
//!
//! [`SecretValue`] 刻意不實作 `Debug` / `Display`，避免呼叫端不小心把憑證值
//! 印進 log、錯誤訊息或 `{:?}`。要拿到實際內容只能呼叫 [`SecretValue::expose`]。
//!
//! 所有函式回傳的 [`SecretError`] 都是固定的中文訊息，絕對不會夾帶憑證內容——
//! 特別是 `keyring` crate 的 `Error::BadEncoding(Vec<u8>)`，它的 payload 就是
//! 讀取失敗時原始的憑證 bytes，這個模組完全不去 `Display`/`Debug` 原始的
//! `keyring::Error`，一律轉成自己固定的訊息。

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// 一個憑證值。刻意不實作 `Debug`/`Display`，讓「印出這個型別」在編譯期就做不到；
/// 需要實際內容時只能呼叫 [`SecretValue::expose`]，逼呼叫端明確意識到自己在碰敏感值。
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出實際內容。呼叫端要自行確保不會把回傳值印進 log 或錯誤訊息。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// 存取安全儲存區失敗的原因。訊息一律是固定的中文說明，不含憑證內容。
#[derive(Debug)]
pub enum SecretError {
    /// 系統安全儲存區存取失敗（鑰匙圈被鎖住、使用者拒絕授權、平台初始化失敗等）。
    Store(String),
    /// 儲存的內容不是合法的 UTF-8 文字。
    BadEncoding,
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::Store(msg) => write!(f, "存取系統安全儲存區失敗：{msg}"),
            SecretError::BadEncoding => write!(f, "儲存的內容不是合法的文字編碼"),
        }
    }
}

impl std::error::Error for SecretError {}

/// 讀寫一組憑證（service + account 定位一筆，值是 [`SecretValue`]）。
///
/// 找不到憑證不是錯誤：`get` 回傳 `Ok(None)`；`delete` 對不存在的憑證也回傳
/// `Ok(())`（視為已經是「登出」狀態，讓呼叫端不用先判斷存不存在）。
pub trait CredentialStore {
    fn set(&self, service: &str, account: &str, value: &SecretValue) -> Result<(), SecretError>;
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>, SecretError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError>;
}

/// 把 `keyring::Error` 轉成 [`SecretError`]。
///
/// 絕對不對原始的 `keyring::Error` 呼叫 `Display`/`Debug`——`Error::BadEncoding`
/// 的 payload 就是讀取失敗時的原始憑證 bytes，一旦印出來就等於洩漏憑證。
/// 所以這裡逐一列舉已知的變體，一律回傳自己寫死的中文訊息。
fn map_keyring_error(err: keyring::Error) -> SecretError {
    match err {
        keyring::Error::BadEncoding(_) => SecretError::BadEncoding,
        keyring::Error::NoStorageAccess(_) => {
            SecretError::Store("無法存取系統安全儲存區（可能被鎖住或權限不足）".to_string())
        }
        keyring::Error::Ambiguous(_) => {
            SecretError::Store("找到多筆符合的憑證，設定不明確".to_string())
        }
        keyring::Error::Invalid(_, _) => SecretError::Store("憑證存取參數不合法".to_string()),
        keyring::Error::TooLong(_, _) => SecretError::Store("憑證識別資訊過長".to_string()),
        keyring::Error::NoEntry => SecretError::Store("找不到指定的憑證".to_string()),
        _ => SecretError::Store("系統安全儲存區發生未預期錯誤".to_string()),
    }
}

/// 正式環境用的實作，底層是 `keyring` crate（macOS 鑰匙圈 / Windows 認證管理員）。
#[derive(Debug, Default)]
pub struct KeychainStore;

impl KeychainStore {
    pub fn new() -> Self {
        Self
    }
}

impl CredentialStore for KeychainStore {
    fn set(&self, service: &str, account: &str, value: &SecretValue) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(service, account).map_err(map_keyring_error)?;
        entry
            .set_password(value.expose())
            .map_err(map_keyring_error)
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>, SecretError> {
        let entry = keyring::Entry::new(service, account).map_err(map_keyring_error)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(SecretValue::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring_error(e)),
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(service, account).map_err(map_keyring_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring_error(e)),
        }
    }
}

/// 測試用的實作：純 Rust `HashMap`，不碰真實系統。
///
/// 用來驗證這個模組自己的業務邏輯（例如「找不到回 `None` 不是 `Err`」），
/// 不需要每次 `cargo test` 都在使用者真實的登入鑰匙圈裡寫測試資料。
#[derive(Debug, Default)]
pub struct InMemoryStore {
    data: Mutex<HashMap<(String, String), String>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryStore {
    fn set(&self, service: &str, account: &str, value: &SecretValue) -> Result<(), SecretError> {
        self.data.lock().unwrap().insert(
            (service.to_string(), account.to_string()),
            value.expose().to_string(),
        );
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>, SecretError> {
        let key = (service.to_string(), account.to_string());
        Ok(self
            .data
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .map(SecretValue))
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
        let key = (service.to_string(), account.to_string());
        self.data.lock().unwrap().remove(&key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_credential_is_none_not_err() {
        let store = InMemoryStore::new();
        assert!(store.get("svc", "acc").unwrap().is_none());
    }

    #[test]
    fn set_then_get_round_trips() {
        let store = InMemoryStore::new();
        store
            .set("svc", "acc", &SecretValue::new("test-value"))
            .unwrap();
        let got = store.get("svc", "acc").unwrap().unwrap();
        assert_eq!(got.expose(), "test-value");
    }

    #[test]
    fn different_service_or_account_are_isolated() {
        let store = InMemoryStore::new();
        store
            .set("svc-a", "acc", &SecretValue::new("value-a"))
            .unwrap();
        store
            .set("svc-b", "acc", &SecretValue::new("value-b"))
            .unwrap();
        store
            .set("svc-a", "acc-2", &SecretValue::new("value-a-2"))
            .unwrap();

        assert_eq!(
            store.get("svc-a", "acc").unwrap().unwrap().expose(),
            "value-a"
        );
        assert_eq!(
            store.get("svc-b", "acc").unwrap().unwrap().expose(),
            "value-b"
        );
        assert_eq!(
            store.get("svc-a", "acc-2").unwrap().unwrap().expose(),
            "value-a-2"
        );
    }

    #[test]
    fn delete_removes_the_credential() {
        let store = InMemoryStore::new();
        store.set("svc", "acc", &SecretValue::new("v")).unwrap();
        store.delete("svc", "acc").unwrap();
        assert!(store.get("svc", "acc").unwrap().is_none());
    }

    #[test]
    fn delete_on_missing_credential_is_ok_not_err() {
        let store = InMemoryStore::new();
        assert!(store.delete("svc", "does-not-exist").is_ok());
    }

    #[test]
    fn overwriting_replaces_the_value() {
        let store = InMemoryStore::new();
        store.set("svc", "acc", &SecretValue::new("old")).unwrap();
        store.set("svc", "acc", &SecretValue::new("new")).unwrap();
        assert_eq!(store.get("svc", "acc").unwrap().unwrap().expose(), "new");
    }

    /// 實際存取這台機器的系統安全儲存區（macOS 鑰匙圈 / Windows 認證管理員）。
    /// 不在 `cargo test` 預設跑——不同開發機、不同 CI 環境的鑰匙圈狀態不一定
    /// 一致（例如 CI 容器裡可能根本沒有鑰匙圈可用），而且這是會真的寫入系統
    /// 儲存區的測試，不該在每次 commit 前自動觸發。
    ///
    /// 用專屬的測試 service 名稱（不是正式環境的
    /// `com.autotrader.app.binance-readonly-*`），並且在同一個測試函式裡完整
    /// 做「先清乾淨 → 存入 → 讀出 → 刪除 → 確認刪乾淨」，結束時不留殘留憑證。
    ///
    /// 手動驗證：`cargo test -p at-secrets -- --ignored`
    #[test]
    #[ignore]
    fn keychain_store_round_trips_against_the_real_os_store() {
        let store = KeychainStore::new();
        let service = "com.autotrader.app.secrets-crate-selftest";
        let account = "selftest";

        // 先確保沒有上次測試失敗留下的殘留。
        store.delete(service, account).unwrap();
        assert!(store.get(service, account).unwrap().is_none());

        store
            .set(service, account, &SecretValue::new("selftest-value"))
            .unwrap();
        let got = store.get(service, account).unwrap().unwrap();
        assert_eq!(got.expose(), "selftest-value");

        store.delete(service, account).unwrap();
        assert!(store.get(service, account).unwrap().is_none());
    }
}
