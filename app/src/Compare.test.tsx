import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Compare } from "./Compare";
import type { BacktestSummary } from "./backtestTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const STRATEGIES = [
  {
    id: "sma_cross",
    name: "均線交叉",
    params: [
      { key: "fastPeriod", label: "快線週期", kind: "integer", default: "10" },
      { key: "slowPeriod", label: "慢線週期", kind: "integer", default: "50" },
    ],
  },
  {
    id: "rsi",
    name: "RSI",
    params: [
      { key: "period", label: "週期", kind: "integer", default: "14" },
      { key: "buyBelow", label: "進場門檻", kind: "decimal", default: "30" },
      { key: "exitAbove", label: "出場門檻", kind: "decimal", default: "70" },
    ],
  },
];

function run(overrides: Partial<BacktestSummary> = {}): BacktestSummary {
  return {
    symbol: "BTCUSDT",
    interval: "1d",
    year: 2024,
    month: 1,
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: { fastPeriod: "5", slowPeriod: "20" },
    startingCapital: "10000",
    barCount: 31,
    curve: [
      { openTime: 0, equity: "10000" },
      { openTime: 1, equity: "10500" },
    ],
    trades: 4,
    liquidations: 0,
    totalReturn: "0.087",
    annualizedReturn: "1.2",
    maxDrawdown: "0.05",
    sharpe: "1.1",
    spanYears: "0.08",
    feeModel: "spot_vip0（現貨 VIP0，吃單 0.1%）",
    slippage: "0.0005",
    dataSourcePath: "/tmp/BTCUSDT-1d-2024-01.csv",
    ...overrides,
  };
}

describe("Compare", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(STRATEGIES);
  });

  it("沒有比較項目時顯示空狀態，點「前往回測」呼叫 onGoToBacktest", () => {
    const onGoToBacktest = vi.fn();
    render(<Compare savedBacktests={[]} onRemove={vi.fn()} onGoToBacktest={onGoToBacktest} />);

    expect(
      screen.getByText("還沒有加入任何回測。先到回測頁跑一次回測，再把結果加入比較。"),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "前往回測" }));
    expect(onGoToBacktest).toHaveBeenCalled();
  });

  it("只有一筆時照樣完整顯示表格與圖表", async () => {
    render(<Compare savedBacktests={[run()]} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    expect(await screen.findByText("快線週期5、慢線週期20")).toBeInTheDocument();
    expect(screen.getByRole("img")).toBeInTheDocument();
    expect(screen.getAllByRole("row")).toHaveLength(2); // 表頭 + 1 筆資料
  });

  it("多筆回測顯示對應數量的表格列與圖例，參數摘要依各自策略的 label 組字", async () => {
    const runs = [
      run(),
      run({
        strategyId: "rsi",
        strategyName: "RSI",
        params: { period: "14", buyBelow: "25", exitAbove: "75" },
        symbol: "ETHUSDT",
        month: 2,
      }),
    ];
    render(<Compare savedBacktests={runs} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    expect(await screen.findByText("快線週期5、慢線週期20")).toBeInTheDocument();
    expect(screen.getByText("週期14、進場門檻25、出場門檻75")).toBeInTheDocument();
    expect(screen.getAllByRole("row")).toHaveLength(3); // 表頭 + 2 筆資料
    expect(screen.getByText("BTCUSDT · 1d · 2024/01")).toBeInTheDocument();
    expect(screen.getByText("ETHUSDT · 1d · 2024/02")).toBeInTheDocument();
  });

  it("點移除按鈕呼叫 onRemove 並帶正確的 index", async () => {
    const onRemove = vi.fn();
    const runs = [run(), run({ symbol: "ETHUSDT" })];
    render(<Compare savedBacktests={runs} onRemove={onRemove} onGoToBacktest={vi.fn()} />);

    const removeButtons = await screen.findAllByRole("button", { name: /從比較中移除/ });
    expect(removeButtons).toHaveLength(2);

    fireEvent.click(removeButtons[1]);
    expect(onRemove).toHaveBeenCalledWith(1);
  });

  it("null 的指標顯示「—」", async () => {
    render(
      <Compare
        savedBacktests={[run({ totalReturn: null, annualizedReturn: null, sharpe: null })]}
        onRemove={vi.fn()}
        onGoToBacktest={vi.fn()}
      />,
    );

    await screen.findByText("快線週期5、慢線週期20");
    expect(screen.getAllByText("—")).toHaveLength(3);
  });
});
