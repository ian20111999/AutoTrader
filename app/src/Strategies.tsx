import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import { StrategyParamForm } from "./StrategyParamForm";
import type { Market } from "./backtestTypes";
import type { LiveSessionDto, SessionRecord } from "./overviewTypes";
import type { PageId } from "./nav";

// 最近 30 天的回測報酬／sparkline 只看「這 30 天內實際跑過的回測」，資料不足
// 不補點、不外推（任務規格：不能為了畫圖編造資料）。
const RECENT_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

// ponytail: 目前四個內建策略現貨/合約都支援（crates/core 的策略只產生
// TargetPosition，不限制市場；市場參數是 Backtest/PaperTrading/TestnetTrading
// 表單自己選的）。沒有「策略限定市場」的後端概念可查，所以這裡誠實地寫死
// 全部支援，不是為了有東西顯示而硬塞。
const SUPPORTED_MARKETS: Market[] = ["spot", "usdm_perp"];

type StrategyTab = "mine" | "builtin";
type MarketFilter = "all" | Market;
type StatusFilter = "all" | "paper" | "testnet" | "deployed" | "none";

type DeployStatus = { label: string; kind: "paper" | "testnet" | "none" };

function paramSummary(strategy: StrategyInfo): string {
  return strategy.params.map((param) => `${param.label}${param.default}`).join("、");
}

function defaultValues(strategy: StrategyInfo): Record<string, string> {
  return Object.fromEntries(strategy.params.map((param) => [param.key, param.default]));
}

// 调參頁可能已經改過這個策略的參數，導去回測/模擬/部署時優先帶那組值，
// 而不是每次都重置回預設值。
function configFor(strategy: StrategyInfo, strategyConfig: StrategyConfig | null): StrategyConfig {
  if (strategyConfig?.strategyId === strategy.id) {
    return strategyConfig;
  }
  return { strategyId: strategy.id, values: defaultValues(strategy) };
}

function deployStatusFor(strategyId: string, liveSessions: LiveSessionDto[]): DeployStatus {
  const live = liveSessions.find((s) => s.strategyId === strategyId);
  if (!live) return { label: "未部署", kind: "none" };
  if (live.kind === "testnet") return { label: "測試網中", kind: "testnet" };
  if (live.kind === "paper") return { label: "模擬中", kind: "paper" };
  // live 單一策略交易還沒做，正常不會走到這裡；保守顯示未部署而不是編造「實盤中」。
  return { label: "未部署", kind: "none" };
}

function formatPercent(raw: string | null): string {
  if (raw === null) return "—";
  const n = Number(raw);
  if (Number.isNaN(n)) return "—";
  return `${(n * 100).toFixed(1)}%`;
}

function formatDate(ms: number): string {
  return new Date(ms).toLocaleDateString("zh-TW");
}

// 小型 sparkline：x 軸是「第幾次回測」（不是真實日期間距，避免幾次回測間隔
// 不均時把圖拉得失真），y 軸是該次回測的總報酬。
function Sparkline({ points }: { points: number[] }) {
  if (points.length < 2) {
    return <p className="strategy-card__sparkline-empty">資料點太少，無法畫趨勢</p>;
  }
  const width = 120;
  const height = 28;
  const min = Math.min(...points, 0);
  const max = Math.max(...points, 0);
  const span = max - min || 1;
  const toX = (i: number) => (i / (points.length - 1)) * width;
  const toY = (v: number) => height - ((v - min) / span) * height;
  const linePoints = points.map((v, i) => `${toX(i)},${toY(v)}`).join(" ");
  return (
    <svg
      viewBox={`0 0 ${width} ${height}`}
      className="strategy-card__sparkline"
      role="img"
      aria-label={`近 ${points.length} 次回測報酬走勢`}
    >
      <polyline points={linePoints} />
    </svg>
  );
}

interface StrategyCardProps {
  strategy: StrategyInfo;
  selected: boolean;
  deployStatus: DeployStatus;
  onSelect: () => void;
  onEdit: () => void;
  onNavigate: (target: PageId, config: StrategyConfig) => void;
  strategyConfig: StrategyConfig | null;
}

