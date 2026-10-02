import { useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { StrategyConfig, StrategyInfo } from "./strategyTypes";
import type {
  PaperSnapshot,
  PaperTradingStatus,
  PaperUpdateEnvelope,
  StartPaperTradingRequest,
} from "./paperTradingTypes";
import { PAPER_TRADING_EVENT } from "./paperTradingTypes";
import { INTERVAL_OPTIONS } from "./backtestTypes";
import type { EquityPoint } from "./backtestTypes";
import type { SessionRecord } from "./overviewTypes";
import { EquityCurveChart } from "./EquityCurveChart";
import { EquityCompareChart } from "./EquityCompareChart";
import { formatSignedPercent } from "./backtestFormat";

interface PaperTradingProps {
  strategyConfig: StrategyConfig | null;
  onGoToStrategies: () => void;
}

type Phase = "idle" | "running" | "stopped" | "failed";

// 畫面記得「上次選中的那場」，重新整理/切回這一頁時還能回到同一場。多場並行時
// 這裡存的是目前選中分頁的 sessionId，不是唯一一場模擬交易的 id。
const SESSION_ID_STORAGE_KEY = "paperTrading.sessionId";

const SESSION_REGISTRY_CHANGED_EVENT = "session-registry-changed";

const STATUS_LABEL: Record<SessionRecord["status"], string> = {
  running: "執行中",
  stopped: "已停止",
  failed: "失敗",
  completed: "已停止",
  interrupted: "中斷",
};

function statusBadgeClass(status: SessionRecord["status"]): string {
  if (status === "running") return "overview-badge";
  if (status === "failed" || status === "interrupted") return "overview-badge overview-badge--danger";
  return "overview-badge overview-badge--stale";
}

function symbolError(raw: string): string | null {
  return raw.trim() === "" ? "請輸入交易對代號" : null;
}

function capitalError(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") return "請輸入起始資金";
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return "必須是數字";
  if (Number(trimmed) <= 0) return "起始資金必須大於 0";
  return null;
}

function sessionStatusToPhase(status: SessionRecord["status"]): Phase {
  if (status === "running") return "running";
  if (status === "failed" || status === "interrupted") return "failed";
  return "stopped"; // "stopped" | "completed"
}

function strategySummary(strategy: StrategyInfo, values: Record<string, string>): string {
  return strategy.params.map((param) => `${param.label}${values[param.key] ?? ""}`).join("、");
}

// 頂層函式（不是元件內的巢狀函式）：`Date.now()` 這種非純函式呼叫只能放在
// render 以外的地方，這裡只會被 handleStart 這個事件處理器呼叫，不會在渲染期間跑。
function buildOptimisticSession(
  id: string,
  request: StartPaperTradingRequest,
  strategyName: string,
): SessionRecord {
  return {
    schemaVersion: 0,
    id,
    kind: "paper",
    market: "spot",
    symbol: request.symbol,
    interval: request.interval,
    strategyId: request.strategyId,
    strategyName,
    params: request.params,
    startedAtMs: Date.now(),
    endedAtMs: null,
    status: "running",
    statusMessage: null,
    startingCapital: request.startingCapital,
    finalEquity: null,
    barsSeen: 0,
    metrics: null,
    saved: false,
    dataSourcePath: null,
  };
}

export function PaperTrading({ strategyConfig, onGoToStrategies }: PaperTradingProps) {
  const [strategies, setStrategies] = useState<StrategyInfo[] | null>(null);
  const [symbol, setSymbol] = useState("BTCUSDT");
  const [interval, setInterval] = useState<string>("1m");
  const [startingCapital, setStartingCapital] = useState("10000");
  const [submitted, setSubmitted] = useState(false);

  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const [sessions, setSessions] = useState<SessionRecord[] | null>(null);
  const [sessionsError, setSessionsError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(() =>
    localStorage.getItem(SESSION_ID_STORAGE_KEY),
  );

  useEffect(() => {
    invoke<StrategyInfo[]>("list_builtin_strategies")
      .then(setStrategies)
      .catch(() => setStrategies([]));
  }, []);

  function refreshSessions() {
    invoke<SessionRecord[]>("list_sessions", { filter: { kinds: ["paper"], limit: 30 } })
      .then((records) => {
        setSessions(records);
        setSessionsError(null);
      })
      .catch((err: unknown) => setSessionsError(String(err)));
  }

  useEffect(() => {
    refreshSessions();
    // "tick" 只是某一場的帳本多了一筆（跟分頁列表無關），不值得為它重查一次
    // 分頁列表；session 開始/停止/失敗才會影響分頁有哪些、狀態是什麼。
    const unlistenPromise = listen<{ change: string }>(SESSION_REGISTRY_CHANGED_EVENT, (event) => {
      if (event.payload.change === "tick") return;
      refreshSessions();
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const sortedSessions = sessions ? [...sessions].sort((a, b) => b.startedAtMs - a.startedAtMs) : null;
  // 選中的分頁：沿用使用者上次選的那場（如果還存在於列表），否則退回最新一場。
  // 用 render 時算出來，不用 effect + setState：避免「先渲染一次 idle 分頁，
  // 下一個 tick 才跳到最新一場」這種多餘的中間渲染。
  const effectiveSelectedId =
    selectedId && sortedSessions?.some((s) => s.id === selectedId)
      ? selectedId
      : sortedSessions?.[0]?.id ?? null;

  useEffect(() => {
    if (effectiveSelectedId) localStorage.setItem(SESSION_ID_STORAGE_KEY, effectiveSelectedId);
  }, [effectiveSelectedId]);

  const isCustomStrategy = strategyConfig?.strategyId === "custom";
  const selectedStrategy =
    strategyConfig && strategies && !isCustomStrategy
      ? (strategies.find((s) => s.id === strategyConfig.strategyId) ?? null)
      : null;

  const fieldErrors = {
    symbol: symbolError(symbol),
    startingCapital: capitalError(startingCapital),
  };
  const hasFieldError = Object.values(fieldErrors).some((message) => message !== null);

  async function handleStart(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (hasFieldError || !strategyConfig) return;

    const request: StartPaperTradingRequest = {
      symbol: symbol.trim(),
      interval,
      strategyId: strategyConfig.strategyId,
      params: strategyConfig.values,
      startingCapital: startingCapital.trim(),
      dslJson: isCustomStrategy ? strategyConfig.dslJson ?? null : null,
    };

    setStarting(true);
    setStartError(null);
    try {
      const newSessionId = await invoke<string>("start_paper_trading", { request });
      // 樂觀插入這場新分頁：不等 list_sessions 重查就能馬上看到它。後端在
      // `start_paper_trading` 插入 registry 之後會 emit `session-registry-changed`
      // （change: "started"），上面的監聽器收到後會重查一次拿到權威版本，
      // 這裡不用自己再呼叫一次 refreshSessions。
      const optimistic = buildOptimisticSession(
        newSessionId,
        request,
        selectedStrategy?.name ?? request.strategyId,
      );
      setSessions((prev) => [optimistic, ...(prev ?? []).filter((s) => s.id !== newSessionId)]);
      setSelectedId(newSessionId);
    } catch (err) {
      setStartError(String(err));
    } finally {
      setStarting(false);
    }
  }

  const selectedSession = sortedSessions?.find((s) => s.id === effectiveSelectedId) ?? null;

  return (
    <div className="backtest">
      <form className="backtest-form" onSubmit={handleStart} noValidate>
        <div className="backtest-form__field">
          <span className="backtest-form__label">策略</span>
          {strategyConfig ? (
            <p className="backtest-form__strategy">
              {isCustomStrategy
                ? `${strategyConfig.dslName ?? "自訂策略（DSL）"}（積木編輯器）`
                : selectedStrategy
                  ? `${selectedStrategy.name}：${strategySummary(selectedStrategy, strategyConfig.values)}`
                  : "讀取策略資料中…"}
            </p>
          ) : (
            <div role="alert" className="backtest-form__no-strategy">
              <p>還沒有選擇策略，請先到策略庫選一個策略並調整參數。</p>
              <button type="button" onClick={onGoToStrategies}>
                前往策略庫
              </button>
            </div>
          )}
        </div>

        <fieldset className="backtest-form__fieldset" disabled={starting}>
          <legend className="backtest-form__label">即時行情</legend>

          <div className="backtest-form__field">
            <label htmlFor="paper-symbol">交易對</label>
            <input
              id="paper-symbol"
              type="text"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value)}
              aria-invalid={submitted && fieldErrors.symbol ? true : undefined}
              aria-describedby={submitted && fieldErrors.symbol ? "paper-symbol-error" : undefined}
            />
            {submitted && fieldErrors.symbol && (
              <p id="paper-symbol-error" role="alert" className="backtest-form__error">
                {fieldErrors.symbol}
              </p>
            )}
          </div>

          <div className="backtest-form__field">
            <label htmlFor="paper-interval">週期</label>
            <select
              id="paper-interval"
              value={interval}
              onChange={(e) => setInterval(e.target.value)}
            >
              {INTERVAL_OPTIONS.map((option) => (
                <option key={option} value={option}>
                  {option}
                </option>
              ))}
            </select>
          </div>

          <div className="backtest-form__field">
            <label htmlFor="paper-capital">虛擬起始資金</label>
            <input
              id="paper-capital"
              type="text"
              inputMode="decimal"
              value={startingCapital}
              onChange={(e) => setStartingCapital(e.target.value)}
              aria-invalid={submitted && fieldErrors.startingCapital ? true : undefined}
              aria-describedby={
                submitted && fieldErrors.startingCapital ? "paper-capital-error" : undefined
              }
            />
            {submitted && fieldErrors.startingCapital && (
              <p id="paper-capital-error" role="alert" className="backtest-form__error">
                {fieldErrors.startingCapital}
              </p>
            )}
          </div>

          <button type="submit" className="backtest-form__submit">
            {starting ? "啟動中…" : "開始模擬"}
          </button>
        </fieldset>
      </form>

      <div className="backtest-main">
        {sessionsError && (
          <p role="alert" className="backtest-status backtest-status--error">
            讀取模擬交易列表失敗：{sessionsError}
          </p>
        )}
        {startError && (
          <p role="alert" className="backtest-status backtest-status--error">
            開始模擬失敗：{startError}
          </p>
        )}

        {sortedSessions && sortedSessions.length > 0 && (
          <div className="strategy-tabs" role="tablist" aria-label="模擬交易場次">
            {sortedSessions.map((s) => (
              <button
                key={s.id}
                type="button"
                role="tab"
                aria-selected={s.id === effectiveSelectedId}
                className={
                  s.id === effectiveSelectedId ? "strategy-tab strategy-tab--active" : "strategy-tab"
                }
                onClick={() => setSelectedId(s.id)}
              >
                {s.symbol}・{s.strategyName}
                <span className={statusBadgeClass(s.status)}>{STATUS_LABEL[s.status]}</span>
              </button>
            ))}
          </div>
        )}

        {selectedSession ? (
          <PaperTradingSession key={selectedSession.id} session={selectedSession} />
        ) : sortedSessions && sortedSessions.length === 0 ? (
          <p className="overview-card__sub">
            還沒有開始過任何模擬交易，填左側表單開始第一場。
          </p>
        ) : null}
      </div>
    </div>
  );
}

interface PaperTradingSessionProps {
  session: SessionRecord;
}

/// 單一場模擬交易的即時狀態 + 停止按鈕 + 模擬 vs 回測比較。每個分頁各自掛載一個，
/// 切分頁時換掉 key 讓這個元件重新掛載，狀態不會互相污染。
///
/// 初始狀態直接沿用分頁列表（`list_sessions`）已經知道的 `session.status`，不等
/// `paper_trading_status` 查完才顯示——剛開始一場新模擬時，使用者不該等一次額外的
/// IPC 往返才看到「執行中」。停止/失敗時也不由這裡呼叫父層重查分頁列表：後端
/// 收尾時會另外 emit `session-registry-changed`（非 "tick"），父層自己的監聽器
/// 會接手重查，這裡重複呼叫沒有必要。
function PaperTradingSession({ session }: PaperTradingSessionProps) {
  const sessionId = session.id;
  const sessionIdRef = useRef(sessionId);
  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  const [phase, setPhase] = useState<Phase>(() => sessionStatusToPhase(session.status));
  const [curve, setCurve] = useState<PaperSnapshot[]>([]);
  const [latest, setLatest] = useState<PaperSnapshot | null>(null);
  const [failedMessage, setFailedMessage] = useState<string | null>(() =>
    sessionStatusToPhase(session.status) === "failed" ? session.statusMessage : null,
  );
  const [statusError, setStatusError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);

  useEffect(() => {
    invoke<PaperTradingStatus>("paper_trading_status", { sessionId })
      .then((status) => {
        if (status.status === "idle") return;
        setPhase(status.status);
        if (status.snapshot) {
          setLatest(status.snapshot);
          setCurve([status.snapshot]);
        }
        if (status.status === "failed") {
          setFailedMessage(status.message);
        }
      })
      .catch((err: unknown) => setStatusError(String(err)));
  }, [sessionId]);

  useEffect(() => {
    const unlistenPromise = listen<PaperUpdateEnvelope>(PAPER_TRADING_EVENT, (event) => {
      const payload = event.payload;
      if (payload.sessionId !== sessionIdRef.current) return;
      if (payload.type === "bar") {
        setLatest(payload.snapshot);
        setCurve((prev) => [...prev, payload.snapshot]);
      } else if (payload.type === "stopped") {
        setPhase("stopped");
      } else {
        setPhase("failed");
        setFailedMessage(payload.message);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const isRunning = phase === "running";

  async function handleStop() {
    setStopping(true);
    setStopError(null);
    try {
      await invoke("stop_paper_trading", { sessionId });
    } catch (err) {
      setStopError(String(err));
    } finally {
      setStopping(false);
    }
  }

  return (
    <div className="paper-trading-session">
      {statusError && (
        <p role="alert" className="backtest-status backtest-status--error">
          讀取模擬交易狀態失敗：{statusError}
        </p>
      )}

      {isRunning && (
        <p role="status" className="backtest-status paper-trading-status--running">
          模擬交易執行中，即時行情、模擬成交，不會送出真實訂單。
        </p>
      )}
      {phase === "stopped" && (
        <p role="status" className="backtest-status">
          已停止模擬交易。
        </p>
      )}
      {phase === "failed" && failedMessage && (
        <p role="alert" className="backtest-status backtest-status--error">
          模擬交易中止：{failedMessage}（帳本已經不可信，請重新開始）
        </p>
      )}

      {latest && (
        <section aria-label="模擬交易結果" className="backtest-result">
          <EquityCurveChart curve={curve} startingCapital={session.startingCapital} />
          <dl className="paper-trading-stats">
            <div className="backtest-metrics__item">
              <dt>模擬權益</dt>
              <dd>{latest.equity}</dd>
            </div>
            <div className="backtest-metrics__item">
              <dt>現金</dt>
              <dd>{latest.cash}</dd>
            </div>
            <div className="backtest-metrics__item">
              <dt>部位</dt>
              <dd>{latest.position}</dd>
            </div>
            <div className="backtest-metrics__item">
              <dt>成交筆數</dt>
              <dd>{latest.trades}</dd>
            </div>
            <div className="backtest-metrics__item">
              <dt>強制平倉次數</dt>
              <dd>{latest.liquidations}</dd>
            </div>
          </dl>

          {stopError && (
            <p role="alert" className="backtest-status backtest-status--error">
              停止失敗：{stopError}
            </p>
          )}
          {isRunning && (
            <button
              type="button"
              className="settings-clear__button paper-trading-stop"
              onClick={handleStop}
              disabled={stopping}
            >
              {stopping ? "停止中…" : "停止模擬"}
            </button>
          )}
        </section>
      )}

      <PaperTradingCompare session={session} />
    </div>
  );
}

type BacktestMatch = SessionRecord | "none" | null;

/// 模擬 vs 回測對比：找「同策略/同交易對/同週期」最新一筆回測紀錄疊圖比較，
/// 找不到就誠實說找不到，不畫假的對比線。差距拆解只給「總報酬差距」這一個
/// 誠實數字——帳本沒有保留每筆成交的手續費/滑價/資金費金額，無法再往下拆。
function PaperTradingCompare({ session }: { session: SessionRecord }) {
  const [match, setMatch] = useState<BacktestMatch>(null);
  const [simCurve, setSimCurve] = useState<EquityPoint[] | null>(null);
  const [backtestCurve, setBacktestCurve] = useState<EquityPoint[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 這個元件掛載時 session 的策略/交易對/週期不會再變（切分頁是整個元件換 key
  // 重新掛載），所以不需要在依賴變動時清掉舊的 match/curve 再重查——初始值本來
  // 就是 null，直接讓非同步結果到了再 setState 即可，不用先同步 reset 一次。
  useEffect(() => {
    invoke<SessionRecord[]>("list_sessions", {
      filter: {
        kinds: ["backtest"],
        strategyId: session.strategyId,
        symbol: session.symbol,
        limit: 50,
      },
    })
      .then((records) => {
        const candidates = records.filter((r) => r.interval === session.interval);
        if (candidates.length === 0) {
          setMatch("none");
          return;
        }
        setMatch([...candidates].sort((a, b) => b.startedAtMs - a.startedAtMs)[0]);
      })
      .catch((err: unknown) => setError(String(err)));
  }, [session.strategyId, session.symbol, session.interval]);

  useEffect(() => {
    if (!match || match === "none") return;
    Promise.all([
      invoke<EquityPoint[]>("read_session_curve", { sessionId: session.id, maxPoints: null }),
      invoke<EquityPoint[]>("read_session_curve", { sessionId: match.id, maxPoints: null }),
    ])
      .then(([sim, backtest]) => {
        setSimCurve(sim);
        setBacktestCurve(backtest);
      })
      .catch((err: unknown) => setError(String(err)));
  }, [match, session.id]);

  if (error) {
    return (
      <p role="alert" className="backtest-status backtest-status--error paper-trading-compare">
        模擬 vs 回測比較失敗：{error}
      </p>
    );
  }

  if (match === null) return null; // 查詢中，不急著顯示空狀態
  if (match === "none") {
    return (
      <section aria-label="模擬 vs 回測比較" className="paper-trading-compare">
        <h3>模擬 vs 回測</h3>
        <p className="overview-card__sub">找不到相同策略/交易對/週期的回測紀錄可供比較。</p>
      </section>
    );
  }

  if (!simCurve || !backtestCurve) {
    return (
      <section aria-label="模擬 vs 回測比較" className="paper-trading-compare">
        <h3>模擬 vs 回測</h3>
        <p className="overview-card__sub">載入比較資料中…</p>
      </section>
    );
  }

  const simLast = simCurve[simCurve.length - 1];
  const simStart = Number(session.startingCapital);
  const simReturnRatio =
    simLast && simStart ? (Number(simLast.equity) - simStart) / simStart : null;
  const backtestReturnRaw = match.metrics?.totalReturn ?? null;
  const backtestReturnRatio = backtestReturnRaw !== null ? Number(backtestReturnRaw) : null;
  const gapRatio =
    simReturnRatio !== null && backtestReturnRatio !== null
      ? simReturnRatio - backtestReturnRatio
      : null;

  return (
    <section aria-label="模擬 vs 回測比較" className="paper-trading-compare">
      <h3>模擬 vs 回測（同策略/交易對/週期，{match.symbol}・{match.interval}）</h3>
      <EquityCompareChart
        simCurve={simCurve}
        simStartingCapital={session.startingCapital}
        backtestCurve={backtestCurve}
        backtestStartingCapital={match.startingCapital}
      />
      <dl className="paper-trading-stats">
        <div className="backtest-metrics__item">
          <dt>模擬報酬</dt>
          <dd>{simReturnRatio !== null ? formatSignedPercent(String(simReturnRatio)) : "—"}</dd>
        </div>
        <div className="backtest-metrics__item">
          <dt>回測報酬（同期）</dt>
          <dd>{formatSignedPercent(backtestReturnRaw)}</dd>
        </div>
        <div className="backtest-metrics__item">
          <dt>總報酬差距</dt>
          <dd>{gapRatio !== null ? formatSignedPercent(String(gapRatio)) : "—"}</dd>
        </div>
      </dl>
      <p className="overview-card__note">
        差距拆解（手續費、滑價、資金費各自貢獻多少）需要逐筆成交與成本記錄；目前模擬交易的
        帳本只保留彙總後的權益快照，沒有逐筆的手續費/滑價/資金費金額，無法誠實拆解，
        這裡只列總報酬差距。
      </p>
    </section>
  );
}
