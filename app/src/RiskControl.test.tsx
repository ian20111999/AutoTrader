import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { RiskControl } from "./RiskControl";
import type { BreakerRule, RiskEvent, RiskStatus } from "./riskControlTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

function rule(overrides: Partial<BreakerRule> = {}): BreakerRule {
  return {
    id: "consecutiveLosses",
    condition: "連續虧損 5 筆",
    action: "暫停這個策略",
    enabled: true,
    mandatory: false,
    note: "一「筆」= 一次從空手到空手的來回。",
    ...overrides,
  };
}

const MANDATORY_RULE = rule({
  id: "reconciliationMismatch",
  condition: "對帳不一致",
  action: "暫停整個帳戶的下單",
  mandatory: true,
  note: "強制開啟。目前的輸入只有四種訊號。",
});

const EVENT: RiskEvent = {
  seq: 7,
  // 2024-10-04T03:02:00Z
  atMs: 20_000 * 86_400_000 + 3 * 3_600_000 + 2 * 60_000,
  clock: "交易所時間",
  session: "testnet-1",
  symbol: "BTCUSDT",
  message: "熔斷觸發：連續虧損 5 筆 → 暫停這個策略",
};

const IDLE_STATUS: RiskStatus = {
  takenAtMs: 1_000,
  accountPause: null,
  sessions: [],
};

/// 每個 command 各自的回傳；沒列到的 command 一律讓測試失敗（比回 undefined
/// 然後在畫面上看到奇怪的結果好找問題）。
function mockCommands(overrides: Record<string, unknown> = {}) {
  const responses: Record<string, unknown> = {
    get_breaker_rules: [rule(), MANDATORY_RULE],
    list_risk_events: [],
    risk_control_status: IDLE_STATUS,
    ...overrides,
  };
  vi.mocked(invoke).mockImplementation((command: string) => {
    if (command in responses) {
      const value = responses[command];
      return value instanceof Error ? Promise.reject(value) : Promise.resolve(value);
    }
    return Promise.reject(new Error(`測試沒有準備 ${command} 的回傳`));
  });
}

describe("RiskControl", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("顯示五條熔斷規則的條件與動作", async () => {
    mockCommands();
    render(<RiskControl />);

    expect(await screen.findByText(/當 連續虧損 5 筆 → 暫停這個策略/)).toBeInTheDocument();
    expect(screen.getByText(/當 對帳不一致 → 暫停整個帳戶的下單/)).toBeInTheDocument();
  });

  it("強制開啟的規則沒有可以關掉的控制元件", async () => {
    mockCommands();
    render(<RiskControl />);
    await screen.findByText(/當 對帳不一致/);

    // 可選的那條有 checkbox，強制的那條只有「強制開啟」標籤。
    expect(screen.getAllByRole("checkbox")).toHaveLength(1);
    expect(screen.getByLabelText("強制開啟")).toBeInTheDocument();
  });

  it("關掉一條規則會呼叫 set_breaker_rule 並套用回傳的新狀態", async () => {
    mockCommands({
      set_breaker_rule: [rule({ enabled: false }), MANDATORY_RULE],
    });
    render(<RiskControl />);
    const toggle = await screen.findByRole("checkbox");
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_breaker_rule", {
        triggerId: "consecutiveLosses",
        enabled: false,
      }),
    );
    await waitFor(() => expect(screen.getByRole("checkbox")).not.toBeChecked());
    expect(screen.getByText("已關閉")).toBeInTheDocument();
  });

  it("後端拒絕關閉強制規則時顯示錯誤，不是悄悄失敗", async () => {
    mockCommands({
      set_breaker_rule: new Error("「對帳不一致」是強制開啟的規則，不能關閉"),
    });
    render(<RiskControl />);
    const toggle = await screen.findByRole("checkbox");

    fireEvent.click(toggle);

    expect(await screen.findByRole("alert")).toHaveTextContent("強制開啟");
  });

  it("觸發紀錄顯示時間、策略與事件", async () => {
    mockCommands({ list_risk_events: [EVENT] });
    render(<RiskControl />);

    expect(await screen.findByText("10/04 03:02")).toBeInTheDocument();
    expect(screen.getByText(/testnet-1・BTCUSDT/)).toBeInTheDocument();
    expect(
      screen.getByText("熔斷觸發：連續虧損 5 筆 → 暫停這個策略"),
    ).toBeInTheDocument();
  });

  it("沒有觸發紀錄時明說，不留空白", async () => {
    mockCommands();
    render(<RiskControl />);

    expect(await screen.findByText("還沒有任何觸發紀錄。")).toBeInTheDocument();
  });

  it("帳戶層被熔斷暫停時顯示警示，並說明怎麼解除", async () => {
    mockCommands({
      risk_control_status: {
        ...IDLE_STATUS,
        accountPause: "對帳不一致 → 暫停整個帳戶的下單",
      } satisfies RiskStatus,
    });
    render(<RiskControl />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("整個帳戶的下單已被熔斷暫停");
    expect(alert).toHaveTextContent("重新啟動 App");
  });

  it("全域曝險顯示每一場執行中策略，並標明熔斷狀態", async () => {
    mockCommands({
      risk_control_status: {
        takenAtMs: 2_000,
        accountPause: null,
        sessions: [
          {
            sessionId: "testnet-1",
            kind: "Testnet",
            symbol: "BTCUSDT",
            position: "0.5",
            equity: "10050.5",
            dailyPnl: "-25.25",
            killSwitch: false,
            asOfMs: 20_000 * 86_400_000,
            breakerTrip: "連續虧損 5 筆 → 暫停這個策略",
          },
        ],
      } satisfies RiskStatus,
    });
    render(<RiskControl />);

    expect(await screen.findByText("BTCUSDT")).toBeInTheDocument();
    expect(screen.getByText("10050.50")).toBeInTheDocument();
    expect(screen.getByText("−25.25")).toBeInTheDocument();
    expect(
      screen.getByText("熔斷已觸發：連續虧損 5 筆 → 暫停這個策略"),
    ).toBeInTheDocument();
  });

  it("還沒生效的全域上限與合約風控都標成「尚未生效」", async () => {
    mockCommands();
    render(<RiskControl />);
    await screen.findByText(/當 連續虧損 5 筆/);

    // 七項全域上限裡有兩項生效、五項尚未生效；合約風控六項全部尚未生效。
    expect(screen.getAllByText("生效中")).toHaveLength(2);
    expect(screen.getAllByText("尚未生效")).toHaveLength(11);
    // 合約風控不可以顯示任何看起來像設定值的數字（例如預設槓桿 2 倍）。
    expect(screen.queryByText(/2 倍/)).not.toBeInTheDocument();
  });

  it("讀取規則失敗時顯示錯誤訊息", async () => {
    mockCommands({ get_breaker_rules: new Error("風控規則讀不到") });
    render(<RiskControl />);

    expect(await screen.findByText(/風控規則讀不到/)).toBeInTheDocument();
  });
});
