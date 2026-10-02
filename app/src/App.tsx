import { useState } from "react";
import "./App.css";
import { SideNav } from "./SideNav";
import { TopBar } from "./TopBar";
import { Overview } from "./Overview";
import { Strategies } from "./Strategies";
import { StrategyEditor } from "./StrategyEditor";
import { Backtest } from "./Backtest";
import { Compare } from "./Compare";
import { PaperTrading } from "./PaperTrading";
import { TestnetTrading } from "./TestnetTrading";
import { Settings } from "./Settings";
import { NAV_ITEMS, type PageId } from "./nav";
import type { StrategyConfig } from "./strategyTypes";
import type { BacktestSummary } from "./backtestTypes";

// PaperTrading/TestnetTrading 頁面掛載時會從這兩個 key 讀「上次那場 session」
// 繼續追蹤（見 PaperTrading.tsx/TestnetTrading.tsx 的 SESSION_ID_STORAGE_KEY）。
// 總覽的「執行中策略」表格要「連到對應頁面並帶 sessionId」，沒有另外做一條
// props 傳遞路徑——寫同一把 key 再切頁面，兩邊頁面不用互相知道對方存在。
const PAPER_SESSION_STORAGE_KEY = "paperTrading.sessionId";
const TESTNET_SESSION_STORAGE_KEY = "testnetTrading.sessionId";

const PAGE_TITLES: Record<PageId, string> = Object.fromEntries(
  NAV_ITEMS.map((item) => [item.id, item.label]),
) as Record<PageId, string>;

function App() {
  const [page, setPage] = useState<PageId>("strategies");
  // 3.5 調參頁面套用的結果：選了哪個策略、目前的參數值。放在這一層是因為
  // 3.6 回測頁面（同一層的另一個分頁）之後要讀這組設定決定跑哪個策略。
  const [strategyConfig, setStrategyConfig] = useState<StrategyConfig | null>(null);
  // 3.6 使用者按「加入比較」存下來的回測結果，3.7 比較頁面讀這個陣列畫表格/疊圖。
  const [savedBacktests, setSavedBacktests] = useState<BacktestSummary[]>([]);

  return (
    <div className="app-shell">
      <SideNav active={page} onSelect={setPage} />
      <div className="app-shell__main">
        <TopBar title={PAGE_TITLES[page]} />
        <main className="app-content">
          {page === "overview" && (
            <Overview
              onOpenPaperTrading={(sessionId) => {
                localStorage.setItem(PAPER_SESSION_STORAGE_KEY, sessionId);
                setPage("paperTrading");
              }}
              onOpenTestnetTrading={(sessionId) => {
                localStorage.setItem(TESTNET_SESSION_STORAGE_KEY, sessionId);
                setPage("testnetTrading");
              }}
              onOpenBacktestCompare={(summary) => {
                setSavedBacktests((prev) => [...prev, summary]);
                setPage("compare");
              }}
            />
          )}
          {page === "strategies" && (
            <Strategies
              strategyConfig={strategyConfig}
              onApplyConfig={setStrategyConfig}
              onNavigate={(target, config) => {
                setStrategyConfig(config);
                setPage(target);
              }}
            />
          )}
          {page === "strategyEditor" && (
            <StrategyEditor
              onUseInBacktest={(config) => {
                setStrategyConfig(config);
                setPage("backtest");
              }}
              onUseInPaperTrading={(config) => {
                setStrategyConfig(config);
                setPage("paperTrading");
              }}
            />
          )}
          {page === "backtest" && (
            <Backtest
              strategyConfig={strategyConfig}
              onGoToStrategies={() => setPage("strategies")}
              onAddToCompare={(summary) => setSavedBacktests((prev) => [...prev, summary])}
            />
          )}
          {page === "compare" && (
            <Compare
              savedBacktests={savedBacktests}
              onRemove={(index) =>
                setSavedBacktests((prev) => prev.filter((_, i) => i !== index))
              }
              onGoToBacktest={() => setPage("backtest")}
            />
          )}
          {page === "paperTrading" && (
            <PaperTrading
              strategyConfig={strategyConfig}
              onGoToStrategies={() => setPage("strategies")}
            />
          )}
          {page === "testnetTrading" && (
            <TestnetTrading
              strategyConfig={strategyConfig}
              onGoToStrategies={() => setPage("strategies")}
            />
          )}
          {page === "settings" && <Settings />}
        </main>
      </div>
    </div>
  );
}

export default App;
