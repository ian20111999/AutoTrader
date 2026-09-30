import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { PaperTrading } from "./PaperTrading";
import type { StrategyConfig } from "./strategyTypes";
import type { PaperTradingStatus, PaperUpdateEvent } from "./paperTradingTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
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

const IDLE_STATUS: PaperTradingStatus = { status: "idle" };

const SNAPSHOT = {
  openTime: 1_704_067_200_000,
  equity: "10062.30",
  cash: "5000",
  position: "0.1",
  trades: 3,
  liquidations: 0,
};

type UpdateHandler = (event: { payload: PaperUpdateEvent }) => void;

function mockInvoke(options?: {
  status?: PaperTradingStatus;
  startImpl?: () => Promise<void>;
  stopImpl?: () => Promise<void>;
}) {
  const status = options?.status ?? IDLE_STATUS;
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
    if (cmd === "paper_trading_status") return Promise.resolve(status);
    if (cmd === "start_paper_trading") return (options?.startImpl ?? (() => Promise.resolve()))();
    if (cmd === "stop_paper_trading") return (options?.stopImpl ?? (() => Promise.resolve()))();
    return Promise.reject(new Error(`unexpected command: ${cmd}`));
  });
}

function mockListen(): { emit: UpdateHandler } {
  let handler: UpdateHandler = () => {};
  vi.mocked(listen).mockImplementation(((_name: string, cb: UpdateHandler) => {
    handler = cb;
    return Promise.resolve(() => {});
  }) as typeof listen);
  return {
    emit: (event) => {
      act(() => handler(event));
    },
  };
}

describe("PaperTrading", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(listen).mockReset();
  });

  it("還沒選策略時顯示提示，點「前往策略庫」呼叫 onGoToStrategies", async () => {
    mockInvoke();
    mockListen();
    const onGoToStrategies = vi.fn();

    render(<PaperTrading strategyConfig={null} onGoToStrategies={onGoToStrategies} />);

    expect(
      await screen.findByText("還沒有選擇策略，請先到策略庫選一個策略並調整參數。"),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "前往策略庫" }));
    expect(onGoToStrategies).toHaveBeenCalled();
  });

  it("有策略設定時顯示策略名稱與參數摘要", async () => {
    mockInvoke();
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(await screen.findByText("均線交叉：快線週期5、慢線週期20")).toBeInTheDocument();
  });

  it("起始資金不合法時擋下送出，不呼叫 start_paper_trading", async () => {
    mockInvoke();
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("虛擬起始資金"), { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));

    expect(await screen.findByText("起始資金必須大於 0")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("start_paper_trading", expect.anything());
  });

  it("送出合法表單時，帶正確的 request 呼叫 start_paper_trading，並顯示執行中狀態", async () => {
    mockInvoke();
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.change(screen.getByLabelText("交易對"), { target: { value: "ethusdt" } });
    fireEvent.change(screen.getByLabelText("虛擬起始資金"), { target: { value: "5000" } });
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("start_paper_trading", {
        request: {
          symbol: "ethusdt",
          interval: "1m",
          strategyId: "sma_cross",
          params: { fastPeriod: "5", slowPeriod: "20" },
          startingCapital: "5000",
        },
      }),
    );

    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");
  });

  it("start_paper_trading 失敗時顯示錯誤訊息", async () => {
    mockInvoke({ startImpl: () => Promise.reject("已經在執行中") });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));

    expect(await screen.findByText("開始模擬失敗：已經在執行中")).toBeInTheDocument();
  });

  it("收到 bar 事件時更新權益/部位/成交統計並累積曲線", async () => {
    mockInvoke();
    const { emit } = mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    emit({ payload: { type: "bar", snapshot: SNAPSHOT } });

    expect(await screen.findByRole("region", { name: "模擬交易結果" })).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
    expect(screen.getByText("0.1")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
  });

  it("收到 stopped 事件時顯示已停止（不是錯誤樣式）", async () => {
    mockInvoke();
    const { emit } = mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    emit({ payload: { type: "bar", snapshot: SNAPSHOT } });
    emit({ payload: { type: "stopped" } });

    const status = await screen.findByText("已停止模擬交易。");
    expect(status).toHaveAttribute("role", "status");
    expect(screen.queryByRole("button", { name: "停止模擬" })).not.toBeInTheDocument();
  });

  it("收到 failed 事件時顯示紅色警告，帳本不可信", async () => {
    mockInvoke();
    const { emit } = mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    emit({ payload: { type: "bar", snapshot: SNAPSHOT } });
    emit({ payload: { type: "failed", message: "第 3 根 K 線的權益變成負數" } });

    const alert = await screen.findByText(
      "模擬交易中止：第 3 根 K 線的權益變成負數（帳本已經不可信，請重新開始）",
    );
    expect(alert).toHaveAttribute("role", "alert");
  });

  it("點「停止模擬」呼叫 stop_paper_trading", async () => {
    mockInvoke();
    const { emit } = mockListen();
    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");
    emit({ payload: { type: "bar", snapshot: SNAPSHOT } });

    fireEvent.click(await screen.findByRole("button", { name: "停止模擬" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("stop_paper_trading"));
  });

  it("頁面掛載時查詢狀態，若已在執行中就直接補上畫面", async () => {
    mockInvoke({ status: { status: "running", snapshot: SNAPSHOT } });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");
    expect(await screen.findByRole("region", { name: "模擬交易結果" })).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "停止模擬" })).toBeInTheDocument();
  });

  it("頁面掛載時查詢狀態，若上次是失敗結束就顯示錯誤訊息與最後快照", async () => {
    mockInvoke({
      status: { status: "failed", snapshot: SNAPSHOT, message: "第 5 根 K 線的價格不是正數" },
    });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(
      await screen.findByText(
        "模擬交易中止：第 5 根 K 線的價格不是正數（帳本已經不可信，請重新開始）",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
  });
});
