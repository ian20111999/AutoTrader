// 健檢摘要（Phase G）：只挑 3 條有把握、門檻值寫死在這裡講清楚的規則，不做看起來
// 聰明但其實是湊出來的評分系統。每條規則的判斷依據都直接印在 detail 裡，使用者
// 自己判斷要不要採信——不是只給一個「良好/注意」的結論卻不說為什麼。
import type { BacktestSummary } from "./backtestTypes";

export type HealthCheckStatus = "good" | "warning" | "unknown";

export interface HealthCheckItem {
  label: string;
  status: HealthCheckStatus;
  detail: string;
}

export const MAX_DRAWDOWN_WARNING_THRESHOLD = 0.5; // 最大回撤 > 50%
export const MIN_TRADES_WARNING_THRESHOLD = 5; // 交易次數 < 5 筆

export function runHealthCheck(summary: BacktestSummary): HealthCheckItem[] {
  const maxDrawdown = Number(summary.maxDrawdown);
  const drawdownPct = (maxDrawdown * 100).toFixed(1);
  const drawdownItem: HealthCheckItem = {
    label: "最大回撤",
    status: maxDrawdown > MAX_DRAWDOWN_WARNING_THRESHOLD ? "warning" : "good",
    detail: `門檻：最大回撤 > ${MAX_DRAWDOWN_WARNING_THRESHOLD * 100}% → 注意（目前 ${drawdownPct}%）`,
  };

  const tradesItem: HealthCheckItem = {
    label: "交易次數",
    status: summary.trades < MIN_TRADES_WARNING_THRESHOLD ? "warning" : "good",
    detail: `門檻：交易次數 < ${MIN_TRADES_WARNING_THRESHOLD} 筆 → 注意（樣本太少，結論不可靠；目前 ${summary.trades} 筆）`,
  };

  const sharpeItem: HealthCheckItem =
    summary.sharpe === null
      ? {
          label: "夏普值",
          status: "unknown",
          detail: "資料不足以計算夏普值（回測期間太短，或報酬率標準差為 0）",
        }
      : {
          label: "夏普值",
          status: Number(summary.sharpe) < 0 ? "warning" : "good",
          detail: `門檻：夏普值 < 0 → 注意（風險調整後報酬為負；目前 ${Number(summary.sharpe).toFixed(2)}）`,
        };

  return [drawdownItem, tradesItem, sharpeItem];
}
