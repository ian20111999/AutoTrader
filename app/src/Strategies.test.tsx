import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
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

describe("Strategies", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("顯示 Rust 端 list_builtin_strategies 回傳的四張策略卡片與參數摘要", async () => {
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);

    render(<Strategies />);

    expect(await screen.findByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.getByText("快線週期10、慢線週期50")).toBeInTheDocument();
    expect(screen.getByText(/布林通道/)).toBeInTheDocument();
    expect(screen.getByText(/唐奇安突破/)).toBeInTheDocument();
    expect(screen.getByText(/^RSI$/)).toBeInTheDocument();
    expect(screen.getAllByRole("button")).toHaveLength(4);
    expect(invoke).toHaveBeenCalledWith("list_builtin_strategies");
  });

  it("改變 Rust 端回傳的預設值時，畫面顯示要跟著變（不是前端寫死）", async () => {
    vi.mocked(invoke).mockResolvedValue([
      {
        id: "sma_cross",
        name: "均線交叉",
        params: [{ key: "fastPeriod", label: "快線週期", kind: "integer", default: "999" }],
      },
    ]);

    render(<Strategies />);

    expect(await screen.findByText("快線週期999")).toBeInTheDocument();
  });

  it("點擊卡片會標示為選中狀態", async () => {
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);

    render(<Strategies />);

    const card = await screen.findByRole("button", { name: /均線交叉/ });
    expect(card).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(card);

    expect(card).toHaveAttribute("aria-pressed", "true");
  });

  it("呼叫失敗時顯示錯誤訊息", async () => {
    vi.mocked(invoke).mockRejectedValue("something broke");

    render(<Strategies />);

    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});
