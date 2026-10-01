import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { BacktestResult } from "./BacktestResult";
import type { BacktestSummary } from "./backtestTypes";

const BASE_SUMMARY: BacktestSummary = {
  symbol: "BTCUSDT",
  interval: "1d",
  year: 2024,
  month: 1,
  strategyId: "sma_cross",
  strategyName: "均線交叉",
  params: { fastPeriod: "10", slowPeriod: "50" },
  startingCapital: "10000",
  barCount: 31,
  curve: [
    { openTime: 0, equity: "10000" },
    { openTime: 86400000, equity: "10500" },
    { openTime: 172800000, equity: "11980" },
  ],
  trades: 4,
  liquidations: 0,
  totalReturn: "0.198",
  annualizedReturn: "0.25",
  maxDrawdown: "0.183",
  sharpe: "0.9",
  spanYears: "1",
  feeModel: "spot_vip0（現貨 VIP0，吃單 0.1%）",
  slippage: "0.0005",
  market: "spot",
  direction: "long_only",
  leverage: "1",
  marginMode: null,
  dataSourcePath: "/tmp/klines/BTCUSDT-1d-2024-01.csv",
};

describe("BacktestResult", () => {
  it("顯示四個績效指標的格式化文字", () => {
    const { container } = render(<BacktestResult summary={BASE_SUMMARY} />);
    const metrics = container.querySelector(".backtest-metrics") as HTMLElement;

    expect(within(metrics).getByText("總報酬")).toBeInTheDocument();
    expect(within(metrics).getByText("+19.8%")).toBeInTheDocument();
    expect(within(metrics).getByText("年化報酬")).toBeInTheDocument();
    expect(within(metrics).getByText("+25.0%")).toBeInTheDocument();
    expect(within(metrics).getByText("最大回撤")).toBeInTheDocument();
    expect(within(metrics).getByText("−18.3%")).toBeInTheDocument();
    expect(within(metrics).getByText("夏普值")).toBeInTheDocument();
    expect(within(metrics).getByText("0.90")).toBeInTheDocument();
  });

  it("total_return / annualized_return 是 null 時顯示「—」而不是 0", () => {
    render(
      <BacktestResult
        summary={{ ...BASE_SUMMARY, totalReturn: null, annualizedReturn: null, sharpe: null }}
      />,
    );

    expect(screen.getAllByText("—")).toHaveLength(3);
  });

  it("顯示交易次數與 K 線根數，強平 0 次時不特別提到強平", () => {
    render(<BacktestResult summary={BASE_SUMMARY} />);

    expect(screen.getByText(/交易 4 筆/)).toBeInTheDocument();
    expect(screen.getByText(/共 31 根 K 線/)).toBeInTheDocument();
    expect(screen.queryByText(/強制平倉/)).not.toBeInTheDocument();
  });

  it("有強平時顯示強平次數", () => {
    render(<BacktestResult summary={{ ...BASE_SUMMARY, liquidations: 2 }} />);

    expect(screen.getByText(/強制平倉 2 次/)).toBeInTheDocument();
  });

  it("誠實標示成本假設是系統固定的，不是使用者調過的", () => {
    render(<BacktestResult summary={BASE_SUMMARY} />);

    expect(screen.getByText(/還不能由你調整/)).toBeInTheDocument();
    expect(screen.getByText(/spot_vip0/)).toBeInTheDocument();
    expect(screen.getByText(/0\.05%/)).toBeInTheDocument();
  });
});
