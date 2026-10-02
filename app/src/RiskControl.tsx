import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  BreakerRule,
  RiskEvent,
  RiskStatus,
  SessionExposure,
} from "./riskControlTypes";

const SESSION_REGISTRY_CHANGED_EVENT = "session-registry-changed";
const EVENT_LIMIT = 50;

// 觸發紀錄的時間一律用 UTC 顯示，而且標出來。紀錄裡的時間多數是**交易所時間**
// （每一列自己會寫），把它轉成本機時區顯示，之後查「這兩件事的順序怎麼會反過來」
// 會被自己的畫面誤導。
function formatUtc(atMs: number): string {
  const d = new Date(atMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getUTCMonth() + 1)}/${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(
    d.getUTCMinutes(),
  )}`;
}

// 金額只用來「顯示」四捨五入，不是下單相關計算（同 Overview.tsx 的理由）。
function formatAmount(raw: string | null): string {
  if (raw === null) return "—";
  return Number(raw).toFixed(2);
}

function formatSignedAmount(raw: string | null): string {
  if (raw === null) return "—";
  const value = Number(raw);
  if (value === 0) return "0";
  return `${value > 0 ? "+" : "−"}${Math.abs(value).toFixed(2)}`;
}

// 一場執行中 session 在風控頁上的狀態文字。熔斷優先：它是「現在送不出單」的原因。
function sessionStatusText(session: SessionExposure): string {
  if (session.breakerTrip) return `熔斷已觸發：${session.breakerTrip}`;
  if (session.killSwitch) return "一鍵停止中";
  if (session.asOfMs === null) return "等第一根收盤 K 線";
  return `更新於 ${formatUtc(session.asOfMs)} UTC`;
}

// 全域上限：哪一項今天真的在擋單、哪一項還沒有資料來源。
// **這張表的誠實度比畫面好不好看重要**：ADR-002 §13.2 把「UI 讓使用者以為自己
// 受到保護」列為這份設計最可能造成實際傷害的地方。
const GLOBAL_LIMITS: { label: string; live: boolean; detail: string }[] = [
  {
    label: "單筆金額上限",
    live: true,
    detail: "生效中。由每一場交易自己的風控設定（測試網交易頁的「單筆最大下單金額」）在送單前擋下。",
  },
  {
    label: "單日虧損上限",
    live: true,
    detail:
      "生效中。由每一場交易自己的風控設定（測試網交易頁的「每日最大虧損」）在送單前擋下；只擋會增加曝險的單，平倉不受影響。",
  },
  {
    label: "單一幣種部位上限",
    live: false,
    detail: "尚未生效：需要跨所有執行中策略的即時部位加總（下面的「全域曝險」只顯示、還沒有擋單）。",
  },
  {
    label: "總部位上限",
    live: false,
    detail: "尚未生效：同上，而且總曝險要用毛曝險加總，需要每個交易對的評價價格。",
  },
  {
    label: "價格偏離保護",
    live: false,
    detail: "尚未生效：需要「下單參考價 vs 最新可信價」兩個價格，目前送單路徑只有收盤價。",
  },
  {
    label: "下單頻率自我上限",
    live: false,
    detail: "尚未生效：需要交易所的速率限制值與時間窗內的送單計數。",
  },
  {
    label: "嚴格模式",
    live: false,
    detail: "尚未生效：它的用途是「部署前檢查未達標時禁止實盤」，而「部署」頁要等實盤（第 7 步）。",
  },
];

// 合約風控：這個專案的送單路徑是現貨市價單，沒有任何查保證金率、強平價、
// 資金費的能力（ADR-002 §7）。所以這裡只說明為什麼還不能生效，**不顯示任何
// 看起來像設定值的數字**——假的 2 倍槓桿上限比沒有這張卡更危險。
const FUTURES_ITEMS: string[] = [
  "槓桿上限",
  "保證金模式（逐倉／全倉）",
  "強平距離下限",
  "資金費率預算",
  "交易所端停損",
  "套利淨曝險上限",
];

export function RiskControl() {
  const [rules, setRules] = useState<BreakerRule[] | null>(null);
  const [rulesError, setRulesError] = useState<string | null>(null);
  const [savingRuleId, setSavingRuleId] = useState<string | null>(null);

  const [events, setEvents] = useState<RiskEvent[] | null>(null);
  const [eventsError, setEventsError] = useState<string | null>(null);

  const [status, setStatus] = useState<RiskStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);

  function refreshEvents() {
    invoke<RiskEvent[]>("list_risk_events", { limit: EVENT_LIMIT })
      .then((list) => {
        setEvents(list);
        setEventsError(null);
      })
      .catch((err) => setEventsError(String(err)));
  }

  function refreshStatus() {
    invoke<RiskStatus>("risk_control_status")
      .then((next) => {
        setStatus(next);
        setStatusError(null);
      })
      .catch((err) => setStatusError(String(err)));
  }

  useEffect(() => {
    invoke<BreakerRule[]>("get_breaker_rules")
      .then((list) => {
        setRules(list);
        setRulesError(null);
      })
      .catch((err) => setRulesError(String(err)));
    refreshEvents();
    refreshStatus();
    // 有 session 開始／停止／更新就重新查一次曝險與紀錄（熔斷觸發會在同一時間
    // 寫進紀錄，所以兩個一起更新）。
    const unlisten = listen(SESSION_REGISTRY_CHANGED_EVENT, () => {
      refreshStatus();
      refreshEvents();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  async function handleToggle(rule: BreakerRule) {
    setSavingRuleId(rule.id);
    setRulesError(null);
    try {
      const updated = await invoke<BreakerRule[]>("set_breaker_rule", {
        triggerId: rule.id,
        enabled: !rule.enabled,
      });
      setRules(updated);
    } catch (err) {
      setRulesError(String(err));
    } finally {
      setSavingRuleId(null);
    }
  }

  return (
    <div className="risk-control">
      {status?.accountPause && (
        <p role="alert" className="testnet-warning">
          整個帳戶的下單已被熔斷暫停（{status.accountPause}）。
          請先到交易所後台確認實際部位，確認完再重新啟動 App 才會解除——這一條刻意沒有
          「解除」按鈕。
        </p>
      )}

      <div className="risk-control__columns">
        <div className="risk-control__column">
          <section className="settings-status" aria-label="熔斷規則">
            <h2>熔斷規則</h2>
            <p className="risk-control__hint">
              每一筆測試網訂單送出之前都會跑一次這些規則，任何一條觸發就擋下那筆單並記一筆
              觸發紀錄。觸發之後不會自動恢復：要停止那場交易再重新啟動。
            </p>
            {rulesError && (
              <p role="alert" className="settings-status__error">
                {rulesError}
              </p>
            )}
            {rules === null && !rulesError && <p>讀取中…</p>}
            {rules?.map((rule) => (
              <div className="risk-control__rule" key={rule.id}>
                <div className="risk-control__rule-head">
                  <span className="risk-control__rule-condition">
                    當 {rule.condition} → {rule.action}
                  </span>
                  {rule.mandatory ? (
                    <span className="risk-control__pill" aria-label="強制開啟">
                      強制開啟
                    </span>
                  ) : (
                    <label className="risk-control__switch">
                      <input
                        type="checkbox"
                        checked={rule.enabled}
                        disabled={savingRuleId === rule.id}
                        onChange={() => handleToggle(rule)}
                      />
                      <span>{rule.enabled ? "已啟用" : "已關閉"}</span>
                    </label>
                  )}
                </div>
                <p className="risk-control__rule-note">{rule.note}</p>
              </div>
            ))}
          </section>

          <section className="settings-status" aria-label="全域上限">
            <h2>全域上限</h2>
            <p className="risk-control__hint">
              現貨與合約共用。標成「尚未生效」的項目現在沒有在保護你，所以這裡不給可以填
              的欄位——填了也沒有人會讀。
            </p>
            <dl className="settings-status__list">
              {GLOBAL_LIMITS.map((item) => (
                <div className="settings-status__item" key={item.label}>
                  <dt>
                    {item.label}{" "}
                    <span
                      className={
                        item.live
                          ? "risk-control__pill risk-control__pill--live"
                          : "risk-control__pill risk-control__pill--todo"
                      }
                    >
                      {item.live ? "生效中" : "尚未生效"}
                    </span>
                  </dt>
                  <dd>{item.detail}</dd>
                </div>
              ))}
            </dl>
          </section>

          <section className="settings-status" aria-label="合約風控">
            <h2>合約風控</h2>
            <p className="risk-control__hint">
              目前的送單路徑是現貨市價單，這個專案還沒有查保證金率、強平價與資金費的能力，
              所以下面這些項目一個都還不能生效，也刻意不顯示預設數字（假的上限比沒有上限
              更危險）。需要合約交易（第 7 步之後）。
            </p>
            <ul className="risk-control__todo-list">
              {FUTURES_ITEMS.map((label) => (
                <li key={label}>
                  {label}
                  <span className="risk-control__pill risk-control__pill--todo">尚未生效</span>
                </li>
              ))}
            </ul>
          </section>
        </div>

        <div className="risk-control__column">
          <section className="settings-status" aria-label="全域曝險">
            <h2>全域曝險</h2>
            {statusError && (
              <p role="alert" className="settings-status__error">
                {statusError}
              </p>
            )}
            {status === null && !statusError && <p>讀取中…</p>}
            {status && status.sessions.length === 0 && <p>目前沒有執行中的策略。</p>}
            {status && status.sessions.length > 0 && (
              <table className="parameter-sweep__table">
                <caption className="parameter-sweep__caption">
                  只顯示、還沒有擋單：跨策略的部位上限要等全域上限落地。
                </caption>
                <thead>
                  <tr>
                    <th scope="col">交易對</th>
                    <th scope="col">部位</th>
                    <th scope="col">權益</th>
                    <th scope="col">今日損益</th>
                    <th scope="col">狀態</th>
                  </tr>
                </thead>
                <tbody>
                  {status.sessions.map((s) => (
                    <tr key={s.sessionId}>
                      <th scope="row">{s.symbol}</th>
                      <td>{s.position}</td>
                      <td>{formatAmount(s.equity)}</td>
                      <td>{formatSignedAmount(s.dailyPnl)}</td>
                      <td>{sessionStatusText(s)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section className="settings-status" aria-label="觸發紀錄">
            <h2>觸發紀錄</h2>
            <p className="risk-control__hint">
              最多保留 500 筆，時間一律以 UTC 顯示（多數是交易所時間）。
            </p>
            {eventsError && (
              <p role="alert" className="settings-status__error">
                {eventsError}
              </p>
            )}
            {events === null && !eventsError && <p>讀取中…</p>}
            {events && events.length === 0 && <p>還沒有任何觸發紀錄。</p>}
            {events && events.length > 0 && (
              <table className="parameter-sweep__table">
                <thead>
                  <tr>
                    <th scope="col">時間（UTC）</th>
                    <th scope="col">策略</th>
                    <th scope="col">事件</th>
                  </tr>
                </thead>
                <tbody>
                  {events.map((event) => (
                    <tr key={event.seq}>
                      <th scope="row">{formatUtc(event.atMs)}</th>
                      <td>
                        {event.session ?? "全域"}
                        {event.symbol ? `・${event.symbol}` : ""}
                      </td>
                      <td className="risk-control__event-message">{event.message}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}
