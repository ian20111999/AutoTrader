import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// 模擬交易頁面會呼叫 listen()；測試環境沒有真的 Tauri IPC，這裡回傳一個不做事的
// unlisten function，避免未處理的 promise rejection。
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

describe("App", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockResolvedValue([]);
  });

  it("顯示側邊導覽的分頁項目", () => {
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "主選單" });
    expect(nav).toBeInTheDocument();
    for (const label of ["總覽", "策略庫", "回測", "比較", "模擬交易", "設定"]) {
      expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
    }
  });

  it("點擊「總覽」切換到總覽分頁", () => {
    render(<App />);

    fireEvent.click(screen.getByRole("button", { name: "總覽" }));

    expect(screen.getByRole("heading", { name: "總覽" })).toBeInTheDocument();
  });

  it("預設顯示策略庫分頁，TopBar標題為「策略庫」", () => {
    render(<App />);
    expect(screen.getByRole("heading", { name: "策略庫" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "策略庫" })).toHaveAttribute("aria-current", "page");
  });

  it("點擊「回測」後切換內容，TopBar標題跟著變、策略庫內容消失", () => {
    render(<App />);

    fireEvent.click(screen.getByRole("button", { name: "回測" }));

    expect(screen.getByRole("heading", { name: "回測" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "回測" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByText(/讀取中|讀取內建策略失敗/)).not.toBeInTheDocument();
  });

  it("點擊「比較」「模擬交易」與「設定」都能各自顯示對應內容", () => {
    render(<App />);

    fireEvent.click(screen.getByRole("button", { name: "比較" }));
    expect(screen.getByRole("heading", { name: "比較" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "模擬交易" }));
    expect(screen.getByRole("heading", { name: "模擬交易" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "設定" }));
    expect(screen.getByRole("heading", { name: "設定" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "正式環境 API 金鑰" })).toBeInTheDocument();
  });

  it("點擊「風控」切換到風控頁，顯示熔斷規則與觸發紀錄", () => {
    render(<App />);

    fireEvent.click(screen.getByRole("button", { name: /風控/ }));

    expect(screen.getByRole("heading", { name: "風控" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "熔斷規則" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "觸發紀錄" })).toBeInTheDocument();
  });
});
