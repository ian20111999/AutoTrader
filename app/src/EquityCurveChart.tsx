import type { EquityPoint } from "./backtestTypes";

// ponytail: 手刻 SVG 折線圖，不加圖表函式庫依賴——目前只需要畫一條線，原生 SVG
// 就夠了。3.7 若要疊圖比較多條曲線且需要更多互動（tooltip、縮放），是重新評估要不
// 要上圖表庫的時機，不是現在就先猜。

interface EquityCurveChartProps {
  curve: EquityPoint[];
  startingCapital: string;
}

const WIDTH = 600;
const HEIGHT = 200;
const PADDING = 8;

export function EquityCurveChart({ curve, startingCapital }: EquityCurveChartProps) {
  if (curve.length < 2) {
    return <p className="equity-chart__empty">資料點太少，無法畫出權益曲線。</p>;
  }

  const values = curve.map((point) => Number(point.equity));
  const starting = Number(startingCapital);
  const min = Math.min(...values, starting);
  const max = Math.max(...values, starting);
  const span = max - min || 1;

  const toX = (index: number) => PADDING + (index / (curve.length - 1)) * (WIDTH - PADDING * 2);
  const toY = (value: number) =>
    HEIGHT - PADDING - ((value - min) / span) * (HEIGHT - PADDING * 2);

  const linePoints = values.map((value, index) => `${toX(index)},${toY(value)}`).join(" ");
  const startY = toY(starting);

  return (
    <svg
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      className="equity-chart"
      role="img"
      aria-label={`權益曲線，從起始資金 ${startingCapital} 到最終權益 ${values[values.length - 1]}`}
    >
      <line
        x1={PADDING}
        y1={startY}
        x2={WIDTH - PADDING}
        y2={startY}
        className="equity-chart__baseline"
      />
      <text x={PADDING} y={toY(max) - 2} className="equity-chart__label">
        {max.toFixed(0)}
      </text>
      <text x={PADDING} y={toY(min) + 10} className="equity-chart__label">
        {min.toFixed(0)}
      </text>
      <polyline points={linePoints} className="equity-chart__line" />
    </svg>
  );
}
