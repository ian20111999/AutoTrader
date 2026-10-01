// 對應 Rust 端 account_permissions.rs 的 AccountPermissions（camelCase）。
// 「測試連線」按鈕打 /api/v3/account 查回來的三個真實權限欄位。
export interface AccountPermissions {
  canTrade: boolean;
  canWithdraw: boolean;
  canDeposit: boolean;
}
