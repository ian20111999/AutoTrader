import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { StrategyParamForm } from "./StrategyParamForm";
import type { StrategyInfo } from "./strategyTypes";

const SMA_CROSS: StrategyInfo = {
  id: "sma_cross",
  name: "均線交叉",
  params: [
    { key: "fastPeriod", label: "快線週期", kind: "integer", default: "10" },
    { key: "slowPeriod", label: "慢線週期", kind: "integer", default: "50" },
  ],
};

const BOLLINGER: StrategyInfo = {
  id: "bollinger",
  name: "布林通道",
  params: [
    { key: "period", label: "週期", kind: "integer", default: "20" },
    { key: "multiplier", label: "標準差倍數", kind: "decimal", default: "2" },
  ],
};

const RSI: StrategyInfo = {
  id: "rsi",
  name: "RSI",
  params: [
    { key: "period", label: "週期", kind: "integer", default: "14" },
    { key: "buyBelow", label: "進場門檻", kind: "decimal", default: "30" },
    { key: "exitAbove", label: "出場門檻", kind: "decimal", default: "70" },
  ],
};

function defaultValuesOf(strategy: StrategyInfo): Record<string, string> {
  return Object.fromEntries(strategy.params.map((p) => [p.key, p.default]));
}

describe("StrategyParamForm", () => {
  it("依 params schema 動態產生對應數量與 label 的欄位（不是為某個策略寫死的表單）", () => {
    render(
      <StrategyParamForm
        strategy={RSI}
        initialValues={defaultValuesOf(RSI)}
        onApply={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("週期")).toHaveValue("14");
    expect(screen.getByLabelText("進場門檻")).toHaveValue("30");
    expect(screen.getByLabelText("出場門檻")).toHaveValue("70");
    expect(screen.getAllByRole("textbox")).toHaveLength(3);
  });

  it("合法值變更時呼叫 onApply 帶上更新後的 state", () => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={SMA_CROSS}
        initialValues={defaultValuesOf(SMA_CROSS)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("快線週期"), { target: { value: "5" } });
    fireEvent.change(screen.getByLabelText("慢線週期"), { target: { value: "20" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).toHaveBeenCalledWith({ fastPeriod: "5", slowPeriod: "20" });
  });

  it.each([
    ["空白", ""],
    ["非數字", "abc"],
    ["零", "0"],
    ["負數", "-1"],
    ["小數", "1.5"],
  ])("integer 欄位輸入%s時擋下並顯示錯誤，不呼叫 onApply", (_label, badValue) => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={SMA_CROSS}
        initialValues={defaultValuesOf(SMA_CROSS)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("快線週期"), { target: { value: badValue } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).not.toHaveBeenCalled();
    const input = screen.getByLabelText("快線週期");
    expect(input).toHaveAttribute("aria-invalid", "true");
    const errorId = input.getAttribute("aria-describedby");
    expect(errorId).toBeTruthy();
    expect(document.getElementById(errorId as string)).toHaveTextContent(/./);
  });

  it("均線交叉：快線不小於慢線時顯示錯誤訊息，不呼叫 onApply", () => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={SMA_CROSS}
        initialValues={defaultValuesOf(SMA_CROSS)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("快線週期"), { target: { value: "50" } });
    fireEvent.change(screen.getByLabelText("慢線週期"), { target: { value: "50" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).not.toHaveBeenCalled();
    expect(screen.getByText("快線週期必須短於慢線週期")).toBeInTheDocument();
  });

  it("布林通道：標準差倍數不能小於等於 0", () => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={BOLLINGER}
        initialValues={defaultValuesOf(BOLLINGER)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("標準差倍數"), { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).not.toHaveBeenCalled();
    expect(screen.getByText("標準差倍數必須大於 0")).toBeInTheDocument();
  });

  it("RSI：門檻超出 0～100 範圍時顯示錯誤", () => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={RSI}
        initialValues={defaultValuesOf(RSI)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("進場門檻"), { target: { value: "120" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).not.toHaveBeenCalled();
    expect(screen.getByText("RSI 門檻必須在 0 到 100 之間")).toBeInTheDocument();
  });

  it("RSI：進場門檻不低於出場門檻時顯示錯誤", () => {
    const onApply = vi.fn();
    render(
      <StrategyParamForm
        strategy={RSI}
        initialValues={defaultValuesOf(RSI)}
        onApply={onApply}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("進場門檻"), { target: { value: "80" } });
    fireEvent.click(screen.getByRole("button", { name: "套用參數" }));

    expect(onApply).not.toHaveBeenCalled();
    expect(screen.getByText("RSI 進場門檻必須低於出場門檻")).toBeInTheDocument();
  });

  it("點「返回策略庫」呼叫 onCancel", () => {
    const onCancel = vi.fn();
    render(
      <StrategyParamForm
        strategy={SMA_CROSS}
        initialValues={defaultValuesOf(SMA_CROSS)}
        onApply={vi.fn()}
        onCancel={onCancel}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "返回策略庫" }));

    expect(onCancel).toHaveBeenCalled();
  });
});