function StrategyCard({
  strategy,
  selected,
  deployStatus,
  onSelect,
  onEdit,
  onNavigate,
  strategyConfig,
}: StrategyCardProps) {
  const [history, setHistory] = useState<SessionRecord[] | null>(null);
  // 卡片掛載當下的時間點，固定住「近 30 天」的邊界，不在每次 render 時重算
  // （react-hooks/purity：render 不能呼叫 Date.now() 這種不純函式）。
  const [now] = useState(() => Date.now());

  useEffect(() => {
    let cancelled = false;
    invoke<SessionRecord[]>("list_sessions", {
      filter: { kinds: ["backtest"], strategyId: strategy.id },
    })
      .then((sessions) => {
        if (!cancelled) setHistory(sessions);
      })
      .catch(() => {
        if (!cancelled) setHistory([]);
      });
    return () => {
      cancelled = true;
    };
  }, [strategy.id]);

  const sorted = history ? [...history].sort((a, b) => b.startedAtMs - a.startedAtMs) : null;
  const latest = sorted?.[0] ?? null;
  const since = now - RECENT_WINDOW_MS;
  const recent = sorted?.filter((s) => s.startedAtMs >= since) ?? [];
  const recentAscending = [...recent].sort((a, b) => a.startedAtMs - b.startedAtMs);
  const recentReturns = recentAscending
    .map((s) => s.metrics?.totalReturn)
    .filter((v): v is string => v !== null && v !== undefined)
    .map(Number);
  const recentAverage =
    recentReturns.length > 0
      ? recentReturns.reduce((a, b) => a + b, 0) / recentReturns.length
      : null;

  return (
    <li
      key={strategy.id}
      className={selected ? "strategy-card strategy-card--selected" : "strategy-card"}
    >
      <div className="strategy-card__header">
        <button
          type="button"
          className="strategy-card__select"
          aria-pressed={selected}
          onClick={onSelect}
        >
          <span className="strategy-card__name">{strategy.name}</span>
          <span className="strategy-card__params">{paramSummary(strategy)}</span>
        </button>
        <span className={`strategy-card__badge strategy-card__badge--${deployStatus.kind}`}>
          {deployStatus.label}
        </span>
      </div>

      <div className="strategy-card__markets">
        {SUPPORTED_MARKETS.map((m) => (
          <span key={m} className="strategy-card__market-tag">
            {m === "spot" ? "現貨" : "合約"}
          </span>
        ))}
      </div>

      <div className="strategy-card__stats">
        {history === null && <p className="strategy-card__stats-loading">讀取回測紀錄中…</p>}
        {history !== null && history.length === 0 && (
          <p className="strategy-card__stats-empty">尚無回測</p>
        )}
        {history !== null && history.length > 0 && latest && (
          <>
            <p className="strategy-card__stats-line">
              回測 {history.length} 次・最近 {formatDate(latest.startedAtMs)}
            </p>
            <div className="strategy-card__metrics">
              <div className="strategy-card__metric">
                <span className="strategy-card__metric-label">回測年化</span>
                <span className="strategy-card__metric-value">
                  {formatPercent(latest.metrics?.annualizedReturn ?? null)}
                </span>
              </div>
              <div className="strategy-card__metric">
                <span className="strategy-card__metric-label">回測最大回撤</span>
                <span className="strategy-card__metric-value">
                  {formatPercent(latest.metrics?.maxDrawdown ?? null)}
                </span>
              </div>
              <div className="strategy-card__metric">
                <span className="strategy-card__metric-label">近 30 天回測報酬</span>
                <span className="strategy-card__metric-value">
                  {recentAverage === null ? "近 30 天無回測資料" : formatPercent(String(recentAverage))}
                </span>
                {/* 誠實標示：這不是實盤報酬，這個專案還沒有實盤/模擬的「30 天報酬」資料來源，
                    只能用該期間內跑過幾次回測的平均總報酬近似。 */}
                <span className="strategy-card__metric-caveat">非實盤，近 30 天回測平均</span>
              </div>
            </div>
            <Sparkline points={recentReturns} />
          </>
        )}
      </div>

      <div className="strategy-card__actions">
        <button
          type="button"
          className="strategy-card__edit"
          aria-label={`調整${strategy.name}參數`}
          onClick={onEdit}
        >
          調整參數
        </button>
        <button
          type="button"
          className="strategy-card__action"
          aria-label={`回測${strategy.name}`}
          onClick={() => onNavigate("backtest", configFor(strategy, strategyConfig))}
        >
          回測
        </button>
        <button
          type="button"
          className="strategy-card__action"
          aria-label={`模擬${strategy.name}`}
          onClick={() => onNavigate("paperTrading", configFor(strategy, strategyConfig))}
        >
          模擬
        </button>
        <button
          type="button"
          className="strategy-card__action"
          aria-label={`部署${strategy.name}`}
          onClick={() => onNavigate("testnetTrading", configFor(strategy, strategyConfig))}
        >
          部署
        </button>
      </div>
    </li>
  );
}

