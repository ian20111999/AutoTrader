//! Binance **公開**行情 WebSocket 串流：即時價格（ticker）與即時 K 線（kline）。
//!
//! 不需要 API 金鑰——市場行情是公開資料，[`spawn`] 不會碰 Keychain，不會跳
//! macOS 的授權對話框。
//!
//! # 同步設計（不是 async）
//!
//! 這個專案目前完全是同步、阻塞式的（`ureq`，沒有 `tokio`）。WebSocket 是長
//! 連線，這裡用同步的 [`tungstenite`] 開一條專門的背景 OS 執行緒跑「連線 →
//! 阻塞讀取 → 解析 → 送進 channel → 斷線就重連」這個迴圈，對外只露出一個
//! [`std::sync::mpsc::Receiver`]，呼叫端完全不需要進入 async context。
//! 引入 `tokio` 换真正的 async 是很大的架構代價（之後每個呼叫端都要決定要不
//! 要進 async），這裡的量（一條或幾條長連線）不需要那個代價。
//!
//! # 收盤 / 未收盤 K 線
//!
//! Binance 的 kline 串流每秒左右都會推一則訊息，代表「這根還在形成中」；
//! 只有訊息裡 `k.x == true` 時，這根 K 線才真正收盤、不會再變動。
//! [`KlineUpdate::is_closed`] 就是這個欄位，呼叫端要存成 1.2 的歷史 K 線
//! 之前，必須先檢查這個欄位——未收盤的 K 線之後還會用同一個 `open_time`
//! 送來更新過的值，提早存進去就是錯的歷史資料。
//! 欄位語意來源：<https://developers.binance.com/docs/binance-spot-api-docs/web-socket-streams>
//!
//! # 重連策略
//!
//! [`Backoff`]：失敗後不會立刻狂重連，等待時間從 1 秒開始每次翻倍、上限 30
//! 秒；只要成功連上一次就重置回 1 秒。

use at_core::{Bar, Fixed, Interval, Symbol};
use serde::Deserialize;
use std::fmt;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const WS_BASE_URL: &str = "wss://stream.binance.com:9443/ws";
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// 組出即時價格（24hr ticker）串流的名字，給 [`spawn`] 用。
pub fn ticker_stream(symbol: &str) -> String {
    format!("{}@ticker", symbol.to_lowercase())
}

/// 組出即時 K 線串流的名字，給 [`spawn`] 用。
pub fn kline_stream(symbol: &str, interval: Interval) -> String {
    format!("{}@kline_{}", symbol.to_lowercase(), interval.as_str())
}

/// 一次即時價格更新（24hr ticker 串流的 `c` 欄位：最新成交價）。
#[derive(Debug, Clone, PartialEq)]
pub struct TickerUpdate {
    pub symbol: Symbol,
    pub last_price: Fixed,
    /// Binance 產生這則事件的時間，UTC 毫秒。
    pub event_time_ms: i64,
}

/// 一次即時 K 線更新。
#[derive(Debug, Clone, PartialEq)]
pub struct KlineUpdate {
    pub symbol: Symbol,
    pub interval: Interval,
    /// 這根 K 線目前的開高低收量。只有 `is_closed == true` 時才是完整、
    /// 不會再變動的一根 K 線，才可以當成 1.2 的歷史 K 線存起來。
    pub bar: Bar,
    /// 這根 K 線是否已經收盤（對應 Binance 訊息裡的 `k.x`）。
    pub is_closed: bool,
    pub event_time_ms: i64,
}

/// 一則行情更新，[`spawn`] 回傳的 channel 送出的內容。
#[derive(Debug, Clone, PartialEq)]
pub enum MarketEvent {
    Ticker(TickerUpdate),
    Kline(KlineUpdate),
}

/// 連線或讀取這條串流失敗的原因。只用來決定「要不要重連」，訊息不含任何
/// 憑證——這條串流本來就是公開資料，不需要金鑰。
#[derive(Debug)]
enum StreamError {
    Connect(String),
    Io(String),
    Closed,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Connect(msg) => write!(f, "連線 Binance 行情串流失敗：{msg}"),
            StreamError::Io(msg) => write!(f, "讀取行情串流訊息失敗：{msg}"),
            StreamError::Closed => write!(f, "行情串流連線已被對方關閉"),
        }
    }
}

impl std::error::Error for StreamError {}

