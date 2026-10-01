// 跟 app/src-tauri/src/backtest.rs 的 BacktestRequest/BacktestSummary/EquityPointDto
// 對應。比例類指標（total_return/max_drawdown...）是 Fixed 的字串表示，跟
// strategyTypes.ts 的 default 同一個理由：避免浮點誤差，格式化交給 backtestFormat.ts。

export interface EquityPoint {
  openTime: number;
  equity: string;
}

// 跟 Rust 的 at_core::Market 對應：現貨不支援槓桿/做空。
export const MARKET_OPTIONS = ["spot", "usdm_perp"] as const;
export type Market = (typeof MARKET_OPTIONS)[number];

// 跟 Rust 的 at_core::DirectionMode 對應。
export const DIRECTION_OPTIONS = ["long_only", "long_short"] as const;
export type Direction = (typeof DIRECTION_OPTIONS)[number];

// 跟 app/src-tauri/src/backtest.rs 的 MarginMode 對應，只有合約市場適用。
export const MARGIN_MODE_OPTIONS = ["isolated", "cross"] as const;
export type MarginMode = (typeof MARGIN_MODE_OPTIONS)[number];

// 超過這個槓桿倍數，UI 顯示「超過風控建議上限」提示（不擋送出）。
export const LEVERAGE_WARNING_THRESHOLD = 2;

export interface BacktestRequest {
  symbol: string;
  interval: string;
  year: number;
  month: number;
  strategyId: string;
  params: Record<string, string>;
  startingCapital: string;
  market: Market;
  direction: Direction;
  leverage: string;
  marginMode: MarginMode | null;
}

export interface BacktestSummary {
  symbol: string;
  interval: string;
  year: number;
  month: number;
  strategyId: string;
  strategyName: string;
  params: Record<string, string>;
  startingCapital: string;
  barCount: number;
  curve: EquityPoint[];
  trades: number;
  liquidations: number;
  totalReturn: string | null;
  annualizedReturn: string | null;
  maxDrawdown: string;
  sharpe: string | null;
  spanYears: string | null;
  feeModel: string;
  slippage: string;
  market: string;
  direction: string;
  leverage: string;
  marginMode: string | null;
  dataSourcePath: string;
}

// at_core::Interval 支援的六個週期，跟 Rust 的 FromStr 一一對應。
export const INTERVAL_OPTIONS = ["1m", "5m", "15m", "1h", "4h", "1d"] as const;
export type IntervalOption = (typeof INTERVAL_OPTIONS)[number];
