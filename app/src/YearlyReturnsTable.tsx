// 逐年表現表格（Phase G）：策略年度報酬 vs BTC 買入持有。重用 Compare 頁
// （3.7 compareMetrics.ts）的 yearsInRuns/yearlyReturn，不重寫一份逐年切割邏輯。
// 欄位依這筆回測實際涵蓋的曆年動態產生，不是寫死 2024/2025/2026。
import type { BacktestSummary } from "./backtestTypes";
import { yearlyReturn, yearsInRuns } from "./compareMetrics";
import { formatSignedPercent } from "./backtestFormat";

interface YearlyReturnsTableProps {
  summary: BacktestSummary;
  baseline: BacktestSummary | null;
}

function fmtCell(value: number | null): string {
  return formatSignedPercent(value === null ? null : String(value));
}

export function YearlyReturnsTable({ summary, baseline }: YearlyReturnsTableProps) {
  const runs = baseline ? [summary, baseline] : [summary];
  const years = yearsInRuns(runs);

  if (years.length === 0) return null;

  return (
    <table className="backtest-yearly-table">
      <caption className="backtest-yearly-table__caption">逐年表現</caption>
      <thead>
        <tr>
          <th scope="col"></th>
          {years.map((year) => (
            <th scope="col" key={year}>
              {year}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row">策略</th>
          {years.map((year) => (
            <td key={year}>{fmtCell(yearlyReturn(summary, year))}</td>
          ))}
        </tr>
        {baseline && (
          <tr>
            <th scope="row">BTC 買入持有</th>
            {years.map((year) => (
              <td key={year}>{fmtCell(yearlyReturn(baseline, year))}</td>
            ))}
          </tr>
        )}
      </tbody>
    </table>
  );
}
