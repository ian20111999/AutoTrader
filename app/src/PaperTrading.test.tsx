import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { PaperTrading } from "./PaperTrading";
import type { StrategyConfig } from "./strategyTypes";
import { PAPER_TRADING_EVENT } from "./paperTradingTypes";
import type { PaperTradingStatus, PaperUpdateEnvelope } from "./paperTradingTypes";
import type { SessionRecord } from "./overviewTypes";

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
const SESSION_ID = "paper-1000-001";

const SNAPSHOT = {
  openTime: 1_704_067_200_000,
  equity: "10062.30",
  cash: "5000",
  position: "0.1",
  trades: 3,
  liquidations: 0,
};

type UpdateHandler = (event: { payload: PaperUpdateEnvelope }) => void;

// 分頁列表（`list_sessions`）裡這場模擬交易的紀錄。預設是「執行中」，個別測試
// 用 overrides 蓋成已停止/失敗等狀態。
function paperSession(overrides: Partial<SessionRecord> = {}): SessionRecord {
  return {
    schemaVersion: 1,
    id: SESSION_ID,
    kind: "paper",
    market: "spot",
    symbol: "BTCUSDT",
    interval: "1m",
    strategyId: "sma_cross",
    strategyName: "均線交叉",
    params: { fastPeriod: "5", slowPeriod: "20" },
    startedAtMs: 1_700_000_000_000,
    endedAtMs: null,
    status: "running",
    statusMessage: null,
    startingCapital: "10000",
    finalEquity: null,
    barsSeen: 0,
    metrics: null,
    saved: false,
    dataSourcePath: null,
    ...overrides,
  };
}

function mockInvoke(options?: {
  status?: PaperTradingStatus;
  startImpl?: () => Promise<string>;
  stopImpl?: () => Promise<void>;
  paperSessions?: SessionRecord[];
  backtestSessions?: SessionRecord[];
  curves?: Record<string, { openTime: number; equity: string }[]>;
}) {
  const status = options?.status ?? IDLE_STATUS;
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    if (cmd === "list_builtin_strategies") return Promise.resolve(STRATEGIES);
    if (cmd === "paper_trading_status") return Promise.resolve(status);
    if (cmd === "start_paper_trading")
      return (options?.startImpl ?? (() => Promise.resolve(SESSION_ID)))();
    if (cmd === "stop_paper_trading") return (options?.stopImpl ?? (() => Promise.resolve()))();
    if (cmd === "list_sessions") {
      const kinds = (args as { filter?: { kinds?: string[] } } | undefined)?.filter?.kinds ?? [];
      if (kinds.includes("backtest")) return Promise.resolve(options?.backtestSessions ?? []);
      return Promise.resolve(options?.paperSessions ?? []);
    }
    if (cmd === "read_session_curve") {
      const sessionId = (args as { sessionId?: string } | undefined)?.sessionId ?? "";
      return Promise.resolve(options?.curves?.[sessionId] ?? []);
    }
    return Promise.reject(new Error(`unexpected command: ${cmd}`));
  });
}

// 每個元件各自 `listen` 不同事件名稱，分開存才不會被互相蓋掉——測試只需要
// 模擬 `PAPER_TRADING_EVENT`，`session-registry-changed` 的監聽器留著不用管。
function mockListen(): { emit: UpdateHandler } {
  const handlers = new Map<string, (event: unknown) => void>();
  vi.mocked(listen).mockImplementation(((name: string, cb: (event: unknown) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => {});
  }) as typeof listen);
  return {
    emit: (event) => {
      act(() => handlers.get(PAPER_TRADING_EVENT)?.(event));
    },
  };
}

