import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CredentialSection } from "./CredentialSection";
import type { AccountPermissions } from "./accountPermissionsTypes";

type CheckStatus = "idle" | "checking" | "success" | "error";

function permissionLabel(granted: boolean): string {
  return granted ? "已開啟" : "已確認關閉";
}

// 設定頁面：正式環境（4.1/4.6 唯讀金鑰）與測試網（6.1）金鑰分開兩個區塊，
// 各自呼叫各自既有的 Keychain command，不混在一起顯示。權限清單只顯示
// 「測試連線」按鈕實際查回來的結果（check_account_permissions 打
// /api/v3/account），查詢前不假裝有資料。合約帳戶設定目前沒有能力查詢，
// 標成「即將推出」。引擎節點（本機/東京節點）不在這次範圍，完全不畫。
export function Settings() {
  const [permissions, setPermissions] = useState<AccountPermissions | null>(null);
  const [checkStatus, setCheckStatus] = useState<CheckStatus>("idle");
  const [checkError, setCheckError] = useState<string | null>(null);

  async function handleCheckPermissions() {
    setCheckStatus("checking");
    setCheckError(null);
    try {
      const result = await invoke<AccountPermissions>("check_account_permissions");
      setPermissions(result);
      setCheckStatus("success");
    } catch (err) {
      setCheckError(String(err));
      setCheckStatus("error");
    }
  }

  return (
    <div className="settings">
      <CredentialSection
        title="正式環境 API 金鑰"
        statusCommand="binance_credentials_status"
        saveCommand="save_binance_credentials"
        clearCommand="clear_binance_credentials"
        keyLabel="API Key"
        secretLabel="API Secret"
        idPrefix="settings"
        saveButtonLabel="儲存"
        clearButtonLabel="清除已儲存的金鑰"
        confirmClearMessage="確定要清除已儲存的 Binance API 金鑰嗎？此動作無法復原。"
      />

      <CredentialSection
        title="測試網 API 金鑰"
        statusCommand="testnet_credentials_status"
        saveCommand="save_testnet_credentials"
        clearCommand="clear_testnet_credentials"
        keyLabel="測試網 API Key"
        secretLabel="測試網 API Secret"
        idPrefix="settings-testnet"
        saveButtonLabel="儲存測試網金鑰"
        clearButtonLabel="清除測試網金鑰"
        confirmClearMessage="確定要清除已儲存的測試網 API 金鑰嗎？此動作無法復原。"
      />

      <section className="settings-status" aria-label="權限檢查清單">
        <h2>權限檢查清單（正式環境）</h2>
        <p>金鑰類型：Ed25519（由 Binance 產生時決定） ・ 儲存位置：系統鑰匙圈</p>
        {checkError && (
          <p role="alert" className="settings-status__error">
            測試連線失敗：{checkError}
          </p>
        )}
        {checkStatus === "success" && (
          <p role="status" className="settings-form__success">
            連線成功，以下是查詢到的實際權限。
          </p>
        )}
        {permissions ? (
          <dl className="settings-status__list">
            <div className="settings-status__item">
              <dt>交易</dt>
              <dd>{permissionLabel(permissions.canTrade)}</dd>
            </div>
            <div className="settings-status__item">
              <dt>提幣</dt>
              <dd>{permissionLabel(permissions.canWithdraw)}</dd>
            </div>
            <div className="settings-status__item">
              <dt>入金</dt>
              <dd>{permissionLabel(permissions.canDeposit)}</dd>
            </div>
          </dl>
        ) : (
          <p>尚未查詢，請按下方按鈕測試連線並查詢目前權限。</p>
        )}
        <button
          type="button"
          className="settings-form__save"
          onClick={handleCheckPermissions}
          disabled={checkStatus === "checking"}
        >
          {checkStatus === "checking" ? "查詢中…" : "測試連線"}
        </button>
      </section>

      <section className="settings-status" aria-label="合約帳戶設定">
        <h2>合約帳戶設定</h2>
        <p>即將推出：目前尚未支援查詢合約帳戶資料。</p>
      </section>
    </div>
  );
}
