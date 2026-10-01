// 全流程整合測試，架構抄 PaperTrading.integration.test.tsx：用 Tauri 官方的
// @tauri-apps/api/mocks（mockIPC + shouldMockEvents）讓 invoke/listen/emit
// 都走真正的實作，只換掉最底層的 window.__TAURI_INTERNALS__。
// 驗證「金鑰狀態 → 開始 → 收到下單/收盤更新 → 一鍵停止 → 停止」與
// 「Failed 顯示警告」在真正的 Tauri 事件系統下也走得通。
import { afterEach, describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { TestnetTrading } from "./TestnetTrading";
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

describe("TestnetTrading 整合測試（真正的 invoke/listen/emit，只換底層 IPC）", () => {
  it("開始 → 收到下單與收盤更新 → 一鍵停止 → 停止，統計都正確更新", async () => {
    const calls: Array<{ cmd: string; payload?: unknown }> = [];
    mockIPC(
      (cmd, payload) => {
        calls.push({ cmd, payload });
        switch (cmd) {
          case "list_builtin_strategies":
            return STRATEGIES;
          case "testnet_credentials_status":
            return { apiKeySet: true, apiSecretSet: true };
          case "testnet_trading_status":
            return { status: "idle" };
          case "start_testnet_trading":
          case "stop_testnet_trading":
          case "set_testnet_kill_switch":
            return null;
          default:
            throw new Error(`unexpected command: ${cmd}`);
        }
      },
      { shouldMockEvents: true },
    );

    render(<TestnetTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={() => {}} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    await screen.findAllByText("已設定");

    fireEvent.click(screen.getByRole("button", { name: "開始測試網交易" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "start_testnet_trading")).toBe(true));
    expect(calls.find((c) => c.cmd === "start_testnet_trading")?.payload).toEqual({
      request: {
        symbol: "BTCUSDT",
        interval: "1m",
        strategyId: "sma_cross",
        params: { fastPeriod: "5", slowPeriod: "20" },
        initialCash: "1000",
        maxDailyLoss: "50",
        maxOrderNotional: "200",
      },
    });
    expect(await screen.findByText("測試網交易執行中，會真的對測試網送出訂單。")).toBeInTheDocument();

    await emit("testnet-trading-update", {
      type: "order",
      outcome: {
        type: "filled",
        orderId: 7,
        side: "Buy",
        requestedQty: "0.01",
        executedQty: "0.01",
        quoteQty: "500",
        fee: "0.5",
        status: "FILLED",
      },
    });
    await screen.findByText(/送出 買進 0.01，成交 0.01（FILLED），手續費 0.5/);

    await emit("testnet-trading-update", {
      type: "bar",
      snapshot: {
        openTime: 1_704_067_200_000,
        equity: "1002.30",
        cash: "500",
        position: "0.01",
        fills: 1,
        blocked: 0,
        feesPaid: "0.5",
        dailyPnl: "2.3",
        killSwitch: false,
      },
    });

    expect(await screen.findByRole("region", { name: "測試網交易結果" })).toBeInTheDocument();
    expect(screen.getByText("1002.30")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "一鍵停止送單" }));
    await waitFor(() =>
      expect(
        calls.some((c) => c.cmd === "set_testnet_kill_switch" && (c.payload as { on: boolean }).on),
      ).toBe(true),
    );

    await emit("testnet-trading-update", {
      type: "bar",
      snapshot: {
        openTime: 1_704_067_260_000,
        equity: "1002.30",
        cash: "500",
        position: "0.01",
        fills: 1,
        blocked: 1,
        feesPaid: "0.5",
        dailyPnl: "2.3",
        killSwitch: true,
      },
    });
    expect(await screen.findByRole("button", { name: "恢復送單" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "停止測試網交易" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "stop_testnet_trading")).toBe(true));

    await emit("testnet-trading-update", { type: "stopped" });

    const status = await screen.findByText("已停止測試網交易。");
    expect(status).toHaveAttribute("role", "status");
    expect(screen.queryByRole("button", { name: "停止測試網交易" })).not.toBeInTheDocument();
  });

  it("failed 事件顯示紅色警告，帳本標記為不可信", async () => {
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "list_builtin_strategies":
            return STRATEGIES;
          case "testnet_credentials_status":
            return { apiKeySet: true, apiSecretSet: true };
          case "testnet_trading_status":
            return { status: "idle" };
          case "start_testnet_trading":
            return null;
          default:
            throw new Error(`unexpected command: ${cmd}`);
        }
      },
      { shouldMockEvents: true },
    );

    render(<TestnetTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "開始測試網交易" }));
    await screen.findByText("測試網交易執行中，會真的對測試網送出訂單。");

    await emit("testnet-trading-update", {
      type: "failed",
      message: "送單失敗：HTTP 狀態碼 418。訂單可能已經送達交易所，請到測試網後台確認實際部位",
    });

    const alert = await screen.findByText(
      "測試網交易中止：送單失敗：HTTP 狀態碼 418。訂單可能已經送達交易所，請到測試網後台確認實際部位（帳本已經不可信，請重新開始）",
    );
    expect(alert).toHaveAttribute("role", "alert");
  });

  it("測試網金鑰未設定時，狀態區塊清楚顯示未設定", async () => {
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "list_builtin_strategies":
            return STRATEGIES;
          case "testnet_credentials_status":
            return { apiKeySet: false, apiSecretSet: false };
          case "testnet_trading_status":
            return { status: "idle" };
          default:
            throw new Error(`unexpected command: ${cmd}`);
        }
      },
      { shouldMockEvents: true },
    );

    render(<TestnetTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={() => {}} />);
    const items = await screen.findAllByText("未設定");
    expect(items).toHaveLength(2);
  });
});
