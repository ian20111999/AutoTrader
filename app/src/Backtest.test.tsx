import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Backtest } from "./Backtest";
import type { StrategyConfig } from "./strategyTypes";
import type { BacktestRequest, BacktestSummary } from "./backtestTypes";

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
];

const STRATEGY_CONFIG: StrategyConfig = {
  strategyId: "sma_cross",
  values: { fastPeriod: "5", slowPeriod: "20" },
};

function summaryFor(symbol: string): BacktestSummary {
  return {
    symbol,
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
      { openTime: 86400000, equity: "10500" },
    ],
    trades: 3,
    liquidations: 0,
    totalReturn: "0.05",
    annualizedReturn: "0.6",
    maxDrawdown: "0.02",
    sharpe: "1.1",
    spanYears: "0.08",
    feeModel: "spot_vip0（現貨 VIP0，吃單 0.1%）",
    slippage: "0.0005",
    market: "spot",
    direction: "long_only",
    leverage: "1",
    marginMode: null,
    dataSourcePath: `/tmp/klines/${symbol}-1d-2024-01.csv`,
  };
}

const SUMMARY = summaryFor("BTCUSDT");

function mockInvoke(
  runBacktestImpl: (request: BacktestRequest) => Promise<BacktestSummary>,
) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
    if (cmd === "run_backtest_command") {
      return runBacktestImpl((args as { request: BacktestRequest }).request);
    }
    return Promise.reject(new Error(`unexpected command: ${cmd}`));
  });
}

