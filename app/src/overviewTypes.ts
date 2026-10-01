// 跟 app/src-tauri/src/sessions.rs 的 LiveSessionDto/SessionFilterDto 對應，
// 以及 crates/session-store/src/record.rs 的 SessionRecord（serde camelCase）。
// 這是這兩個 command 第一個前端消費端（ADR-001 Phase C）。

export interface LiveSessionDto {
  sessionId: string;
  kind: "backtest" | "paper" | "testnet" | "live";
  market: "spot" | "usdmPerp";
  symbol: string;
  interval: string;
  strategyId: string;
  strategyName: string;
  status: "running";
  startedAtMs: number;
  equity: string | null;
  position: string;
  dailyPnl: string | null;
  asOfMs: number | null;
  stale: boolean;
  killSwitch: boolean | null;
  barsSeen: number;
}

export interface SessionFilterDto {
  kinds?: string[];
  strategyId?: string;
  symbol?: string;
  sinceMs?: number;
  untilMs?: number;
  savedOnly?: boolean;
  limit?: number;
}

export type SessionStatus = "running" | "stopped" | "failed" | "completed" | "interrupted";

export interface SessionMetrics {
  totalReturn: string | null;
  annualizedReturn: string | null;
  maxDrawdown: string | null;
  sharpe: string | null;
  spanYears: string | null;
}

// 歷史查詢（list_sessions）回傳的一筆紀錄，對應 `SessionRecord`。總覽只用得到
// 其中一部分欄位，但型別照 Rust 端完整定義，避免之後要用到新欄位又要回頭改。
export interface SessionRecord {
  schemaVersion: number;
  id: string;
  kind: "backtest" | "paper" | "testnet" | "live";
  market: "spot" | "usdmPerp";
  symbol: string;
  interval: string;
  strategyId: string;
  strategyName: string;
  params: Record<string, string>;
  startedAtMs: number;
  endedAtMs: number | null;
  status: SessionStatus;
  statusMessage: string | null;
  startingCapital: string;
  finalEquity: string | null;
  barsSeen: number;
  metrics: SessionMetrics | null;
  saved: boolean;
  dataSourcePath: string | null;
}
