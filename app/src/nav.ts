// 分頁識別碼；四個名稱要跟 ROADMAP 3.4-3.7 對應。
export type PageId = "strategies" | "backtest" | "compare" | "settings";

export interface NavItem {
  id: PageId;
  label: string;
}

export const NAV_ITEMS: NavItem[] = [
  { id: "strategies", label: "策略庫" },
  { id: "backtest", label: "回測" },
  { id: "compare", label: "比較" },
  { id: "settings", label: "設定" },
];