/// 斷線重連的指數退避：失敗一次等 `initial`，之後每次翻倍、封頂在 `max`；
/// 成功一次就 [`reset`](Backoff::reset) 回 `initial`。
///
/// 刻意讓「現在幾點」完全不出現在這個型別裡：它只管「等多久」，不管時間怎麼
/// 流逝，測試不需要真的等待就能檢查退避序列對不對。
#[derive(Debug, Clone)]
struct Backoff {
    initial: Duration,
    max: Duration,
    current: Option<Duration>,
}

impl Backoff {
    fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            current: None,
        }
    }

    /// 連線或讀取失敗時呼叫：回傳這次該等多久再重試，同時讓下一次的等待
    /// 時間翻倍（不超過 `max`）。
    fn failure_delay(&mut self) -> Duration {
        let delay = match self.current {
            None => self.initial,
            Some(prev) => prev.saturating_mul(2).min(self.max),
        };
        self.current = Some(delay);
        delay
    }

    /// 連線成功後呼叫：下一次失敗會從 `initial` 重新開始算。
    fn reset(&mut self) {
        self.current = None;
    }
}

/// 一條已經連上的串流：只管「讀下一則文字訊息」，讓連線邏輯可以在測試裡
/// 換成假的來源，不需要真的連網路。
trait MessageSource {
    fn read_message(&mut self) -> Result<String, StreamError>;
}

struct WsSource(WebSocket<MaybeTlsStream<TcpStream>>);

impl MessageSource for WsSource {
    fn read_message(&mut self) -> Result<String, StreamError> {
        loop {
            match self.0.read() {
                Ok(Message::Text(text)) => return Ok(text.as_str().to_string()),
                Ok(Message::Close(_)) => return Err(StreamError::Closed),
                // Ping/Pong/Binary/Frame：tungstenite 收到 Ping 會自動排入 Pong
                // 回覆，在下一次 read/write/flush 時送出，這裡不用手動處理。
                Ok(_) => continue,
                Err(e) => return Err(StreamError::Io(e.to_string())),
            }
        }
    }
}

/// 裝一次 process 級的 rustls [`rustls::crypto::CryptoProvider`]。
///
/// tungstenite 的 rustls TLS 後端本身不會自動裝這個；如果同一個執行檔裡有
/// 別的相依套件也用 rustls（這個 workspace 裡 `ureq` 就是），第一次建立
/// TLS 連線前沒有人裝好 provider 就會直接 panic。用 [`Once`] 確保只裝一次、
/// 而且不管裝不裝得成都不當成錯誤——已經有別人裝過同一個也沒關係。
fn ensure_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn connect_real(stream_name: &str) -> Result<Box<dyn MessageSource>, StreamError> {
    ensure_crypto_provider();
    let url = format!("{WS_BASE_URL}/{stream_name}");
    let (socket, _response) =
        tungstenite::connect(url).map_err(|e| StreamError::Connect(e.to_string()))?;
    Ok(Box::new(WsSource(socket)))
}

/// 連線 → 讀取 → 解析 → 送進 channel → 失敗就退避重連，這個迴圈本體。
/// `connect`／`sleep` 都是參數，讓測試可以完全不連真實網路、不真的等待。
///
/// 迴圈只有一個出口：`sender.send` 失敗（呼叫端把 [`MarketStreamHandle`]
/// 連同 `events` 一起 drop 掉）。
///
/// ponytail: 這代表呼叫端在連線中斷、目前正在退避等待時把 receiver drop
/// 掉，背景執行緒不會立刻停止，要等下一次成功連線、送出下一則事件才會發現
/// receiver 已經沒人收了。這條串流沒有下單、沒有資源競爭風險，多跑幾秒沒有
/// 實際代價；真的需要「立刻停止」時再加一個 `AtomicBool` 在迴圈開頭檢查。
fn run_with(
    mut connect: impl FnMut() -> Result<Box<dyn MessageSource>, StreamError>,
    sender: &mpsc::Sender<MarketEvent>,
    backoff: &mut Backoff,
    mut sleep: impl FnMut(Duration),
) {
    loop {
        if let Ok(mut source) = connect() {
            backoff.reset();
            while let Ok(text) = source.read_message() {
                if let Some(event) = parse_event(&text) {
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            }
        }
        sleep(backoff.failure_delay());
    }
}

/// 背景執行緒持有的控制代碼；把它（連同 `events`）drop 掉就會讓背景執行緒
/// 在下次送出事件時自然結束（見 [`run_with`] 的 ponytail 註解）。
pub struct MarketStreamHandle {
    pub events: mpsc::Receiver<MarketEvent>,
    _worker: thread::JoinHandle<()>,
}

/// 連上一條 Binance 公開行情串流（名字用 [`ticker_stream`] 或
/// [`kline_stream`] 組），背景執行緒負責連線、解析訊息、斷線後指數退避重連。
pub fn spawn(stream_name: impl Into<String>) -> MarketStreamHandle {
    let stream_name = stream_name.into();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut backoff = Backoff::new(INITIAL_BACKOFF, MAX_BACKOFF);
        run_with(
            || connect_real(&stream_name),
            &tx,
            &mut backoff,
            thread::sleep,
        );
    });
    MarketStreamHandle {
        events: rx,
        _worker: worker,
    }
}

