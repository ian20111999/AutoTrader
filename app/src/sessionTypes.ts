// 跟 app/src-tauri/src/sessions.rs 的 SessionFilterDto/LiveSessionDto、
// crates/session-store/src/record.rs 的 SessionRecord/SessionMetrics 對應。
// 金額/報酬率一律是字串（Rust 端 Fixed 沒有 Serialize，字串是這個專案跨
// 邊界傳 Fixed 的既有慣例）。

export type SessionKind = "backtest" | "paper" | "testnet" | "live";

export interface SessionFilterDto {
  kinds?: SessionKind[];
  strategyId?: string;
  symbol?: string;
  sinceMs?: number;
  untilMs?: number;
  savedOnly?: boolean;
  limit?: number;
}

export interface SessionMetrics {
  totalReturn: string | null;
  annualizedReturn: string | null;
  maxDrawdown: string | null;
  sharpe: string | null;
  spanYears: string | null;
}

export interface SessionRecord {
  schemaVersion: number;
  id: string;
  kind: SessionKind;
  market: "spot" | "usdmPerp";
  symbol: string;
  interval: string;
  strategyId: string;
  strategyName: string;
  params: Record<string, string>;
  startedAtMs: number;
  endedAtMs: number | null;
  status: "running" | "stopped" | "failed" | "completed" | "interrupted";
  statusMessage: string | null;
  startingCapital: string;
  finalEquity: string | null;
  barsSeen: number;
  metrics: SessionMetrics | null;
  saved: boolean;
}

// list_live_sessions 的精簡版本只會回 "running"。
export interface LiveSessionDto {
  sessionId: string;
  kind: SessionKind;
  market: "spot" | "usdmPerp";
  symbol: string;
  interval: string;
  strategyId: string;
  strategyName: string;
  status: "running";
  startedAtMs: number;
}
