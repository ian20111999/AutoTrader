// 分頁識別碼；對應 ROADMAP 3.4-3.7、5.4、6.5，以及 Phase C 的總覽。
export type PageId =
  | "overview"
  | "strategies"
  | "strategyEditor"
  | "backtest"
  | "compare"
  | "paperTrading"
  | "testnetTrading"
  | "settings";

export interface NavItem {
  id: PageId;
  label: string;
}

// 給路由與頁首標題用：只有已經做出來、可以切換的分頁。
export const NAV_ITEMS: NavItem[] = [
  { id: "overview", label: "總覽" },
  { id: "strategies", label: "策略庫" },
  { id: "strategyEditor", label: "策略編輯器" },
  { id: "backtest", label: "回測" },
  { id: "compare", label: "比較" },
  { id: "paperTrading", label: "模擬交易" },
  { id: "testnetTrading", label: "測試網交易" },
  { id: "settings", label: "設定" },
];

// SideNav 顯示用的分組結構（design-reference.md「SideNav（共用元件）」一節）。
// disabled 項目背後功能是另一個更大的工程（部署/即時交易/高頻監控/風控），
// 這裡只先放導覽項佔位，不建立對應頁面；id 刻意不屬於 PageId，避免被誤接去路由。
// 策略編輯器（Phase F3）已經接上頁面，不再是佔位項。
export interface SideNavEntry {
  id: PageId | string;
  label: string;
  disabled?: boolean;
}

export interface SideNavGroup {
  // null：總覽獨立一項，設計稿上沒有分組標題。
  label: string | null;
  items: SideNavEntry[];
}

export const SIDE_NAV_GROUPS: SideNavGroup[] = [
  { label: null, items: [{ id: "overview", label: "總覽" }] },
  {
    label: "研究",
    items: [
      { id: "strategies", label: "策略庫" },
      { id: "strategyEditor", label: "策略編輯器" },
      { id: "backtest", label: "回測" },
      { id: "compare", label: "比較" },
    ],
  },
  {
    label: "交易",
    items: [
      { id: "paperTrading", label: "模擬交易" },
      { id: "deployment", label: "部署", disabled: true },
      { id: "testnetTrading", label: "測試網交易" },
      { id: "liveTrading", label: "即時交易", disabled: true },
      { id: "highFrequency", label: "高頻監控", disabled: true },
    ],
  },
  {
    label: "系統",
    items: [
      { id: "riskControl", label: "風控", disabled: true },
      { id: "settings", label: "設定" },
    ],
  },
];
