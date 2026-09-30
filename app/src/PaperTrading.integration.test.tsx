// 全流程整合測試：不像 PaperTrading.test.tsx 手動 vi.mock 兩個模組各自的函式，
// 這裡改用 Tauri 官方提供的 @tauri-apps/api/mocks（mockIPC + shouldMockEvents），
// 讓 invoke/listen/emit 都走真正的實作，只有最底層的 window.__TAURI_INTERNALS__
// 被換成假的。目的是驗證「開始 → 收到即時更新 → 停止」與「Failed 顯示警告」
// 這兩條路徑在真正的 Tauri 事件系統下也走得通，不只是符合我自己手刻的 mock 介面。
//
// ponytail: 專案裡沒有 Playwright（3.1-4.6 的所有畫面都只有 Vitest + Testing
// Library），這裡沿用同一套工具、換一種 mock 方式做端對端流程驗證，不必為了這一步
// 另外引入一個全新的測試框架。
import { afterEach, describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { PaperTrading } from "./PaperTrading";
import type { StrategyConfig } from "./strategyTypes";

const STRATEGY_CONFIG: StrategyConfig = {
  strategyId: "sma_cross",
  values: { fastPeriod: "5", slowPeriod: "20" },
};

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

afterEach(() => {
  clearMocks();
});

describe("PaperTrading 整合測試（真正的 invoke/listen/emit，只換底層 IPC）", () => {
  it("開始 → 收到即時更新 → 停止，統計與曲線都正確更新", async () => {
    const calls: Array<{ cmd: string; payload?: unknown }> = [];
    mockIPC(
      (cmd, payload) => {
        calls.push({ cmd, payload });
        switch (cmd) {
          case "list_builtin_strategies":
            return STRATEGIES;
          case "paper_trading_status":
            return { status: "idle" };
          case "start_paper_trading":
          case "stop_paper_trading":
            return null;
          default:
            throw new Error(`unexpected command: ${cmd}`);
        }
      },
      { shouldMockEvents: true },
    );

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={() => {}} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "start_paper_trading")).toBe(true));
    expect(calls.find((c) => c.cmd === "start_paper_trading")?.payload).toEqual({
      request: {
        symbol: "BTCUSDT",
        interval: "1m",
        strategyId: "sma_cross",
        params: { fastPeriod: "5", slowPeriod: "20" },
        startingCapital: "10000",
      },
    });
    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");

    await emit("paper-trading-update", {
      type: "bar",
      snapshot: {
        openTime: 1_704_067_200_000,
        equity: "10062.30",
        cash: "5000",
        position: "0.1",
        trades: 3,
        liquidations: 0,
      },
    });

    expect(await screen.findByRole("region", { name: "模擬交易結果" })).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "停止模擬" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "stop_paper_trading")).toBe(true));

    await emit("paper-trading-update", { type: "stopped" });

    const status = await screen.findByText("已停止模擬交易。");
    expect(status).toHaveAttribute("role", "status");
    expect(screen.queryByRole("button", { name: "停止模擬" })).not.toBeInTheDocument();
  });

  it("failed 事件顯示紅色警告，帳本標記為不可信", async () => {
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "list_builtin_strategies":
            return STRATEGIES;
          case "paper_trading_status":
            return { status: "idle" };
          case "start_paper_trading":
            return null;
          default:
            throw new Error(`unexpected command: ${cmd}`);
        }
      },
      { shouldMockEvents: true },
    );

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    await emit("paper-trading-update", {
      type: "failed",
      message: "第 3 根 K 線的權益變成負數",
    });

    const alert = await screen.findByText(
      "模擬交易中止：第 3 根 K 線的權益變成負數（帳本已經不可信，請重新開始）",
    );
    expect(alert).toHaveAttribute("role", "alert");
  });
});
