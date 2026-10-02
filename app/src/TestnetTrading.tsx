import { useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import type {
  OrderOutcome,
  StartTestnetTradingRequest,
  TestnetSnapshot,
  TestnetTradingStatus,
  TestnetUpdateEnvelope,
} from "./testnetTradingTypes";
import { TESTNET_TRADING_EVENT } from "./testnetTradingTypes";
import type { TestnetCredentialStatus } from "./testnetSettingsTypes";
import { INTERVAL_OPTIONS } from "./backtestTypes";
import { EquityCurveChart } from "./EquityCurveChart";

interface TestnetTradingProps {
  strategyConfig: StrategyConfig | null;
  onGoToStrategies: () => void;
}

type Phase = "idle" | "running" | "stopped" | "failed";
type SaveStatus = "idle" | "saving" | "success" | "error";
type ClearStatus = "idle" | "clearing" | "error";

// 理由同 PaperTrading.tsx：畫面只看「現在這一場」，但 command 介面改成
// 多 session 之後每個 command 都要帶 sessionId，存進 localStorage 讓
// 重新整理/切回這一頁時還能查到同一場的狀態。
const SESSION_ID_STORAGE_KEY = "testnetTrading.sessionId";

function statusLabel(isSet: boolean): string {
  return isSet ? "已設定" : "未設定";
}

function symbolError(raw: string): string | null {
  return raw.trim() === "" ? "請輸入交易對代號" : null;
}

// 起始資金／每日最大虧損／單筆最大下單金額共用同一組驗證規則：
// 必須是大於 0 的數字（跟 PaperTrading.tsx 的 capitalError 同一個理由）。
function positiveAmountError(raw: string, label: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return `請輸入${label}`;
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return "必須是數字";
  if (Number(trimmed) <= 0) return `${label}必須大於 0`;
  return null;
}

function strategySummary(strategy: StrategyInfo, values: Record<string, string>): string {
  return strategy.params.map((param) => `${param.label}${values[param.key] ?? ""}`).join("、");
}

// 下單結果換成一行可以直接顯示的中文訊息；Blocked/Invalid 的訊息已經是
// Rust 端組好的完整中文句子（見 testnet_trading.rs 的 OrderOutcomeDto），
// 這裡只需要處理 Filled 的欄位組裝。
function orderOutcomeMessage(outcome: OrderOutcome): string {
  switch (outcome.type) {
    case "blocked":
    case "globallyBlocked":
    case "invalid":
      return outcome.message;
    case "filled":
      return `送出 ${outcome.side === "Buy" ? "買進" : "賣出"} ${outcome.requestedQty}，成交 ${outcome.executedQty}（${outcome.status}），手續費 ${outcome.fee}`;
  }
}

export function TestnetTrading({ strategyConfig, onGoToStrategies }: TestnetTradingProps) {
  // ---- 測試網 API 金鑰（6.1 的 Keychain，跟 4.6 正式環境金鑰分開兩組）----
  const [credStatus, setCredStatus] = useState<TestnetCredentialStatus | null>(null);
  const [credStatusError, setCredStatusError] = useState<string | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [apiSecret, setApiSecret] = useState("");
  const [saveStatus, setSaveStatus] = useState<SaveStatus>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [clearStatus, setClearStatus] = useState<ClearStatus>("idle");
  const [clearError, setClearError] = useState<string | null>(null);

  // ---- 交易設定 ----
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [symbol, setSymbol] = useState("BTCUSDT");
  const [interval, setInterval] = useState<string>("1m");
  const [initialCash, setInitialCash] = useState("1000");
  const [maxDailyLoss, setMaxDailyLoss] = useState("50");
  const [maxOrderNotional, setMaxOrderNotional] = useState("200");
  const [submitted, setSubmitted] = useState(false);

  // ---- 執行狀態 ----
  const [sessionId, setSessionId] = useState<string | null>(() =>
    localStorage.getItem(SESSION_ID_STORAGE_KEY),
  );
  const sessionIdRef = useRef(sessionId);
  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  const [phase, setPhase] = useState<Phase>("idle");
  const [curve, setCurve] = useState<TestnetSnapshot[]>([]);
  const [latest, setLatest] = useState<TestnetSnapshot | null>(null);
  const [failedMessage, setFailedMessage] = useState<string | null>(null);
  const [lastOrder, setLastOrder] = useState<OrderOutcome | null>(null);

  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [killSwitchBusy, setKillSwitchBusy] = useState(false);
  const [killSwitchError, setKillSwitchError] = useState<string | null>(null);

  async function loadCredStatus() {
    try {
      const result = await invoke<TestnetCredentialStatus>("testnet_credentials_status");
      setCredStatus(result);
      setCredStatusError(null);
    } catch (err) {
      setCredStatusError(String(err));
    }
  }

  useEffect(() => {
    invoke<TestnetCredentialStatus>("testnet_credentials_status")
      .then((result) => {
        setCredStatus(result);
        setCredStatusError(null);
      })
      .catch((err: unknown) => setCredStatusError(String(err)));
  }, []);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  // 頁面掛載/切回來時，補上最後已知狀態，理由同 PaperTrading.tsx。
  useEffect(() => {
    if (!sessionId) return;
    invoke<TestnetTradingStatus>("testnet_trading_status", { sessionId })
      .then((status) => {
        if (status.status === "idle") return;
        setPhase(status.status);
        if (status.snapshot) {
          setLatest(status.snapshot);
          setCurve([status.snapshot]);
        }
        if (status.status === "failed") {
          setFailedMessage(status.message);
        }
      })
      .catch((err: unknown) => setStatusError(String(err)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<TestnetUpdateEnvelope>(TESTNET_TRADING_EVENT, (event) => {
      const payload = event.payload;
      if (payload.sessionId !== sessionIdRef.current) return;
      if (payload.type === "order") {
        setLastOrder(payload.outcome);
      } else if (payload.type === "bar") {
        setLatest(payload.snapshot);
        setCurve((prev) => [...prev, payload.snapshot]);
      } else if (payload.type === "stopped") {
        setPhase("stopped");
      } else {
        setPhase("failed");
        setFailedMessage(payload.message);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const selectedStrategy =
    strategyConfig && strategies
      ? (strategies.find((s) => s.id === strategyConfig.strategyId) ?? null)
      : null;

  const fieldErrors = {
    symbol: symbolError(symbol),
    initialCash: positiveAmountError(initialCash, "起始資金"),
    maxDailyLoss: positiveAmountError(maxDailyLoss, "每日最大虧損"),
    maxOrderNotional: positiveAmountError(maxOrderNotional, "單筆最大下單金額"),
  };
  const hasFieldError = Object.values(fieldErrors).some((message) => message !== null);
  const isRunning = phase === "running";
  const killSwitchOn = latest?.killSwitch ?? false;

  async function handleSaveCredentials(event: FormEvent) {
    event.preventDefault();
    setSaveStatus("saving");
    setSaveError(null);
    try {
      // apiKey/apiSecret 只在這裡送出去存進 Keychain，不寫進任何 log（比照 Settings.tsx）。
      await invoke("save_testnet_credentials", { apiKey, apiSecret });
      setApiKey("");
      setApiSecret("");
      setSaveStatus("success");
      await loadCredStatus();
    } catch (err) {
      setSaveError(String(err));
      setSaveStatus("error");
    }
  }

  async function handleClearCredentials() {
    if (!window.confirm("確定要清除已儲存的測試網 API 金鑰嗎？此動作無法復原。")) {
      return;
    }
    setClearStatus("clearing");
    setClearError(null);
    try {
      await invoke("clear_testnet_credentials");
      setClearStatus("idle");
      await loadCredStatus();
    } catch (err) {
      setClearError(String(err));
      setClearStatus("error");
    }
  }

  async function handleStart(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasFieldError || !strategyConfig) return;

    const request: StartTestnetTradingRequest = {
      symbol: symbol.trim(),
      interval,
      strategyId: strategyConfig.strategyId,
      params: strategyConfig.values,
      initialCash: initialCash.trim(),
      maxDailyLoss: maxDailyLoss.trim(),
      maxOrderNotional: maxOrderNotional.trim(),
    };

    setStarting(true);
    setStartError(null);
    setStopError(null);
    setKillSwitchError(null);
    setCurve([]);
    setLatest(null);
    setFailedMessage(null);
    setLastOrder(null);
    try {
      const newSessionId = await invoke<string>("start_testnet_trading", { request });
      localStorage.setItem(SESSION_ID_STORAGE_KEY, newSessionId);
      setSessionId(newSessionId);
      setPhase("running");
    } catch (err) {
      setStartError(String(err));
    } finally {
      setStarting(false);
    }
  }

  async function handleStop() {
    if (!sessionId) return;
    setStopping(true);
    setStopError(null);
    try {
      await invoke("stop_testnet_trading", { sessionId });
    } catch (err) {
      setStopError(String(err));
    } finally {
      setStopping(false);
    }
  }

  async function handleToggleKillSwitch() {
    if (!sessionId) return;
    setKillSwitchBusy(true);
    setKillSwitchError(null);
    try {
      await invoke("set_testnet_kill_switch", { sessionId, on: !killSwitchOn });
    } catch (err) {
      setKillSwitchError(String(err));
    } finally {
      setKillSwitchBusy(false);
    }
  }

  return (
    <div className="backtest">
      <form className="backtest-form" onSubmit={handleStart} noValidate>
        <p role="alert" className="testnet-warning">
          這個頁面會對 Binance 測試網（Demo Trading）送出真實訂單，不是模擬交易。
          測試網資產沒有實際價值，但下單、成交、風控都是真的在跑。
        </p>

        <section className="settings-status" aria-label="測試網連線狀態">
          <h2>測試網 API 金鑰</h2>
          {credStatusError && (
            <p role="alert" className="settings-status__error">
              讀取測試網連線狀態失敗：{credStatusError}
            </p>
          )}
          {!credStatusError && !credStatus && <p>讀取中…</p>}
          {credStatus && (
            <dl className="settings-status__list">
              <div className="settings-status__item">
                <dt>API Key</dt>
                <dd>{statusLabel(credStatus.apiKeySet)}</dd>
              </div>
              <div className="settings-status__item">
                <dt>API Secret</dt>
                <dd>{statusLabel(credStatus.apiSecretSet)}</dd>
              </div>
            </dl>
          )}
          <form className="settings-form" onSubmit={handleSaveCredentials}>
            <div className="settings-form__field">
              <label htmlFor="testnet-api-key">測試網 API Key</label>
              <input
                id="testnet-api-key"
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
              <label htmlFor="testnet-api-secret">測試網 API Secret</label>
              <input
                id="testnet-api-secret"
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
            <button
              type="submit"
              className="settings-form__save"
              disabled={saveStatus === "saving"}
            >
              {saveStatus === "saving" ? "儲存中…" : "儲存測試網金鑰"}
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
              onClick={handleClearCredentials}
              disabled={clearStatus === "clearing"}
            >
              {clearStatus === "clearing" ? "清除中…" : "清除測試網金鑰"}
            </button>
          </div>
        </section>

        <div className="backtest-form__field">
          <span className="backtest-form__label">策略</span>
          {strategyConfig ? (
            <p className="backtest-form__strategy">
              {selectedStrategy
                ? `${selectedStrategy.name}：${strategySummary(selectedStrategy, strategyConfig.values)}`
                : "讀取策略資料中…"}
            </p>
          ) : (
            <div role="alert" className="backtest-form__no-strategy">
              <p>還沒有選擇策略，請先到策略庫選一個策略並調整參數。</p>
              <button type="button" onClick={onGoToStrategies}>
                前往策略庫
              </button>
            </div>
          )}
        </div>

        <fieldset className="backtest-form__fieldset" disabled={isRunning || starting}>
          <legend className="backtest-form__label">即時行情</legend>

          <div className="backtest-form__field">
            <label htmlFor="testnet-symbol">交易對</label>
            <input
              id="testnet-symbol"
              type="text"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value)}
              aria-invalid={submitted && fieldErrors.symbol ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.symbol ? "testnet-symbol-error" : undefined
              }
            />
            {submitted && fieldErrors.symbol && (
              <p id="testnet-symbol-error" role="alert" className="backtest-form__error">
                {fieldErrors.symbol}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="testnet-interval">週期</label>
            <select
              id="testnet-interval"
              value={interval}
              onChange={(e) => setInterval(e.target.value)}
            >
              {INTERVAL_OPTIONS.map((option) => (
                <option key={option} value={option}>
                  {option}
                </option>
              ))}
            </select>
          </div>
        </fieldset>

        <fieldset className="backtest-form__fieldset" disabled={isRunning || starting}>
          <legend className="backtest-form__label">風控設定</legend>

          <div className="backtest-form__field">
            <label htmlFor="testnet-initial-cash">起始資金</label>
            <input
              id="testnet-initial-cash"
              type="text"
              inputMode="decimal"
              value={initialCash}
              onChange={(e) => setInitialCash(e.target.value)}
              aria-invalid={submitted && fieldErrors.initialCash ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.initialCash ? "testnet-initial-cash-error" : undefined
              }
            />
            {submitted && fieldErrors.initialCash && (
              <p id="testnet-initial-cash-error" role="alert" className="backtest-form__error">
                {fieldErrors.initialCash}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="testnet-max-daily-loss">每日最大虧損</label>
            <input
              id="testnet-max-daily-loss"
              type="text"
              inputMode="decimal"
              value={maxDailyLoss}
              onChange={(e) => setMaxDailyLoss(e.target.value)}
              aria-invalid={submitted && fieldErrors.maxDailyLoss ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.maxDailyLoss ? "testnet-max-daily-loss-error" : undefined
              }
            />
            {submitted && fieldErrors.maxDailyLoss && (
              <p id="testnet-max-daily-loss-error" role="alert" className="backtest-form__error">
                {fieldErrors.maxDailyLoss}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="testnet-max-order-notional">單筆最大下單金額</label>
            <input
              id="testnet-max-order-notional"
              type="text"
              inputMode="decimal"
              value={maxOrderNotional}
              onChange={(e) => setMaxOrderNotional(e.target.value)}
              aria-invalid={submitted && fieldErrors.maxOrderNotional ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.maxOrderNotional
                  ? "testnet-max-order-notional-error"
                  : undefined
              }
            />
            {submitted && fieldErrors.maxOrderNotional && (
              <p
                id="testnet-max-order-notional-error"
                role="alert"
                className="backtest-form__error"
              >
                {fieldErrors.maxOrderNotional}
              </p>
            )}
          </div>

          <button type="submit" className="backtest-form__submit">
            {starting ? "啟動中…" : "開始測試網交易"}
          </button>
        </fieldset>
      </form>

      <div className="backtest-main">
        {statusError && (
          <p role="alert" className="backtest-status backtest-status--error">
            讀取測試網交易狀態失敗：{statusError}
          </p>
        )}
        {startError && (
          <p role="alert" className="backtest-status backtest-status--error">
            開始測試網交易失敗：{startError}
          </p>
        )}

        {isRunning && (
          <p role="status" className="backtest-status paper-trading-status--running">
            測試網交易執行中，會真的對測試網送出訂單。
          </p>
        )}
        {phase === "stopped" && (
          <p role="status" className="backtest-status">
            已停止測試網交易。
          </p>
        )}
        {phase === "failed" && failedMessage && (
          <p role="alert" className="backtest-status backtest-status--error">
            測試網交易中止：{failedMessage}（帳本已經不可信，請重新開始）
          </p>
        )}
        {lastOrder && (
          <p role="status" className="testnet-trading-last-order">
            最近一次下單：{orderOutcomeMessage(lastOrder)}
          </p>
        )}

        {latest && (
          <section aria-label="測試網交易結果" className="backtest-result">
            <EquityCurveChart curve={curve} startingCapital={initialCash} />
            <dl className="paper-trading-stats">
              <div className="backtest-metrics__item">
                <dt>權益</dt>
                <dd>{latest.equity}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>現金</dt>
                <dd>{latest.cash}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>部位</dt>
                <dd>{latest.position}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>累計手續費</dt>
                <dd>{latest.feesPaid}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>成交筆數</dt>
                <dd>{latest.fills}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>被風控擋下次數</dt>
                <dd>{latest.blocked}</dd>
              </div>
            </dl>

            <div className="testnet-trading-controls">
              {killSwitchError && (
                <p role="alert" className="backtest-status backtest-status--error">
                  一鍵停止操作失敗：{killSwitchError}
                </p>
              )}
              <div className="testnet-trading-killswitch">
                <p className="testnet-trading-killswitch__hint">
                  一鍵停止：暫停送出新訂單，行情與帳本繼續更新，隨時可以恢復。
                </p>
                <button
                  type="button"
                  className="testnet-trading-killswitch__button"
                  onClick={handleToggleKillSwitch}
                  disabled={killSwitchBusy || !isRunning}
                  aria-pressed={killSwitchOn}
                >
                  {killSwitchOn
                    ? killSwitchBusy
                      ? "恢復中…"
                      : "恢復送單"
                    : killSwitchBusy
                      ? "停止中…"
                      : "一鍵停止送單"}
                </button>
              </div>

              {stopError && (
                <p role="alert" className="backtest-status backtest-status--error">
                  停止失敗：{stopError}
                </p>
              )}
              {isRunning && (
                <div className="testnet-trading-stop">
                  <p className="testnet-trading-stop__hint">
                    停止：整條交易連線收工，連行情也會一併斷線。
                  </p>
                  <button
                    type="button"
                    className="settings-clear__button paper-trading-stop"
                    onClick={handleStop}
                    disabled={stopping}
                  >
                    {stopping ? "停止中…" : "停止測試網交易"}
                  </button>
                </div>
              )}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
