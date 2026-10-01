// 跟 app/src-tauri/src/paper_trading.rs 的 StartPaperTradingRequest/PaperSnapshotDto/
// PaperUpdateEvent/PaperTradingStatusDto 對應。equity/cash/position 是 Fixed 的字串
// 表示，跟 backtestTypes.ts 的 EquityPoint 同一個理由：避免浮點誤差。

export interface StartPaperTradingRequest {
  symbol: string;
  interval: string;
  strategyId: string;
  params: Record<string, string>;
  startingCapital: string;
}

export interface PaperSnapshot {
  openTime: number;
  equity: string;
  cash: string;
  position: string;
  trades: number;
  liquidations: number;
}

// Rust 端用 #[serde(tag = "type", rename_all = "camelCase")]，變體名稱也會被轉成
// camelCase（單一單字時等於小寫），跟這裡的字面值一一對應。
export type PaperUpdateEvent =
  | { type: "bar"; snapshot: PaperSnapshot }
  | { type: "stopped" }
  | { type: "failed"; message: string };

// Rust 端的事件 payload 外層多包一個 sessionId（#[serde(flatten)]），
// 前端用它過濾只處理自己這場 session 的事件（ADR-001 §7.3）。
export type PaperUpdateEnvelope = PaperUpdateEvent & { sessionId: string };

export type PaperTradingStatus =
  | { status: "idle" }
  | { status: "running"; snapshot: PaperSnapshot | null }
  | { status: "stopped"; snapshot: PaperSnapshot | null }
  | { status: "failed"; snapshot: PaperSnapshot | null; message: string };

// Rust 端 app.emit 用的事件名稱。
export const PAPER_TRADING_EVENT = "paper-trading-update";
