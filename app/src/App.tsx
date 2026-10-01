import { useState } from "react";
import "./App.css";
import { SideNav } from "./SideNav";
import { TopBar } from "./TopBar";
import { Strategies } from "./Strategies";
import { Backtest } from "./Backtest";
import { Compare } from "./Compare";
import { PaperTrading } from "./PaperTrading";
import { TestnetTrading } from "./TestnetTrading";
import { Settings } from "./Settings";
import { NAV_ITEMS, type PageId } from "./nav";
import type { StrategyConfig } from "./strategyTypes";
import type { BacktestSummary } from "./backtestTypes";

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
          {page === "strategies" && (
            <Strategies strategyConfig={strategyConfig} onApplyConfig={setStrategyConfig} />
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
