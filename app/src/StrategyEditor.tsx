import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  DSL_LIMITS,
  type Cond,
  type Direction,
  type DslValidationResult,
  type Expr,
  type IndicatorName,
  type PriceField,
  type StrategyAst,
} from "./strategyDslTypes";
import type { StrategyConfig } from "./strategyTypes";
import type { BacktestSummary } from "./backtestTypes";
import {
  defaultCond,
  defaultExpr,
  defaultIndicatorExpr,
  deleteSavedStrategy,
  indicatorTakesSource,
  loadSavedStrategies,
  mirrorCond,
  needsOrderFlowData,
  needsOutput,
  outputOptionsFor,
  paramFieldsFor,
  upsertSavedStrategy,
  type SavedStrategy,
} from "./strategyEditorModel";
import { formatDrawdown, formatSignedPercent } from "./backtestFormat";

interface StrategyEditorProps {
  onUseInBacktest: (config: StrategyConfig) => void;
  onUseInPaperTrading: (config: StrategyConfig) => void;
}

const PRICE_FIELDS: { value: PriceField; label: string }[] = [
  { value: "open", label: "開盤價" },
  { value: "high", label: "最高價" },
  { value: "low", label: "最低價" },
  { value: "close", label: "收盤價" },
  { value: "volume", label: "成交量" },
  { value: "trades", label: "成交筆數（訂單流）" },
  { value: "taker_buy_ratio", label: "主動買盤佔比（訂單流）" },
];

const INDICATOR_OPTIONS: { value: IndicatorName; label: string }[] = [
  { value: "sma", label: "SMA 簡單均線" },
  { value: "ema", label: "EMA 指數均線" },
  { value: "rsi", label: "RSI" },
  { value: "macd", label: "MACD" },
  { value: "bb", label: "布林通道" },
  { value: "atr", label: "ATR" },
  { value: "donchian", label: "唐奇安通道" },
  { value: "highest", label: "N 根最高價" },
  { value: "lowest", label: "N 根最低價" },
];

type CompareKind = "gt" | "gte" | "lt" | "lte" | "cross_above" | "cross_below";
const COMPARE_OPTIONS: { value: CompareKind; label: string }[] = [
  { value: "gt", label: "大於" },
  { value: "gte", label: "大於或等於" },
  { value: "lt", label: "小於" },
  { value: "lte", label: "小於或等於" },
  { value: "cross_above", label: "向上穿越" },
  { value: "cross_below", label: "向下穿越" },
];

const COND_KIND_OPTIONS: { value: Cond["kind"]; label: string }[] = [
  ...COMPARE_OPTIONS,
  { value: "all", label: "全部成立（AND）" },
  { value: "any", label: "任一成立（OR）" },
  { value: "sustained", label: "持續 N 根成立" },
];

// ---------------------------------------------------------------------------
// Expr 編輯器：價格／常數／指標三選一，指標可以巢狀（來源也是一個 Expr）。
// ---------------------------------------------------------------------------

function OffsetInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (next: number) => void;
}) {
  return (
    <label className="dsl-field dsl-field--inline">
      <span>位移（往前幾根）</span>
      <input
        aria-label={`${label}位移`}
        type="number"
        min={0}
        max={DSL_LIMITS.maxOffset}
        value={value}
        onChange={(e) => onChange(Math.max(0, Math.trunc(Number(e.target.value) || 0)))}
      />
    </label>
  );
}

