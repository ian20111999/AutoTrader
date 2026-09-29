// 跟 app/src-tauri/src/strategies.rs 的 StrategyInfo/StrategyParam 對應。
// default 用字串（Rust 那邊是 Fixed/整數的精確文字表示，不用 number 避免浮點誤差）。

export type ParamKind = "integer" | "decimal";

export interface StrategyParam {
  key: string;
  label: string;
  kind: ParamKind;
  default: string;
}

export interface StrategyInfo {
  id: string;
  name: string;
  params: StrategyParam[];
}

// 3.5 調參頁面套用後的結果：選了哪個策略、每個參數欄位目前的值（未轉型的原始
// 字串，跟 StrategyParam.default 一樣）。3.6 回測頁面會讀這個狀態決定要跑哪組
// 參數，所以放在 App.tsx 這一層，兩個分頁都能存取。
export interface StrategyConfig {
  strategyId: string;
  values: Record<string, string>;
}
