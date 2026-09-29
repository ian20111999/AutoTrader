import { useState } from "react";
import "./App.css";
import { SideNav } from "./SideNav";
import { TopBar } from "./TopBar";
import { Strategies } from "./Strategies";
import { NAV_ITEMS, type PageId } from "./nav";
import packageJson from "../package.json";

const PAGE_TITLES: Record<PageId, string> = Object.fromEntries(
  NAV_ITEMS.map((item) => [item.id, item.label]),
) as Record<PageId, string>;

function App() {
  const [page, setPage] = useState<PageId>("strategies");

  return (
    <div className="app-shell">
      <SideNav active={page} onSelect={setPage} />
      <div className="app-shell__main">
        <TopBar title={PAGE_TITLES[page]} />
        <main className="app-content">
          {page === "strategies" && <Strategies />}
          {page === "backtest" && <p>回測頁面開發中（ROADMAP 3.6）。</p>}
          {page === "compare" && <p>回測比較頁面開發中（ROADMAP 3.7）。</p>}
          {page === "settings" && <p>自動交易台 v{packageJson.version}</p>}
        </main>
      </div>
    </div>
  );
}

export default App;
