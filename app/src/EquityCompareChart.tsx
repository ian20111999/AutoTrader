import type { EquityPoint } from "./backtestTypes";

// ponytail: 跟 EquityCurveChart/OverviewEquityChart 同一招，手刻 SVG，不換圖表庫。
// 兩條曲線的起始資金可能不同（模擬用使用者填的虛擬資金，回測用當時設定的資金），
// 所以都換算成「相對起始資金的報酬率（%）」才能疊圖比較，不是直接疊權益金額。

interface EquityCompareChartProps {
  simCurve: EquityPoint[];
  simStartingCapital: string;
  backtestCurve: EquityPoint[];
  backtestStartingCapital: string;
}

const WIDTH = 600;
const HEIGHT = 220;
const PADDING = 8;

function toReturnSeries(
  curve: EquityPoint[],
  startingCapital: string,
): { t: number; pct: number }[] {
  const start = Number(startingCapital);
  if (!start) return [];
  return curve.map((p) => ({
    t: p.openTime,
    pct: ((Number(p.equity) - start) / start) * 100,
  }));
}

export function EquityCompareChart({
  simCurve,
  simStartingCapital,
  backtestCurve,
  backtestStartingCapital,
}: EquityCompareChartProps) {
  const sim = toReturnSeries(simCurve, simStartingCapital);
  const backtest = toReturnSeries(backtestCurve, backtestStartingCapital);

  if (sim.length < 2 || backtest.length < 2) {
    return <p className="equity-chart__empty">資料點太少，無法畫出模擬 vs 回測對比圖。</p>;
  }

  const allT = [...sim, ...backtest].map((p) => p.t);
  const allPct = [...sim, ...backtest].map((p) => p.pct);
  const minT = Math.min(...allT);
  const maxT = Math.max(...allT);
  const spanT = maxT - minT || 1;
  const minPct = Math.min(...allPct, 0);
  const maxPct = Math.max(...allPct, 0);
  const spanPct = maxPct - minPct || 1;

  const toX = (t: number) => PADDING + ((t - minT) / spanT) * (WIDTH - PADDING * 2);
  const toY = (pct: number) =>
    HEIGHT - PADDING - ((pct - minPct) / spanPct) * (HEIGHT - PADDING * 2);

  const simLast = sim[sim.length - 1];
  const backtestLast = backtest[backtest.length - 1];
  const simPoints = sim.map((p) => `${toX(p.t)},${toY(p.pct)}`).join(" ");
  const backtestPoints = backtest.map((p) => `${toX(p.t)},${toY(p.pct)}`).join(" ");

  return (
    <svg
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      className="equity-chart"
      role="img"
      aria-label={`模擬報酬 ${simLast.pct.toFixed(1)}%，回測報酬 ${backtestLast.pct.toFixed(1)}%`}
    >
      <line
        x1={PADDING}
        y1={toY(0)}
        x2={WIDTH - PADDING}
        y2={toY(0)}
        className="equity-chart__baseline"
      />
      <polyline points={backtestPoints} className="equity-compare-chart__backtest" />
      <polyline points={simPoints} className="equity-compare-chart__sim" />
      <text x={PADDING} y={12} className="equity-chart__label equity-compare-chart__legend-sim">
        模擬 {simLast.pct.toFixed(1)}%
      </text>
      <text
        x={PADDING}
        y={26}
        className="equity-chart__label equity-compare-chart__legend-backtest"
      >
        回測 {backtestLast.pct.toFixed(1)}%
      </text>
    </svg>
  );
}
