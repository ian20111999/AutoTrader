// 各幣種對總報酬貢獻條狀圖（Phase G）：使用者這次回測選了多個交易對時，各自的
// total_return 並排比較（各幣種各自獨立跑一次，不做組合數學）。
//
// ponytail: 用 CSS flex + 寬度百分比畫長條，不用 SVG 也不加圖表函式庫——只是
// 並排比較幾個數字，純 CSS 就夠表達，比手刻 SVG 座標軸還簡單。
import type { BacktestSummary } from "./backtestTypes";
import { formatSignedPercent, signClass } from "./backtestFormat";

interface SymbolContributionChartProps {
  results: BacktestSummary[];
}

export function SymbolContributionChart({ results }: SymbolContributionChartProps) {
  const magnitudes = results.map((r) =>
    r.totalReturn === null ? 0 : Math.abs(Number(r.totalReturn)),
  );
  const max = Math.max(...magnitudes, 0.0001);

  return (
    <div className="symbol-contribution" aria-label="各幣種對總報酬的貢獻">
      {results.map((r) => {
        const value = r.totalReturn === null ? 0 : Number(r.totalReturn);
        const widthPct = (Math.abs(value) / max) * 100;
        return (
          <div className="symbol-contribution__row" key={r.symbol}>
            <span className="symbol-contribution__symbol">{r.symbol}</span>
            <div className="symbol-contribution__track">
              <div
                className={
                  value >= 0
                    ? "symbol-contribution__bar symbol-contribution__bar--positive"
                    : "symbol-contribution__bar symbol-contribution__bar--negative"
                }
                style={{ width: `${widthPct}%` }}
              />
            </div>
            <span className={signClass(r.totalReturn)}>{formatSignedPercent(r.totalReturn)}</span>
          </div>
        );
      })}
    </div>
  );
}
