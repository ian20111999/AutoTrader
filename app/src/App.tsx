import { useState } from "react";
import "./App.css";
import { SideNav } from "./SideNav";
import { TopBar } from "./TopBar";
import { Strategies } from "./Strategies";
import { NAV_ITEMS, type PageId } from "./nav";
import type { StrategyConfig } from "./strategyTypes";
import packageJson from "../package.json";

const PAGE_TITLES: Record<PageId, string> = Object.fromEntries(
  NAV_ITEMS.map((item) => [item.id, item.label]),
) as Record<PageId, string>;

function App() {
  const [page, setPage] = useState<PageId>("strategies");
  // 3.5 調參頁面套用的結果：選了哪個策略、目前的參數值。放在這一層是因為
  // 3.6 回測頁面（同一層的另一個分頁）之後要讀這組設定決定跑哪個策略。
  const [strategyConfig, setStrategyConfig] = useState<StrategyConfig | null>(null);

  return (
    <div className="app-shell">
      <SideNav active={page} onSelect={setPage} />
      <div className="app-shell__main">
        <TopBar title={PAGE_TITLES[page]} />
        <main className="app-content">
          {page === "strategies" && (
            <Strategies strategyConfig={strategyConfig} onApplyConfig={setStrategyConfig} />
          )}
          {page === "backtest" && <p>回測頁面開發中（ROADMAP 3.6）。</p>}
          {page === "compare" && <p>回測比較頁面開發中（ROADMAP 3.7）。</p>}
          {page === "settings" && <p>自動交易台 v{packageJson.version}</p>}
        </main>
      </div>
    </div>
  );
}

export default App;
