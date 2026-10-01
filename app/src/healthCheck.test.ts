import { describe, expect, it } from "vitest";
import { runHealthCheck } from "./healthCheck";
import type { BacktestSummary } from "./backtestTypes";

const BASE_SUMMARY: BacktestSummary = {
  sessionId: "s1",
  symbol: "BTCUSDT",
  interval: "1d",
  year: 2024,
  month: 1,
  strategyId: "sma_cross",
  strategyName: "均線交叉",
  params: {},
  startingCapital: "10000",
  barCount: 31,
  curve: [],
  trades: 10,
  liquidations: 0,
  totalReturn: "0.1",
  annualizedReturn: "0.2",
  maxDrawdown: "0.1",
  sharpe: "1.2",
  spanYears: "1",
  feeModel: "spot_vip0",
  slippage: "0.0005",
  market: "spot",
  direction: "long_only",
  leverage: "1",
  marginMode: null,
  dataSourcePath: "/tmp/fake.csv",
};

describe("runHealthCheck", () => {
  it("回撤小、交易夠多、夏普為正時三項都是良好", () => {
    const items = runHealthCheck(BASE_SUMMARY);
    expect(items).toHaveLength(3);
    expect(items.every((i) => i.status === "good")).toBe(true);
  });

  it("最大回撤超過 50% 標示注意", () => {
    const items = runHealthCheck({ ...BASE_SUMMARY, maxDrawdown: "0.6" });
    const drawdown = items.find((i) => i.label === "最大回撤");
    expect(drawdown?.status).toBe("warning");
    expect(drawdown?.detail).toContain("50%");
  });

  it("交易次數少於 5 筆標示注意", () => {
    const items = runHealthCheck({ ...BASE_SUMMARY, trades: 3 });
    const trades = items.find((i) => i.label === "交易次數");
    expect(trades?.status).toBe("warning");
  });

  it("夏普值為負標示注意", () => {
    const items = runHealthCheck({ ...BASE_SUMMARY, sharpe: "-0.5" });
    const sharpe = items.find((i) => i.label === "夏普值");
    expect(sharpe?.status).toBe("warning");
  });

  it("夏普值是 null 時標示資料不足，不是硬湊良好或注意", () => {
    const items = runHealthCheck({ ...BASE_SUMMARY, sharpe: null });
    const sharpe = items.find((i) => i.label === "夏普值");
    expect(sharpe?.status).toBe("unknown");
  });
});