interface StrategiesProps {
  strategyConfig: StrategyConfig | null;
  onApplyConfig: (config: StrategyConfig) => void;
  onNavigate: (target: PageId, config: StrategyConfig) => void;
}

export function Strategies({ strategyConfig, onApplyConfig, onNavigate }: StrategiesProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [tab, setTab] = useState<StrategyTab>("builtin");
  const [marketFilter, setMarketFilter] = useState<MarketFilter>("all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [liveSessions, setLiveSessions] = useState<LiveSessionDto[]>([]);

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch((err: unknown) => setError(String(err)));
  }, []);

  useEffect(() => {
    invoke<LiveSessionDto[]>("list_live_sessions")
      .then(setLiveSessions)
      .catch(() => setLiveSessions([]));
  }, []);

  if (error) {
    return <p role="alert">讀取內建策略失敗：{error}</p>;
  }
  if (!strategies) {
    return <p>讀取中…</p>;
  }

  const editingStrategy = strategies.find((strategy) => strategy.id === editingId) ?? null;
  if (editingStrategy) {
    const initialValues =
      strategyConfig?.strategyId === editingStrategy.id
        ? strategyConfig.values
        : defaultValues(editingStrategy);
    return (
      <StrategyParamForm
        strategy={editingStrategy}
        initialValues={initialValues}
        onApply={(values) => {
          onApplyConfig({ strategyId: editingStrategy.id, values });
          setSelectedId(editingStrategy.id);
          setEditingId(null);
        }}
        onCancel={() => setEditingId(null)}
      />
    );
  }

  const visibleStrategies =
    tab === "mine"
      ? []
      : strategies.filter((strategy) => {
          if (marketFilter !== "all" && !SUPPORTED_MARKETS.includes(marketFilter)) {
            return false;
          }
          const status = deployStatusFor(strategy.id, liveSessions);
          if (statusFilter === "none" && status.kind !== "none") return false;
          if (statusFilter === "paper" && status.kind !== "paper") return false;
          if (statusFilter === "testnet" && status.kind !== "testnet") return false;
          if (statusFilter === "deployed" && status.kind === "none") return false;
          return true;
        });

  return (
    <section aria-label="策略庫">
      <div className="strategy-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "mine"}
          className={tab === "mine" ? "strategy-tab strategy-tab--active" : "strategy-tab"}
          onClick={() => setTab("mine")}
        >
          我的策略
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "builtin"}
          className={tab === "builtin" ? "strategy-tab strategy-tab--active" : "strategy-tab"}
          onClick={() => setTab("builtin")}
        >
          內建範本
        </button>
      </div>

      {tab === "builtin" && (
        <div className="strategy-filters">
          <label>
            市場
            <select
              aria-label="市場"
              value={marketFilter}
              onChange={(e) => setMarketFilter(e.target.value as MarketFilter)}
            >
              <option value="all">全部市場</option>
              <option value="spot">現貨</option>
              <option value="usdm_perp">合約</option>
            </select>
          </label>
          <label>
            狀態
            <select
              aria-label="狀態"
              value={statusFilter}
              onChange={(e) => setStatusFilter(e.target.value as StatusFilter)}
            >
              <option value="all">全部狀態</option>
              <option value="deployed">模擬中或測試網中</option>
              <option value="paper">模擬中</option>
              <option value="testnet">測試網中</option>
              <option value="none">未部署</option>
            </select>
          </label>
        </div>
      )}

      {tab === "mine" && (
        <p className="strategy-empty-state">
          還沒有儲存過的自訂策略。目前只能在回測頁用 JSON 手動帶入自訂 DSL，還沒有「存成策略庫項目」的流程。
        </p>
      )}

      {tab === "builtin" && (
        <ul className="strategy-grid">
          {visibleStrategies.map((strategy) => (
            <StrategyCard
              key={strategy.id}
              strategy={strategy}
              selected={strategy.id === selectedId}
              deployStatus={deployStatusFor(strategy.id, liveSessions)}
              onSelect={() => setSelectedId(strategy.id)}
              onEdit={() => setEditingId(strategy.id)}
              onNavigate={onNavigate}
              strategyConfig={strategyConfig}
            />
          ))}
        </ul>
      )}
    </section>
  );
}
