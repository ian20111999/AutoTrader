import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { CompareChart } from "./CompareChart";
import { colorForIndex } from "./compareColors";
import type { BacktestSummary } from "./backtestTypes";

function run(overrides: Partial<BacktestSummary> = {}): BacktestSummary {
  return {
    symbol: "BTCUSDT",
    interval: "1d",
    year: 2024,
    month: 1,
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: {},
    startingCapital: "10000",
    barCount: 3,
    curve: [
      { openTime: 0, equity: "10000" },
      { openTime: 1, equity: "10500" },
      { openTime: 2, equity: "10200" },
    ],
    trades: 1,
    liquidations: 0,
    totalReturn: "0.02",
    annualizedReturn: "0.3",
    maxDrawdown: "0.03",
    sharpe: "1",
    spanYears: "0.1",
    feeModel: "spot_vip0",
    slippage: "0.0005",
    dataSourcePath: "/tmp/x.csv",
    ...overrides,
  };
}

describe("CompareChart", () => {
  it("每筆回測畫一條折線", () => {
    render(<CompareChart runs={[run(), run({ symbol: "ETHUSDT" })]} />);

    const chart = screen.getByRole("img");
    expect(chart.querySelectorAll("polyline")).toHaveLength(2);
  });

  it("資料點不足的回測不畫線，但不影響其他回測正常畫出來", () => {
    render(<CompareChart runs={[run(), run({ curve: [{ openTime: 0, equity: "10000" }] })]} />);

    const chart = screen.getByRole("img");
    expect(chart.querySelectorAll("polyline")).toHaveLength(1);
  });

  it("全部回測都沒有足夠資料時顯示文字說明，不畫空圖表", () => {
    render(<CompareChart runs={[run({ curve: [] })]} />);

    expect(
      screen.getByText("目前的回測都沒有足夠的權益曲線資料可以疊圖。"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("沒有任何回測時也顯示文字說明", () => {
    render(<CompareChart runs={[]} />);

    expect(
      screen.getByText("目前的回測都沒有足夠的權益曲線資料可以疊圖。"),
    ).toBeInTheDocument();
  });
});

describe("colorForIndex", () => {
  it("同一個 index 永遠回傳同一個顏色", () => {
    expect(colorForIndex(0)).toBe(colorForIndex(0));
  });

  it("不同 index 循環使用調色盤", () => {
    expect(colorForIndex(0)).not.toBe(colorForIndex(1));
  });
});