#[derive(Debug, Deserialize)]
struct RawKlineEvent {
    #[serde(rename = "E")]
    event_time: i64,
    s: String,
    k: RawKline,
}

#[derive(Debug, Deserialize)]
struct RawKline {
    #[serde(rename = "t")]
    open_time: i64,
    #[serde(rename = "i")]
    interval: String,
    o: String,
    h: String,
    l: String,
    c: String,
    v: String,
    x: bool,
}

#[derive(Debug, Deserialize)]
struct RawTickerEvent {
    #[serde(rename = "E")]
    event_time: i64,
    s: String,
    /// 最新成交價。
    c: String,
}

fn kline_event(raw: RawKlineEvent) -> Option<MarketEvent> {
    let symbol = Symbol::new(&raw.s).ok()?;
    let interval: Interval = raw.k.interval.parse().ok()?;
    let bar = Bar {
        open_time: raw.k.open_time,
        open: raw.k.o.parse().ok()?,
        high: raw.k.h.parse().ok()?,
        low: raw.k.l.parse().ok()?,
        close: raw.k.c.parse().ok()?,
        volume: raw.k.v.parse().ok()?,
    };
    bar.validate().ok()?;
    Some(MarketEvent::Kline(KlineUpdate {
        symbol,
        interval,
        bar,
        is_closed: raw.k.x,
        event_time_ms: raw.event_time,
    }))
}

fn ticker_event(raw: RawTickerEvent) -> Option<MarketEvent> {
    let symbol = Symbol::new(&raw.s).ok()?;
    let last_price = raw.c.parse().ok()?;
    Some(MarketEvent::Ticker(TickerUpdate {
        symbol,
        last_price,
        event_time_ms: raw.event_time,
    }))
}

