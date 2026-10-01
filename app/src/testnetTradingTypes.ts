// 跟 app/src-tauri/src/testnet_trading.rs 的 StartTestnetTradingRequest/
// TestnetSnapshotDto/OrderOutcomeDto/TestnetUpdateEvent/TestnetTradingStatusDto
// 對應。金額欄位是 Fixed 的字串表示，理由同 paperTradingTypes.ts。

export interface StartTestnetTradingRequest {
  symbol: string;
  interval: string;
  strategyId: string;
  params: Record<string, string>;
  initialCash: string;
  maxDailyLoss: string;
  maxOrderNotional: string;
}

export interface TestnetSnapshot {
  openTime: number;
  equity: string;
  cash: string;
  position: string;
  fills: number;
  blocked: number;
  feesPaid: string;
  dailyPnl: string | null;
  killSwitch: boolean;
}

// Rust 端用 #[serde(tag = "type", rename_all = "camelCase")]。
export type OrderOutcome =
  | { type: "blocked"; message: string }
  | { type: "invalid"; message: string }
  | {
      type: "filled";
      orderId: number;
      side: string;
      requestedQty: string;
      executedQty: string;
      quoteQty: string;
      fee: string;
      status: string;
    };

export type TestnetUpdateEvent =
  | { type: "order"; outcome: OrderOutcome }
  | { type: "bar"; snapshot: TestnetSnapshot }
  | { type: "stopped" }
  | { type: "failed"; message: string };

export type TestnetTradingStatus =
  | { status: "idle" }
  | { status: "running"; snapshot: TestnetSnapshot | null }
  | { status: "stopped"; snapshot: TestnetSnapshot | null }
  | { status: "failed"; snapshot: TestnetSnapshot | null; message: string };

// Rust 端 app.emit 用的事件名稱。
export const TESTNET_TRADING_EVENT = "testnet-trading-update";
