// 參數穩定度熱力圖（Phase G）：雙參數網格 × 年化報酬。使用者按鈕手動觸發
// （不是表單一送出就自動跑）——這是運算量比較大的功能，每一格都要重新跑一次
// 完整回測，不該在使用者還沒要求的時候就默默跑掉。
//
// 只取策略參數清單的前兩個當軸（parameterSweep.ts 的 sweepableAxes），候選值圍繞
// 使用者已經調好的參數值展開（buildAxisValues），兩軸各 5 個值＝最多 25 組，在
// 後端 MAX_SWEEP_COMBINATIONS（64）之內。多交易對時只用第一個交易對跑（熱力圖
// 是「這組參數在這個市場/期間穩不穩」的敏感度分析，不需要每個幣種各畫一張）。
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyInfo } from "./strategyTypes";
import type {
  Direction,
  Market,
  MarginMode,
  ParameterSweepRequest,
  ParameterSweepResult,
} from "./backtestTypes";
import { buildAxisValues, sweepableAxes } from "./parameterSweep";

interface ParameterStabilityHeatmapProps {
  strategy: StrategyInfo;
  paramValues: Record<string, string>;
  symbol: string;
  interval: string;
  year: number;
  month: number;
  startingCapital: string;
  market: Market;
  direction: Direction;
  leverage: string;
  marginMode: MarginMode | null;
}

type Status = "idle" | "loading" | "error" | "success";

// 正/負年化報酬各自的顏色強度上限：超過 ±50% 就封頂，避免單一極端值把整張
// 熱力圖的色階拉到看不出差異。
const COLOR_CAP_PCT = 50;
const POSITIVE_RGB = "34, 195, 142"; // compareColors.PALETTE 的綠（#22C38E）
const NEGATIVE_RGB = "244, 85, 94"; // compareColors.PALETTE 的紅（#F4555E）

function cellBackground(annualizedReturn: string | null): string {
  if (annualizedReturn === null) return "transparent";
  const pct = Number(annualizedReturn) * 100;
  const alpha = Math.min(Math.abs(pct), COLOR_CAP_PCT) / COLOR_CAP_PCT;
  const rgb = pct >= 0 ? POSITIVE_RGB : NEGATIVE_RGB;
  return `rgba(${rgb}, ${alpha.toFixed(2)})`;
}

export function ParameterStabilityHeatmap({
  strategy,
  paramValues,
  symbol,
  interval,
  year,
  month,
  startingCapital,
  market,
  direction,
  leverage,
  marginMode,
}: ParameterStabilityHeatmapProps) {
  const [status, setStatus] = useState<Status>("idle");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [result, setResult] = useState<ParameterSweepResult | null>(null);

  const axes = sweepableAxes(strategy);
  if (!axes) {
    return (
      <p className="parameter-sweep__unsupported">
        {strategy.name} 只有一個可調參數，不支援雙參數穩定度熱力圖。
      </p>
    );
  }
  const [paramX, paramY] = axes;

  async function handleRun() {
    const xValues = buildAxisValues(paramX, paramValues[paramX.key] ?? paramX.default);
    const yValues = buildAxisValues(paramY, paramValues[paramY.key] ?? paramY.default);

    const request: ParameterSweepRequest = {
      symbol,
      interval,
      year,
      month,
      strategyId: strategy.id,
      baseParams: paramValues,
      paramX: { key: paramX.key, values: xValues },
      paramY: { key: paramY.key, values: yValues },
      startingCapital,
      market,
      direction,
      leverage,
      marginMode,
    };

    setStatus("loading");
    setErrorMessage(null);
    try {
      const sweepResult = await invoke<ParameterSweepResult>("run_parameter_sweep_command", {
        request,
      });
      setResult(sweepResult);
      setStatus("success");
    } catch (err) {
      setErrorMessage(String(err));
      setStatus("error");
    }
  }

  return (
    <div className="parameter-sweep">
      <button
        type="button"
        className="parameter-sweep__run-button"
        onClick={handleRun}
        disabled={status === "loading"}
      >
        {status === "loading" ? "計算中…" : `執行參數掃描（${paramX.label} × ${paramY.label}）`}
      </button>

      {status === "loading" && (
        <p role="status" className="parameter-sweep__status">
          正在跑多組回測算年化報酬，請稍候…
        </p>
      )}
      {status === "error" && errorMessage && (
        <p role="alert" className="parameter-sweep__status parameter-sweep__status--error">
          參數掃描失敗：{errorMessage}
        </p>
      )}
      {status === "success" && result && <HeatmapTable result={result} />}
    </div>
  );
}

function HeatmapTable({ result }: { result: ParameterSweepResult }) {
  const xValues = [...new Set(result.cells.map((c) => c.paramXValue))];
  const yValues = [...new Set(result.cells.map((c) => c.paramYValue))];
  const cellAt = (x: string, y: string) =>
    result.cells.find((c) => c.paramXValue === x && c.paramYValue === y) ?? null;

  return (
    <table className="parameter-sweep__table">
      <caption className="parameter-sweep__caption">
        參數穩定度熱力圖（年化報酬，橫軸 {result.paramXKey}、縱軸 {result.paramYKey}）
      </caption>
      <thead>
        <tr>
          <th scope="col"></th>
          {xValues.map((x) => (
            <th scope="col" key={x}>
              {x}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {yValues.map((y) => (
          <tr key={y}>
            <th scope="row">{y}</th>
            {xValues.map((x) => {
              const cell = cellAt(x, y);
              const display =
                cell?.annualizedReturn !== null && cell?.annualizedReturn !== undefined
                  ? `${(Number(cell.annualizedReturn) * 100).toFixed(1)}%`
                  : "—";
              return (
                <td
                  key={x}
                  style={{ background: cellBackground(cell?.annualizedReturn ?? null) }}
                  title={cell?.error ?? undefined}
                >
                  {display}
                </td>
              );
            })}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
