import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { BuiltinStrategies } from "./BuiltinStrategies";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

describe("BuiltinStrategies", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("顯示 Rust 端 list_builtin_strategies 回傳的名稱與參數預設值", async () => {
    vi.mocked(invoke).mockResolvedValue([
      {
        id: "sma_cross",
        name: "均線交叉",
        params: [
          { key: "fastPeriod", label: "快線週期", kind: "integer", default: "10" },
          { key: "slowPeriod", label: "慢線週期", kind: "integer", default: "50" },
        ],
      },
    ]);

    render(<BuiltinStrategies />);

    expect(await screen.findByText(/均線交叉/)).toBeInTheDocument();
    expect(screen.getByText(/快線週期=10/)).toBeInTheDocument();
    expect(screen.getByText(/慢線週期=50/)).toBeInTheDocument();
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

    render(<BuiltinStrategies />);

    expect(await screen.findByText(/快線週期=999/)).toBeInTheDocument();
  });

  it("呼叫失敗時顯示錯誤訊息", async () => {
    vi.mocked(invoke).mockRejectedValue("something broke");

    render(<BuiltinStrategies />);

    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});
