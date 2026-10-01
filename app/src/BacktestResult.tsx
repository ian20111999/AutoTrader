import type { BacktestSummary } from "./backtestTypes";
import { EquityCurveChart } from "./EquityCurveChart";
import { CompareChart } from "./CompareChart";
import { colorForIndex } from "./compareColors";
import { YearlyReturnsTable } from "./YearlyReturnsTable";
import { HealthCheckSummary } from "./HealthCheckSummary";
import {
  formatDrawdown,
  formatPercentMagnitude,
  formatSharpe,
  formatSignedPercent,
  signClass,
} from "./backtestFormat";

interface BacktestResultProps {
  summary: BacktestSummary;
  /** BTC 買入持有基準（跟這筆回測同一個 interval/year/month/startingCapital）。
   * 還沒抓到（loading 中或抓失敗）就是 `null`——不畫 vs BTC 的疊圖，不假造資料。 */
  baseline?: BacktestSummary | null;
}

export function BacktestResult({ summary, baseline = null }: BacktestResultProps) {
  return (
    <section aria-label={`回測結果：${summary.symbol}`} className="backtest-result">
      <EquityCurveChart curve={summary.curve} startingCapital={summary.startingCapital} />

      {baseline && (
        <div className="backtest-result__baseline">
          <ul className="compare-legend">
            <li className="compare-legend__item">
              <span
                className="compare-legend__swatch"
                style={{ background: colorForIndex(0) }}
                aria-hidden="true"
              />
              {summary.strategyName}
            </li>
            <li className="compare-legend__item">
              <span
                className="compare-legend__swatch"
                style={{ background: colorForIndex(1) }}
                aria-hidden="true"
              />
              {baseline.strategyName}
            </li>
          </ul>
          <CompareChart runs={[summary, baseline]} />
        </div>
      )}

      <YearlyReturnsTable summary={summary} baseline={baseline} />

      <HealthCheckSummary summary={summary} />

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
