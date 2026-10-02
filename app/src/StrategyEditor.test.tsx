import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { StrategyEditor } from "./StrategyEditor";
import type { DslValidationResult } from "./strategyDslTypes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockedInvoke = invoke as unknown as ReturnType<typeof vi.fn>;

function mockValidation(result: DslValidationResult) {
  mockedInvoke.mockImplementation((command: string) => {
    if (command === "validate_strategy_ast") return Promise.resolve(result);
    return Promise.reject(new Error(`未預期的 command：${command}`));
  });
}

describe("StrategyEditor", () => {
  beforeEach(() => {
    localStorage.clear();
    mockedInvoke.mockReset();
  });

  it("預設積木樹通過後端驗證後顯示綠色訊息，並啟用送出按鈕", async () => {
    mockValidation({ valid: true, error: null });
    render(<StrategyEditor onUseInBacktest={vi.fn()} onUseInPaperTrading={vi.fn()} />);

    await waitFor(() =>
      expect(screen.getByText("✓ 這份策略通過後端驗證，可以使用")).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "送去回測" })).toBeEnabled();
  });

  it("後端回報不合法時顯示紅字錯誤，送出按鈕停用", async () => {
    mockValidation({ valid: false, error: "做多進場條件 → 週期必須在 1 到 2000 之間" });
    render(<StrategyEditor onUseInBacktest={vi.fn()} onUseInPaperTrading={vi.fn()} />);

    await waitFor(() =>
      expect(
        screen.getByText("✗ 做多進場條件 → 週期必須在 1 到 2000 之間"),
      ).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "送去回測" })).toBeDisabled();
  });

  it("切換方向到多空時出現做空進場／出場積木", async () => {
    mockValidation({ valid: true, error: null });
    render(<StrategyEditor onUseInBacktest={vi.fn()} onUseInPaperTrading={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/這份策略通過後端驗證/)).toBeInTheDocument());

    expect(screen.queryByText("做空進場")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("方向"), { target: { value: "long_short" } });

    expect(screen.getByText("做空進場")).toBeInTheDocument();
    expect(screen.getByText("做空出場")).toBeInTheDocument();
    // 多空策略不能送去模擬交易（模擬交易目前只支援現貨只做多）。
    expect(screen.getByRole("button", { name: "送去模擬交易" })).toBeDisabled();
  });

  it("驗證通過後點「送去回測」把 dslJson 帶出去", async () => {
    mockValidation({ valid: true, error: null });
    const onUseInBacktest = vi.fn();
    render(<StrategyEditor onUseInBacktest={onUseInBacktest} onUseInPaperTrading={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "送去回測" })).toBeEnabled(),
    );

    fireEvent.change(screen.getByLabelText("策略名稱"), { target: { value: "我的策略" } });
    fireEvent.click(screen.getByRole("button", { name: "送去回測" }));

    expect(onUseInBacktest).toHaveBeenCalledTimes(1);
    const config = onUseInBacktest.mock.calls[0][0];
    expect(config.strategyId).toBe("custom");
    expect(config.dslName).toBe("我的策略");
    const ast = JSON.parse(config.dslJson);
    expect(ast.direction).toBe("long_only");
    expect(ast.longEntry.kind).toBe("gt");
  });

  it("沒輸入名稱時儲存會擋下並提示", async () => {
    mockValidation({ valid: true, error: null });
    render(<StrategyEditor onUseInBacktest={vi.fn()} onUseInPaperTrading={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/這份策略通過後端驗證/)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "儲存到本機瀏覽器" }));

    expect(screen.getByText("請先輸入策略名稱才能儲存")).toBeInTheDocument();
  });

  it("輸入名稱儲存後出現在本機清單，可以刪除", async () => {
    mockValidation({ valid: true, error: null });
    render(<StrategyEditor onUseInBacktest={vi.fn()} onUseInPaperTrading={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/這份策略通過後端驗證/)).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText("策略名稱"), { target: { value: "均線改良版" } });
    fireEvent.click(screen.getByRole("button", { name: "儲存到本機瀏覽器" }));

    expect(screen.getByText("均線改良版")).toBeInTheDocument();
    expect(screen.getByText(/已儲存到本機瀏覽器/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "刪除" }));
    expect(screen.getByText("還沒有在這台電腦儲存過策略。")).toBeInTheDocument();
  });
});
