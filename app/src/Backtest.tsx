import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import type {
  BacktestRequest,
  BacktestSummary,
  Direction,
  Market,
  MarginMode,
} from "./backtestTypes";
import { INTERVAL_OPTIONS, LEVERAGE_WARNING_THRESHOLD } from "./backtestTypes";
import { BacktestResult } from "./BacktestResult";
import { SymbolContributionChart } from "./SymbolContributionChart";
import { ParameterStabilityHeatmap } from "./ParameterStabilityHeatmap";

interface BacktestProps {
  strategyConfig: StrategyConfig | null;
  onGoToStrategies: () => void;
  onAddToCompare: (summary: BacktestSummary) => void;
}

function symbolsError(raw: string): string | null {
  return parseSymbols(raw).length === 0 ? "請至少輸入一個交易對代號" : null;
}

// 逗號分隔多個交易對；每個都各自跑一次回測（3.6 的多幣種選擇）。
function parseSymbols(raw: string): string[] {
  return raw
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== "");
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

function leverageError(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return "請輸入槓桿倍數";
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return "必須是數字";
  if (Number(trimmed) <= 0) return "槓桿倍數必須大於 0";
  return null;
}

function strategySummary(strategy: StrategyInfo, values: Record<string, string>): string {
  return strategy.params.map((param) => `${param.label}${values[param.key] ?? ""}`).join("、");
}

type Status = "idle" | "loading" | "error" | "success";

