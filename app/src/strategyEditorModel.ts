// Phase F3：策略編輯器的積木樹預設值／鏡像／本機存檔邏輯。
// 純函式（不碰 Tauri/DOM），方便直接測試；StrategyEditor.tsx 只負責畫面。
//
// 存檔機制：ponytail——用瀏覽器 localStorage，不是雲端或正式的策略庫持久化
// （專案目前沒有這個後端機制，見 docs/architecture/2026-10-01-strategy-dsl.md
// 第 8 節範圍邊界）。畫面上要標示「儲存在本機瀏覽器，換一台電腦看不到」。

import {
  DSL_SCHEMA_VERSION,
  type BbOutput,
  type Cond,
  type Direction,
  type DonchianOutput,
  type Expr,
  type IndicatorName,
  type IndicatorParams,
  type MacdOutput,
  type PriceField,
  type StrategyAst,
  type SwingOutput,
} from "./strategyDslTypes";

export function defaultExpr(): Expr {
  return { kind: "price", field: "close" };
}

/**
 * 這個價格欄位是不是需要 K 線的訂單流資料（成交筆數／主動買盤佔比）。
 * 用來在積木 UI 顯示「舊資料可能沒有這個欄位」的提示——實際檢查永遠在後端
 * （`needs_order_flow()` + 載入端硬錯誤），這裡只是提前讓使用者知道。
 */
export function needsOrderFlowData(field: PriceField): boolean {
  return field === "trades" || field === "taker_buy_ratio";
}

export function defaultParamsFor(name: IndicatorName): IndicatorParams {
  switch (name) {
    case "macd":
      return { fast: 12, slow: 26, signal: 9 };
    case "bb":
      return { period: 20, mult: "2" };
    case "rsi":
      return { period: 14 };
    case "swing_high":
    case "swing_low":
      return { left: 2, right: 2 };
    default:
      return { period: 20 };
  }
}

export function defaultOutputFor(
  name: IndicatorName,
): BbOutput | MacdOutput | DonchianOutput | SwingOutput | undefined {
  switch (name) {
    case "bb":
      return "middle";
    case "macd":
      return "line";
    case "donchian":
      return "high";
    case "swing_high":
    case "swing_low":
      return "last";
    default:
      return undefined;
  }
}

/**
 * atr／donchian／swing_high／swing_low 吃整根 K 線，不接受 source
 * （跟 compile() 的規則一致；swing 不接受 source 是正確性需求，見
 * at_core::strategy_dsl::mod.rs 的說明：否則「左右各 N 根」會悄悄變成
 * 「左右各 N 根有值的K線」）。
 */
export function indicatorTakesSource(name: IndicatorName): boolean {
  return name !== "atr" && name !== "donchian" && name !== "swing_high" && name !== "swing_low";
}

export function needsOutput(name: IndicatorName): boolean {
  return name === "bb" || name === "macd" || name === "donchian" || name === "swing_high" || name === "swing_low";
}

export function outputOptionsFor(name: IndicatorName): string[] {
  if (name === "bb") return ["upper", "middle", "lower"];
  if (name === "macd") return ["line", "signal", "histogram"];
  if (name === "donchian") return ["high", "low"];
  if (name === "swing_high" || name === "swing_low") return ["last", "previous"];
  return [];
}

export function paramFieldsFor(name: IndicatorName): { key: keyof IndicatorParams; label: string }[] {
  if (name === "macd") {
    return [
      { key: "fast", label: "快線週期" },
      { key: "slow", label: "慢線週期" },
      { key: "signal", label: "訊號線週期" },
    ];
  }
  if (name === "bb") {
    return [
      { key: "period", label: "週期" },
      { key: "mult", label: "標準差倍數" },
    ];
  }
  if (name === "swing_high" || name === "swing_low") {
    return [
      { key: "left", label: "左側根數" },
      { key: "right", label: "右側根數（確認延遲）" },
    ];
  }
  return [{ key: "period", label: "週期" }];
}

export function defaultIndicatorExpr(
  name: IndicatorName = "sma",
): Extract<Expr, { kind: "indicator" }> {
  return {
    kind: "indicator",
    name,
    params: defaultParamsFor(name),
    output: defaultOutputFor(name),
  };
}

export function defaultCond(): Cond {
  return { kind: "gt", left: defaultExpr(), right: defaultIndicatorExpr("sma") };
}

const MIRROR_KIND: Partial<Record<string, string>> = {
  gt: "lt",
  lt: "gt",
  gte: "lte",
  lte: "gte",
  cross_above: "cross_below",
  cross_below: "cross_above",
};

