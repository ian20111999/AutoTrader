// 跟 app/src-tauri/src/backtest.rs 的 BacktestRequest/BacktestSummary/EquityPointDto
// 對應。比例類指標（total_return/max_drawdown...）是 Fixed 的字串表示，跟
// strategyTypes.ts 的 default 同一個理由：避免浮點誤差，格式化交給 backtestFormat.ts。

export interface EquityPoint {
  openTime: number;
  equity: string;
}

export interface BacktestRequest {
  symbol: string;
  interval: string;
  year: number;
  month: number;
  strategyId: string;
  params: Record<string, string>;
  startingCapital: string;
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
  dataSourcePath: string;
}

// at_core::Interval 支援的六個週期，跟 Rust 的 FromStr 一一對應。
export const INTERVAL_OPTIONS = ["1m", "5m", "15m", "1h", "4h", "1d"] as const;
export type IntervalOption = (typeof INTERVAL_OPTIONS)[number];
