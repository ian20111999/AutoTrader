import { useState } from "react";
import type { BacktestSummary } from "./backtestTypes";
import { colorForIndex } from "./compareColors";
import { toDrawdownSeries, toReturnSeries } from "./compareMetrics";

// 疊圖比較的核心問題：每筆回測是不同交易對/不同月份，時間戳完全對不上，直接照
// openTime 畫在同一個時間軸只會讓曲線散在圖的不同區域，比較不出東西。用「這筆
// 回測跑到第幾根 K 線（0～1 的相對進度）」當 X 軸、「相對起始資金的累積報酬%」
// 當 Y 軸，兩個問題一次解決：不同長度、不同起始資金的回測才能疊在同一張圖上比
// 形狀。代價是 X 軸不是真的日曆時間，只是「回測期間跑了多少比例」——這是刻意
// 的簡化，在 docs/steps/3.7 有記錄。
//
// ponytail: 手刻 SVG（沿用 3.6 EquityCurveChart 的做法，多畫幾條 <polyline> +
// 顏色循環）。多條線疊圖、外加一個顏色圖例，複雜度還是線性成長（每多一筆回測就
// 多畫一條線），不需要學一個圖表函式庫的 API 或多背一個依賴；沒有要做 tooltip、
// 縮放、log 座標這些真的需要函式庫的互動需求。

interface CompareChartProps {
  runs: BacktestSummary[];
}

const WIDTH = 900;
const HEIGHT = 220;
const PADDING = 8;

type ChartView = "return" | "drawdown";

const VIEW_LABELS: Record<ChartView, string> = {
  return: "累積報酬",
  drawdown: "回撤",
};

export function CompareChart({ runs }: CompareChartProps) {
  const [view, setView] = useState<ChartView>("return");
  const toSeries = view === "return" ? toReturnSeries : toDrawdownSeries;

  const series = runs.map((run, index) => ({
    color: colorForIndex(index),
    values: toSeries(run),
  }));
  const plottable = series.filter((s) => s.values.length >= 2);

  const ariaLabel =
    view === "return"
      ? "多筆回測的累積報酬疊圖，橫軸是各自回測期間的相對進度，不是日曆時間"
      : "多筆回測的回撤疊圖，橫軸是各自回測期間的相對進度，不是日曆時間";

  return (
    <div className="compare-chart-view">
      <div className="compare-chart-view__toggle" role="group" aria-label="疊圖檢視切換">
        {(Object.keys(VIEW_LABELS) as ChartView[]).map((key) => (
          <button
            key={key}
            type="button"
            className="compare-chart-view__toggle-button"
            aria-pressed={view === key}
            onClick={() => setView(key)}
          >
            {VIEW_LABELS[key]}
          </button>
        ))}
      </div>
      {plottable.length === 0 ? (
        <p className="compare-chart__empty">目前的回測都沒有足夠的權益曲線資料可以疊圖。</p>
      ) : (
        <CompareChartSvg plottable={plottable} ariaLabel={ariaLabel} />
      )}
    </div>
  );
}

function CompareChartSvg({
  plottable,
  ariaLabel,
}: {
  plottable: { color: string; values: number[] }[];
  ariaLabel: string;
}) {
  const allValues = plottable.flatMap((s) => s.values).concat(0);
  const min = Math.min(...allValues);
  const max = Math.max(...allValues);
  const span = max - min || 1;

  const toY = (value: number) => HEIGHT - PADDING - ((value - min) / span) * (HEIGHT - PADDING * 2);
  const zeroY = toY(0);

  return (
    <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} className="compare-chart" role="img" aria-label={ariaLabel}>
      <line
        x1={PADDING}
        y1={zeroY}
        x2={WIDTH - PADDING}
        y2={zeroY}
        className="equity-chart__baseline"
      />
      <text x={PADDING} y={toY(max) - 2} className="equity-chart__label">
        {max.toFixed(0)}%
      </text>
      <text x={PADDING} y={toY(min) + 10} className="equity-chart__label">
        {min.toFixed(0)}%
      </text>
      {plottable.map((s, seriesIndex) => {
        const points = s.values
          .map((value, index) => {
            const x = PADDING + (index / (s.values.length - 1)) * (WIDTH - PADDING * 2);
            return `${x},${toY(value)}`;
          })
          .join(" ");
        return (
          <polyline
            key={seriesIndex}
            points={points}
            className="compare-chart__line"
            style={{ stroke: s.color }}
          />
        );
      })}
    </svg>
  );
}