describe("PaperTrading", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(listen).mockReset();
    localStorage.clear();
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
          dslJson: null,
        },
      }),
    );

    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");
    expect(localStorage.getItem("paperTrading.sessionId")).toBe(SESSION_ID);
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

    emit({ payload: { sessionId: SESSION_ID, type: "bar", snapshot: SNAPSHOT } });

    expect(await screen.findByRole("region", { name: "模擬交易結果" })).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
    expect(screen.getByText("0.1")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
  });

  it("不同 session 的事件會被過濾掉，不更新畫面", async () => {
    mockInvoke();
    const { emit } = mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    emit({ payload: { sessionId: "paper-other-session", type: "bar", snapshot: SNAPSHOT } });

    expect(screen.queryByRole("region", { name: "模擬交易結果" })).not.toBeInTheDocument();
  });

  it("收到 stopped 事件時顯示已停止（不是錯誤樣式）", async () => {
    mockInvoke();
    const { emit } = mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");

    emit({ payload: { sessionId: SESSION_ID, type: "bar", snapshot: SNAPSHOT } });
    emit({ payload: { sessionId: SESSION_ID, type: "stopped" } });

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

    emit({ payload: { sessionId: SESSION_ID, type: "bar", snapshot: SNAPSHOT } });
    emit({
      payload: { sessionId: SESSION_ID, type: "failed", message: "第 3 根 K 線的權益變成負數" },
    });

    const alert = await screen.findByText(
      "模擬交易中止：第 3 根 K 線的權益變成負數（帳本已經不可信，請重新開始）",
    );
    expect(alert).toHaveAttribute("role", "alert");
  });

  it("點「停止模擬」帶 sessionId 呼叫 stop_paper_trading", async () => {
    mockInvoke();
    const { emit } = mockListen();
    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");
    fireEvent.click(screen.getByRole("button", { name: "開始模擬" }));
    await screen.findByRole("status");
    emit({ payload: { sessionId: SESSION_ID, type: "bar", snapshot: SNAPSHOT } });

    fireEvent.click(await screen.findByRole("button", { name: "停止模擬" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("stop_paper_trading", { sessionId: SESSION_ID }),
    );
  });

  it("頁面掛載時若 localStorage 記得 sessionId 就查詢狀態，已在執行中就直接補上畫面", async () => {
    localStorage.setItem("paperTrading.sessionId", SESSION_ID);
    mockInvoke({
      status: { status: "running", snapshot: SNAPSHOT },
      paperSessions: [paperSession({ status: "running" })],
    });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");
    expect(await screen.findByRole("region", { name: "模擬交易結果" })).toBeInTheDocument();
    expect(screen.getByText("10062.30")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "停止模擬" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("paper_trading_status", { sessionId: SESSION_ID });
  });

  it("頁面掛載時沒有記得的 sessionId 就不查詢狀態", async () => {
    mockInvoke();
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);
    await screen.findByText("均線交叉：快線週期5、慢線週期20");

    expect(invoke).not.toHaveBeenCalledWith("paper_trading_status", expect.anything());
  });

  it("頁面掛載時查詢狀態，若上次是失敗結束就顯示錯誤訊息與最後快照", async () => {
    localStorage.setItem("paperTrading.sessionId", SESSION_ID);
    mockInvoke({
      status: { status: "failed", snapshot: SNAPSHOT, message: "第 5 根 K 線的價格不是正數" },
      paperSessions: [
        paperSession({ status: "failed", statusMessage: "第 5 根 K 線的價格不是正數" }),
      ],
    });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(
      await screen.findByText(
        "模擬交易中止：第 5 根 K 線的價格不是正數（帳本已經不可信，請重新開始）",
      ),
    ).toBeInTheDocument();
    // 失敗訊息來自分頁列表（session.statusMessage），同步就顯示；完整帳本快照
    // 要等 paper_trading_status 查完才補上，兩者是獨立的非同步進度，要分開等。
    expect(await screen.findByText("10062.30")).toBeInTheDocument();
  });

  it("有多場模擬交易時顯示分頁，切換分頁會換掉顯示的那一場", async () => {
    const other = paperSession({
      id: "paper-2000-002",
      symbol: "ETHUSDT",
      status: "stopped",
      startedAtMs: 1_800_000_000_000, // 比 SESSION_ID 新，預設應該選到這一場
      statusMessage: null,
    });
    mockInvoke({ paperSessions: [paperSession({ status: "running" }), other] });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    const tablist = await screen.findByRole("tablist", { name: "模擬交易場次" });
    expect(tablist).toBeInTheDocument();
    const btcTab = screen.getByRole("tab", { name: /BTCUSDT/ });
    const ethTab = screen.getByRole("tab", { name: /ETHUSDT/ });
    // 預設選中最新開始的那一場（ETHUSDT，已停止）。
    expect(ethTab).toHaveAttribute("aria-selected", "true");
    expect(btcTab).toHaveAttribute("aria-selected", "false");
    expect(await screen.findByText("已停止模擬交易。")).toBeInTheDocument();

    fireEvent.click(btcTab);

    expect(btcTab).toHaveAttribute("aria-selected", "true");
    expect(await screen.findByRole("status")).toHaveTextContent("模擬交易執行中");
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("paper_trading_status", { sessionId: SESSION_ID }),
    );
  });

  it("找到同策略/交易對/週期的回測紀錄時疊圖比較並列出總報酬差距", async () => {
    const backtestRecord: SessionRecord = {
      schemaVersion: 1,
      id: "backtest-1",
      kind: "backtest",
      market: "spot",
      symbol: "BTCUSDT",
      interval: "1m",
      strategyId: "sma_cross",
      strategyName: "均線交叉",
      params: {},
      startedAtMs: 1_690_000_000_000,
      endedAtMs: 1_690_100_000_000,
      status: "completed",
      statusMessage: null,
      startingCapital: "10000",
      finalEquity: "10500",
      barsSeen: 100,
      metrics: {
        totalReturn: "0.05",
        annualizedReturn: null,
        maxDrawdown: "0.02",
        sharpe: null,
        spanYears: null,
      },
      saved: false,
      dataSourcePath: null,
    };
    mockInvoke({
      status: { status: "running", snapshot: SNAPSHOT },
      paperSessions: [paperSession({ status: "running" })],
      backtestSessions: [backtestRecord],
      curves: {
        [SESSION_ID]: [
          { openTime: 1_690_000_000_000, equity: "10000" },
          { openTime: 1_690_050_000_000, equity: "10100" },
          { openTime: 1_690_100_000_000, equity: "10200" },
        ],
        "backtest-1": [
          { openTime: 1_690_000_000_000, equity: "10000" },
          { openTime: 1_690_050_000_000, equity: "10250" },
          { openTime: 1_690_100_000_000, equity: "10500" },
        ],
      },
    });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(
      await screen.findByRole("heading", { name: /模擬 vs 回測/ }),
    ).toBeInTheDocument();
    expect(await screen.findByText("+2.0%")).toBeInTheDocument(); // 模擬報酬
    expect(screen.getByText("+5.0%")).toBeInTheDocument(); // 回測報酬
    expect(screen.getByText("−3.0%")).toBeInTheDocument(); // 總報酬差距
  });

  it("找不到同策略/交易對/週期的回測紀錄時誠實顯示找不到，不畫假的對比線", async () => {
    mockInvoke({
      status: { status: "running", snapshot: SNAPSHOT },
      paperSessions: [paperSession({ status: "running" })],
      backtestSessions: [],
    });
    mockListen();

    render(<PaperTrading strategyConfig={STRATEGY_CONFIG} onGoToStrategies={vi.fn()} />);

    expect(
      await screen.findByText("找不到相同策略/交易對/週期的回測紀錄可供比較。"),
    ).toBeInTheDocument();
  });
});