function IndicatorEditor({
  label,
  value,
  onChange,
}: {
  label: string;
  value: Extract<Expr, { kind: "indicator" }>;
  onChange: (next: Expr) => void;
}) {
  const takesSource = indicatorTakesSource(value.name);

  function handleNameChange(name: IndicatorName) {
    const next = defaultIndicatorExpr(name);
    onChange({ ...next, offset: value.offset });
  }

  return (
    <div className="dsl-indicator">
      <select
        aria-label={`${label}指標`}
        value={value.name}
        onChange={(e) => handleNameChange(e.target.value as IndicatorName)}
      >
        {INDICATOR_OPTIONS.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>

      {paramFieldsFor(value.name).map((field) => (
        <label key={field.key} className="dsl-field dsl-field--inline">
          <span>{field.label}</span>
          <input
            aria-label={`${label}${field.label}`}
            type="text"
            inputMode={field.key === "mult" ? "decimal" : "numeric"}
            value={String(value.params[field.key] ?? "")}
            onChange={(e) => {
              const raw = e.target.value;
              const nextParams = { ...value.params };
              if (field.key === "mult") {
                nextParams.mult = raw;
              } else {
                const n = raw === "" ? undefined : Math.trunc(Number(raw));
                nextParams[field.key] = raw !== "" && Number.isFinite(n) ? n : undefined;
              }
              onChange({ ...value, params: nextParams });
            }}
          />
        </label>
      ))}

      {needsOutput(value.name) && (
        <label className="dsl-field dsl-field--inline">
          <span>輸出</span>
          <select
            aria-label={`${label}輸出`}
            value={value.output ?? ""}
            onChange={(e) => onChange({ ...value, output: e.target.value as never })}
          >
            {outputOptionsFor(value.name).map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </label>
      )}

      <OffsetInput
        label={label}
        value={value.offset ?? 0}
        onChange={(offset) => onChange({ ...value, offset })}
      />

      {takesSource && (
        <div className="dsl-indicator__source">
          <ExprEditor
            label={`${label}來源`}
            value={value.source ?? defaultExpr()}
            onChange={(source) => onChange({ ...value, source })}
          />
        </div>
      )}
    </div>
  );
}

function ExprEditor({
  label,
  value,
  onChange,
}: {
  label: string;
  value: Expr;
  onChange: (next: Expr) => void;
}) {
  function handleKindChange(kind: Expr["kind"]) {
    if (kind === "price") onChange(defaultExpr());
    else if (kind === "number") onChange({ kind: "number", value: "0" });
    else onChange(defaultIndicatorExpr());
  }

  return (
    <div className="dsl-expr">
      <span className="dsl-expr__label">{label}</span>
      <select
        aria-label={`${label}類型`}
        value={value.kind}
        onChange={(e) => handleKindChange(e.target.value as Expr["kind"])}
      >
        <option value="price">價格／成交量</option>
        <option value="number">常數</option>
        <option value="indicator">指標</option>
      </select>

      {value.kind === "price" && (
        <>
          <select
            aria-label={`${label}欄位`}
            value={value.field}
            onChange={(e) => onChange({ ...value, field: e.target.value as PriceField })}
          >
            {PRICE_FIELDS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
          <OffsetInput
            label={label}
            value={value.offset ?? 0}
            onChange={(offset) => onChange({ ...value, offset })}
          />
          {needsOrderFlowData(value.field) && (
            <p className="strategy-editor__hint">
              需要重新下載過的 K 線資料才有這個欄位；用舊格式資料回測／模擬交易／測試網會在送出時收到錯誤提示。
            </p>
          )}
        </>
      )}

      {value.kind === "number" && (
        <input
          aria-label={`${label}數值`}
          type="text"
          inputMode="decimal"
          value={value.value}
          onChange={(e) => onChange({ ...value, value: e.target.value })}
        />
      )}

      {value.kind === "indicator" && (
        <IndicatorEditor label={label} value={value} onChange={onChange} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Cond 編輯器：比較／AND／OR／持續 N 根，巢狀用縮排表示。
// ---------------------------------------------------------------------------

function isCompareCond(cond: Cond): cond is Extract<Cond, { kind: CompareKind }> {
  return (
    cond.kind === "gt" || cond.kind === "gte" || cond.kind === "lt" || cond.kind === "lte" ||
    cond.kind === "cross_above" || cond.kind === "cross_below"
  );
}

function CondEditor({
  label,
  value,
  onChange,
  onRemove,
  depth = 0,
}: {
  label: string;
  value: Cond;
  onChange: (next: Cond) => void;
  onRemove?: () => void;
  depth?: number;
}) {
  function handleKindChange(kind: Cond["kind"]) {
    if (kind === "all" || kind === "any") {
      onChange({ kind, children: [defaultCond()] });
    } else if (kind === "sustained") {
      onChange({ kind: "sustained", bars: 3, inner: defaultCond() });
    } else {
      onChange({ kind, left: defaultExpr(), right: defaultIndicatorExpr() });
    }
  }

  return (
    <div className="dsl-cond" style={{ marginLeft: depth * 20 }}>
      <div className="dsl-cond__header">
        <span className="dsl-cond__label">{label}</span>
        <select
          aria-label={`${label}條件類型`}
          value={value.kind}
          onChange={(e) => handleKindChange(e.target.value as Cond["kind"])}
        >
          {COND_KIND_OPTIONS.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        {onRemove && (
          <button type="button" className="dsl-cond__remove" onClick={onRemove}>
            移除
          </button>
        )}
      </div>

      {isCompareCond(value) && (
        <div className="dsl-cond__compare">
          <ExprEditor
            label={`${label} 左側`}
            value={value.left}
            onChange={(left) => onChange({ ...value, left })}
          />
          <ExprEditor
            label={`${label} 右側`}
            value={value.right}
            onChange={(right) => onChange({ ...value, right })}
          />
        </div>
      )}

      {(value.kind === "all" || value.kind === "any") && (
        <div className="dsl-cond__children">
          {value.children.map((child, i) => (
            <CondEditor
              key={i}
              label={`${label} － 第 ${i + 1} 條`}
              value={child}
              depth={depth + 1}
              onChange={(next) => {
                const children = [...value.children];
                children[i] = next;
                onChange({ ...value, children });
              }}
              onRemove={
                value.children.length > 1
                  ? () => onChange({ ...value, children: value.children.filter((_, j) => j !== i) })
                  : undefined
              }
            />
          ))}
          <button
            type="button"
            className="dsl-cond__add"
            onClick={() => onChange({ ...value, children: [...value.children, defaultCond()] })}
          >
            + 新增子條件
          </button>
        </div>
      )}

      {value.kind === "sustained" && (
        <div className="dsl-cond__sustained">
          <label className="dsl-field dsl-field--inline">
            <span>持續根數</span>
            <input
              aria-label={`${label}持續根數`}
              type="number"
              min={1}
              max={DSL_LIMITS.maxPeriod}
              value={value.bars}
              onChange={(e) =>
                onChange({ ...value, bars: Math.max(1, Math.trunc(Number(e.target.value) || 1)) })
              }
            />
          </label>
          <CondEditor
            label={`${label} － 內層`}
            value={value.inner}
            depth={depth + 1}
            onChange={(inner) => onChange({ ...value, inner })}
          />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 整頁：策略名稱/方向/部位設定 + 四棵樹 + 即時驗證 + 存檔 + 送去回測/模擬交易
// ---------------------------------------------------------------------------

// 快速回測結果卡固定用這個窗口示範（不是使用者可調的完整回測設定——那是
// 回測頁的事，見 ADR §13.2：前端不自己算指標，一律呼叫後端跑一次真的回測）。
const PREVIEW_SYMBOL = "BTCUSDT";
const PREVIEW_INTERVAL = "1h";
const PREVIEW_YEAR = 2024;
const PREVIEW_MONTH = 1;

export function StrategyEditor({ onUseInBacktest, onUseInPaperTrading }: StrategyEditorProps) {
  const [name, setName] = useState("");
  const [direction, setDirection] = useState<Direction>("long_only");
  const [positionPct, setPositionPct] = useState("100");
  const [leverage, setLeverage] = useState("1");
  const [longEntry, setLongEntry] = useState<Cond>(() => defaultCond());
  const [longExit, setLongExit] = useState<Cond>(() => defaultCond());
  const [shortEntry, setShortEntry] = useState<Cond>(() => mirrorCond(defaultCond()));
  const [shortExit, setShortExit] = useState<Cond>(() => mirrorCond(defaultCond()));

  const [savedList, setSavedList] = useState<SavedStrategy[]>(() => loadSavedStrategies());
  const [loadedId, setLoadedId] = useState<string | null>(null);
  const [savedSnapshot, setSavedSnapshot] = useState<string | null>(null);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);

  const [validation, setValidation] = useState<DslValidationResult | null>(null);
  const [validating, setValidating] = useState(false);

  const [previewStatus, setPreviewStatus] = useState<"idle" | "loading" | "success" | "error">(
    "idle",
  );
  const [preview, setPreview] = useState<BacktestSummary | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const ast: StrategyAst = useMemo(
    () => ({
      schemaVersion: 1,
      direction,
      sizing: { positionPct: positionPct.trim(), leverage: leverage.trim() },
      longEntry,
      longExit,
      shortEntry: direction === "long_short" ? shortEntry : null,
      shortExit: direction === "long_short" ? shortExit : null,
    }),
    [direction, positionPct, leverage, longEntry, longExit, shortEntry, shortExit],
  );

  const dslJson = useMemo(() => JSON.stringify(ast), [ast]);
  const dirty = savedSnapshot !== JSON.stringify({ name, ast });

  // 即時驗證：每次積木樹變動都 debounce 400ms 後呼叫後端真正的 compile()
  // 邊界檢查（跟按「執行回測」走的是同一條路徑，不會有「這裡說合法，送出
  // 卻失敗」的落差——見 backend 的 validate_strategy_ast 文件註解）。
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setValidating(true);
      invoke<DslValidationResult>("validate_strategy_ast", { dslJson })
        .then((result) => setValidation(result))
        .catch((err: unknown) => setValidation({ valid: false, error: String(err) }))
        .finally(() => setValidating(false));
    }, 400);
    return () => window.clearTimeout(timer);
  }, [dslJson]);

  function handleDirectionChange(next: Direction) {
    setDirection(next);
    if (next === "long_short") {
      // 預設鏡像多單（ADR §3.5 的 UI 便利功能），使用者可以再自己改。
      setShortEntry(mirrorCond(longEntry));
      setShortExit(mirrorCond(longExit));
    }
  }

  function handleMirrorShort() {
    setShortEntry(mirrorCond(longEntry));
    setShortExit(mirrorCond(longExit));
  }

  function handleSave() {
    const trimmedName = name.trim();
    if (trimmedName === "") {
      setSaveMessage("請先輸入策略名稱才能儲存");
      return;
    }
    const saved = upsertSavedStrategy(trimmedName, ast, loadedId);
    setLoadedId(saved.id);
    setSavedSnapshot(JSON.stringify({ name: trimmedName, ast }));
    setSavedList(loadSavedStrategies());
    setSaveMessage(`已儲存到本機瀏覽器：${saved.name}`);
  }

  function handleLoad(entry: SavedStrategy) {
    setName(entry.name);
    setDirection(entry.ast.direction);
    setPositionPct(entry.ast.sizing.positionPct);
    setLeverage(entry.ast.sizing.leverage);
    setLongEntry(entry.ast.longEntry);
    setLongExit(entry.ast.longExit);
    setShortEntry(entry.ast.shortEntry ?? mirrorCond(entry.ast.longEntry));
    setShortExit(entry.ast.shortExit ?? mirrorCond(entry.ast.longExit));
    setLoadedId(entry.id);
    setSavedSnapshot(JSON.stringify({ name: entry.name, ast: entry.ast }));
    setSaveMessage(`已讀取：${entry.name}`);
  }

  function handleDelete(id: string) {
    deleteSavedStrategy(id);
    setSavedList(loadSavedStrategies());
    if (loadedId === id) {
      setLoadedId(null);
      setSavedSnapshot(null);
    }
  }

  function handleNew() {
    setName("");
    setDirection("long_only");
    setPositionPct("100");
    setLeverage("1");
    setLongEntry(defaultCond());
    setLongExit(defaultCond());
    setShortEntry(mirrorCond(defaultCond()));
    setShortExit(mirrorCond(defaultCond()));
    setLoadedId(null);
    setSavedSnapshot(null);
    setSaveMessage(null);
  }

  function buildConfig(): StrategyConfig {
    return {
      strategyId: "custom",
      values: {},
      dslJson,
      dslName: name.trim() || "未命名策略",
    };
  }

  async function handlePreview() {
    setPreviewStatus("loading");
    setPreviewError(null);
    try {
      const summary = await invoke<BacktestSummary>("run_backtest_command", {
        request: {
          symbol: PREVIEW_SYMBOL,
          interval: PREVIEW_INTERVAL,
          year: PREVIEW_YEAR,
          month: PREVIEW_MONTH,
          strategyId: "custom",
          params: {},
          startingCapital: "10000",
          market: direction === "long_short" ? "usdm_perp" : "spot",
          direction,
          leverage: "1",
          marginMode: direction === "long_short" ? "isolated" : null,
          dslJson,
        },
      });
      setPreview(summary);
      setPreviewStatus("success");
    } catch (err) {
      setPreviewError(String(err));
      setPreviewStatus("error");
    }
  }

  const canUse = validation?.valid === true;

  return (
    <section aria-label="策略編輯器" className="strategy-editor">
      <div className="strategy-editor__topbar">
        <label className="dsl-field">
          <span>策略名稱</span>
          <input
            aria-label="策略名稱"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如：均線交叉改良版"
          />
        </label>

        <label className="dsl-field">
          <span>方向</span>
          <select
            aria-label="方向"
            value={direction}
            onChange={(e) => handleDirectionChange(e.target.value as Direction)}
          >
            <option value="long_only">只做多</option>
            <option value="long_short">多空</option>
          </select>
        </label>

        <label className="dsl-field">
          <span>每次進場投入權益 %</span>
          <input
            aria-label="每次進場投入權益百分比"
            type="text"
            inputMode="decimal"
            value={positionPct}
            onChange={(e) => setPositionPct(e.target.value)}
          />
        </label>

        <label className="dsl-field">
          <span>策略內建槓桿</span>
          <input
            aria-label="策略內建槓桿"
            type="text"
            inputMode="decimal"
            value={leverage}
            onChange={(e) => setLeverage(e.target.value)}
          />
        </label>

        {dirty && (
          <span className="strategy-editor__unsaved" role="status">
            ● 未儲存
          </span>
        )}
      </div>
      <p className="strategy-editor__hint">
        這裡的槓桿是策略自己算目標部位用的（目標部位 = 權益% × 槓桿）；送去回測／模擬交易時，
        市場、交易對、週期、執行層的槓桿倍數在那邊的表單另外設定。
      </p>

      <div className="strategy-editor__trees">
        <CondEditor
          label="做多進場"
          value={longEntry}
          onChange={setLongEntry}
        />
        <CondEditor label="做多出場" value={longExit} onChange={setLongExit} />

        {direction === "long_short" && (
          <>
            <div className="strategy-editor__short-header">
              <span>做空條件（預設鏡像多單，可自行修改）</span>
              <button type="button" onClick={handleMirrorShort}>
                重新鏡像多單
              </button>
            </div>
            <CondEditor label="做空進場" value={shortEntry} onChange={setShortEntry} />
            <CondEditor label="做空出場" value={shortExit} onChange={setShortExit} />
          </>
        )}
      </div>

      <div className="strategy-editor__validation" role="status">
        {validating && <p>驗證中…</p>}
        {!validating && validation?.valid === true && (
          <p className="strategy-editor__valid">✓ 這份策略通過後端驗證，可以使用</p>
        )}
        {!validating && validation?.valid === false && (
          <p role="alert" className="strategy-editor__invalid">
            ✗ {validation.error}
          </p>
        )}
      </div>

      <div className="strategy-editor__actions">
        <button type="button" onClick={handleSave}>
          儲存到本機瀏覽器
        </button>
        <button type="button" onClick={handleNew}>
          新增一份
        </button>
        <button type="button" disabled={!canUse} onClick={() => onUseInBacktest(buildConfig())}>
          送去回測
        </button>
        <button
          type="button"
          disabled={!canUse || direction === "long_short"}
          title={
            direction === "long_short"
              ? "模擬交易目前只支援現貨（只做多），多空策略請改送去回測"
              : undefined
          }
          onClick={() => onUseInPaperTrading(buildConfig())}
        >
          送去模擬交易
        </button>
        <button type="button" disabled={!canUse} onClick={handlePreview}>
          {previewStatus === "loading" ? "回測中…" : "快速回測結果卡"}
        </button>
      </div>
      {saveMessage && <p className="strategy-editor__save-message">{saveMessage}</p>}
      <p className="strategy-editor__hint">
        儲存只會存在這台電腦的瀏覽器本機儲存空間（localStorage），換一台電腦或清除瀏覽器資料就會不見；
        目前沒有雲端或跨裝置的策略庫持久化機制。
      </p>

      {previewStatus === "error" && previewError && (
        <p role="alert" className="backtest-status backtest-status--error">
          快速回測失敗：{previewError}
        </p>
      )}
      {previewStatus === "success" && preview && (
        <div className="strategy-editor__preview" aria-label="快速回測結果卡">
          <p className="strategy-editor__preview-note">
            示範窗口：{PREVIEW_SYMBOL} {PREVIEW_INTERVAL}（{PREVIEW_YEAR}-
            {String(PREVIEW_MONTH).padStart(2, "0")}），不是完整的多月回測——
            完整設定請用「送去回測」。
          </p>
          <div className="strategy-editor__preview-metrics">
            <div>
              <span className="strategy-editor__preview-label">年化報酬</span>
              <span>{formatSignedPercent(preview.annualizedReturn)}</span>
            </div>
            <div>
              <span className="strategy-editor__preview-label">最大回撤</span>
              <span>{formatDrawdown(preview.maxDrawdown)}</span>
            </div>
            <div>
              <span className="strategy-editor__preview-label">交易次數</span>
              <span>{preview.trades}</span>
            </div>
          </div>
        </div>
      )}

      <div className="strategy-editor__saved-list">
        <h2 className="backtest-form__label">已儲存的策略（本機）</h2>
        {savedList.length === 0 ? (
          <p className="strategy-empty-state">還沒有在這台電腦儲存過策略。</p>
        ) : (
          <ul>
            {savedList.map((entry) => (
              <li key={entry.id}>
                <span>{entry.name}</span>
                <button type="button" onClick={() => handleLoad(entry)}>
                  讀取
                </button>
                <button type="button" onClick={() => handleDelete(entry.id)}>
                  刪除
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
