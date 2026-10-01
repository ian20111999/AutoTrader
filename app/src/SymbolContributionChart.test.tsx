import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { SymbolContributionChart } from "./SymbolContributionChart";
import type { BacktestSummary } from "./backtestTypes";

function summaryFor(symbol: string, totalReturn: string | null): BacktestSummary {
  return {
    sessionId: `s-${symbol}`,
    symbol,
    interval: "1d",
    year: 2024,
    month: 1,
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: {},
    startingCapital: "10000",
    barCount: 31,
    curve: [],
    trades: 5,
    liquidations: 0,
    totalReturn,
    annualizedReturn: null,
    maxDrawdown: "0.1",
    sharpe: null,
    spanYears: "1",
    feeModel: "spot_vip0",
    slippage: "0.0005",
    market: "spot",
    direction: "long_only",
    leverage: "1",
    marginMode: null,
    dataSourcePath: "/tmp/fake.csv",
  };
}

describe("SymbolContributionChart", () => {
  it("每個交易對各畫一列，顯示代號與格式化後的總報酬", () => {
    render(
      <SymbolContributionChart
        results={[summaryFor("BTCUSDT", "0.2"), summaryFor("ETHUSDT", "-0.1")]}
      />,
    );

    expect(screen.getByText("BTCUSDT")).toBeInTheDocument();
    expect(screen.getByText("+20.0%")).toBeInTheDocument();
    expect(screen.getByText("ETHUSDT")).toBeInTheDocument();
    expect(screen.getByText("−10.0%")).toBeInTheDocument();
  });

  it("total_return 是 null 時顯示「—」，不假造 0", () => {
    render(<SymbolContributionChart results={[summaryFor("BTCUSDT", null)]} />);
    expect(screen.getByText("—")).toBeInTheDocument();
  });
});
