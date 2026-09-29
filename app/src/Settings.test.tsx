import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Settings } from "./Settings";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// 假的測試憑證字串，格式上一看就不是真實金鑰，測完也不會留在任何系統儲存區
// （Settings 元件只呼叫被 mock 掉的 invoke，不會真的碰 Keychain）。
const FAKE_API_KEY = "fake-test-key-12345";
const FAKE_API_SECRET = "fake-test-secret-67890";

function mockInvoke(handlers: Record<string, (args?: unknown) => unknown>) {
  vi.mocked(invoke).mockImplementation((async (cmd: string, args?: unknown) => {
    if (!(cmd in handlers)) throw new Error(`未預期的 command：${cmd}`);
    return handlers[cmd](args);
  }) as typeof invoke);
}

describe("Settings", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("載入時顯示目前的連線狀態（都未設定）", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: false, apiSecretSet: false }),
    });

    render(<Settings />);

    const items = await screen.findAllByText("未設定");
    expect(items).toHaveLength(2);
  });

  it("兩個欄位都已設定時顯示「已設定」", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: true, apiSecretSet: true }),
    });

    render(<Settings />);

    expect(await screen.findAllByText("已設定")).toHaveLength(2);
  });

  it("查詢狀態失敗時顯示錯誤訊息", async () => {
    mockInvoke({
      binance_credentials_status: () => {
        throw "查詢失敗";
      },
    });

    render(<Settings />);

    expect(await screen.findByRole("alert")).toHaveTextContent("讀取連線狀態失敗");
  });

  it("輸入框是 password 類型（遮蔽輸入）", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: false, apiSecretSet: false }),
    });

    render(<Settings />);
    await screen.findAllByText("未設定");

    expect(screen.getByLabelText("API Key")).toHaveAttribute("type", "password");
    expect(screen.getByLabelText("API Secret")).toHaveAttribute("type", "password");
  });

  it("送出表單會呼叫 save_binance_credentials，成功後清空輸入框並重新整理狀態", async () => {
    let saved: { apiKey: string; apiSecret: string } | null = null;
    mockInvoke({
      binance_credentials_status: () => ({
        apiKeySet: saved !== null,
        apiSecretSet: saved !== null,
      }),
      save_binance_credentials: (args) => {
        saved = args as { apiKey: string; apiSecret: string };
        return null;
      },
    });

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: FAKE_API_KEY } });
    fireEvent.change(screen.getByLabelText("API Secret"), { target: { value: FAKE_API_SECRET } });
    fireEvent.click(screen.getByRole("button", { name: "儲存" }));

    await screen.findByText("已儲存。");

    expect(saved).toEqual({ apiKey: FAKE_API_KEY, apiSecret: FAKE_API_SECRET });
    expect(screen.getByLabelText("API Key")).toHaveValue("");
    expect(screen.getByLabelText("API Secret")).toHaveValue("");
    await waitFor(() => expect(screen.getAllByText("已設定")).toHaveLength(2));
  });

  it("留白就送出表單不會呼叫 save_binance_credentials（HTML5 required 擋下）", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: false, apiSecretSet: false }),
    });

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.click(screen.getByRole("button", { name: "儲存" }));

    expect(invoke).not.toHaveBeenCalledWith("save_binance_credentials", expect.anything());
  });

  it("儲存失敗時顯示 Rust 端回傳的錯誤訊息，不清空輸入框", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: false, apiSecretSet: false }),
      save_binance_credentials: () => {
        throw "API Key 不能是空白";
      },
    });

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: FAKE_API_KEY } });
    fireEvent.change(screen.getByLabelText("API Secret"), { target: { value: FAKE_API_SECRET } });
    fireEvent.click(screen.getByRole("button", { name: "儲存" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("儲存失敗");
    expect(screen.getByLabelText("API Key")).toHaveValue(FAKE_API_KEY);
  });

  it("點擊清除前會先跳出確認對話框，取消就不會呼叫 clear_binance_credentials", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: true, apiSecretSet: true }),
    });
    vi.spyOn(window, "confirm").mockReturnValue(false);

    render(<Settings />);
    await screen.findAllByText("已設定");

    fireEvent.click(screen.getByRole("button", { name: "清除已儲存的金鑰" }));

    expect(window.confirm).toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith("clear_binance_credentials");
  });

  it("確認清除後呼叫 clear_binance_credentials 並重新整理狀態為未設定", async () => {
    let cleared = false;
    mockInvoke({
      binance_credentials_status: () => ({
        apiKeySet: !cleared,
        apiSecretSet: !cleared,
      }),
      clear_binance_credentials: () => {
        cleared = true;
        return null;
      },
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<Settings />);
    await screen.findAllByText("已設定");

    fireEvent.click(screen.getByRole("button", { name: "清除已儲存的金鑰" }));

    await waitFor(() => expect(screen.getAllByText("未設定")).toHaveLength(2));
    expect(invoke).toHaveBeenCalledWith("clear_binance_credentials");
  });

  it("清除失敗時顯示錯誤訊息", async () => {
    mockInvoke({
      binance_credentials_status: () => ({ apiKeySet: true, apiSecretSet: true }),
      clear_binance_credentials: () => {
        throw "清除失敗";
      },
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<Settings />);
    await screen.findAllByText("已設定");

    fireEvent.click(screen.getByRole("button", { name: "清除已儲存的金鑰" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("清除失敗");
  });
});
