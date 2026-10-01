import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyInfo } from "./strategyTypes";
import type { BacktestSummary } from "./backtestTypes";
import { CompareChart } from "./CompareChart";
import { colorForIndex } from "./compareColors";
import { formatDrawdown, formatSharpe, formatSignedPercent, signClass } from "./backtestFormat";
import { generateInsight, trailing12MonthsReturn, yearlyReturn, yearsInRuns } from "./compareMetrics";

/** `savedBacktests` 裡要抓同一段 BTC 買入持有基準曲線的那幾個（interval/year/month）區間。 */
function distinctBaselinePeriods(
  runs: BacktestSummary[],
): { interval: string; year: number; month: number; startingCapital: string }[] {
  const seen = new Map<string, { interval: string; year: number; month: number; startingCapital: string }>();
  for (const run of runs) {
    const key = `${run.interval}|${run.year}|${run.month}`;
    if (!seen.has(key)) {
      seen.set(key, {
        interval: run.interval,
        year: run.year,
        month: run.month,
        startingCapital: run.startingCapital,
      });
    }
  }
  return [...seen.values()];
}

function periodLabel(period: { year: number; month: number }): string {
  return `${period.year}/${String(period.month).padStart(2, "0")}`;
}

interface CompareProps {
  savedBacktests: BacktestSummary[];
  onRemove: (index: number) => void;
  onGoToBacktest: () => void;
}

// 策略庫（3.4/3.5）跟回測頁（3.6）都各自呼叫過一次 list_builtin_strategies 取得
// 參數 label；這裡沒有另外做一個共用快取，跟前兩步同一個判斷——這個呼叫是本機
// 讀取、沒有下載，成本很低，還沒遇到需要共用快取的效能問題。
function paramSummaryFor(summary: BacktestSummary, strategies: StrategyInfo[] | null): string {
  const strategy = strategies?.find((s) => s.id === summary.strategyId);
  if (!strategy) {
    return Object.entries(summary.params)
      .map(([key, value]) => `${key}${value}`)
      .join("、");
  }
  return strategy.params
    .map((param) => `${param.label}${summary.params[param.key] ?? ""}`)
    .join("、");
}

function dataRangeFor(summary: BacktestSummary): string {
  return `${summary.symbol} · ${summary.interval} · ${summary.year}/${String(summary.month).padStart(2, "0")}`;
}

/** `number | null`（compareMetrics 的回傳）轉成 `formatSignedPercent` 吃的 `string | null`。 */
function fmtRatioCell(value: number | null): string {
  return formatSignedPercent(value === null ? null : String(value));
}

