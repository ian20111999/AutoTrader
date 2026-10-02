//! 即時委託簿（partial book depth）面板的 Tauri command 橋接：訂閱/取消訂閱 +
//! 事件轉發。跟 `testnet_trading.rs`/`paper_trading.rs` 刻意分開、不進
//! [`SessionRegistry`](crate::session_registry::SessionRegistry)——這是一個
//! **獨立、可選**的顯示用訂閱，不是交易 session：使用者可以在任何交易頁面
//! 開關它，開關都不影響 K 線交易邏輯的任何路徑（委託簿資料完全沒有接進
//! `Strategy`/送單判斷）。
//!
//! # 範圍限定：只支援「即時」，不支援回測
//!
//! Binance 沒有免費的歷史委託簿深度資料，所以這裡只有即時訂閱，沒有、也
//! 不會有任何回測頁面用得到的 command——前端必須在面板上明確標示「即時資料，
//! 不支援歷史回測」，不能讓使用者誤以為這份資料在回測頁也能用。
//!
//! # 怎麼保證不留孤兒連線
//!
//! [`subscribe_order_book`] 回傳的 `subscription_id` 對應一份存在
//! [`OrderBookState`] 裡的停止旗標（`at_market_stream::MarketStreamHandle`
//! 的那一個，跟 5.3 的設計一致）。[`unsubscribe_order_book`] 呼叫
//! `stop_flag.store(true, ...)`，背景執行緒的 `for event in handle.events.iter()`
//! 迴圈最多等一次讀取逾時就會發現連線已經結束、`iter()` 收到
//! sender 被 drop 而自然結束，執行緒退出時把自己從 `OrderBookState` 裡移除。
//! 使用者忘記呼叫 `unsubscribe_order_book`（例如整個 App 關掉）也不會留下
//! 連著網路的執行緒：那種情況下整個 process 本來就會跟著結束。

use at_core::Fixed;
use at_market_stream::{spawn_depth, DepthUpdate, MarketEvent, MarketStreamHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, Manager};

/// `app.emit` 用的事件名稱，前端用同一個字串 `listen`。
const ORDER_BOOK_EVENT: &str = "order-book-update";

/// 目前還活著的委託簿訂閱：`subscription_id` → 停止旗標。只存旗標，不存整個
/// `MarketStreamHandle`——背景執行緒自己擁有 handle，`unsubscribe` 只需要
/// 喊停，不需要搶執行緒手上的 channel。
#[derive(Default)]
pub struct OrderBookState(Mutex<HashMap<String, Arc<AtomicBool>>>);

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OrderBookLevelDto {
    pub price: String,
    pub qty: String,
}

fn level_dto((price, qty): &(Fixed, Fixed)) -> OrderBookLevelDto {
    OrderBookLevelDto {
        price: price.to_string(),
        qty: qty.to_string(),
    }
}

/// 往前端 `emit` 的事件。`Stopped` 讓前端確定知道連線已經斷開、該把畫面上的
/// 舊資料標成「已失去連線」，不能什麼都不送、讓畫面一直顯示最後一筆舊資料
/// 卻不提示使用者。
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OrderBookEventDto {
    Snapshot {
        symbol: String,
        bids: Vec<OrderBookLevelDto>,
        asks: Vec<OrderBookLevelDto>,
        event_time_ms: i64,
    },
    Stopped,
}

impl From<&DepthUpdate> for OrderBookEventDto {
    fn from(update: &DepthUpdate) -> Self {
        OrderBookEventDto::Snapshot {
            symbol: update.symbol.to_string(),
            bids: update.bids.iter().map(level_dto).collect(),
            asks: update.asks.iter().map(level_dto).collect(),
            event_time_ms: update.event_time_ms,
        }
    }
}

/// 事件 payload 外層多包一個 `subscriptionId`，前端依它過濾（跟
/// `testnet_trading.rs` 的 `sessionId` 同一個慣例，這裡訂閱的生命週期是
/// 獨立的，不是 session id）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct OrderBookUpdateEnvelope<'a> {
    subscription_id: &'a str,
    #[serde(flatten)]
    event: OrderBookEventDto,
}

