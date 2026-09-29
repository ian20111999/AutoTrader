import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CredentialStatus } from "./settingsTypes";

type SaveStatus = "idle" | "saving" | "success" | "error";
type ClearStatus = "idle" | "clearing" | "error";

function statusLabel(isSet: boolean): string {
  return isSet ? "已設定" : "未設定";
}

// 這一步完全不連 Binance 網路，只是把 4.1 的 KeychainStore 包成表單。
export function Settings() {
  const [status, setStatus] = useState<CredentialStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [apiSecret, setApiSecret] = useState("");
  const [saveStatus, setSaveStatus] = useState<SaveStatus>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [clearStatus, setClearStatus] = useState<ClearStatus>("idle");
  const [clearError, setClearError] = useState<string | null>(null);

  async function loadStatus() {
    try {
      const result = await invoke<CredentialStatus>("binance_credentials_status");
      setStatus(result);
      setStatusError(null);
    } catch (err) {
      setStatusError(String(err));
    }
  }

  useEffect(() => {
    invoke<CredentialStatus>("binance_credentials_status")
      .then((result) => {
        setStatus(result);
        setStatusError(null);
      })
      .catch((err: unknown) => setStatusError(String(err)));
  }, []);

  async function handleSave(event: FormEvent) {
    event.preventDefault();
    setSaveStatus("saving");
    setSaveError(null);
    try {
      // apiKey/apiSecret 只在這裡送出去存進 Keychain，不寫進任何 log。
      await invoke("save_binance_credentials", { apiKey, apiSecret });
      setApiKey("");
      setApiSecret("");
      setSaveStatus("success");
      await loadStatus();
    } catch (err) {
      setSaveError(String(err));
      setSaveStatus("error");
    }
  }

  async function handleClear() {
    if (!window.confirm("確定要清除已儲存的 Binance API 金鑰嗎？此動作無法復原。")) {
      return;
    }
    setClearStatus("clearing");
    setClearError(null);
    try {
      await invoke("clear_binance_credentials");
      setClearStatus("idle");
      await loadStatus();
    } catch (err) {
      setClearError(String(err));
      setClearStatus("error");
    }
  }

  return (
    <section className="settings" aria-label="連線設定">
      <div className="settings-status">
        <h2>Binance 連線狀態</h2>
        {statusError && (
          <p role="alert" className="settings-status__error">
            讀取連線狀態失敗：{statusError}
          </p>
        )}
        {!statusError && !status && <p>讀取中…</p>}
        {status && (
          <dl className="settings-status__list">
            <div className="settings-status__item">
              <dt>API Key</dt>
              <dd>{statusLabel(status.apiKeySet)}</dd>
            </div>
            <div className="settings-status__item">
              <dt>API Secret</dt>
              <dd>{statusLabel(status.apiSecretSet)}</dd>
            </div>
          </dl>
        )}
      </div>

      <form className="settings-form" onSubmit={handleSave}>
        <h2>更新 API 金鑰</h2>
        <div className="settings-form__field">
          <label htmlFor="settings-api-key">API Key</label>
          <input
            id="settings-api-key"
            type="password"
            autoComplete="off"
            required
            value={apiKey}
            onChange={(event) => {
              setApiKey(event.target.value);
              setSaveStatus("idle");
            }}
          />
        </div>
        <div className="settings-form__field">
          <label htmlFor="settings-api-secret">API Secret</label>
          <input
            id="settings-api-secret"
            type="password"
            autoComplete="off"
            required
            value={apiSecret}
            onChange={(event) => {
              setApiSecret(event.target.value);
              setSaveStatus("idle");
            }}
          />
        </div>
        {saveStatus === "error" && saveError && (
          <p role="alert" className="settings-form__error">
            儲存失敗：{saveError}
          </p>
        )}
        {saveStatus === "success" && (
          <p role="status" className="settings-form__success">
            已儲存。
          </p>
        )}
        <button type="submit" className="settings-form__save" disabled={saveStatus === "saving"}>
          {saveStatus === "saving" ? "儲存中…" : "儲存"}
        </button>
      </form>

      <div className="settings-clear">
        {clearStatus === "error" && clearError && (
          <p role="alert" className="settings-form__error">
            清除失敗：{clearError}
          </p>
        )}
        <button
          type="button"
          className="settings-clear__button"
          onClick={handleClear}
          disabled={clearStatus === "clearing"}
        >
          {clearStatus === "clearing" ? "清除中…" : "清除已儲存的金鑰"}
        </button>
      </div>
    </section>
  );
}