/// 把一則原始文字訊息解析成 [`MarketEvent`]。
///
/// ponytail: 看不懂的訊息（JSON 壞掉、欄位對不上、數字格式不對）直接回傳
/// `None`、安靜跳過，不會讓背景執行緒 panic 或整條連線中斷——下一則訊息
/// 一兩秒後就會再來。這條串流只有 ticker/kline 兩種事件類型，真的收到解析
/// 不了的訊息幾乎只會是 Binance 換了欄位格式，屬於「之後要重新查證」的情況
/// 而不是「呼叫端此刻能做什麼」的情況。
fn parse_event(text: &str) -> Option<MarketEvent> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    match value.get("e")?.as_str()? {
        "kline" => kline_event(serde_json::from_value(value).ok()?),
        "24hrTicker" => ticker_event(serde_json::from_value(value).ok()?),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    // ---- stream 名字 ----

    #[test]
    fn stream_names_are_lowercase_symbol_plus_suffix() {
        assert_eq!(ticker_stream("BTCUSDT"), "btcusdt@ticker");
        assert_eq!(kline_stream("BTCUSDT", Interval::M1), "btcusdt@kline_1m");
        assert_eq!(kline_stream("ethusdt", Interval::H4), "ethusdt@kline_4h");
    }

    // ---- 訊息解析：真實格式的片段（來自 Binance 官方文件範例，欄位補齊）----

    const KLINE_OPEN: &str = r#"{
        "e": "kline", "E": 1790000000123, "s": "BTCUSDT",
        "k": {
            "t": 1790000000000, "T": 1790000059999, "s": "BTCUSDT", "i": "1m",
            "f": 100, "L": 200, "o": "50000.00", "c": "50010.50", "h": "50020.00",
            "l": "49990.00", "v": "12.345", "n": 50, "x": false, "q": "617000.00",
            "V": "6.0", "Q": "300000.00", "B": "0"
        }
    }"#;

    const KLINE_CLOSED: &str = r#"{
        "e": "kline", "E": 1790000060123, "s": "BTCUSDT",
        "k": {
            "t": 1790000000000, "T": 1790000059999, "s": "BTCUSDT", "i": "1m",
            "f": 100, "L": 260, "o": "50000.00", "c": "50030.00", "h": "50040.00",
            "l": "49980.00", "v": "20.0", "n": 80, "x": true, "q": "1000000.00",
            "V": "10.0", "Q": "500000.00", "B": "0"
        }
    }"#;

    const TICKER: &str = r#"{
        "e": "24hrTicker", "E": 1790000000123, "s": "BTCUSDT",
        "p": "100.00", "P": "0.2", "w": "50000", "x": "49900.00", "c": "50010.50",
        "Q": "0.5", "b": "50010.00", "B": "1", "a": "50011.00", "A": "1",
        "o": "49900.00", "h": "50100.00", "l": "49800.00", "v": "1000.0",
        "q": "50000000.0", "O": 1789913600000, "C": 1790000000000,
        "F": 1, "L": 2, "n": 100
    }"#;

    #[test]
    fn parses_an_unclosed_kline_and_marks_it_not_closed() {
        let event = parse_event(KLINE_OPEN).expect("應該解析成功");
        let MarketEvent::Kline(k) = event else {
            panic!("應該是 Kline 事件");
        };
        assert!(!k.is_closed, "k.x = false 時不可以說它已經收盤");
        assert_eq!(k.symbol, Symbol::new("BTCUSDT").unwrap());
        assert_eq!(k.interval, Interval::M1);
        assert_eq!(k.bar.open_time, 1_790_000_000_000);
        assert_eq!(k.bar.open, fx("50000.00"));
        assert_eq!(k.bar.close, fx("50010.50"));
        assert_eq!(k.bar.high, fx("50020.00"));
        assert_eq!(k.bar.low, fx("49990.00"));
        assert_eq!(k.bar.volume, 12.345);
        assert_eq!(k.event_time_ms, 1_790_000_000_123);
    }

    #[test]
    fn parses_a_closed_kline_and_marks_it_closed() {
        let event = parse_event(KLINE_CLOSED).expect("應該解析成功");
        let MarketEvent::Kline(k) = event else {
            panic!("應該是 Kline 事件");
        };
        assert!(k.is_closed, "k.x = true 時這根 K 線已經收盤定案");
        assert_eq!(k.bar.close, fx("50030.00"));
    }

    #[test]
    fn open_and_closed_updates_for_the_same_bar_share_open_time() {
        // 同一根 K 線的「還在形成」跟「已收盤」訊息必須是同一個 open_time，
        // 呼叫端才可能把它們對成同一根、用最後一則收盤的覆蓋前面的。
        let MarketEvent::Kline(open) = parse_event(KLINE_OPEN).unwrap() else {
            panic!()
        };
        let MarketEvent::Kline(closed) = parse_event(KLINE_CLOSED).unwrap() else {
            panic!()
        };
        assert_eq!(open.bar.open_time, closed.bar.open_time);
    }

    #[test]
    fn parses_ticker_last_price() {
        let event = parse_event(TICKER).expect("應該解析成功");
        let MarketEvent::Ticker(t) = event else {
            panic!("應該是 Ticker 事件");
        };
        assert_eq!(t.symbol, Symbol::new("BTCUSDT").unwrap());
        assert_eq!(t.last_price, fx("50010.50"));
        assert_eq!(t.event_time_ms, 1_790_000_000_123);
    }

    #[test]
    fn garbage_text_is_skipped_not_panicking() {
        assert_eq!(parse_event("not json at all"), None);
        assert_eq!(parse_event(r#"{"e":"somethingElse"}"#), None);
        assert_eq!(parse_event(r#"{"no_e_field":true}"#), None);
        assert_eq!(
            parse_event(
                r#"{"e":"kline","E":1,"s":"BTCUSDT","k":{"t":1,"i":"1m","o":"not-a-number","h":"1","l":"1","c":"1","v":"1","x":false}}"#
            ),
            None,
            "價格欄位不是合法數字時要安靜跳過，不能 panic"
        );
    }

    #[test]
    fn bar_that_fails_validation_is_skipped() {
        // 最高價比開盤價還低：資料本身不合理，1.2 Bar::validate 會擋下來。
        let bad = r#"{"e":"kline","E":1,"s":"BTCUSDT","k":{"t":1,"i":"1m","o":"100","h":"50","l":"1","c":"10","v":"1","x":false}}"#;
        assert_eq!(parse_event(bad), None);
    }

    // ---- Backoff：不依賴真實時間流逝 ----

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(b.failure_delay(), Duration::from_secs(1));
        assert_eq!(b.failure_delay(), Duration::from_secs(2));
        assert_eq!(b.failure_delay(), Duration::from_secs(4));
        assert_eq!(b.failure_delay(), Duration::from_secs(8));
        assert_eq!(b.failure_delay(), Duration::from_secs(16));
        assert_eq!(b.failure_delay(), Duration::from_secs(30), "封頂在 30 秒");
        assert_eq!(
            b.failure_delay(),
            Duration::from_secs(30),
            "封頂之後不會再往上"
        );
    }

    #[test]
    fn backoff_resets_to_initial_after_reset() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        b.failure_delay();
        b.failure_delay();
        b.reset();
        assert_eq!(
            b.failure_delay(),
            Duration::from_secs(1),
            "reset 之後下一次失敗要從 initial 重新開始"
        );
    }

    // ---- run_with：假連線 + 假時間，驗證「失敗退避、成功重置、收到訊息」----
    //
    // run_with 是同步阻塞的迴圈，測試裡直接在當前執行緒呼叫它（不像正式的
    // `spawn` 會另開執行緒），所以沒辦法「一邊讓它跑一邊在外面讀 channel」。
    // 要讓它自然結束，得從注入的 `sleep` closure 裡（run_with 每次斷線後
    // 唯一會呼叫回測試程式碼的地方）把已經送到 channel 裡的事件收走，
    // 收到後就把 receiver drop 掉，這樣下一次 `sender.send` 會失敗、
    // run_with 才會回傳（見 [`run_with`] 文件：這是它唯一的出口）。

    struct FakeSource {
        remaining: Vec<&'static str>,
    }

    impl MessageSource for FakeSource {
        fn read_message(&mut self) -> Result<String, StreamError> {
            match self.remaining.pop() {
                Some(text) => Ok(text.to_string()),
                None => Err(StreamError::Closed),
            }
        }
    }

    #[test]
    fn retries_with_backoff_then_recovers_and_forwards_events() {
        // 前兩次 connect 失敗、第三次成功並吐出一則 ticker 訊息再斷線。
        let attempt = RefCell::new(0);
        let sleeps = RefCell::new(Vec::new());
        let received = RefCell::new(Vec::new());
        let (tx, rx) = mpsc::channel();
        let rx = RefCell::new(Some(rx));
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));

        run_with(
            || {
                *attempt.borrow_mut() += 1;
                if *attempt.borrow() < 3 {
                    Err(StreamError::Connect("測試用的假失敗".to_string()))
                } else {
                    Ok(Box::new(FakeSource {
                        remaining: vec![TICKER],
                    }) as Box<dyn MessageSource>)
                }
            },
            &tx,
            &mut backoff,
            |d| {
                sleeps.borrow_mut().push(d);
                if let Some(r) = rx.borrow().as_ref() {
                    while let Ok(event) = r.try_recv() {
                        received.borrow_mut().push(event);
                    }
                }
                if !received.borrow().is_empty() {
                    rx.borrow_mut().take();
                }
            },
        );

        assert_eq!(received.borrow().len(), 1, "應該收到那一則 ticker 事件");
        assert!(matches!(received.borrow()[0], MarketEvent::Ticker(_)));
        assert_eq!(
            sleeps.borrow().as_slice(),
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(1),
            ],
            "前兩次失敗照 1 秒、2 秒退避；第三次連線成功過，收到訊息後斷線，\
             下一次退避要重新從 1 秒開始，不是接著 2 秒繼續翻倍"
        );
    }

    // ---- 真實連線（需要網路，預設不跑）----

    /// 實際連上 Binance 公開行情串流，收到至少一則訊息就算成功。公開端點，
    /// 不需要金鑰、不會跳 Keychain 授權對話框。
    ///
    /// 手動驗證：`cargo test -p at-market-stream -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn connects_to_real_binance_ticker_stream() {
        let handle = spawn(ticker_stream("BTCUSDT"));
        let event = handle
            .events
            .recv_timeout(Duration::from_secs(15))
            .expect("15 秒內應該要收到至少一則真實的 ticker 訊息");
        let MarketEvent::Ticker(t) = event else {
            panic!("應該是 Ticker 事件");
        };
        println!("收到即時價格：{} = {}", t.symbol, t.last_price);
        assert!(t.last_price > Fixed::ZERO);
    }
}
