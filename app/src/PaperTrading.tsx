import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import type {
  PaperSnapshot,
  PaperTradingStatus,
  PaperUpdateEvent,
  StartPaperTradingRequest,
} from "./paperTradingTypes";
import { PAPER_TRADING_EVENT } from "./paperTradingTypes";
import { INTERVAL_OPTIONS } from "./backtestTypes";
import { EquityCurveChart } from "./EquityCurveChart";

interface PaperTradingProps {
  strategyConfig: StrategyConfig | null;
  onGoToStrategies: () => void;
}

type Phase = "idle" | "running" | "stopped" | "failed";

function symbolError(raw: string): string | null {
  return raw.trim() === "" ? "請輸入交易對代號" : null;
}

function capitalError(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return "請輸入起始資金";
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return "必須是數字";
  if (Number(trimmed) <= 0) return "起始資金必須大於 0";
  return null;
}

function strategySummary(strategy: StrategyInfo, values: Record<string, string>): string {
  return strategy.params.map((param) => `${param.label}${values[param.key] ?? ""}`).join("、");
}

export function PaperTrading({ strategyConfig, onGoToStrategies }: PaperTradingProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [symbol, setSymbol] = useState("BTCUSDT");
  const [interval, setInterval] = useState<string>("1m");
  const [startingCapital, setStartingCapital] = useState("10000");
  const [submitted, setSubmitted] = useState(false);

  const [phase, setPhase] = useState<Phase>("idle");
  const [curve, setCurve] = useState<PaperSnapshot[]>([]);
  const [latest, setLatest] = useState<PaperSnapshot | null>(null);
  const [failedMessage, setFailedMessage] = useState<string | null>(null);

  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  // 頁面掛載/切回來時，補上最後已知狀態，不用等下一個事件才有東西可看。
  // latest_snapshot() 只有「目前狀態」、沒有完整曲線歷史，所以這裡最多只能補回一個點，
  // 曲線會從這一點開始往後累積。
  useEffect(() => {
    invoke<PaperTradingStatus>("paper_trading_status")
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
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<PaperUpdateEvent>(PAPER_TRADING_EVENT, (event) => {
      const payload = event.payload;
      if (payload.type === "bar") {
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
      // catch：卸載時失敗（例如背景視窗已經關閉）沒有補救動作，安靜忽略即可。
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const selectedStrategy =
    strategyConfig && strategies
      ? (strategies.find((s) => s.id === strategyConfig.strategyId) ?? null)
      : null;

  const fieldErrors = {
    symbol: symbolError(symbol),
    startingCapital: capitalError(startingCapital),
  };
  const hasFieldError = Object.values(fieldErrors).some((message) => message !== null);
  const isRunning = phase === "running";

  async function handleStart(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasFieldError || !strategyConfig) return;

    const request: StartPaperTradingRequest = {
      symbol: symbol.trim(),
      interval,
      strategyId: strategyConfig.strategyId,
      params: strategyConfig.values,
      startingCapital: startingCapital.trim(),
    };

    setStarting(true);
    setStartError(null);
    setStopError(null);
    setCurve([]);
    setLatest(null);
    setFailedMessage(null);
    try {
      await invoke("start_paper_trading", { request });
      setPhase("running");
    } catch (err) {
      setStartError(String(err));
    } finally {
      setStarting(false);
    }
  }

  async function handleStop() {
    setStopping(true);
    setStopError(null);
    try {
      await invoke("stop_paper_trading");
    } catch (err) {
      setStopError(String(err));
    } finally {
      setStopping(false);
    }
  }

  return (
    <div className="backtest">
      <form className="backtest-form" onSubmit={handleStart} noValidate>
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
            <label htmlFor="paper-symbol">交易對</label>
            <input
              id="paper-symbol"
              type="text"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value)}
              aria-invalid={submitted && fieldErrors.symbol ? true : undefined}
              aria-describedby={submitted && fieldErrors.symbol ? "paper-symbol-error" : undefined}
            />
            {submitted && fieldErrors.symbol && (
              <p id="paper-symbol-error" role="alert" className="backtest-form__error">
                {fieldErrors.symbol}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="paper-interval">週期</label>
            <select
              id="paper-interval"
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

          <div className="backtest-form__field">
            <label htmlFor="paper-capital">虛擬起始資金</label>
            <input
              id="paper-capital"
              type="text"
              inputMode="decimal"
              value={startingCapital}
              onChange={(e) => setStartingCapital(e.target.value)}
              aria-invalid={submitted && fieldErrors.startingCapital ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.startingCapital ? "paper-capital-error" : undefined
              }
            />
            {submitted && fieldErrors.startingCapital && (
              <p id="paper-capital-error" role="alert" className="backtest-form__error">
                {fieldErrors.startingCapital}
              </p>
            )}
          </div>

          <button type="submit" className="backtest-form__submit">
            {starting ? "啟動中…" : "開始模擬"}
          </button>
        </fieldset>
      </form>

      <div className="backtest-main">
        {statusError && (
          <p role="alert" className="backtest-status backtest-status--error">
            讀取模擬交易狀態失敗：{statusError}
          </p>
        )}
        {startError && (
          <p role="alert" className="backtest-status backtest-status--error">
            開始模擬失敗：{startError}
          </p>
        )}

        {isRunning && (
          <p role="status" className="backtest-status paper-trading-status--running">
            模擬交易執行中，即時行情、模擬成交，不會送出真實訂單。
          </p>
        )}
        {phase === "stopped" && (
          <p role="status" className="backtest-status">
            已停止模擬交易。
          </p>
        )}
        {phase === "failed" && failedMessage && (
          <p role="alert" className="backtest-status backtest-status--error">
            模擬交易中止：{failedMessage}（帳本已經不可信，請重新開始）
          </p>
        )}

        {latest && (
          <section aria-label="模擬交易結果" className="backtest-result">
            <EquityCurveChart curve={curve} startingCapital={startingCapital} />
            <dl className="paper-trading-stats">
              <div className="backtest-metrics__item">
                <dt>模擬權益</dt>
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
                <dt>成交筆數</dt>
                <dd>{latest.trades}</dd>
              </div>
              <div className="backtest-metrics__item">
                <dt>強制平倉次數</dt>
                <dd>{latest.liquidations}</dd>
              </div>
            </dl>

            {stopError && (
              <p role="alert" className="backtest-status backtest-status--error">
                停止失敗：{stopError}
              </p>
            )}
            {isRunning && (
              <button
                type="button"
                className="settings-clear__button paper-trading-stop"
                onClick={handleStop}
                disabled={stopping}
              >
                {stopping ? "停止中…" : "停止模擬"}
              </button>
            )}
          </section>
        )}
      </div>
    </div>
  );
}