export function Backtest({ strategyConfig, onGoToStrategies, onAddToCompare }: BacktestProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [symbols, setSymbols] = useState("BTCUSDT");
  const [interval, setInterval] = useState<string>("1d");
  const [month, setMonth] = useState("2024-01");
  const [startingCapital, setStartingCapital] = useState("10000");
  const [market, setMarket] = useState<Market>("spot");
  const [direction, setDirection] = useState<Direction>("long_only");
  const [leverage, setLeverage] = useState("1");
  const [marginMode, setMarginMode] = useState<MarginMode>("isolated");
  const [submitted, setSubmitted] = useState(false);
  const [status, setStatus] = useState<Status>("idle");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [results, setResults] = useState<BacktestSummary[] | null>(null);
  const [addedToCompare, setAddedToCompare] = useState(false);
  const [baseline, setBaseline] = useState<BacktestSummary | null>(null);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  // BTC 買入持有基準（vs BTC 疊圖、逐年表現表格用）：跟 3.7 Compare 頁同一個
  // command，用這批結果共用的 interval/year/month/startingCapital 抓一次就夠
  // （這批結果本來就是同一次表單送出、只有交易對不同）。抓不到（離線、下載
  // 失敗）就維持 null，呼叫端不畫 vs BTC 的部分，不假造資料。
  useEffect(() => {
    if (!results || results.length === 0) return;
    let cancelled = false;
    const first = results[0];
    invoke<BacktestSummary>("run_buy_hold_baseline_command", {
      request: {
        interval: first.interval,
        year: first.year,
        month: first.month,
        startingCapital: first.startingCapital,
      },
    })
      .then((summary) => {
        if (!cancelled) setBaseline(summary);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [results]);

  const isFutures = market === "usdm_perp";

  // 現貨不支援槓桿/做空：切回現貨時把這兩個設定復位，而不是讓表單送出一個
  // 現貨+槓桿的不合法組合（後端 summarize 也會擋，但前端先復位體驗比較好）。
  function handleMarketChange(next: Market) {
    setMarket(next);
    if (next === "spot") {
      setDirection("long_only");
      setLeverage("1");
    }
  }

  const isCustomStrategy = strategyConfig?.strategyId === "custom";
  const selectedStrategy =
    strategyConfig && strategies && !isCustomStrategy
      ? (strategies.find((s) => s.id === strategyConfig.strategyId) ?? null)
      : null;

  const fieldErrors = {
    symbols: symbolsError(symbols),
    month: monthError(month),
    startingCapital: capitalError(startingCapital),
    leverage: leverageError(leverage),
  };
  const hasFieldError = Object.values(fieldErrors).some((message) => message !== null);
  const leverageValue = Number(leverage);
  const overLeverageLimit =
    !fieldErrors.leverage && leverageValue > LEVERAGE_WARNING_THRESHOLD;

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasFieldError || !strategyConfig) return;

    const [yearStr, monthStr] = month.split("-");
    const symbolList = parseSymbols(symbols);

    setStatus("loading");
    setErrorMessage(null);
    setResults(null);
    setBaseline(null);
    setAddedToCompare(false);
    try {
      const runs = await Promise.all(
        symbolList.map((symbol) => {
          const request: BacktestRequest = {
            symbol,
            interval,
            year: Number(yearStr),
            month: Number(monthStr),
            strategyId: strategyConfig.strategyId,
            params: strategyConfig.values,
            startingCapital: startingCapital.trim(),
            market,
            direction,
            leverage: leverage.trim(),
            marginMode: isFutures ? marginMode : null,
            dslJson: isCustomStrategy ? strategyConfig.dslJson ?? null : null,
          };
          return invoke<BacktestSummary>("run_backtest_command", { request });
        }),
      );
      setResults(runs);
      setStatus("success");
    } catch (err) {
      setErrorMessage(String(err));
      setStatus("error");
    }
  }

  function handleAddToCompare() {
    if (!results) return;
    results.forEach(onAddToCompare);
    setAddedToCompare(true);
  }

  return (
    <div className="backtest">
      <form className="backtest-form" onSubmit={handleSubmit} noValidate>
        <div className="backtest-form__field">
          <span className="backtest-form__label">策略</span>
          {strategyConfig ? (
            <p className="backtest-form__strategy">
              {isCustomStrategy
                ? `${strategyConfig.dslName ?? "自訂策略（DSL）"}（積木編輯器）`
                : selectedStrategy
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
            <label htmlFor="backtest-symbol">交易對（可逗號分隔多個）</label>
            <input
              id="backtest-symbol"
              type="text"
              value={symbols}
              onChange={(e) => setSymbols(e.target.value)}
              aria-invalid={submitted && fieldErrors.symbols ? true : undefined}
              aria-describedby={submitted && fieldErrors.symbols ? "backtest-symbol-error" : undefined}
            />
            {submitted && fieldErrors.symbols && (
              <p id="backtest-symbol-error" role="alert" className="backtest-form__error">
                {fieldErrors.symbols}
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
        </fieldset>

        <fieldset className="backtest-form__fieldset" disabled={status === "loading"}>
          <legend className="backtest-form__label">槓桿與方向</legend>

          <div className="backtest-form__field">
            <label htmlFor="backtest-market">市場</label>
            <select
              id="backtest-market"
              value={market}
              onChange={(e) => handleMarketChange(e.target.value as Market)}
            >
              <option value="spot">現貨</option>
              <option value="usdm_perp">U 本位合約</option>
            </select>
          </div>

          <div className="backtest-form__field">
            <label htmlFor="backtest-direction">方向</label>
            <select
              id="backtest-direction"
              value={direction}
              onChange={(e) => setDirection(e.target.value as Direction)}
              disabled={!isFutures}
            >
              <option value="long_only">只做多</option>
              <option value="long_short">多空</option>
            </select>
          </div>

          <div className="backtest-form__field">
            <label htmlFor="backtest-leverage">槓桿倍數</label>
            <input
              id="backtest-leverage"
              type="range"
              min="1"
              max="20"
              step="0.5"
              value={leverage}
              onChange={(e) => setLeverage(e.target.value)}
              disabled={!isFutures}
              aria-describedby="backtest-leverage-value"
            />
            <span id="backtest-leverage-value" className="backtest-form__range-value">
              {leverage}x
            </span>
            {submitted && fieldErrors.leverage && (
              <p role="alert" className="backtest-form__error">
                {fieldErrors.leverage}
              </p>
            )}
            {overLeverageLimit && (
              <p className="backtest-form__hint backtest-form__hint--warning">
                超過風控建議上限（{LEVERAGE_WARNING_THRESHOLD}x）
              </p>
            )}
          </div>

          {isFutures && (
            <div className="backtest-form__field">
              <label htmlFor="backtest-margin-mode">保證金模式</label>
              <select
                id="backtest-margin-mode"
                value={marginMode}
                onChange={(e) => setMarginMode(e.target.value as MarginMode)}
              >
                <option value="isolated">逐倉</option>
                <option value="cross">全倉</option>
              </select>
            </div>
          )}

          <div className="backtest-form__field backtest-form__toggle">
            <label htmlFor="backtest-real-funding">
              計入真實歷史資金費率
              <span className="backtest-form__hint"> （即將推出）</span>
            </label>
            <input id="backtest-real-funding" type="checkbox" checked={false} disabled readOnly />
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
        {status === "success" && results && (
          <>
            {results.length > 1 && (
              <section aria-label="各幣種對總報酬的貢獻" className="backtest-contribution-section">
                <h2 className="backtest-form__label">各幣種對總報酬的貢獻</h2>
                <SymbolContributionChart results={results} />
              </section>
            )}

            <div className="backtest-results">
              {results.map((summary) => (
                <BacktestResult key={summary.symbol} summary={summary} baseline={baseline} />
              ))}
            </div>

            <div className="backtest-result-actions">
              <button
                type="button"
                className="backtest-form__submit"
                onClick={handleAddToCompare}
                disabled={addedToCompare}
              >
                {addedToCompare ? "已加入比較 ✓" : "加入比較"}
              </button>
            </div>

            {selectedStrategy && strategyConfig && strategyConfig.strategyId !== "custom" && (
              <section aria-label="參數穩定度熱力圖" className="parameter-sweep-section">
                <h2 className="backtest-form__label">參數穩定度</h2>
                <ParameterStabilityHeatmap
                  strategy={selectedStrategy}
                  paramValues={strategyConfig.values}
                  symbol={results[0].symbol}
                  interval={interval}
                  year={Number(month.split("-")[0])}
                  month={Number(month.split("-")[1])}
                  startingCapital={startingCapital.trim()}
                  market={market}
                  direction={direction}
                  leverage={leverage.trim()}
                  marginMode={isFutures ? marginMode : null}
                />
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}
