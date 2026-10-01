// 分頁識別碼；對應 ROADMAP 3.4-3.7、5.4、6.5。
export type PageId =
  | "strategies"
  | "backtest"
  | "compare"
  | "paperTrading"
  | "testnetTrading"
  | "settings";

export interface NavItem {
  id: PageId;
  label: string;
}

export const NAV_ITEMS: NavItem[] = [
  { id: "strategies", label: "策略庫" },
  { id: "backtest", label: "回測" },
  { id: "compare", label: "比較" },
  { id: "paperTrading", label: "模擬交易" },
  { id: "testnetTrading", label: "測試網交易" },
  { id: "settings", label: "設定" },
];
