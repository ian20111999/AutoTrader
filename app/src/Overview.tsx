import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { LiveSessionDto, SessionRecord } from "./overviewTypes";
import type { BacktestSummary, EquityPoint } from "./backtestTypes";
import { OverviewEquityChart, type OverviewEquityPoint } from "./OverviewEquityChart";
import { formatSignedPercent, formatDrawdown, formatSharpe } from "./backtestFormat";

const SESSION_REGISTRY_CHANGED_EVENT = "session-registry-changed";
const SIXTY_DAYS_MS = 60 * 24 * 60 * 60 * 1000;
const RECENT_BACKTEST_LIMIT = 8;

interface OverviewProps {
  onOpenPaperTrading: (sessionId: string) => void;
  onOpenTestnetTrading: (sessionId: string) => void;
  onOpenBacktestCompare: (summary: BacktestSummary) => void;
}

const KIND_LABEL: Record<string, string> = {
  backtest: "回測",
  paper: "模擬",
  testnet: "測試網",
  live: "實盤",
};

const MARKET_LABEL: Record<string, string> = {
  spot: "現貨",
  usdmPerp: "U 本位合約",
};

// 金額（equity/dailyPnl）用字串傳遞的 Fixed：這裡的 Number() 只用來「顯示」
// 四捨五入，不是下單相關計算，符合專案規則。沒有統一貨幣單位可標示（交易對
// 不保證都是 USDT 計價），所以不加貨幣後綴，只給正負號。
function formatSignedAmount(raw: string | null): string {
  if (raw === null) return "—";
  const value = Number(raw);
  if (value === 0) return "0";
  const sign = value > 0 ? "+" : "−";
  return `${sign}${Math.abs(value).toFixed(2)}`;
}

function formatAmount(raw: string | null): string {
  if (raw === null) return "—";
  return Number(raw).toFixed(2);
}

function amountClass(raw: string | null): string {
  if (raw === null) return "";
  const value = Number(raw);
  if (value > 0) return "backtest-metrics__value--positive";
  if (value < 0) return "backtest-metrics__value--negative";
  return "";
}

// 「最近回測」列點了要加進比較頁：SessionRecord 沒有存 year/month（比較頁拿
// 來抓對應 BTC 買入持有基準用），所以從 dataSourcePath 的檔名（固定
// `YYYY-MM.csv` 格式，見 app/src-tauri/src/backtest.rs 的下載/讀取路徑）解析；
// 解析不到就退回用 startedAtMs 的月份——基準曲線可能因此抓不到對應月份，
// 但不影響這筆回測本身的指標顯示是正確的。
function deriveYearMonth(record: SessionRecord): { year: number; month: number } {
  const match = record.dataSourcePath?.match(/(\d{4})-(\d{2})/);
  if (match) {
    return { year: Number(match[1]), month: Number(match[2]) };
  }
  const d = new Date(record.startedAtMs);
  return { year: d.getUTCFullYear(), month: d.getUTCMonth() + 1 };
}

function recordToSummary(record: SessionRecord, curve: EquityPoint[]): BacktestSummary {
  const { year, month } = deriveYearMonth(record);
  return {
    sessionId: record.id,
    symbol: record.symbol,
    interval: record.interval,
    year,
    month,
    strategyId: record.strategyId,
    strategyName: record.strategyName,
    params: record.params,
    startingCapital: record.startingCapital,
    barCount: record.barsSeen,
    curve,
    trades: 0,
    liquidations: 0,
    totalReturn: record.metrics?.totalReturn ?? null,
    annualizedReturn: record.metrics?.annualizedReturn ?? null,
    maxDrawdown: record.metrics?.maxDrawdown ?? "0",
    sharpe: record.metrics?.sharpe ?? null,
    spanYears: record.metrics?.spanYears ?? null,
    feeModel: "",
    slippage: "0",
    market: record.market,
    direction: "long_only",
    leverage: "1",
    marginMode: null,
    dataSourcePath: record.dataSourcePath ?? "",
  };
}

