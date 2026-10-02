// 跟 crates/core/src/strategy_dsl/mod.rs 的 StrategyAst 對應。
//
// 命名風格（跟 crates/core/src/strategy_dsl/serialization.rs 的契約測試鎖住的
// 規則一致，不要自己猜）：
// - 欄位名用 camelCase（schemaVersion、longEntry、positionPct...）
// - `kind` 的值用 snake_case（cross_above、long_only...）
//
// 數值一律用十進位字串（number.value、sizing.positionPct、sizing.leverage、
// bb 的 mult），不是 JSON number：JSON number 在 JS 端是 f64，"0.1" 會變成
// 0.1000000000000000055…，而這個數字會決定要不要下單。只有「根數」類的參數
// （period、bars、offset）用 number，因為它們不是價格。
//
// 這份型別只描述 JSON 形狀，給之後的積木 UI／`validate_strategy_ast` command
// 用；這次不做積木編輯器本身。

/** 目前支援的 schema 版本，跟 at_core::strategy_dsl::SCHEMA_VERSION 對應。 */
export const DSL_SCHEMA_VERSION = 1;

export type Direction = "long_only" | "long_short";

/** 部位大小：目標部位 = positionPct / 100 × leverage。 */
export interface Sizing {
  /** 每次進場投入的權益百分比，"100" = 滿倉。範圍 (0, 100]。 */
  positionPct: string;
  /** 槓桿倍數，範圍 [1, 125]。現貨必須是 "1"。 */
  leverage: string;
}

/**
 * "trades"／"taker_buy_ratio" 需要 Bar.order_flow（成交筆數、主動買盤量）：
 * 舊格式本機 K 線檔（6 欄）或還沒用 Phase I2 之後格式重新下載過的資料沒有這兩個欄位，
 * 送出回測／模擬交易／測試網時後端會回傳明確錯誤（見
 * docs/architecture/2026-10-02-bar-order-flow-fields.md §7.2），不是靜默當成 0。
 */
export type PriceField = "open" | "high" | "low" | "close" | "volume" | "trades" | "taker_buy_ratio";

export type IndicatorName =
  | "sma"
  | "ema"
  | "rsi"
  | "macd"
  | "bb"
  | "atr"
  | "donchian"
  | "highest"
  | "lowest";

/** bb 的多輸出選項。 */
export type BbOutput = "upper" | "middle" | "lower";
/** macd 的多輸出選項。 */
export type MacdOutput = "line" | "signal" | "histogram";
/** donchian 的多輸出選項。 */
export type DonchianOutput = "high" | "low";

/**
 * 指標參數。用具名欄位而不是 Record<string, unknown>，跟 Rust 端的
 * IndicatorParams（deny_unknown_fields）一樣，拼錯的參數名要在型別層就擋下來。
 *
 * - sma/ema/rsi/atr/highest/lowest：只吃 period。
 * - macd：吃 fast/slow/signal。
 * - bb：吃 period 與 mult（十進位字串）。
 * - donchian：只吃 period。
 */
export interface IndicatorParams {
  period?: number;
  fast?: number;
  slow?: number;
  signal?: number;
  /** 布林通道的標準差倍數，十進位字串（如 "2"、"1.5"）。 */
  mult?: string;
}

export type Expr =
  | {
      kind: "price";
      field: PriceField;
      /** 往前位移幾根。0 = 這根（已收盤），1 = 前一根。省略 = 0。上限 500。 */
      offset?: number;
    }
  | {
      kind: "number";
      /** 十進位字串，例如 "0"、"1.5"、"-2.3"。 */
      value: string;
    }
  | {
      kind: "indicator";
      name: IndicatorName;
      /** 餵給指標的數列。省略 = 收盤價。atr／donchian 不接受這個欄位。 */
      source?: Expr;
      params: IndicatorParams;
      /** 多輸出指標要選哪一路（bb/macd/donchian 必填，單輸出指標不可以填）。 */
      output?: BbOutput | MacdOutput | DonchianOutput;
      /** 取這個指標幾根之前的值。省略 = 0。上限 500。 */
      offset?: number;
    };

export type Cond =
  | { kind: "gt"; left: Expr; right: Expr }
  | { kind: "gte"; left: Expr; right: Expr }
  | { kind: "lt"; left: Expr; right: Expr }
  | { kind: "lte"; left: Expr; right: Expr }
  /** 向上穿越：前一根 left <= right 且這根 left > right。 */
  | { kind: "cross_above"; left: Expr; right: Expr }
  /** 向下穿越：前一根 left >= right 且這根 left < right。 */
  | { kind: "cross_below"; left: Expr; right: Expr }
  /** AND，children 不可為空。 */
  | { kind: "all"; children: Cond[] }
  /** OR，children 不可為空。 */
  | { kind: "any"; children: Cond[] }
  /** inner 連續成立 bars 根（含這根）。 */
  | { kind: "sustained"; inner: Cond; bars: number };

/**
 * 一份自訂策略：四棵條件樹 + 部位設定。跟 at_core::strategy_dsl::StrategyAst
 * 一一對應，是 `BacktestRequest.dslJson`（序列化後的字串）要符合的形狀。
 */
export interface StrategyAst {
  schemaVersion: typeof DSL_SCHEMA_VERSION;
  direction: Direction;
  sizing: Sizing;
  longEntry: Cond;
  longExit: Cond;
  /** direction 是 long_only 時必須是 null／省略。 */
  shortEntry?: Cond | null;
  /** direction 是 long_only 時必須是 null／省略。 */
  shortExit?: Cond | null;
}

// 編譯期信任邊界（跟 at_core::strategy_dsl 的常數對應）。給積木 UI 做即時的
// 「還剩多少額度」提示用；真正的驗證永遠在後端 compile() 做一次，前端這份
// 常數只是回顯上限，不是另一套驗證規則。
export const DSL_LIMITS = {
  /** 攤平後的節點總數上限。 */
  maxNodes: 512,
  /** 條件樹的深度上限。 */
  maxDepth: 32,
  /** 指標週期與 sustained.bars 的上限。 */
  maxPeriod: 2000,
  /** 往前位移的根數上限。 */
  maxOffset: 500,
  /** 槓桿上限（只是防手滑，真正的上限由 at-risk-control 把關）。 */
  maxLeverage: 125,
} as const;

/** `validate_strategy_ast` command 的回傳，跟 Rust 的 DslValidationResult 對應。 */
export interface DslValidationResult {
  valid: boolean;
  /** valid 是 false 時才有值；繁體中文、帶路徑。 */
  error: string | null;
}
