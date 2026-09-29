import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import { StrategyParamForm } from "./StrategyParamForm";

// 參數摘要：簡短標籤+預設值,例如「快線週期10、慢線週期50」。
// 完整表單（含驗證、修改）在調參頁面，這裡只顯示唯讀摘要。
function paramSummary(strategy: StrategyInfo): string {
  return strategy.params.map((param) => `${param.label}${param.default}`).join("、");
}

function defaultValues(strategy: StrategyInfo): Record<string, string> {
  return Object.fromEntries(strategy.params.map((param) => [param.key, param.default]));
}

interface StrategiesProps {
  strategyConfig: StrategyConfig | null;
  onApplyConfig: (config: StrategyConfig) => void;
}

export function Strategies({ strategyConfig, onApplyConfig }: StrategiesProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);

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

  const editingStrategy = strategies.find((strategy) => strategy.id === editingId) ?? null;
  if (editingStrategy) {
    const initialValues =
      strategyConfig?.strategyId === editingStrategy.id
        ? strategyConfig.values
        : defaultValues(editingStrategy);
    return (
      <StrategyParamForm
        strategy={editingStrategy}
        initialValues={initialValues}
        onApply={(values) => {
          onApplyConfig({ strategyId: editingStrategy.id, values });
          setSelectedId(editingStrategy.id);
          setEditingId(null);
        }}
        onCancel={() => setEditingId(null)}
      />
    );
  }

  return (
    <section aria-label="內建策略">
      <ul className="strategy-grid">
        {strategies.map((strategy) => (
          <li
            key={strategy.id}
            className={
              strategy.id === selectedId
                ? "strategy-card strategy-card--selected"
                : "strategy-card"
            }
          >
            <button
              type="button"
              className="strategy-card__select"
              aria-pressed={strategy.id === selectedId}
              onClick={() => setSelectedId(strategy.id)}
            >
              <span className="strategy-card__name">{strategy.name}</span>
              <span className="strategy-card__params">{paramSummary(strategy)}</span>
            </button>
            <button
              type="button"
              className="strategy-card__edit"
              aria-label={`調整${strategy.name}參數`}
              onClick={() => setEditingId(strategy.id)}
            >
              調整參數
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