describe("Backtest", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("還沒選策略時顯示提示，點「前往策略庫」呼叫 onGoToStrategies", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));
    const onGoToStrategies = vi.fn();

    render(
      <Backtest
        strategyConfig={null}
        onGoToStrategies={onGoToStrategies}
        onAddToCompare={vi.fn()}
      />,
    );

    expect(
      await screen.findByText("還沒有選擇策略，請先到策略庫選一個策略並調整參數。"),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "前往策略庫" }));
    expect(onGoToStrategies).toHaveBeenCalled();
  });

  it("有策略設定時顯示策略名稱與參數摘要", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );

    expect(await screen.findByText("均線交叉：快線週期5、慢線週期20")).toBeInTheDocument();
  });

  it("起始資金不合法時擋下送出，不呼叫 run_backtest_command", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("起始資金"), { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    expect(await screen.findByText("起始資金必須大於 0")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("run_backtest_command", expect.anything());
  });

  it("交易對留空時擋下送出", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("交易對（可逗號分隔多個）"), {
      target: { value: "  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    expect(await screen.findByText("請至少輸入一個交易對代號")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("run_backtest_command", expect.anything());
  });

  it("送出合法表單時，帶正確的 request 呼叫 run_backtest_command（預設現貨/只做多/1倍槓桿）", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("交易對（可逗號分隔多個）"), {
      target: { value: "ethusdt" },
    });
    fireEvent.change(screen.getByLabelText("年月"), { target: { value: "2024-03" } });
    fireEvent.change(screen.getByLabelText("起始資金"), { target: { value: "5000" } });
    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("run_backtest_command", {
        request: {
          symbol: "ethusdt",
          interval: "1d",
          year: 2024,
          month: 3,
          strategyId: "sma_cross",
          params: { fastPeriod: "5", slowPeriod: "20" },
          startingCapital: "5000",
          market: "spot",
          direction: "long_only",
          leverage: "1",
          marginMode: null,
        },
      }),
    );
  });

  it("輸入逗號分隔的多個交易對時，各自呼叫一次 run_backtest_command 並各自顯示結果", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("交易對（可逗號分隔多個）"), {
      target: { value: "BTCUSDT, ETHUSDT" },
    });
    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "run_backtest_command",
        expect.objectContaining({ request: expect.objectContaining({ symbol: "BTCUSDT" }) }),
      ),
    );
    expect(invoke).toHaveBeenCalledWith(
      "run_backtest_command",
      expect.objectContaining({ request: expect.objectContaining({ symbol: "ETHUSDT" }) }),
    );

    expect(await screen.findByRole("region", { name: "回測結果：BTCUSDT" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "回測結果：ETHUSDT" })).toBeInTheDocument();
  });

  it("切到合約市場時可以選方向/槓桿/保證金模式，切回現貨會復位", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    expect(screen.getByLabelText("方向")).toBeDisabled();
    expect(screen.queryByLabelText("保證金模式")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("市場"), { target: { value: "usdm_perp" } });
    expect(screen.getByLabelText("方向")).not.toBeDisabled();
    expect(screen.getByLabelText("保證金模式")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("方向"), { target: { value: "long_short" } });
    fireEvent.change(screen.getByLabelText("槓桿倍數"), { target: { value: "3" } });
    fireEvent.change(screen.getByLabelText("保證金模式"), { target: { value: "cross" } });

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("run_backtest_command", {
        request: expect.objectContaining({
          market: "usdm_perp",
          direction: "long_short",
          leverage: "3",
          marginMode: "cross",
        }),
      }),
    );

    // 切回現貨：方向跟槓桿應該復位成只做多/1倍。
    fireEvent.change(screen.getByLabelText("市場"), { target: { value: "spot" } });
    expect(screen.getByLabelText("方向")).toHaveValue("long_only");
    expect(screen.getByLabelText("方向")).toBeDisabled();
  });

  it("槓桿超過 2x 顯示風控上限提示，但不擋送出", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("市場"), { target: { value: "usdm_perp" } });
    fireEvent.change(screen.getByLabelText("槓桿倍數"), { target: { value: "3" } });

    expect(await screen.findByText(/超過風控建議上限/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("run_backtest_command", expect.anything()));
  });

  it("「計入真實歷史資金費率」開關是 disabled 並標示即將推出", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    const toggle = screen.getByLabelText(/計入真實歷史資金費率/);
    expect(toggle).toBeDisabled();
    expect(toggle).not.toBeChecked();
    expect(screen.getByText(/即將推出/)).toBeInTheDocument();
  });

  it("送出後顯示 loading 狀態，回傳後顯示結果", async () => {
    let resolveRun: (summary: BacktestSummary) => void = () => {};
    const runPromise = new Promise<BacktestSummary>((resolve) => {
      resolveRun = resolve;
    });
    mockInvoke(() => runPromise);

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    expect(await screen.findByRole("status")).toHaveTextContent("回測中");

    resolveRun(SUMMARY);

    expect(await screen.findByRole("region", { name: "回測結果：BTCUSDT" })).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("結果出來後點「加入比較」把每個交易對的結果都加入，按鈕變成已加入狀態", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));
    const onAddToCompare = vi.fn();

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={onAddToCompare}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("交易對（可逗號分隔多個）"), {
      target: { value: "BTCUSDT, ETHUSDT" },
    });
    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));
    await screen.findByRole("region", { name: "回測結果：BTCUSDT" });
    await screen.findByRole("region", { name: "回測結果：ETHUSDT" });

    const addButton = screen.getByRole("button", { name: "加入比較" });
    fireEvent.click(addButton);

    expect(onAddToCompare).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "已加入比較 ✓" })).toBeDisabled();
  });

  it("重新執行回測時，「加入比較」的已加入狀態會重置", async () => {
    mockInvoke((req) => Promise.resolve(summaryFor(req.symbol)));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));
    await screen.findByRole("region", { name: "回測結果：BTCUSDT" });
    fireEvent.click(screen.getByRole("button", { name: "加入比較" }));
    expect(screen.getByRole("button", { name: "已加入比較 ✓" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));
    await screen.findByRole("region", { name: "回測結果：BTCUSDT" });

    expect(screen.getByRole("button", { name: "加入比較" })).toBeInTheDocument();
  });

  it("run_backtest_command 失敗時顯示錯誤訊息，不會卡在 loading", async () => {
    mockInvoke(() => Promise.reject("找不到這個月的歷史資料"));

    render(
      <Backtest
        strategyConfig={STRATEGY_CONFIG}
        onGoToStrategies={vi.fn()}
        onAddToCompare={vi.fn()}
      />,
    );
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "執行回測" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("找不到這個月的歷史資料");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
