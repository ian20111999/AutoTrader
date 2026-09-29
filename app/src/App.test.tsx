import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

describe("App", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockResolvedValue([]);
  });

  it("顯示側邊導覽的四個分頁項目", () => {
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "主選單" });
    expect(nav).toBeInTheDocument();
    for (const label of ["策略庫", "回測", "比較", "設定"]) {
      expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
    }
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

  it("點擊「比較」與「設定」都能各自顯示對應內容", () => {
    render(<App />);

    fireEvent.click(screen.getByRole("button", { name: "比較" }));
    expect(screen.getByRole("heading", { name: "比較" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "設定" }));
    expect(screen.getByRole("heading", { name: "設定" })).toBeInTheDocument();
    expect(screen.getByText(/^自動交易台 v/)).toBeInTheDocument();
  });
});