export function Compare({ savedBacktests, onRemove, onGoToBacktest }: CompareProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  // key = `${interval}|${year}|${month}`，value 是抓回來的基準曲線；抓失敗的期間
  // 不記錄任何值（維持 undefined），對應的基準列就不顯示——沒有資料就不畫，不
  // 硬湊一列假數字。
  const [baselines, setBaselines] = useState<Map<string, BacktestSummary>>(new Map());

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  const periods = useMemo(() => distinctBaselinePeriods(savedBacktests), [savedBacktests]);

  useEffect(() => {
    for (const period of periods) {
      const key = `${period.interval}|${period.year}|${period.month}`;
      if (baselines.has(key)) continue;
      invoke<BacktestSummary>("run_buy_hold_baseline_command", { request: period })
        .then((summary) => {
          setBaselines((prev) => new Map(prev).set(key, summary));
        })
        .catch(() => {
          // 抓不到（例如離線、下載失敗）就不顯示這段期間的基準列，不假造資料。
        });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- baselines 只用來判斷「已經抓過」，放進依賴會造成無限重抓
  }, [periods]);

  const years = useMemo(() => yearsInRuns(savedBacktests), [savedBacktests]);
  const insight = useMemo(
    () => generateInsight(savedBacktests, strategies),
    [savedBacktests, strategies],
  );
  const baselineRows = periods
    .map((period) => baselines.get(`${period.interval}|${period.year}|${period.month}`))
    .filter((summary): summary is BacktestSummary => summary !== undefined);

  if (savedBacktests.length === 0) {
    return (
      <div role="alert" className="compare-empty">
        <p>還沒有加入任何回測。先到回測頁跑一次回測，再把結果加入比較。</p>
        <button type="button" onClick={onGoToBacktest}>
          前往回測
        </button>
      </div>
    );
  }

  return (
    <div className="compare">
      <ul className="compare-legend">
        {savedBacktests.map((summary, index) => (
          <li key={index} className="compare-legend__item">
            <span
              className="compare-legend__swatch"
              style={{ background: colorForIndex(index) }}
              aria-hidden="true"
            />
            {summary.strategyName}（{dataRangeFor(summary)}）
          </li>
        ))}
      </ul>

      <section aria-label="累積報酬疊圖" className="compare-chart-section">
        <CompareChart runs={savedBacktests} />
      </section>

      {insight.kind !== "none" && (
        <section aria-label="關鍵發現摘要" className="compare-insight">
          {insight.kind === "single-param-diff" ? (
            <p>{insight.text}</p>
          ) : (
            <p className="compare-insight__muted">設定差異較多，無法自動摘要。</p>
          )}
        </section>
      )}

      <table className="compare-table">
        <caption className="compare-table__caption">指標比較</caption>
        <thead>
          <tr>
            <th scope="col" aria-label="顏色"></th>
            <th scope="col">策略・參數</th>
            <th scope="col">資料範圍</th>
            <th scope="col">總報酬</th>
            <th scope="col">年化</th>
            <th scope="col">最大回撤</th>
            <th scope="col">夏普</th>
            {years.map((year) => (
              <th scope="col" key={year}>
                {year}
              </th>
            ))}
            <th scope="col">近12個月</th>
            <th scope="col">交易</th>
            <th scope="col" aria-label="移除"></th>
          </tr>
        </thead>
        <tbody>
          {savedBacktests.map((summary, index) => (
            <tr key={index}>
              <td>
                <span
                  className="compare-legend__swatch"
                  style={{ background: colorForIndex(index) }}
                  aria-hidden="true"
                />
              </td>
              <td>
                <div className="compare-table__strategy">
                  <span className="compare-table__strategy-name">{summary.strategyName}</span>
                  <span className="compare-table__strategy-params">
                    {paramSummaryFor(summary, strategies)}
                  </span>
                </div>
              </td>
              <td>{dataRangeFor(summary)}</td>
              <td className={signClass(summary.totalReturn)}>
                {formatSignedPercent(summary.totalReturn)}
              </td>
              <td className={signClass(summary.annualizedReturn)}>
                {formatSignedPercent(summary.annualizedReturn)}
              </td>
              <td className="backtest-metrics__value--negative">
                {formatDrawdown(summary.maxDrawdown)}
              </td>
              <td>{formatSharpe(summary.sharpe)}</td>
              {years.map((year) => (
                <td key={year}>{fmtRatioCell(yearlyReturn(summary, year))}</td>
              ))}
              <td>{fmtRatioCell(trailing12MonthsReturn(summary))}</td>
              <td>{summary.trades}</td>
              <td>
                <button
                  type="button"
                  className="compare-table__remove"
                  aria-label={`從比較中移除 ${summary.strategyName}（${dataRangeFor(summary)}）`}
                  onClick={() => onRemove(index)}
                >
                  移除
                </button>
              </td>
            </tr>
          ))}
          {baselineRows.map((summary, index) => (
            <tr key={`baseline-${index}`} className="compare-table__baseline-row">
              <td aria-hidden="true"></td>
              <td>
                <div className="compare-table__strategy">
                  <span className="compare-table__strategy-name">
                    {baselineRows.length > 1
                      ? `${summary.strategyName}（${periodLabel(summary)}）`
                      : summary.strategyName}
                  </span>
                  <span className="compare-table__strategy-params">基準</span>
                </div>
              </td>
              <td>{dataRangeFor(summary)}</td>
              <td className={signClass(summary.totalReturn)}>
                {formatSignedPercent(summary.totalReturn)}
              </td>
              <td className={signClass(summary.annualizedReturn)}>
                {formatSignedPercent(summary.annualizedReturn)}
              </td>
              <td className="backtest-metrics__value--negative">
                {formatDrawdown(summary.maxDrawdown)}
              </td>
              <td>{formatSharpe(summary.sharpe)}</td>
              {years.map((year) => (
                <td key={year}>{fmtRatioCell(yearlyReturn(summary, year))}</td>
              ))}
              <td>{fmtRatioCell(trailing12MonthsReturn(summary))}</td>
              <td>—</td>
              <td></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