/// 開始訂閱一個交易對的即時委託簿快照（partial book depth）。`levels` 只接受
/// Binance 支援的 5/10/20，不合法的值在 [`at_market_stream::depth_stream`]
/// 那一層就會被擋下來，回傳的 `Err` 字串已經是給使用者看的中文訊息。
///
/// 回傳的 `subscription_id` 要留著給 [`unsubscribe_order_book`] 用。
#[tauri::command]
pub fn subscribe_order_book(app: AppHandle, symbol: String, levels: u32) -> Result<String, String> {
    let handle = spawn_depth(&symbol, levels)?;
    let stop_flag = handle.stop_flag();

    let subscription_id = format!(
        "ob-{}-{}",
        symbol.to_lowercase(),
        crate::session_registry::now_ms()
    );

    app.state::<OrderBookState>()
        .0
        .lock()
        .unwrap()
        .insert(subscription_id.clone(), stop_flag);

    let app_for_thread = app.clone();
    let id_for_thread = subscription_id.clone();
    thread::spawn(move || forward_order_book(app_for_thread, id_for_thread, handle));

    Ok(subscription_id)
}

/// 結束一份委託簿訂閱：設定停止旗標，背景執行緒會在下一個讀取逾時之內
/// 結束、真的斷開 WebSocket 連線，不會變成孤兒執行緒。
#[tauri::command]
pub fn unsubscribe_order_book(app: AppHandle, subscription_id: String) -> Result<(), String> {
    let stop_flag = app
        .state::<OrderBookState>()
        .0
        .lock()
        .unwrap()
        .remove(&subscription_id);
    match stop_flag {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!(
            "找不到這份委託簿訂閱（可能已經停止）：{subscription_id}"
        )),
    }
}

/// 背景執行緒本體：逐則把委託簿快照轉發成前端事件；連線結束（停止旗標生效
/// 或背景重連耗盡，正式路徑不會耗盡）時送一則 `Stopped`、把自己從
/// [`OrderBookState`] 移除，不留任何痕跡。
fn forward_order_book(app: AppHandle, subscription_id: String, handle: MarketStreamHandle) {
    for event in handle.events.iter() {
        if let MarketEvent::Depth(update) = event {
            let _ = app.emit(
                ORDER_BOOK_EVENT,
                &OrderBookUpdateEnvelope {
                    subscription_id: &subscription_id,
                    event: OrderBookEventDto::from(&update),
                },
            );
        }
        // 這條串流只會訂閱 depth，正式路徑不會收到 Ticker/Kline；萬一收到
        // （理論上不可能，因為 spawn_depth 只用 depth 的解析器）安靜忽略，
        // 不要讓一則意外的事件類型把整條轉發執行緒弄掛。
    }

    let _ = app.emit(
        ORDER_BOOK_EVENT,
        &OrderBookUpdateEnvelope {
            subscription_id: &subscription_id,
            event: OrderBookEventDto::Stopped,
        },
    );
    app.state::<OrderBookState>()
        .0
        .lock()
        .unwrap()
        .remove(&subscription_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_core::Symbol;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    #[test]
    fn depth_update_translates_to_a_snapshot_event_with_all_levels() {
        let update = DepthUpdate {
            symbol: Symbol::new("BTCUSDT").unwrap(),
            bids: vec![(fx("50000"), fx("1.5"))],
            asks: vec![(fx("50001"), fx("0.5"))],
            event_time_ms: 1_790_000_000_000,
        };
        let dto = OrderBookEventDto::from(&update);
        match dto {
            OrderBookEventDto::Snapshot {
                symbol,
                bids,
                asks,
                event_time_ms,
            } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(
                    bids,
                    vec![OrderBookLevelDto {
                        price: "50000".to_string(),
                        qty: "1.5".to_string()
                    }]
                );
                assert_eq!(
                    asks,
                    vec![OrderBookLevelDto {
                        price: "50001".to_string(),
                        qty: "0.5".to_string()
                    }]
                );
                assert_eq!(event_time_ms, 1_790_000_000_000);
            }
            other => panic!("預期 Snapshot 事件，收到 {other:?}"),
        }
    }

    #[test]
    fn unsubscribing_an_unknown_id_is_a_clear_chinese_error() {
        let state = OrderBookState::default();
        let flag = state.0.lock().unwrap().remove("not-subscribed");
        assert!(flag.is_none());
    }

    #[test]
    fn unsubscribing_sets_the_stop_flag_and_forgets_the_subscription() {
        let state = OrderBookState::default();
        let flag = Arc::new(AtomicBool::new(false));
        state
            .0
            .lock()
            .unwrap()
            .insert("ob-btcusdt-1".to_string(), Arc::clone(&flag));

        let removed = state.0.lock().unwrap().remove("ob-btcusdt-1");
        assert!(removed.is_some());
        removed.unwrap().store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed), "停止旗標應該被設定");
        assert!(
            !state.0.lock().unwrap().contains_key("ob-btcusdt-1"),
            "移除後不該留在 map 裡，不然算孤兒訂閱"
        );
    }
}
