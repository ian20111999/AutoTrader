import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyInfo } from "./strategyTypes";
import type { BacktestSummary } from "./backtestTypes";
import { CompareChart } from "./CompareChart";
import { colorForIndex } from "./compareColors";
import { formatDrawdown, formatSharpe, formatSignedPercent, signClass } from "./backtestFormat";

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

export function Compare({ savedBacktests, onRemove, onGoToBacktest }: CompareProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

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
        </tbody>
      </table>
    </div>
  );
}
