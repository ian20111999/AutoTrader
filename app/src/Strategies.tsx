import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyInfo } from "./strategyTypes";

// 參數摘要：簡短標籤+預設值,例如「快線週期10、慢線週期50」。
// 完整表單（含驗證、修改）是 3.5 調參頁面的事，這裡只顯示唯讀摘要。
function paramSummary(strategy: StrategyInfo): string {
  return strategy.params.map((param) => `${param.label}${param.default}`).join("、");
}

export function Strategies() {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch((err: unknown) => setError(String(err)));
  }, []);

  if (error) {
    return <p role="alert">讀取內建策略失敗：{error}</p>;
  }
  if (!strategies) {
    return <p>讀取中…</p>;
  }

  return (
    <section aria-label="內建策略">
      <div className="strategy-grid">
        {strategies.map((strategy) => (
          <button
            key={strategy.id}
            type="button"
            className={
              strategy.id === selectedId
                ? "strategy-card strategy-card--selected"
                : "strategy-card"
            }
            aria-pressed={strategy.id === selectedId}
            onClick={() => setSelectedId(strategy.id)}
          >
            <span className="strategy-card__name">{strategy.name}</span>
            <span className="strategy-card__params">{paramSummary(strategy)}</span>
          </button>
        ))}
      </div>
    </section>
  );
}
