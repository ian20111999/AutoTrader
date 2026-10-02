// 跟 app/src-tauri/src/order_book.rs 的 OrderBookEventDto 對應。價格/數量是
// Fixed 的字串表示（理由同 testnetTradingTypes.ts）。
//
// 範圍限定：這只是即時委託簿「顯示用」的型別，不支援歷史回測——Binance
// 沒有免費的歷史委託簿資料，回測頁不應該、也沒有用到這些型別。

export interface OrderBookLevel {
  price: string;
  qty: string;
}

// Rust 端用 #[serde(tag = "type", rename_all = "camelCase")]。
export type OrderBookEvent =
  | {
      type: "snapshot";
      symbol: string;
      bids: OrderBookLevel[];
      asks: OrderBookLevel[];
      eventTimeMs: number;
    }
  | { type: "stopped" };

// Rust 端的事件 payload 外層多包一個 subscriptionId（#[serde(flatten)]），
// 前端用它過濾只處理自己這份訂閱的事件。
export type OrderBookUpdateEnvelope = OrderBookEvent & { subscriptionId: string };

// Rust 端 app.emit 用的事件名稱。
export const ORDER_BOOK_EVENT = "order-book-update";

// Binance partial book depth stream 只支援這三種深度檔數（見
// at_market_stream::depth_stream 的查證）。
export const ORDER_BOOK_LEVELS = [5, 10, 20] as const;
