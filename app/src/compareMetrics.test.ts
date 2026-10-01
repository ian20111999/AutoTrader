import { describe, expect, it } from "vitest";
import {
  generateInsight,
  toDrawdownSeries,
  toReturnSeries,
  trailing12MonthsReturn,
  yearlyReturn,
  yearsInRuns,
} from "./compareMetrics";
import type { BacktestSummary } from "./backtestTypes";

const MS_PER_DAY = 24 * 60 * 60 * 1000;

function run(overrides: Partial<BacktestSummary> = {}): BacktestSummary {
  return {
    sessionId: "backtest-test-1",
    symbol: "BTCUSDT",
    interval: "1d",
    year: 2024,
    month: 1,
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: { fastPeriod: "5", slowPeriod: "20" },
    startingCapital: "10000",
    barCount: 2,
    curve: [
      { openTime: Date.UTC(2024, 0, 1), equity: "10000" },
      { openTime: Date.UTC(2024, 0, 31), equity: "11000" },
    ],
    trades: 4,
    liquidations: 0,
    totalReturn: "0.1",
    annualizedReturn: "1.2",
    maxDrawdown: "0.05",
    sharpe: "1.1",
    spanYears: "0.08",
    feeModel: "spot_vip0",
    slippage: "0.0005",
    dataSourcePath: "/tmp/x.csv",
    ...overrides,
  };
}

describe("yearsInRuns", () => {
  it("回傳所有回測曲線裡實際出現過的曆年，依時間排序", () => {
    const runs = [
      run({ curve: [{ openTime: Date.UTC(2025, 5, 1), equity: "1" }] }),
      run({ curve: [{ openTime: Date.UTC(2024, 0, 1), equity: "1" }] }),
    ];
    expect(yearsInRuns(runs)).toEqual([2024, 2025]);
  });

  it("沒有任何回測或曲線為空時回傳空陣列", () => {
    expect(yearsInRuns([])).toEqual([]);
    expect(yearsInRuns([run({ curve: [] })])).toEqual([]);
  });
});

describe("yearlyReturn", () => {
  it("曲線整段都落在同一年，回傳該年首尾報酬率", () => {
    expect(yearlyReturn(run(), 2024)).toBeCloseTo(0.1);
  });

  it("查詢的年份不在曲線範圍內，回傳 null", () => {
    expect(yearlyReturn(run(), 2025)).toBeNull();
  });

  it("那一年只有一個資料點，資料不夠算報酬，回傳 null", () => {
    const r = run({
      curve: [
        { openTime: Date.UTC(2024, 0, 1), equity: "10000" },
        { openTime: Date.UTC(2025, 0, 1), equity: "12000" },
      ],
    });
    expect(yearlyReturn(r, 2025)).toBeNull();
  });

  it("跨年的曲線，各年各自用自己區間內的首尾點計算", () => {
    const r = run({
      curve: [
        { openTime: Date.UTC(2024, 11, 15), equity: "10000" },
        { openTime: Date.UTC(2024, 11, 31), equity: "10500" },
        { openTime: Date.UTC(2025, 0, 1), equity: "10600" },
        { openTime: Date.UTC(2025, 0, 15), equity: "11660" },
      ],
    });
    expect(yearlyReturn(r, 2024)).toBeCloseTo(0.05);
    expect(yearlyReturn(r, 2025)).toBeCloseTo(0.1);
  });
});

describe("trailing12MonthsReturn", () => {
  it("曲線涵蓋超過 365 天，用最後一點往前推 365 天當基準算報酬", () => {
    const last = Date.UTC(2025, 0, 1);
    const r = run({
      curve: [
        { openTime: last - 400 * MS_PER_DAY, equity: "10000" },
        { openTime: last - 365 * MS_PER_DAY, equity: "10200" },
        { openTime: last, equity: "12240" },
      ],
    });
    expect(trailing12MonthsReturn(r)).toBeCloseTo(0.2);
  });

  it("曲線最早的一點比 365 天前還晚，資料不夠涵蓋近 12 個月，回傳 null", () => {
    expect(trailing12MonthsReturn(run())).toBeNull();
  });

  it("只有一個資料點，回傳 null", () => {
    expect(trailing12MonthsReturn(run({ curve: [{ openTime: 0, equity: "10000" }] }))).toBeNull();
  });
});

describe("toReturnSeries / toDrawdownSeries", () => {
  it("報酬序列是相對起始資金的累積報酬%", () => {
    const [first, second] = toReturnSeries(run());
    expect(first).toBe(0);
    expect(second).toBeCloseTo(10);
  });

  it("回撤序列永遠 <= 0，新高點回撤是 0", () => {
    const r = run({
      curve: [
        { openTime: 0, equity: "10000" },
        { openTime: 1, equity: "12000" },
        { openTime: 2, equity: "9000" },
        { openTime: 3, equity: "13000" },
      ],
    });
    const dd = toDrawdownSeries(r);
    expect(dd[0]).toBe(0);
    expect(dd[1]).toBe(0);
    expect(dd[2]).toBeCloseTo(-25);
    expect(dd[3]).toBe(0);
  });
});

describe("generateInsight", () => {
  it("剛好兩筆、只有一個參數不同，自動生成一句話摘要", () => {
    const a = run({ params: { fastPeriod: "5", slowPeriod: "20" }, annualizedReturn: "0.198", maxDrawdown: "0.183" });
    const b = run({ params: { fastPeriod: "10", slowPeriod: "20" }, annualizedReturn: "0.337", maxDrawdown: "0.341" });
    const insight = generateInsight([a, b], null);
    expect(insight.kind).toBe("single-param-diff");
    if (insight.kind === "single-param-diff") {
      expect(insight.text).toContain("5");
      expect(insight.text).toContain("10");
      expect(insight.text).toContain("+19.8%");
      expect(insight.text).toContain("+33.7%");
      expect(insight.text).toContain("−18.3%");
      expect(insight.text).toContain("−34.1%");
    }
  });

  it("不只一個參數不同，回傳 too-complex 不硬湊摘要", () => {
    const a = run({ params: { fastPeriod: "5", slowPeriod: "20" } });
    const b = run({ params: { fastPeriod: "10", slowPeriod: "30" } });
    expect(generateInsight([a, b], null)).toEqual({ kind: "too-complex" });
  });

  it("市場範圍（symbol/interval/年月/起始資金）不同，回傳 too-complex", () => {
    const a = run({ symbol: "BTCUSDT" });
    const b = run({ symbol: "ETHUSDT" });
    expect(generateInsight([a, b], null)).toEqual({ kind: "too-complex" });
  });

  it("不是剛好兩筆回測，回傳 none", () => {
    expect(generateInsight([run()], null)).toEqual({ kind: "none" });
    expect(generateInsight([run(), run(), run()], null)).toEqual({ kind: "none" });
  });

  it("用策略的 params label 組句子，不是直接顯示 key", () => {
    const strategies = [
      {
        id: "sma_cross",
        params: [
          { key: "fastPeriod", label: "快線週期" },
          { key: "slowPeriod", label: "慢線週期" },
        ],
      },
    ];
    const a = run({ params: { fastPeriod: "5", slowPeriod: "20" } });
    const b = run({ params: { fastPeriod: "10", slowPeriod: "20" } });
    const insight = generateInsight([a, b], strategies);
    expect(insight.kind).toBe("single-param-diff");
    if (insight.kind === "single-param-diff") {
      expect(insight.text.startsWith("只改快線週期")).toBe(true);
    }
  });
});
