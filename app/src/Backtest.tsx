import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import type { BacktestRequest, BacktestSummary } from "./backtestTypes";
import { INTERVAL_OPTIONS } from "./backtestTypes";
import { BacktestResult } from "./BacktestResult";

interface BacktestProps {
  strategyConfig: StrategyConfig | null;
  onGoToStrategies: () => void;
}

function symbolError(raw: string): string | null {
  return raw.trim() === "" ? "請輸入交易對代號" : null;
}

function monthError(raw: string): string | null {
  return /^\d{4}-\d{2}$/.test(raw) ? null : "請選擇年月";
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

type Status = "idle" | "loading" | "error" | "success";

export function Backtest({ strategyConfig, onGoToStrategies }: BacktestProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [symbol, setSymbol] = useState("BTCUSDT");
  const [interval, setInterval] = useState<string>("1d");
  const [month, setMonth] = useState("2024-01");
  const [startingCapital, setStartingCapital] = useState("10000");
  const [submitted, setSubmitted] = useState(false);
  const [status, setStatus] = useState<Status>("idle");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [summary, setSummary] = useState<BacktestSummary | null>(null);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  const selectedStrategy =
    strategyConfig && strategies
      ? (strategies.find((s) => s.id === strategyConfig.strategyId) ?? null)
      : null;

  const fieldErrors = {
    symbol: symbolError(symbol),
    month: monthError(month),
    startingCapital: capitalError(startingCapital),
  };
  const hasFieldError = Object.values(fieldErrors).some((message) => message !== null);

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasFieldError || !strategyConfig) return;

    const [yearStr, monthStr] = month.split("-");
    const request: BacktestRequest = {
      symbol: symbol.trim(),
      interval,
      year: Number(yearStr),
      month: Number(monthStr),
      strategyId: strategyConfig.strategyId,
      params: strategyConfig.values,
      startingCapital: startingCapital.trim(),
    };

    setStatus("loading");
    setErrorMessage(null);
    setSummary(null);
    try {
      const result = await invoke<BacktestSummary>("run_backtest_command", { request });
      setSummary(result);
      setStatus("success");
    } catch (err) {
      setErrorMessage(String(err));
      setStatus("error");
    }
  }

  return (
    <div className="backtest">
      <form className="backtest-form" onSubmit={handleSubmit} noValidate>
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

        <fieldset className="backtest-form__fieldset" disabled={status === "loading"}>
          <legend className="backtest-form__label">資料範圍</legend>

          <div className="backtest-form__field">
            <label htmlFor="backtest-symbol">交易對</label>
            <input
              id="backtest-symbol"
              type="text"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value)}
              aria-invalid={submitted && fieldErrors.symbol ? true : undefined}
              aria-describedby={submitted && fieldErrors.symbol ? "backtest-symbol-error" : undefined}
            />
            {submitted && fieldErrors.symbol && (
              <p id="backtest-symbol-error" role="alert" className="backtest-form__error">
                {fieldErrors.symbol}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="backtest-interval">週期</label>
            <select
              id="backtest-interval"
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
            <label htmlFor="backtest-month">年月</label>
            <input
              id="backtest-month"
              type="month"
              value={month}
              onChange={(e) => setMonth(e.target.value)}
              aria-invalid={submitted && fieldErrors.month ? true : undefined}
              aria-describedby={submitted && fieldErrors.month ? "backtest-month-error" : undefined}
            />
            {submitted && fieldErrors.month && (
              <p id="backtest-month-error" role="alert" className="backtest-form__error">
                {fieldErrors.month}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="backtest-capital">起始資金</label>
            <input
              id="backtest-capital"
              type="text"
              inputMode="decimal"
              value={startingCapital}
              onChange={(e) => setStartingCapital(e.target.value)}
              aria-invalid={submitted && fieldErrors.startingCapital ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.startingCapital ? "backtest-capital-error" : undefined
              }
            />
            {submitted && fieldErrors.startingCapital && (
              <p id="backtest-capital-error" role="alert" className="backtest-form__error">
                {fieldErrors.startingCapital}
              </p>
            )}
          </div>

          <button type="submit" className="backtest-form__submit">
            {status === "loading" ? "回測中…" : "執行回測"}
          </button>
        </fieldset>
      </form>

      <div className="backtest-main">
        {status === "loading" && (
          <p role="status" className="backtest-status">
            回測中…如果是第一次跑這個月份的資料，會先下載歷史 K 線，可能需要一些時間，請稍候。
          </p>
        )}
        {status === "error" && errorMessage && (
          <p role="alert" className="backtest-status backtest-status--error">
            回測失敗：{errorMessage}
          </p>
        )}
        {status === "success" && summary && <BacktestResult summary={summary} />}
      </div>
    </div>
  );
}
