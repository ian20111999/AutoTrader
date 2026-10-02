import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { OrderBookEvent, OrderBookUpdateEnvelope } from "./orderBookTypes";
import { ORDER_BOOK_EVENT, ORDER_BOOK_LEVELS } from "./orderBookTypes";

interface OrderBookPanelProps {
  symbol: string;
}

/// 即時委託簿面板：使用者自己選擇要不要訂閱，跟 K 線交易邏輯完全獨立——
/// 這裡的資料只給人看，不會、也不應該被接進任何送單判斷。開關這個面板
/// 不影響交易中的 session。
export function OrderBookPanel({ symbol }: OrderBookPanelProps) {
  const [enabled, setEnabled] = useState(false);
  const [levels, setLevels] = useState<number>(10);
  const [error, setError] = useState<string | null>(null);
  const [lost, setLost] = useState(false);
  const [snapshot, setSnapshot] = useState<
    (OrderBookEvent & { type: "snapshot" }) | null
  >(null);

  const subscriptionIdRef = useRef<string | null>(null);

  // 訂閱生命週期：enabled/symbol/levels 任何一個變了都重新訂閱一次，
  // effect 清理時一定呼叫 unsubscribe_order_book——這是唯一能保證背景
  // 執行緒真的斷線、不留孤兒連線的地方（理由見 order_book.rs 模組文件）。
  // 上一次訂閱殘留的 snapshot/error/lost 狀態也在清理時一起丟掉，不會
  // 讓使用者看到換了交易對或深度檔數之後、還顯示著舊訂閱的資料。
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;

    invoke<string>("subscribe_order_book", { symbol, levels })
      .then((id) => {
        if (cancelled) {
          invoke("unsubscribe_order_book", { subscriptionId: id }).catch(() => {});
          return;
        }
        subscriptionIdRef.current = id;
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(String(err));
      });

    return () => {
      cancelled = true;
      const id = subscriptionIdRef.current;
      subscriptionIdRef.current = null;
      if (id) {
        invoke("unsubscribe_order_book", { subscriptionId: id }).catch(() => {});
      }
      setSnapshot(null);
      setError(null);
      setLost(false);
    };
  }, [enabled, symbol, levels]);

  useEffect(() => {
    const unlistenPromise = listen<OrderBookUpdateEnvelope>(ORDER_BOOK_EVENT, (event) => {
      const payload = event.payload;
      if (payload.subscriptionId !== subscriptionIdRef.current) return;
      if (payload.type === "snapshot") {
        setSnapshot(payload);
        setLost(false);
      } else {
        // 連線斷了：清楚標示「已失去連線」，不能讓畫面繼續顯示最後一筆
        // 舊資料卻看起來一切正常。
        setLost(true);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const connecting = enabled && !snapshot && !error && !lost;

  return (
    <section aria-label="即時委託簿" className="order-book-panel">
      <div className="order-book-panel__header">
        <h3>即時委託簿</h3>
        <p role="note" className="order-book-panel__disclaimer">
          即時資料，不支援歷史回測。
        </p>
      </div>

      <div className="order-book-panel__controls">
        <label>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
          />
          顯示委託簿
        </label>
        <label>
          深度檔數
          <select
            value={levels}
            disabled={enabled}
            onChange={(e) => setLevels(Number(e.target.value))}
          >
            {ORDER_BOOK_LEVELS.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </div>

      {!enabled && <p className="order-book-panel__hint">勾選「顯示委託簿」開始訂閱。</p>}
      {connecting && <p role="status">連線中…</p>}
      {enabled && error && (
        <p role="alert" className="order-book-panel__error">
          訂閱委託簿失敗：{error}
        </p>
      )}
      {enabled && lost && (
        <p role="alert" className="order-book-panel__error">
          委託簿連線已中斷，畫面以下資料為最後一次收到的快照，已過期。
        </p>
      )}
      {enabled && snapshot && (
        <div className="order-book-panel__book">
          <div className="order-book-panel__side">
            <h4>賣價</h4>
            <table>
              <tbody>
                {[...snapshot.asks].reverse().map((level, i) => (
                  <tr key={`ask-${i}`}>
                    <td className="order-book-panel__price order-book-panel__price--ask">
                      {level.price}
                    </td>
                    <td>{level.qty}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="order-book-panel__side">
            <h4>買價</h4>
            <table>
              <tbody>
                {snapshot.bids.map((level, i) => (
                  <tr key={`bid-${i}`}>
                    <td className="order-book-panel__price order-book-panel__price--bid">
                      {level.price}
                    </td>
                    <td>{level.qty}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </section>
  );
}
