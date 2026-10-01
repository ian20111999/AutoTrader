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
    sessionId: "backtest-test-1",
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
    vi.mocked(invoke).mockImplementation((command: string) => {
      if (command === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
      if (command === "run_buy_hold_baseline_command") {
        // 預設讓 BTC 基準請求一直 pending（不 resolve），等同「還沒抓回來」，
        // 不影響既有測試對表格列數/內容的斷言。要測基準列的測試自己覆寫這個 mock。
        return new Promise(() => {});
      }
      return Promise.reject(new Error(`未預期的 invoke 呼叫：${command}`));
    });
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
    // 總報酬/年化/夏普三個 null 指標各一個「—」，加上近 12 個月報酬（曲線只有
    // 兩個 1970 年的假時間戳，資料不夠涵蓋 365 天）也是「—」。
    expect(screen.getAllByText("—")).toHaveLength(4);
  });

  it("逐年報酬欄位依曲線實際出現的曆年動態產生，年度內的首尾報酬正確換算成百分比", () => {
    render(
      <Compare
        savedBacktests={[
          run({
            curve: [
              { openTime: Date.UTC(2024, 0, 1), equity: "10000" },
              { openTime: Date.UTC(2024, 11, 31), equity: "11000" },
            ],
          }),
        ]}
        onRemove={vi.fn()}
        onGoToBacktest={vi.fn()}
      />,
    );

    expect(screen.getByRole("columnheader", { name: "2024" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "近12個月" })).toBeInTheDocument();
    // 2024 全年（1/1→12/31）跟近 12 個月（資料不足 365 天，用曲線第一點當基準）
    // 這個測試資料剛好算出同一個報酬率，所以兩欄都會是「+10.0%」。
    expect(screen.getAllByText("+10.0%")).toHaveLength(2);
  });

  it("抓得到 BTC 買入持有基準曲線時，多顯示一列基準列，交易次數顯示「—」", async () => {
    vi.mocked(invoke).mockImplementation((command: string) => {
      if (command === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
      if (command === "run_buy_hold_baseline_command") {
        return Promise.resolve(
          run({
            strategyId: "_buy_hold_baseline",
            strategyName: "BTC 買入持有",
            params: {},
            totalReturn: "0.3",
            annualizedReturn: "0.3",
            maxDrawdown: "0.5",
            sharpe: "0.8",
            trades: 1,
          }),
        );
      }
      return Promise.reject(new Error(`未預期的 invoke 呼叫：${command}`));
    });

    render(<Compare savedBacktests={[run()]} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    expect(await screen.findByText("BTC 買入持有")).toBeInTheDocument();
    const baselineRow = (await screen.findByText("BTC 買入持有")).closest("tr");
    expect(baselineRow).not.toBeNull();
    expect(baselineRow).toHaveTextContent("基準");
    expect(baselineRow).toHaveTextContent("—");
  });

  it("BTC 基準曲線抓取失敗時不顯示基準列（不硬湊假資料）", async () => {
    vi.mocked(invoke).mockImplementation((command: string) => {
      if (command === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
      if (command === "run_buy_hold_baseline_command") {
        return Promise.reject(new Error("離線"));
      }
      return Promise.reject(new Error(`未預期的 invoke 呼叫：${command}`));
    });

    render(<Compare savedBacktests={[run()]} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    await screen.findByText("快線週期5、慢線週期20");
    expect(screen.queryByText("BTC 買入持有")).not.toBeInTheDocument();
  });

  it("剛好兩筆、只有一個參數不同時顯示關鍵發現摘要", async () => {
    const runs = [
      run({ params: { fastPeriod: "5", slowPeriod: "20" }, annualizedReturn: "0.198", maxDrawdown: "0.183" }),
      run({ params: { fastPeriod: "10", slowPeriod: "20" }, annualizedReturn: "0.337", maxDrawdown: "0.341" }),
    ];
    render(<Compare savedBacktests={runs} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    expect(
      await screen.findByText((text) => text.startsWith("只改快線週期")),
    ).toBeInTheDocument();
  });

  it("兩筆回測差異不只一個參數時，顯示「無法自動摘要」而不是硬湊的分析", () => {
    const runs = [
      run({ params: { fastPeriod: "5", slowPeriod: "20" } }),
      run({ params: { fastPeriod: "10", slowPeriod: "30" } }),
    ];
    render(<Compare savedBacktests={runs} onRemove={vi.fn()} onGoToBacktest={vi.fn()} />);

    expect(screen.getByText("設定差異較多，無法自動摘要。")).toBeInTheDocument();
  });
});
