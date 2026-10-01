import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

// 4.6 正式環境金鑰（Settings.tsx）跟 6.1 測試網金鑰（TestnetTrading.tsx）
// UI 結構完全一樣，只差 Tauri command 名稱與文案——抽成共用元件，兩邊都
// 呼叫各自既有的三個 command（status/save/clear），各自的 Keychain
// service name 仍然是分開兩組，這個元件本身不是新的真相來源，只是重用
// 畫面骨架。
export interface CredentialStatusLike {
  apiKeySet: boolean;
  apiSecretSet: boolean;
}

type SaveStatus = "idle" | "saving" | "success" | "error";
type ClearStatus = "idle" | "clearing" | "error";

function statusLabel(isSet: boolean): string {
  return isSet ? "已設定" : "未設定";
}

export interface CredentialSectionProps {
  title: string;
  statusCommand: string;
  saveCommand: string;
  clearCommand: string;
  keyLabel: string;
  secretLabel: string;
  idPrefix: string;
  saveButtonLabel: string;
  clearButtonLabel: string;
  confirmClearMessage: string;
}

export function CredentialSection({
  title,
  statusCommand,
  saveCommand,
  clearCommand,
  keyLabel,
  secretLabel,
  idPrefix,
  saveButtonLabel,
  clearButtonLabel,
  confirmClearMessage,
}: CredentialSectionProps) {
  const [status, setStatus] = useState<CredentialStatusLike | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [apiSecret, setApiSecret] = useState("");
  const [saveStatus, setSaveStatus] = useState<SaveStatus>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [clearStatus, setClearStatus] = useState<ClearStatus>("idle");
  const [clearError, setClearError] = useState<string | null>(null);

  async function loadStatus() {
    try {
      const result = await invoke<CredentialStatusLike>(statusCommand);
      setStatus(result);
      setStatusError(null);
    } catch (err) {
      setStatusError(String(err));
    }
  }

  useEffect(() => {
    invoke<CredentialStatusLike>(statusCommand)
      .then((result) => {
        setStatus(result);
        setStatusError(null);
      })
      .catch((err: unknown) => setStatusError(String(err)));
  }, [statusCommand]);

  async function handleSave(event: FormEvent) {
    event.preventDefault();
    setSaveStatus("saving");
    setSaveError(null);
    try {
      // apiKey/apiSecret 只在這裡送出去存進 Keychain，不寫進任何 log。
      await invoke(saveCommand, { apiKey, apiSecret });
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
    if (!window.confirm(confirmClearMessage)) {
      return;
    }
    setClearStatus("clearing");
    setClearError(null);
    try {
      await invoke(clearCommand);
      setClearStatus("idle");
      await loadStatus();
    } catch (err) {
      setClearError(String(err));
      setClearStatus("error");
    }
  }

  const keyId = `${idPrefix}-api-key`;
  const secretId = `${idPrefix}-api-secret`;

  return (
    <section className="settings-status" aria-label={title}>
      <h2>{title}</h2>
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

      <form className="settings-form" onSubmit={handleSave}>
        <div className="settings-form__field">
          <label htmlFor={keyId}>{keyLabel}</label>
          <input
            id={keyId}
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
          <label htmlFor={secretId}>{secretLabel}</label>
          <input
            id={secretId}
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
          {saveStatus === "saving" ? "儲存中…" : saveButtonLabel}
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
          {clearStatus === "clearing" ? "清除中…" : clearButtonLabel}
        </button>
      </div>
    </section>
  );
}
