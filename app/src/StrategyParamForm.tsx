import { useState } from "react";
import type { FormEvent } from "react";
import type { StrategyInfo, StrategyParam } from "./strategyTypes";

function integerError(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return "請輸入數值";
  if (!/^\d+$/.test(trimmed)) return "必須是整數";
  if (Number(trimmed) <= 0) return "週期必須大於 0";
  return null;
}

function decimalError(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return "請輸入數值";
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return "必須是數字";
  return null;
}

// 對照 at_core::strategies::StrategyParamError 的語意：週期>0、快線<慢線、
// 標準差倍數>0、RSI 門檻 0～100 且進場<出場。schema 的 kind（integer/decimal）
// 只能表達型別，這裡補上型別表達不出來的上下限與跨欄位規則。表單驗證只在前端
// 做，3.6 實際執行回測時 Rust 端本來就會用同樣規則再驗證一次。
function fieldSemanticError(strategyId: string, key: string, raw: string): string | null {
  const value = Number(raw);
  if (!Number.isFinite(value)) return null;
  if (strategyId === "bollinger" && key === "multiplier" && value <= 0) {
    return "標準差倍數必須大於 0";
  }
  if (strategyId === "rsi" && (key === "buyBelow" || key === "exitAbove")) {
    if (value < 0 || value > 100) return "RSI 門檻必須在 0 到 100 之間";
  }
  // 訂單流確認突破與主動買盤動能共用這個參數 key，規則也一樣（對應
  // at_core 的 StrategyParamError::TakerRatioOutOfRange），所以不逐個策略列。
  if (key === "takerBuyThreshold" && (value < 0.5 || value > 1)) {
    return "主動買盤佔比門檻必須在 0.5 到 1 之間";
  }
  return null;
}

function fieldError(strategyId: string, param: StrategyParam, raw: string): string | null {
  const basicError = param.kind === "integer" ? integerError(raw) : decimalError(raw);
  return basicError ?? fieldSemanticError(strategyId, param.key, raw);
}

function crossFieldError(strategyId: string, values: Record<string, string>): string | null {
  if (strategyId === "sma_cross") {
    const fast = Number(values.fastPeriod);
    const slow = Number(values.slowPeriod);
    if (Number.isFinite(fast) && Number.isFinite(slow) && fast >= slow) {
      return "快線週期必須短於慢線週期";
    }
  }
  if (strategyId === "rsi") {
    const buy = Number(values.buyBelow);
    const exit = Number(values.exitAbove);
    if (Number.isFinite(buy) && Number.isFinite(exit) && buy >= exit) {
      return "RSI 進場門檻必須低於出場門檻";
    }
  }
  return null;
}

interface StrategyParamFormProps {
  strategy: StrategyInfo;
  initialValues: Record<string, string>;
  onApply: (values: Record<string, string>) => void;
  onCancel: () => void;
}

export function StrategyParamForm({
  strategy,
  initialValues,
  onApply,
  onCancel,
}: StrategyParamFormProps) {
  const [values, setValues] = useState<Record<string, string>>(initialValues);
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  const [submitted, setSubmitted] = useState(false);

  const fieldErrors: Record<string, string> = {};
  for (const param of strategy.params) {
    const err = fieldError(strategy.id, param, values[param.key] ?? "");
    if (err) fieldErrors[param.key] = err;
  }
  const formError =
    Object.keys(fieldErrors).length === 0 ? crossFieldError(strategy.id, values) : null;

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (Object.keys(fieldErrors).length === 0 && formError === null) {
      onApply(values);
    }
  }

  return (
    <form className="strategy-param-form" onSubmit={handleSubmit} noValidate>
      <h2>{strategy.name}</h2>
      {strategy.params.map((param) => {
        const showError = (touched[param.key] || submitted) && fieldErrors[param.key];
        const errorId = `${param.key}-error`;
        return (
          <div className="strategy-param-form__field" key={param.key}>
            <label htmlFor={param.key}>{param.label}</label>
            <input
              id={param.key}
              type="text"
              inputMode={param.kind === "integer" ? "numeric" : "decimal"}
              value={values[param.key] ?? ""}
              onChange={(event) =>
                setValues((prev) => ({ ...prev, [param.key]: event.target.value }))
              }
              onBlur={() => setTouched((prev) => ({ ...prev, [param.key]: true }))}
              aria-invalid={showError ? true : undefined}
              aria-describedby={showError ? errorId : undefined}
            />
            {showError && (
              <p id={errorId} role="alert" className="strategy-param-form__error">
                {fieldErrors[param.key]}
              </p>
            )}
          </div>
        );
      })}
      {submitted && formError && (
        <p role="alert" className="strategy-param-form__error">
          {formError}
        </p>
      )}
      <div className="strategy-param-form__actions">
        <button type="button" className="strategy-card__edit" onClick={onCancel}>
          返回策略庫
        </button>
        <button type="submit" className="strategy-param-form__apply">
          套用參數
        </button>
      </div>
    </form>
  );
}
