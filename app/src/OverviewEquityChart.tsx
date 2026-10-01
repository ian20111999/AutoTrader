// ponytail: 跟 EquityCurveChart 同一招——手刻 SVG，不另外引入圖表函式庫。
// 這裡畫的是「已結束 session 的最終權益」依時間排列的點，不是連續的帳戶餘額
// 曲線——不同 session 彼此的權益數字沒有關聯（不同交易對/起始資金），用線
// 連起來會讓人誤讀成一條走勢線，所以刻意只畫散點，不連線。

export interface OverviewEquityPoint {
  atMs: number;
  equity: number;
  label: string;
}

interface OverviewEquityChartProps {
  points: OverviewEquityPoint[];
}

const WIDTH = 720;
const HEIGHT = 180;
const PADDING = 10;

export function OverviewEquityChart({ points }: OverviewEquityChartProps) {
  if (points.length === 0) {
    return <p className="equity-chart__empty">還沒有已結束的模擬／測試網 session，暫時沒有資料可畫。</p>;
  }
  if (points.length === 1) {
    return (
      <p className="equity-chart__empty">
        目前只有一筆已結束的 session（{points[0].label}：{points[0].equity.toFixed(2)}），
        還不足以畫出曲線。
      </p>
    );
  }

  const times = points.map((p) => p.atMs);
  const values = points.map((p) => p.equity);
  const minT = Math.min(...times);
  const maxT = Math.max(...times);
  const spanT = maxT - minT || 1;
  const minV = Math.min(...values);
  const maxV = Math.max(...values);
  const spanV = maxV - minV || 1;

  const toX = (atMs: number) => PADDING + ((atMs - minT) / spanT) * (WIDTH - PADDING * 2);
  const toY = (value: number) =>
    HEIGHT - PADDING - ((value - minV) / spanV) * (HEIGHT - PADDING * 2);

  return (
    <svg
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      className="equity-chart"
      role="img"
      aria-label={`${points.length} 筆已結束 session 的最終權益走勢，從 ${minV.toFixed(2)} 到 ${maxV.toFixed(2)}`}
    >
      <text x={PADDING} y={toY(maxV) - 2} className="equity-chart__label">
        {maxV.toFixed(0)}
      </text>
      <text x={PADDING} y={toY(minV) + 10} className="equity-chart__label">
        {minV.toFixed(0)}
      </text>
      {points.map((p, i) => (
        <circle key={i} cx={toX(p.atMs)} cy={toY(p.equity)} r={2.5} className="overview-equity-chart__dot">
          <title>
            {p.label}：{p.equity.toFixed(2)}
          </title>
        </circle>
      ))}
    </svg>
  );
}