export function Overview({
  onOpenPaperTrading,
  onOpenTestnetTrading,
  onOpenBacktestCompare,
}: OverviewProps) {
  const [liveSessions, setLiveSessions] = useState<LiveSessionDto[] | null>(null);
  const [liveError, setLiveError] = useState<string | null>(null);

  const [recentBacktests, setRecentBacktests] = useState<SessionRecord[] | null>(null);
  const [curveRecords, setCurveRecords] = useState<SessionRecord[] | null>(null);
  const [addingToCompare, setAddingToCompare] = useState<string | null>(null);
  const [addError, setAddError] = useState<string | null>(null);

  function refreshLiveSessions() {
    invoke<LiveSessionDto[]>("list_live_sessions")
      .then((sessions) => {
        setLiveSessions(sessions);
        setLiveError(null);
      })
      .catch((err) => setLiveError(String(err)));
  }

  useEffect(() => {
    refreshLiveSessions();
    const unlisten = listen(SESSION_REGISTRY_CHANGED_EVENT, () => refreshLiveSessions());
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    invoke<SessionRecord[]>("list_sessions", {
      filter: { kinds: ["backtest"], limit: RECENT_BACKTEST_LIMIT },
    })
      .then(setRecentBacktests)
      .catch(() => setRecentBacktests([]));

    invoke<SessionRecord[]>("list_sessions", {
      filter: { kinds: ["paper", "testnet"], sinceMs: Date.now() - SIXTY_DAYS_MS },
    })
      .then(setCurveRecords)
      .catch(() => setCurveRecords([]));
  }, []);

  const liveByKind = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const s of liveSessions ?? []) {
      counts[s.kind] = (counts[s.kind] ?? 0) + 1;
    }
    return counts;
  }, [liveSessions]);

  const curvePoints: OverviewEquityPoint[] = useMemo(() => {
    return (curveRecords ?? [])
      .filter((r) => r.finalEquity !== null)
      .map((r) => ({
        atMs: r.endedAtMs ?? r.startedAtMs,
        equity: Number(r.finalEquity),
        label: `${r.symbol}・${KIND_LABEL[r.kind] ?? r.kind}`,
      }))
      .sort((a, b) => a.atMs - b.atMs);
  }, [curveRecords]);

  function handleRowClick(session: LiveSessionDto) {
    if (session.kind === "paper") {
      onOpenPaperTrading(session.sessionId);
    } else if (session.kind === "testnet") {
      onOpenTestnetTrading(session.sessionId);
    }
    // backtest/live 目前不會出現在 list_live_sessions（backtest 不是即時場次、
    // live 還沒有實作），點了沒有對應頁面可去，不處理。
  }

  async function handleAddToCompare(record: SessionRecord) {
    setAddingToCompare(record.id);
    setAddError(null);
    try {
      const curve = await invoke<EquityPoint[]>("read_session_curve", {
        sessionId: record.id,
        maxPoints: null,
      });
      onOpenBacktestCompare(recordToSummary(record, curve));
    } catch (err) {
      setAddError(String(err));
    } finally {
      setAddingToCompare(null);
    }
  }

  return (
    <div className="overview">
      <section className="overview-stats" aria-label="統計卡片">
        <div className="overview-card">
          <h2 className="overview-card__title">執行中策略數</h2>
          <p className="overview-card__big">{liveSessions?.length ?? "—"}</p>
          <p className="overview-card__sub">
            模擬 {liveByKind.paper ?? 0}・測試網 {liveByKind.testnet ?? 0}
          </p>
          {liveError && <p className="backtest-status--error">{liveError}</p>}
        </div>

        <div className="overview-card overview-card--wide">
          <h2 className="overview-card__title">帳戶權益 / 今日損益</h2>
          <p className="overview-card__note">
            這個專案沒有查詢交易所真實帳戶餘額的端點；以下是各執行中 session
            的本地帳本（模擬／測試網虛擬資金），逐場列出、不加總——不同
            session 的起始資金與假設不同，加總沒有金融意義。
          </p>
          {liveSessions && liveSessions.length > 0 ? (
            <ul className="overview-equity-list">
              {liveSessions.map((s) => (
                <li key={s.sessionId} className="overview-equity-list__item">
                  <span>
                    {s.symbol}・{KIND_LABEL[s.kind] ?? s.kind}
                    {s.stale && <span className="overview-badge overview-badge--stale">過期</span>}
                  </span>
                  <span className={amountClass(s.equity)}>權益 {formatAmount(s.equity)}</span>
                  <span className={amountClass(s.dailyPnl)}>
                    今日損益 {formatSignedAmount(s.dailyPnl)}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="overview-card__sub">目前沒有執行中的 session。</p>
          )}
        </div>
      </section>

      <section className="overview-section" aria-label="60 天權益曲線">
        <h2 className="overview-section__title">已結束 session 的最終權益</h2>
        <p className="overview-card__note">
          每個點是一場已結束模擬／測試網 session 收尾時的最終權益（不是連續的帳戶餘額），
          非交易所真實餘額。資料範圍：近 60 天內結束的 session，目前累積多少就顯示多少。
        </p>
        <OverviewEquityChart points={curvePoints} />
      </section>

      <section className="overview-section" aria-label="執行中策略">
        <h2 className="overview-section__title">執行中策略</h2>
        {liveSessions && liveSessions.length > 0 ? (
          <table className="compare-table">
            <caption className="compare-table__caption">點一列可前往該 session 的畫面</caption>
            <thead>
              <tr>
                <th scope="col">策略</th>
                <th scope="col">市場</th>
                <th scope="col">交易對</th>
                <th scope="col">週期</th>
                <th scope="col">模式</th>
                <th scope="col">今日損益</th>
                <th scope="col">部位</th>
                <th scope="col">狀態</th>
              </tr>
            </thead>
            <tbody>
              {liveSessions.map((s) => (
                <tr
                  key={s.sessionId}
                  className="overview-table__row"
                  tabIndex={0}
                  role="button"
                  aria-label={`前往 ${s.symbol} ${KIND_LABEL[s.kind] ?? s.kind} session`}
                  onClick={() => handleRowClick(s)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") handleRowClick(s);
                  }}
                >
                  <td>{s.strategyName}</td>
                  <td>{MARKET_LABEL[s.market] ?? s.market}</td>
                  <td>{s.symbol}</td>
                  <td>{s.interval}</td>
                  <td>{KIND_LABEL[s.kind] ?? s.kind}</td>
                  <td className={amountClass(s.dailyPnl)}>{formatSignedAmount(s.dailyPnl)}</td>
                  <td>{s.position}</td>
                  <td>
                    {s.stale ? (
                      <span className="overview-badge overview-badge--stale">資料過期</span>
                    ) : (
                      "執行中"
                    )}
                    {s.killSwitch && (
                      <span className="overview-badge overview-badge--danger">熔斷</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="overview-card__sub">目前沒有執行中的策略。</p>
        )}
      </section>

      <section className="overview-section" aria-label="最近回測">
        <h2 className="overview-section__title">最近回測</h2>
        {addError && <p className="backtest-status--error">{addError}</p>}
        {recentBacktests && recentBacktests.length > 0 ? (
          <table className="compare-table">
            <thead>
              <tr>
                <th scope="col">策略</th>
                <th scope="col">交易對</th>
                <th scope="col">週期</th>
                <th scope="col">總報酬</th>
                <th scope="col">最大回撤</th>
                <th scope="col">夏普</th>
                <th scope="col" aria-label="加入比較"></th>
              </tr>
            </thead>
            <tbody>
              {recentBacktests.map((r) => (
                <tr key={r.id}>
                  <td>{r.strategyName}</td>
                  <td>{r.symbol}</td>
                  <td>{r.interval}</td>
                  <td className={amountClass(r.metrics?.totalReturn ?? null)}>
                    {formatSignedPercent(r.metrics?.totalReturn ?? null)}
                  </td>
                  <td className="backtest-metrics__value--negative">
                    {r.metrics?.maxDrawdown ? formatDrawdown(r.metrics.maxDrawdown) : "—"}
                  </td>
                  <td>{formatSharpe(r.metrics?.sharpe ?? null)}</td>
                  <td>
                    <button
                      type="button"
                      onClick={() => handleAddToCompare(r)}
                      disabled={addingToCompare === r.id}
                    >
                      {addingToCompare === r.id ? "載入中…" : "加入比較"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="overview-card__sub">還沒有任何回測紀錄。</p>
        )}
      </section>
    </div>
  );
}
