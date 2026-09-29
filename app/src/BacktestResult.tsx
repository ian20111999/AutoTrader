import type { BacktestSummary } from "./backtestTypes";
import { EquityCurveChart } from "./EquityCurveChart";
import {
  formatDrawdown,
  formatPercentMagnitude,
  formatSharpe,
  formatSignedPercent,
} from "./backtestFormat";

interface BacktestResultProps {
  summary: BacktestSummary;
}

function signClass(raw: string | null): string {
  if (raw === null) return "";
  const value = Number(raw);
  if (value > 0) return "backtest-metrics__value--positive";
  if (value < 0) return "backtest-metrics__value--negative";
  return "";
}

export function BacktestResult({ summary }: BacktestResultProps) {
  return (
    <section aria-label="回測結果" className="backtest-result">
      <EquityCurveChart curve={summary.curve} startingCapital={summary.startingCapital} />

      <dl className="backtest-metrics">
        <div className="backtest-metrics__item">
          <dt>總報酬</dt>
          <dd className={signClass(summary.totalReturn)}>
            {formatSignedPercent(summary.totalReturn)}
          </dd>
        </div>
        <div className="backtest-metrics__item">
          <dt>年化報酬</dt>
          <dd className={signClass(summary.annualizedReturn)}>
            {formatSignedPercent(summary.annualizedReturn)}
          </dd>
        </div>
        <div className="backtest-metrics__item">
          <dt>最大回撤</dt>
          <dd className="backtest-metrics__value--negative">
            {formatDrawdown(summary.maxDrawdown)}
          </dd>
        </div>
        <div className="backtest-metrics__item">
          <dt>夏普值</dt>
          <dd>{formatSharpe(summary.sharpe)}</dd>
        </div>
      </dl>

      <p className="backtest-result__trades">
        交易 {summary.trades} 筆
        {summary.liquidations > 0 ? `，強制平倉 ${summary.liquidations} 次` : ""}
        ・共 {summary.barCount} 根 K 線
      </p>

      {/* feeModel/slippage 是 Rust 端目前寫死的系統預設（見 backtest.rs 的
          DEFAULT_SLIPPAGE 註解），這裡故意用小字＋明講「還不能調整」，不讓使用者
          以為這是他自己設定過的成本。 */}
      <p className="backtest-result__cost-disclosure">
        成本假設（系統目前固定使用，還不能由你調整）：{summary.feeModel}，滑價{" "}
        {formatPercentMagnitude(summary.slippage)}
      </p>
    </section>
  );
}
