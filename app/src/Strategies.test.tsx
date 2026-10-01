import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Strategies } from "./Strategies";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const FOUR_BUILTIN_STRATEGIES = [
  {
    id: "sma_cross",
    name: "均線交叉",
    params: [
      { key: "fastPeriod", label: "快線週期", kind: "integer", default: "10" },
      { key: "slowPeriod", label: "慢線週期", kind: "integer", default: "50" },
    ],
  },
  {
    id: "bollinger",
    name: "布林通道",
    params: [
      { key: "period", label: "週期", kind: "integer", default: "20" },
      { key: "multiplier", label: "標準差倍數", kind: "decimal", default: "2" },
    ],
  },
  {
    id: "donchian",
    name: "唐奇安突破",
    params: [
      { key: "entryPeriod", label: "進場週期", kind: "integer", default: "20" },
      { key: "exitPeriod", label: "出場週期", kind: "integer", default: "10" },
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

// 把 invoke 依指令名分派，取代「不管呼叫什麼都回同一包資料」的舊寫法——
// 新版 Strategies 同時呼叫 list_builtin_strategies / list_sessions /
// list_live_sessions，各自要回傳形狀不同的資料。
function mockInvoke(options: {
  sessionsByStrategy?: Record<string, unknown[]>;
  liveSessions?: unknown[];
} = {}) {
  const { sessionsByStrategy = {}, liveSessions = [] } = options;
  vi.mocked(invoke).mockImplementation(((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_builtin_strategies") {
      return Promise.resolve(FOUR_BUILTIN_STRATEGIES);
    }
    if (cmd === "list_live_sessions") {
      return Promise.resolve(liveSessions);
    }
    if (cmd === "list_sessions") {
      const filter = args?.filter as { strategyId?: string } | undefined;
      const id = filter?.strategyId ?? "";
      return Promise.resolve(sessionsByStrategy[id] ?? []);
    }
    return Promise.reject(new Error(`未預期的指令：${cmd}`));
  }) as typeof invoke);
}

function backtestSession(overrides: Record<string, unknown> = {}) {
  return {
    schemaVersion: 1,
    id: "backtest-1",
    kind: "backtest",
    market: "spot",
    symbol: "BTCUSDT",
    interval: "1d",
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: {},
    startedAtMs: Date.now(),
    endedAtMs: Date.now(),
    status: "completed",
    statusMessage: null,
    startingCapital: "10000",
    finalEquity: "10500",
    barsSeen: 100,
    metrics: {
      totalReturn: "0.05",
      annualizedReturn: "0.2",
      maxDrawdown: "-0.1",
      sharpe: "1.2",
      spanYears: "1",
    },
    saved: false,
    ...overrides,
  };
}

describe("Strategies", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("顯示 Rust 端 list_builtin_strategies 回傳的四張策略卡片與參數摘要", async () => {
    mockInvoke();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    expect(await screen.findByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.getByText("快線週期10、慢線週期50")).toBeInTheDocument();
    expect(screen.getByText(/布林通道/)).toBeInTheDocument();
    expect(screen.getByText(/唐奇安突破/)).toBeInTheDocument();
    expect(screen.getByText(/^RSI$/)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("list_builtin_strategies");
  });

  it("改變 Rust 端回傳的預設值時，畫面顯示要跟著變（不是前端寫死）", async () => {
    vi.mocked(invoke).mockImplementation(((cmd: string) => {
      if (cmd === "list_builtin_strategies") {
        return Promise.resolve([
          {
            id: "sma_cross",
            name: "均線交叉",
            params: [{ key: "fastPeriod", label: "快線週期", kind: "integer", default: "999" }],
          },
        ]);
      }
      if (cmd === "list_live_sessions") return Promise.resolve([]);
      if (cmd === "list_sessions") return Promise.resolve([]);
      return Promise.reject(new Error(`未預期的指令：${cmd}`));
    }) as typeof invoke);

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    expect(await screen.findByText("快線週期999")).toBeInTheDocument();
  });

  it("點擊卡片會標示為選中狀態", async () => {
    mockInvoke();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    const card = await screen.findByRole("button", { name: /^均線交叉/ });
    expect(card).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(card);

    expect(card).toHaveAttribute("aria-pressed", "true");
  });

  it("點擊「調整參數」會切換到該策略的調參表單", async () => {
    mockInvoke();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "調整均線交叉參數" }));

    expect(screen.getByRole("heading", { name: "均線交叉" })).toBeInTheDocument();
    expect(screen.getByLabelText("快線週期")).toHaveValue("10");
    expect(screen.getByLabelText("慢線週期")).toHaveValue("50");
    // 只顯示被點的那個策略的表單，不是四個策略都渲染。
    expect(screen.queryByText("布林通道")).not.toBeInTheDocument();
  });

  it("套用調參表單後回到列表、呼叫 onApplyConfig、並選中該策略", async () => {
    mockInvoke();
    const onApplyConfig = vi.fn();

    render(
      <Strategies strategyConfig={null} onApplyConfig={onApplyConfig} onNavigate={vi.fn()} />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "調整均線交叉參數" }));
    fireEvent.change(screen.getByLabelText("快線週期"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApplyConfig).toHaveBeenCalledWith({
      strategyId: "sma_cross",
      values: { fastPeriod: "5", slowPeriod: "50" },
    });
    // 回到列表，剛剛調過參的策略應該是選中狀態。
    expect(screen.getByRole("button", { name: /^均線交叉/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("點「返回策略庫」不會呼叫 onApplyConfig", async () => {
    mockInvoke();
    const onApplyConfig = vi.fn();

    render(
      <Strategies strategyConfig={null} onApplyConfig={onApplyConfig} onNavigate={vi.fn()} />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "調整均線交叉參數" }));
    fireEvent.click(screen.getByRole("button", { name: "返回策略庫" }));

    expect(onApplyConfig).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^均線交叉/ })).toBeInTheDocument();
  });

  it("呼叫失敗時顯示錯誤訊息", async () => {
    vi.mocked(invoke).mockRejectedValue("something broke");

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });

  it("預設顯示「內建範本」分頁；切到「我的策略」顯示誠實空狀態，不是硬塞內建策略", async () => {
    mockInvoke();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    await screen.findByText(/均線交叉/);
    expect(screen.getByRole("tab", { name: "內建範本" })).toHaveAttribute("aria-selected", "true");

    fireEvent.click(screen.getByRole("tab", { name: "我的策略" }));

    expect(screen.getByText(/還沒有儲存過的自訂策略/)).toBeInTheDocument();
    expect(screen.queryByText(/均線交叉/)).not.toBeInTheDocument();
  });

  it("有進行中的模擬 session 時卡片顯示「模擬中」徽章，否則顯示「未部署」", async () => {
    mockInvoke({
      liveSessions: [
        {
          sessionId: "paper-1",
          kind: "paper",
          market: "spot",
          symbol: "BTCUSDT",
          interval: "1m",
          strategyId: "sma_cross",
          strategyName: "均線交叉",
          status: "running",
          startedAtMs: Date.now(),
        },
      ],
    });

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    const smaCard = (await screen.findByText(/均線交叉/)).closest("li") as HTMLElement;
    expect(within(smaCard).getByText("模擬中")).toBeInTheDocument();

    const bollingerCard = screen.getByText(/布林通道/).closest("li") as HTMLElement;
    expect(within(bollingerCard).getByText("未部署")).toBeInTheDocument();
  });

  it("有回測歷史時顯示次數、最近時間與回測年化／最大回撤；沒有歷史時誠實顯示尚無資料", async () => {
    mockInvoke({
      sessionsByStrategy: {
        sma_cross: [
          backtestSession({ id: "b2", startedAtMs: 2000, metrics: { ...backtestSession().metrics, annualizedReturn: "0.3" } }),
          backtestSession({ id: "b1", startedAtMs: 1000 }),
        ],
      },
    });

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    const smaCard = (await screen.findByText(/均線交叉/)).closest("li") as HTMLElement;
    await waitFor(() => expect(within(smaCard).getByText(/回測 2 次/)).toBeInTheDocument());
    expect(within(smaCard).getByText(/30\.0%/)).toBeInTheDocument();

    const bollingerCard = screen.getByText(/布林通道/).closest("li") as HTMLElement;
    await waitFor(() =>
      expect(within(bollingerCard).getByText("尚無回測")).toBeInTheDocument(),
    );
  });

  it("「回測」按鈕帶著該策略預設參數導去回測頁", async () => {
    mockInvoke();
    const onNavigate = vi.fn();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={onNavigate} />);

    fireEvent.click(await screen.findByRole("button", { name: "回測均線交叉" }));

    expect(onNavigate).toHaveBeenCalledWith("backtest", {
      strategyId: "sma_cross",
      values: { fastPeriod: "10", slowPeriod: "50" },
    });
  });

  it("「模擬」按鈕導去模擬交易頁、「部署」按鈕導去測試網交易頁", async () => {
    mockInvoke();
    const onNavigate = vi.fn();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={onNavigate} />);

    fireEvent.click(await screen.findByRole("button", { name: "模擬均線交叉" }));
    expect(onNavigate).toHaveBeenCalledWith(
      "paperTrading",
      expect.objectContaining({ strategyId: "sma_cross" }),
    );

    fireEvent.click(screen.getByRole("button", { name: "部署均線交叉" }));
    expect(onNavigate).toHaveBeenCalledWith(
      "testnetTrading",
      expect.objectContaining({ strategyId: "sma_cross" }),
    );
  });

  it("市場篩選只留下支援該市場的策略（目前四個內建策略現貨/合約都支援）", async () => {
    mockInvoke();

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    await screen.findByText(/均線交叉/);
    fireEvent.change(screen.getByLabelText("市場"), { target: { value: "usdm_perp" } });

    expect(screen.getByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.getByText(/RSI/)).toBeInTheDocument();
  });

  it("狀態篩選只留下符合狀態的策略", async () => {
    mockInvoke({
      liveSessions: [
        {
          sessionId: "paper-1",
          kind: "paper",
          market: "spot",
          symbol: "BTCUSDT",
          interval: "1m",
          strategyId: "sma_cross",
          strategyName: "均線交叉",
          status: "running",
          startedAtMs: Date.now(),
        },
      ],
    });

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} onNavigate={vi.fn()} />);

    await screen.findByText(/均線交叉/);
    fireEvent.change(screen.getByLabelText("狀態"), { target: { value: "deployed" } });

    expect(screen.getByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.queryByText(/布林通道/)).not.toBeInTheDocument();
  });
});
