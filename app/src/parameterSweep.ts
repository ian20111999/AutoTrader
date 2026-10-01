// 參數穩定度熱力圖（Phase G）用的純函式：挑哪兩個參數當軸、每個軸要掃哪些候選值。
// 不開放使用者自己選軸或自訂候選值範圍——固定取策略參數清單的前兩個，候選值圍繞
// 「使用者在調參頁面已經設定好的值」展開，是老實的「對目前設定做敏感度分析」，
// 不是憑空亂猜一個範圍。
import type { StrategyInfo, StrategyParam } from "./strategyTypes";

/** 每個軸固定展開 5 個候選值（含目前值），兩軸相乘 = 25 組，在後端
 * MAX_SWEEP_COMBINATIONS（64）之內留有餘裕。 */
const STEPS_EACH_SIDE = 2;

/** 挑出要掃描的兩個參數：策略參數清單的前兩個。少於 2 個參數的策略
 * （目前四個內建策略都 >= 2 個）不支援熱力圖，呼叫端應該檢查這裡回傳 null。 */
export function sweepableAxes(strategy: StrategyInfo): [StrategyParam, StrategyParam] | null {
  if (strategy.params.length < 2) return null;
  return [strategy.params[0], strategy.params[1]];
}

/** 以目前參數值為中心，往上下各展開 `STEPS_EACH_SIDE` 步，生成候選值（遞增排序、
 * 去重）。整數參數下限夾到 1（週期至少要 1 根 K 線）；步幅取目前值的 30%
 * （整數至少 1、小數目前值是 0 時退回固定 0.5），不是隨便猜的範圍。
 * 目前值不是合法數字（理論上不會發生，表單已經驗證過）就只回這一個值，不瞎掰。 */
export function buildAxisValues(param: StrategyParam, currentValue: string): string[] {
  const current = Number(currentValue);
  if (!Number.isFinite(current)) return [currentValue];

  if (param.kind === "integer") {
    const step = Math.max(1, Math.round(Math.abs(current) * 0.3));
    const values = new Set<number>();
    for (let i = -STEPS_EACH_SIDE; i <= STEPS_EACH_SIDE; i++) {
      const v = Math.round(current + i * step);
      if (v >= 1) values.add(v);
    }
    return [...values].sort((a, b) => a - b).map(String);
  }

  const step = current !== 0 ? Math.abs(current) * 0.3 : 0.5;
  const values = new Set<string>();
  for (let i = -STEPS_EACH_SIDE; i <= STEPS_EACH_SIDE; i++) {
    values.add((current + i * step).toFixed(4));
  }
  return [...values].sort((a, b) => Number(a) - Number(b));
}
