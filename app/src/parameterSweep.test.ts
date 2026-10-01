import { describe, expect, it } from "vitest";
import { buildAxisValues, sweepableAxes } from "./parameterSweep";
import type { StrategyInfo, StrategyParam } from "./strategyTypes";

const SMA_CROSS: StrategyInfo = {
  id: "sma_cross",
  name: "均線交叉",
  params: [
    { key: "fastPeriod", label: "快線週期", kind: "integer", default: "10" },
    { key: "slowPeriod", label: "慢線週期", kind: "integer", default: "50" },
  ],
};

const ONE_PARAM_STRATEGY: StrategyInfo = {
  id: "fake",
  name: "假策略",
  params: [{ key: "period", label: "週期", kind: "integer", default: "10" }],
};

describe("sweepableAxes", () => {
  it("策略有 >= 2 個參數時，回傳前兩個當軸", () => {
    expect(sweepableAxes(SMA_CROSS)).toEqual([SMA_CROSS.params[0], SMA_CROSS.params[1]]);
  });

  it("策略只有 1 個參數時回傳 null（不支援雙參數熱力圖）", () => {
    expect(sweepableAxes(ONE_PARAM_STRATEGY)).toBeNull();
  });
});

describe("buildAxisValues", () => {
  it("整數參數：以目前值為中心展開 5 個遞增候選值，含目前值", () => {
    const param: StrategyParam = { key: "fastPeriod", label: "快線", kind: "integer", default: "10" };
    const values = buildAxisValues(param, "10");
    expect(values).toContain("10");
    expect(values.length).toBeGreaterThanOrEqual(3);
    const nums = values.map(Number);
    expect(nums).toEqual([...nums].sort((a, b) => a - b));
  });

  it("整數參數下限夾到 1，不會出現 0 或負數週期", () => {
    const param: StrategyParam = { key: "period", label: "週期", kind: "integer", default: "10" };
    const values = buildAxisValues(param, "2");
    for (const v of values) {
      expect(Number(v)).toBeGreaterThanOrEqual(1);
    }
  });

  it("小數參數：以目前值為中心展開候選值", () => {
    const param: StrategyParam = { key: "multiplier", label: "倍數", kind: "decimal", default: "2" };
    const values = buildAxisValues(param, "2");
    expect(values).toContain("2.0000");
    const nums = values.map(Number);
    expect(nums).toEqual([...nums].sort((a, b) => a - b));
  });

  it("目前值不是合法數字時，只回傳這一個值，不瞎猜範圍", () => {
    const param: StrategyParam = { key: "x", label: "x", kind: "integer", default: "10" };
    expect(buildAxisValues(param, "not-a-number")).toEqual(["not-a-number"]);
  });
});
