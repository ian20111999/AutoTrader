import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

const BOTH_UNSET = { apiKeySet: false, apiSecretSet: false };
const BOTH_SET = { apiKeySet: true, apiSecretSet: true };

function baseHandlers(overrides: Record<string, (args?: unknown) => unknown> = {}) {
  return {
    binance_credentials_status: () => BOTH_UNSET,
    testnet_credentials_status: () => BOTH_UNSET,
    ...overrides,
  };
}

describe("Settings", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("載入時分開顯示正式環境與測試網兩組金鑰狀態（都未設定）", async () => {
    mockInvoke(baseHandlers());

    render(<Settings />);

    expect(await screen.findAllByText("未設定")).toHaveLength(4);
    expect(screen.getByRole("region", { name: "正式環境 API 金鑰" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "測試網 API 金鑰" })).toBeInTheDocument();
  });

  it("正式環境與測試網各自已設定時分開顯示「已設定」", async () => {
    mockInvoke(
      baseHandlers({
        binance_credentials_status: () => BOTH_SET,
        testnet_credentials_status: () => BOTH_UNSET,
      }),
    );

    render(<Settings />);

    const prodSection = await screen.findByRole("region", { name: "正式環境 API 金鑰" });
    const testnetSection = screen.getByRole("region", { name: "測試網 API 金鑰" });
    await waitFor(() => {
      expect(within(prodSection).getAllByText("已設定")).toHaveLength(2);
      expect(within(testnetSection).getAllByText("未設定")).toHaveLength(2);
    });
  });

  it("儲存正式環境金鑰呼叫 save_binance_credentials，不影響測試網狀態", async () => {
    let saved: { apiKey: string; apiSecret: string } | null = null;
    mockInvoke(
      baseHandlers({
        binance_credentials_status: () => ({
          apiKeySet: saved !== null,
          apiSecretSet: saved !== null,
        }),
        save_binance_credentials: (args) => {
          saved = args as { apiKey: string; apiSecret: string };
          return null;
        },
      }),
    );

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: FAKE_API_KEY } });
    fireEvent.change(screen.getByLabelText("API Secret"), { target: { value: FAKE_API_SECRET } });
    fireEvent.click(screen.getByRole("button", { name: "儲存" }));

    await waitFor(() => expect(saved).toEqual({ apiKey: FAKE_API_KEY, apiSecret: FAKE_API_SECRET }));
    expect(invoke).not.toHaveBeenCalledWith("save_testnet_credentials", expect.anything());
  });

  it("儲存測試網金鑰呼叫 save_testnet_credentials，不影響正式環境狀態", async () => {
    let saved: { apiKey: string; apiSecret: string } | null = null;
    mockInvoke(
      baseHandlers({
        testnet_credentials_status: () => ({
          apiKeySet: saved !== null,
          apiSecretSet: saved !== null,
        }),
        save_testnet_credentials: (args) => {
          saved = args as { apiKey: string; apiSecret: string };
          return null;
        },
      }),
    );

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.change(screen.getByLabelText("測試網 API Key"), {
      target: { value: FAKE_API_KEY },
    });
    fireEvent.change(screen.getByLabelText("測試網 API Secret"), {
      target: { value: FAKE_API_SECRET },
    });
    fireEvent.click(screen.getByRole("button", { name: "儲存測試網金鑰" }));

    await waitFor(() => expect(saved).toEqual({ apiKey: FAKE_API_KEY, apiSecret: FAKE_API_SECRET }));
    expect(invoke).not.toHaveBeenCalledWith("save_binance_credentials", expect.anything());
  });

  it("清除正式環境金鑰前會跳出確認對話框，取消就不會呼叫 clear_binance_credentials", async () => {
    mockInvoke(baseHandlers({ binance_credentials_status: () => BOTH_SET }));
    vi.spyOn(window, "confirm").mockReturnValue(false);

    render(<Settings />);
    await screen.findAllByText("已設定");

    fireEvent.click(screen.getByRole("button", { name: "清除已儲存的金鑰" }));

    expect(window.confirm).toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith("clear_binance_credentials");
  });

  it("權限清單一開始顯示尚未查詢，不會畫出假資料", async () => {
    mockInvoke(baseHandlers());

    render(<Settings />);
    await screen.findAllByText("未設定");

    expect(screen.getByText("尚未查詢，請按下方按鈕測試連線並查詢目前權限。")).toBeInTheDocument();
  });

  it("按下測試連線成功後顯示 check_account_permissions 查回來的真實權限", async () => {
    mockInvoke(
      baseHandlers({
        check_account_permissions: () => ({
          canTrade: true,
          canWithdraw: false,
          canDeposit: true,
        }),
      }),
    );

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.click(screen.getByRole("button", { name: "測試連線" }));

    await screen.findByText("連線成功，以下是查詢到的實際權限。");
    const permissionSection = screen.getByRole("region", { name: "權限檢查清單" });
    expect(within(permissionSection).getAllByText("已開啟")).toHaveLength(2);
    expect(within(permissionSection).getByText("已確認關閉")).toBeInTheDocument();
  });

  it("測試連線失敗時顯示錯誤訊息，不畫出假權限清單", async () => {
    mockInvoke(
      baseHandlers({
        check_account_permissions: () => {
          throw "讀取 API 憑證失敗";
        },
      }),
    );

    render(<Settings />);
    await screen.findAllByText("未設定");

    fireEvent.click(screen.getByRole("button", { name: "測試連線" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("測試連線失敗");
    expect(
      screen.getByText("尚未查詢，請按下方按鈕測試連線並查詢目前權限。"),
    ).toBeInTheDocument();
  });

  it("合約帳戶設定標示即將推出，不是假互動元件", async () => {
    mockInvoke(baseHandlers());

    render(<Settings />);
    await screen.findAllByText("未設定");

    const futuresSection = screen.getByRole("region", { name: "合約帳戶設定" });
    expect(within(futuresSection).getByText(/即將推出/)).toBeInTheDocument();
  });
});