/**
 * 方向相反的價格欄位／指標配對：鏡像一個「多單看漲」的 Expr 時，這些要換成
 * 它們的反方向版本，不只是翻轉比較運算子。
 *
 * - `high`／`low`：FVG 這種用影線表達的結構性訊號，鏡像時每一側的「上緣」要
 *   變成「下緣」（`gt(low[0], high[2])` → `lt(high[0], low[2])`）。
 * - `swing_high`／`swing_low`：同一個理由，BOS 用 `swing_high` 表達「創新高」，
 *   鏡像後要變成 `swing_low`（「創新低」）。
 *
 * `sma`／`ema`／`rsi`／`open`／`close`／`volume` 等沒有方向對稱的概念，不在
 * 這個表裡——鏡像時維持原樣（見 `strategyEditorModel.test.ts` 的既有測試）。
 */
const MIRROR_PRICE_FIELD: Partial<Record<PriceField, PriceField>> = {
  high: "low",
  low: "high",
};

const MIRROR_INDICATOR: Partial<Record<IndicatorName, IndicatorName>> = {
  swing_high: "swing_low",
  swing_low: "swing_high",
};

/**
 * 鏡像一個數值節點本身的方向——**不交換 left／right 的位置**，只換每一側
 * 自己的內容（既有陷阱：之前的 `mirrorCond()` 完全不碰 `Expr`，見
 * ADR-004 第 10 節；FVG／BOS 這種左右兩側本來就不對稱的結構，光翻轉比較
 * 運算子不夠，兩側各自的「方向」也要翻）。
 */
function mirrorExpr(expr: Expr): Expr {
  if (expr.kind === "price") {
    const field = MIRROR_PRICE_FIELD[expr.field] ?? expr.field;
    return { ...expr, field };
  }
  if (expr.kind === "indicator") {
    const name = MIRROR_INDICATOR[expr.name] ?? expr.name;
    return { ...expr, name, source: expr.source ? mirrorExpr(expr.source) : expr.source };
  }
  return expr;
}

/**
 * 做空條件預設鏡像多單：大於→小於、向上穿越→向下穿越，巢狀結構不變；
 * `left`／`right` 兩側各自的 `Expr` 也要鏡像（見 `mirrorExpr`），不是只翻轉
 * 運算子——否則 FVG／BOS 的做空版本會產生錯誤訊號（ADR-004 第 10 節）。
 */
export function mirrorCond(cond: Cond): Cond {
  switch (cond.kind) {
    case "all":
    case "any":
      return { kind: cond.kind, children: cond.children.map(mirrorCond) };
    case "sustained":
      return { kind: "sustained", bars: cond.bars, inner: mirrorCond(cond.inner) };
    default: {
      const mirrored = MIRROR_KIND[cond.kind] ?? cond.kind;
      return {
        ...cond,
        kind: mirrored,
        left: mirrorExpr(cond.left),
        right: mirrorExpr(cond.right),
      } as Cond;
    }
  }
}

export function defaultAst(direction: Direction, longEntry: Cond, longExit: Cond): StrategyAst {
  return {
    schemaVersion: DSL_SCHEMA_VERSION,
    direction,
    sizing: { positionPct: "100", leverage: "1" },
    longEntry,
    longExit,
    shortEntry: direction === "long_short" ? mirrorCond(longEntry) : null,
    shortExit: direction === "long_short" ? mirrorCond(longExit) : null,
  };
}

// ---------------------------------------------------------------------------
// 本機存檔（localStorage，見檔頭說明）
// ---------------------------------------------------------------------------

export interface SavedStrategy {
  id: string;
  name: string;
  updatedAt: number;
  ast: StrategyAst;
}

const STORAGE_KEY = "strategyEditor.savedStrategies";

export function loadSavedStrategies(): SavedStrategy[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as SavedStrategy[]) : [];
  } catch {
    // ponytail: 讀取失敗（私密模式、資料被手動改壞）就當作沒有存檔，不 throw
    // 把整頁弄壞。
    return [];
  }
}

export function persistSavedStrategies(list: SavedStrategy[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
  } catch {
    // ponytail: 寫入失敗（容量滿了）靜默忽略，呼叫端的 UI 不會收到「已儲存」
    // 的錯誤回饋，因為呼叫端自己讀一次 loadSavedStrategies() 確認才算數。
  }
}

export function upsertSavedStrategy(
  name: string,
  ast: StrategyAst,
  existingId?: string | null,
): SavedStrategy {
  const list = loadSavedStrategies();
  const id = existingId ?? `local_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
  const entry: SavedStrategy = { id, name, updatedAt: Date.now(), ast };
  persistSavedStrategies([...list.filter((s) => s.id !== id), entry]);
  return entry;
}

export function deleteSavedStrategy(id: string): void {
  persistSavedStrategies(loadSavedStrategies().filter((s) => s.id !== id));
}
