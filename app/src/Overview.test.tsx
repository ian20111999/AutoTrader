import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Overview } from "./Overview";
import type { LiveSessionDto, SessionRecord } from "./overviewTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const LIVE_PAPER: LiveSessionDto = {
  sessionId: "paper-1000-001",
  kind: "paper",
  market: "spot",
  symbol: "BTCUSDT",
  interval: "1m",
  strategyId: "sma_cross",
  strategyName: "均線交叉",
  status: "running",
  startedAtMs: 1_000,
  equity: "10120.50",
  position: "0.1",
  dailyPnl: "120.50",
  asOfMs: 2_000,
  stale: false,
  killSwitch: null,
  barsSeen: 42,
};

const LIVE_TESTNET: LiveSessionDto = {
  ...LIVE_PAPER,
  sessionId: "testnet-1000-001",
  kind: "testnet",
  symbol: "ETHUSDT",
  stale: true,
  killSwitch: true,
};

const RECENT_BACKTEST: SessionRecord = {
  schemaVersion: 1,
  id: "backtest-1000-001",
  kind: "backtest",
  market: "spot",
  symbol: "BTCUSDT",
  interval: "1h",
  strategyId: "sma_cross",
  strategyName: "均線交叉",
  params: {},
  startedAtMs: 1_000,
  endedAtMs: 2_000,
  status: "completed",
  statusMessage: null,
  startingCapital: "10000",
  finalEquity: "10500",
  barsSeen: 100,
  metrics: {
    totalReturn: "0.05",
    annualizedReturn: "0.2",
    maxDrawdown: "0.1",
    sharpe: "1.2",
    spanYears: "0.25",
  },
  saved: false,
  dataSourcePath: "/data/BTCUSDT/1h/2026-01.csv",
};

function mockInvoke(options?: {
  live?: LiveSessionDto[];
  recent?: SessionRecord[];
  curveRecords?: SessionRecord[];
}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_live_sessions") return Promise.resolve(options?.live ?? []);
    if (cmd === "list_sessions") {
      const filter = args?.filter as { kinds?: string[] } | undefined;
      if (filter?.kinds?.includes("backtest")) {
        return Promise.resolve(options?.recent ?? []);
      }
      return Promise.resolve(options?.curveRecords ?? []);
    }
    if (cmd === "read_session_curve") {
      return Promise.resolve([{ openTime: 1, equity: "10000" }]);
    }
    return Promise.reject(new Error(`unexpected command: ${cmd}`));
  });
}

const NOOP_HANDLERS = {
  onOpenPaperTrading: vi.fn(),
  onOpenTestnetTrading: vi.fn(),
  onOpenBacktestCompare: vi.fn(),
};

describe("Overview", () => {
  it("顯示執行中策略數，依模擬/測試網分類，不分「實盤」", async () => {
    mockInvoke({ live: [LIVE_PAPER, LIVE_TESTNET] });
    render(<Overview {...NOOP_HANDLERS} />);

    await waitFor(() => expect(screen.getByText("2")).toBeInTheDocument());
    expect(screen.getByText("模擬 1・測試網 1")).toBeInTheDocument();
  });

  it("帳戶權益卡片逐場列出權益，不顯示加總的單一數字，並標示非真實餘額", async () => {
    mockInvoke({ live: [LIVE_PAPER] });
    render(<Overview {...NOOP_HANDLERS} />);

    await waitFor(() => expect(screen.getByText(/BTCUSDT・模擬/)).toBeInTheDocument());
    expect(screen.getByText(/沒有查詢交易所真實帳戶餘額的端點/)).toBeInTheDocument();
    expect(screen.getByText(/權益 10120.50/)).toBeInTheDocument();
  });

  it("執行中策略表格點一列會依種類呼叫對應的開啟 callback", async () => {
    const onOpenPaperTrading = vi.fn();
    mockInvoke({ live: [LIVE_PAPER] });
    render(
      <Overview
        onOpenPaperTrading={onOpenPaperTrading}
        onOpenTestnetTrading={vi.fn()}
        onOpenBacktestCompare={vi.fn()}
      />,
    );

    const row = await screen.findByRole("button", { name: /前往 BTCUSDT 模擬 session/ });
    fireEvent.click(row);

    expect(onOpenPaperTrading).toHaveBeenCalledWith("paper-1000-001");
  });

  it("過期資料顯示「資料過期」徽章", async () => {
    mockInvoke({ live: [LIVE_TESTNET] });
    render(<Overview {...NOOP_HANDLERS} />);

    expect(await screen.findByText("資料過期")).toBeInTheDocument();
  });

  it("最近回測清單「加入比較」會讀曲線後呼叫 onOpenBacktestCompare", async () => {
    const onOpenBacktestCompare = vi.fn();
    mockInvoke({ recent: [RECENT_BACKTEST] });
    render(
      <Overview
        onOpenPaperTrading={vi.fn()}
        onOpenTestnetTrading={vi.fn()}
        onOpenBacktestCompare={onOpenBacktestCompare}
      />,
    );

    const button = await screen.findByRole("button", { name: "加入比較" });
    fireEvent.click(button);

    await waitFor(() => expect(onOpenBacktestCompare).toHaveBeenCalledTimes(1));
    const summary = onOpenBacktestCompare.mock.calls[0][0];
    expect(summary.sessionId).toBe("backtest-1000-001");
    expect(summary.year).toBe(2026);
    expect(summary.month).toBe(1);
  });

  it("沒有執行中策略時顯示空狀態文字，不留空表格", async () => {
    mockInvoke({ live: [] });
    render(<Overview {...NOOP_HANDLERS} />);

    expect(await screen.findByText("目前沒有執行中的策略。")).toBeInTheDocument();
  });

  it("掛載時訂閱 session-registry-changed 事件", async () => {
    mockInvoke({ live: [] });
    render(<Overview {...NOOP_HANDLERS} />);

    await waitFor(() =>
      expect(listen).toHaveBeenCalledWith("session-registry-changed", expect.any(Function)),
    );
  });
});
