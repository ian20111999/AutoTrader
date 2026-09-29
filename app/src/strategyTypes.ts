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
