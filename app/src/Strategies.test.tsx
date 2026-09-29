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

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} />);

    expect(await screen.findByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.getByText("快線週期10、慢線週期50")).toBeInTheDocument();
    expect(screen.getByText(/布林通道/)).toBeInTheDocument();
    expect(screen.getByText(/唐奇安突破/)).toBeInTheDocument();
    expect(screen.getByText(/^RSI$/)).toBeInTheDocument();
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

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} />);

    expect(await screen.findByText("快線週期999")).toBeInTheDocument();
  });

  it("點擊卡片會標示為選中狀態", async () => {
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} />);

    const card = await screen.findByRole("button", { name: /^均線交叉/ });
    expect(card).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(card);

    expect(card).toHaveAttribute("aria-pressed", "true");
  });

  it("點擊「調整參數」會切換到該策略的調參表單", async () => {
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "調整均線交叉參數" }));

    expect(screen.getByRole("heading", { name: "均線交叉" })).toBeInTheDocument();
    expect(screen.getByLabelText("快線週期")).toHaveValue("10");
    expect(screen.getByLabelText("慢線週期")).toHaveValue("50");
    // 只顯示被點的那個策略的表單，不是四個策略都渲染。
    expect(screen.queryByText("布林通道")).not.toBeInTheDocument();
  });

  it("套用調參表單後回到列表、呼叫 onApplyConfig、並選中該策略", async () => {
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);
    const onApplyConfig = vi.fn();

    render(<Strategies strategyConfig={null} onApplyConfig={onApplyConfig} />);

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
    vi.mocked(invoke).mockResolvedValue(FOUR_BUILTIN_STRATEGIES);
    const onApplyConfig = vi.fn();

    render(<Strategies strategyConfig={null} onApplyConfig={onApplyConfig} />);

    fireEvent.click(await screen.findByRole("button", { name: "調整均線交叉參數" }));
    fireEvent.click(screen.getByRole("button", { name: "返回策略庫" }));

    expect(onApplyConfig).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^均線交叉/ })).toBeInTheDocument();
  });

  it("呼叫失敗時顯示錯誤訊息", async () => {
    vi.mocked(invoke).mockRejectedValue("something broke");

    render(<Strategies strategyConfig={null} onApplyConfig={vi.fn()} />);

    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});
