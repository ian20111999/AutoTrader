import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { SideNav } from "./SideNav";

describe("SideNav", () => {
  it("顯示研究／交易／系統三個分組標題", () => {
    render(<SideNav active="strategies" onSelect={vi.fn()} />);

    expect(screen.getByText("研究")).toBeInTheDocument();
    expect(screen.getByText("交易")).toBeInTheDocument();
    expect(screen.getByText("系統")).toBeInTheDocument();
  });

  it("已實作的項目可以點擊並觸發 onSelect", () => {
    const onSelect = vi.fn();
    render(<SideNav active="strategies" onSelect={onSelect} />);

    fireEvent.click(screen.getByRole("button", { name: /回測/ }));

    expect(onSelect).toHaveBeenCalledWith("backtest");
  });

  it("「總覽」已接上頁面，點擊會觸發 onSelect", () => {
    const onSelect = vi.fn();
    render(<SideNav active="strategies" onSelect={onSelect} />);

    fireEvent.click(screen.getByRole("button", { name: "總覽" }));

    expect(onSelect).toHaveBeenCalledWith("overview");
  });

  it("「策略編輯器」已接上頁面（Phase F3），點擊會觸發 onSelect", () => {
    const onSelect = vi.fn();
    render(<SideNav active="strategies" onSelect={onSelect} />);

    const button = screen.getByRole("button", { name: /策略編輯器/ });
    expect(button).not.toBeDisabled();

    fireEvent.click(button);

    expect(onSelect).toHaveBeenCalledWith("strategyEditor");
  });

  it.each([
    "部署",
    "即時交易",
    "高頻監控",
    "風控",
  ])("「%s」是 disabled 狀態，點了不會觸發 onSelect", (label) => {
    const onSelect = vi.fn();
    render(<SideNav active="strategies" onSelect={onSelect} />);

    const button = screen.getByRole("button", { name: new RegExp(label) });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("title", "即將推出");

    fireEvent.click(button);

    expect(onSelect).not.toHaveBeenCalled();
  });

  it("disabled 項目顯示「即將推出」提示文字", () => {
    render(<SideNav active="strategies" onSelect={vi.fn()} />);

    expect(screen.getAllByText("即將推出").length).toBeGreaterThan(0);
  });
});
