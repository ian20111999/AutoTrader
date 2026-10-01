// 健檢摘要（Phase G）：列出 healthCheck.ts 算出的每一條規則，良好/注意/資料不足
// 都顯示，不是只秀警示——讓使用者看得到判斷依據（門檻值），而不是一個看起來
// 很聰明但其實是黑盒子的結論。
import type { BacktestSummary } from "./backtestTypes";
import { runHealthCheck } from "./healthCheck";
import type { HealthCheckStatus } from "./healthCheck";

const STATUS_LABEL: Record<HealthCheckStatus, string> = {
  good: "良好",
  warning: "注意",
  unknown: "資料不足",
};

const STATUS_CLASS: Record<HealthCheckStatus, string> = {
  good: "health-check__status--good",
  warning: "health-check__status--warning",
  unknown: "health-check__status--unknown",
};

interface HealthCheckSummaryProps {
  summary: BacktestSummary;
}

export function HealthCheckSummary({ summary }: HealthCheckSummaryProps) {
  const items = runHealthCheck(summary);

  return (
    <ul className="health-check" aria-label="健檢摘要">
      {items.map((item) => (
        <li key={item.label} className="health-check__item">
          <span className={`health-check__status ${STATUS_CLASS[item.status]}`}>
            {STATUS_LABEL[item.status]}
          </span>
          <span className="health-check__label">{item.label}</span>
          <span className="health-check__detail">{item.detail}</span>
        </li>
      ))}
    </ul>
  );
}
