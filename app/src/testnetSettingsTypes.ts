// 對應 Rust 端 testnet_settings.rs 的 TestnetCredentialStatus（camelCase）。
// 跟 settingsTypes.ts 的 CredentialStatus 結構一樣，分開成獨立型別是因為
// 這是另一組測試網專用金鑰，型別上不想讓兩邊互相替用。
export interface TestnetCredentialStatus {
  apiKeySet: boolean;
  apiSecretSet: boolean;
}
