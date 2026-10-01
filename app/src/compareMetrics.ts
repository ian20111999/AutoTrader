// Compare 頁（3.7）的純函式：逐年報酬、近 12 個月報酬、回撤曲線、關鍵發現摘要。
// 全部從既有的 BacktestSummary.curve（已經跑出來的權益曲線）算，不叫後端重算。
//
// 目前每筆回測固定只涵蓋一個月（Backtest.tsx 的 `year`/`month` 是單選，不是
// 區間），所以「逐年報酬」實際上大多數時候只有一欄有資料、其餘是「—」——
// 這是老實反映現有資料的樣子，不是 bug。等之後 roadmap 支援多月份回測，這裡
// 不用改，欄位會自然長出資料。
import type { BacktestSummary, EquityPoint } from "./backtestTypes";

const MS_PER_DAY = 24 * 60 * 60 * 1000;
const TRAILING_WINDOW_MS = 365 * MS_PER_DAY;

function yearOf(openTimeMs: number): number {
  return new Date(openTimeMs).getUTCFullYear();
}

/** 把 equity 曲線切成「每個曆年各自的那一段」，年份用 UTC 避免時區造成切錯年。 */
function splitByYear(curve: EquityPoint[]): Map<number, EquityPoint[]> {
  const buckets = new Map<number, EquityPoint[]>();
  for (const point of curve) {
    const year = yearOf(point.openTime);
    const bucket = buckets.get(year);
    if (bucket) bucket.push(point);
    else buckets.set(year, [point]);
  }
  return buckets;
}

function returnOf(points: EquityPoint[]): number | null {
  if (points.length < 2) return null;
  const first = Number(points[0].equity);
  const last = Number(points[points.length - 1].equity);
  if (first === 0) return null;
  return last / first - 1;
}

/**
 * 回測曲線裡實際出現過的曆年，依時間排序。拿來動態長出「逐年報酬」欄位，
 * 不是寫死 2024/2025/2026——哪幾年要看，取決於使用者實際比較了哪幾筆回測。
 */
export function yearsInRuns(runs: BacktestSummary[]): number[] {
  const years = new Set<number>();
  for (const run of runs) {
    for (const point of run.curve) {
      years.add(yearOf(point.openTime));
    }
  }
  return [...years].sort((a, b) => a - b);
}

/** 某一筆回測在某個曆年的報酬率（0.1 = +10%），資料不夠（不在這筆曲線範圍內）回 null。 */
export function yearlyReturn(run: BacktestSummary, year: number): number | null {
  const bucket = splitByYear(run.curve).get(year);
  return bucket ? returnOf(bucket) : null;
}

/**
 * 近 12 個月報酬：抓曲線最後一點往前推 365 天的區間。曲線最早的一點如果比這個
 * 區間起點還晚，代表資料不夠涵蓋完整 12 個月，老實回 null，不假裝算得出來。
 */
export function trailing12MonthsReturn(run: BacktestSummary): number | null {
  const curve = run.curve;
  if (curve.length < 2) return null;
  const lastTime = curve[curve.length - 1].openTime;
  const windowStart = lastTime - TRAILING_WINDOW_MS;
  if (curve[0].openTime > windowStart) return null;

  let baseline = curve[0];
  for (const point of curve) {
    if (point.openTime > windowStart) break;
    baseline = point;
  }
  return returnOf([baseline, curve[curve.length - 1]]);
}

/** 相對起始資金的累積報酬% 序列，CompareChart 疊圖「累積報酬」檢視用。 */
export function toReturnSeries(run: BacktestSummary): number[] {
  const starting = Number(run.startingCapital);
  if (starting === 0) return run.curve.map(() => 0);
  return run.curve.map((point) => (Number(point.equity) / starting - 1) * 100);
}

/** 相對歷史最高權益的回撤% 序列（永遠 <= 0），CompareChart 疊圖「回撤」檢視用。 */
export function toDrawdownSeries(run: BacktestSummary): number[] {
  let peak = -Infinity;
  return run.curve.map((point) => {
    const equity = Number(point.equity);
    peak = Math.max(peak, equity);
    if (peak <= 0) return 0;
    return (equity / peak - 1) * 100;
  });
}

const PARAM_DIFF_SCOPE_KEYS = [
  "strategyId",
  "symbol",
  "interval",
  "year",
  "month",
  "startingCapital",
] as const satisfies readonly (keyof BacktestSummary)[];

/**
 * 兩筆回測之間，有哪些參數 key 的值不一樣（聯集兩邊的 key，缺的那邊當作
 * undefined，這樣「A 有這個參數、B 沒有」也算一種差異，不會被漏掉）。
 */
function differingParamKeys(a: BacktestSummary, b: BacktestSummary): string[] {
  const keys = new Set([...Object.keys(a.params), ...Object.keys(b.params)]);
  return [...keys].filter((key) => a.params[key] !== b.params[key]);
}

function paramLabel(key: string, strategies: { params: { key: string; label: string }[] }[] | null, strategyId: string): string {
  const strategy = strategies?.find((s) => (s as { id?: string }).id === strategyId);
  return strategy?.params.find((p) => p.key === key)?.label ?? key;
}

/**
 * 關鍵發現摘要：只在「剛好兩筆回測、其他設定都一樣、只有一個參數不同」時才
 * 自動生成一句話；其他情況（0/1 筆、或差異不只一個參數）回傳對應的狀態，由
 * 呼叫端決定要不要顯示訊息，絕不湊一句看起來像分析但其實是誤導的文字。
 */
export type CompareInsight =
  | { kind: "none" }
  | { kind: "too-complex" }
  | { kind: "single-param-diff"; text: string };

export function generateInsight(
  runs: BacktestSummary[],
  strategies: { id: string; params: { key: string; label: string }[] }[] | null,
): CompareInsight {
  if (runs.length !== 2) return { kind: "none" };
  const [a, b] = runs;

  const scopeDiffers = PARAM_DIFF_SCOPE_KEYS.some((key) => a[key] !== b[key]);
  const diffKeys = differingParamKeys(a, b);
  if (scopeDiffers || diffKeys.length !== 1) return { kind: "too-complex" };

  const key = diffKeys[0];
  const label = paramLabel(key, strategies, a.strategyId);
  const valueA = a.params[key] ?? "（無）";
  const valueB = b.params[key] ?? "（無）";

  const annA = a.annualizedReturn === null ? null : Number(a.annualizedReturn) * 100;
  const annB = b.annualizedReturn === null ? null : Number(b.annualizedReturn) * 100;
  const ddA = Number(a.maxDrawdown) * 100;
  const ddB = Number(b.maxDrawdown) * 100;

  const fmtPct = (v: number) => `${v >= 0 ? "+" : "−"}${Math.abs(v).toFixed(1)}%`;
  const fmtAnn = (v: number | null) => (v === null ? "—" : fmtPct(v));

  const text = `只改${label}：${valueA} → ${valueB}：年化 ${fmtAnn(annA)} → ${fmtAnn(annB)}，最大回撤 −${ddA.toFixed(1)}% → −${ddB.toFixed(1)}%`;
  return { kind: "single-param-diff", text };
}
