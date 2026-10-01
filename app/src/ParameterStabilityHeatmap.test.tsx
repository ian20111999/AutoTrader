import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ParameterStabilityHeatmap } from "./ParameterStabilityHeatmap";
import type { StrategyInfo } from "./strategyTypes";
import type { ParameterSweepResult } from "./backtestTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

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

const DEFAULT_PROPS = {
  paramValues: { fastPeriod: "10", slowPeriod: "50" },
  symbol: "BTCUSDT",
  interval: "1d",
  year: 2024,
  month: 1,
  startingCapital: "10000",
  market: "spot" as const,
  direction: "long_only" as const,
  leverage: "1",
  marginMode: null,
};

describe("ParameterStabilityHeatmap", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("策略只有一個參數時顯示不支援訊息，不畫按鈕", () => {
    render(<ParameterStabilityHeatmap strategy={ONE_PARAM_STRATEGY} {...DEFAULT_PROPS} />);
    expect(screen.getByText(/不支援雙參數穩定度熱力圖/)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("點擊執行按鈕會呼叫 run_parameter_sweep_command，成功後畫出網格", async () => {
    const result: ParameterSweepResult = {
      paramXKey: "fastPeriod",
      paramYKey: "slowPeriod",
      cells: [
        { paramXValue: "7", paramYValue: "40", annualizedReturn: "0.1", error: null },
        { paramXValue: "10", paramYValue: "40", annualizedReturn: "0.2", error: null },
      ],
    };
    vi.mocked(invoke).mockResolvedValue(result);

    render(<ParameterStabilityHeatmap strategy={SMA_CROSS} {...DEFAULT_PROPS} />);

    fireEvent.click(screen.getByRole("button", { name: /執行參數掃描/ }));

    expect(await screen.findByText("20.0%")).toBeInTheDocument();
    expect(screen.getByText("10.0%")).toBeInTheDocument();
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "run_parameter_sweep_command",
        expect.objectContaining({
          request: expect.objectContaining({
            strategyId: "sma_cross",
            paramX: expect.objectContaining({ key: "fastPeriod" }),
            paramY: expect.objectContaining({ key: "slowPeriod" }),
          }),
        }),
      ),
    );
  });

  it("失敗時顯示錯誤訊息", async () => {
    vi.mocked(invoke).mockRejectedValue("參數掃描組合數超過上限");

    render(<ParameterStabilityHeatmap strategy={SMA_CROSS} {...DEFAULT_PROPS} />);
    fireEvent.click(screen.getByRole("button", { name: /執行參數掃描/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("參數掃描組合數超過上限");
  });

  it("某一格計算失敗時顯示「—」而不是假造數字", async () => {
    const result: ParameterSweepResult = {
      paramXKey: "fastPeriod",
      paramYKey: "slowPeriod",
      cells: [
        { paramXValue: "50", paramYValue: "10", annualizedReturn: null, error: "快線週期必須短於慢線週期" },
      ],
    };
    vi.mocked(invoke).mockResolvedValue(result);

    render(<ParameterStabilityHeatmap strategy={SMA_CROSS} {...DEFAULT_PROPS} />);
    fireEvent.click(screen.getByRole("button", { name: /執行參數掃描/ }));

    expect(await screen.findByText("—")).toBeInTheDocument();
  });
});
