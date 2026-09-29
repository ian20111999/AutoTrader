import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyInfo } from "./strategyTypes";

// 3.2 橋接驗證用的陽春列表：只證明資料真的從 Rust 的
// list_builtin_strategies 流過來，不是前端寫死。正式版面在 3.4。
export function BuiltinStrategies() {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

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
    <ul>
      {strategies.map((strategy) => (
        <li key={strategy.id}>
          {strategy.name}：
          {strategy.params.map((param) => `${param.label}=${param.default}`).join("、")}
        </li>
      ))}
    </ul>
  );
}
