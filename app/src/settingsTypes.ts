// 對應 Rust 端 settings.rs 的 CredentialStatus（camelCase，見 backtest.rs 同樣的
// serde(rename_all = "camelCase") 慣例）。刻意不含憑證內容欄位。
export interface CredentialStatus {
  apiKeySet: boolean;
  apiSecretSet: boolean;
}
