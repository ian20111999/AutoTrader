# ADR-001：Session Registry 與歷史持久化

- **狀態**：Proposed（等實作前的審查）
- **日期**：2026-10-01
- **對應計畫**：`docs/plans/2026-10-01-wireframe完整補齊.md` Phase B
- **下游依賴**：Phase C（總覽）、Phase D（模擬交易多策略並行）、Phase E（風控跨策略）、Phase H（策略庫強化）
- **範圍**：只做設計。這份文件不含任何實作程式碼。

---

## 1. 背景與問題

Wireframe（`docs/design-reference.md`）要求的四件事，現有實作一件都做不到：

| Wireframe 要求 | 現狀 | 缺什麼 |
|---|---|---|
| 模擬交易「可同時管理多個模擬」（運行中／已停止標籤、各自的起訖時間與報酬） | 一次只能跑一場 | 多 session 追蹤 |
| 總覽：執行中策略表格（跨所有 session）、最近回測清單、60 天權益曲線 | 整頁不存在 | 多 session 追蹤 **＋** 歷史持久化 |
| 策略庫：每個策略的「回測次數與最近時間」「近 30 天實際報酬」+ sparkline | 卡片只有靜態名稱與參數 schema | 歷史持久化 |
| 風控：跨策略全域上限（總部位、單日虧損加總） | `at-risk-control` 是單一 session 的兩個數字 | 「現在有哪些 session 在跑」的查詢介面 |

### 現狀盤點（事實）

**單一 session 是刻意的設計，不是漏做。**

- `crates/paper-trading/src/lib.rs:122` `PaperTradingHandle`：`updates: mpsc::Receiver<PaperUpdate>` + 私有的 `stop: Arc<AtomicBool>`（和 4.5 行情連線共用）+ `latest: Arc<Mutex<Option<PaperSnapshot>>>`。`spawn`（同檔 `:161`）一次接一條行情、一個策略。
- `crates/testnet-trading/src/lib.rs:222` `TestnetTradingHandle`：同結構，多一個私有的 `kill_switch: Arc<AtomicBool>` 與 `set_kill_switch()`／`kill_switch()`（`:237`）。
- `app/src-tauri/src/paper_trading.rs:115` 的 `Inner` 是 `{ state: RunState, stop_flag: Option<Arc<AtomicBool>>, latest: Option<Dto> }`，`:125` 包成 `PaperTradingState(Mutex<Inner>)`，`:227` 明確拒絕第二場（「模擬交易已經在執行中」）。
- `app/src-tauri/src/testnet_trading.rs:195` 的 `Inner` 把**整個 handle** 留在共用狀態裡，`:306` `forward_updates` 每次只鎖 200 毫秒就放手，讓 `stop`／`set_kill_switch` 能插進來。
- `docs/steps/5.4-模擬交易頁面.md` 的「範圍取捨」表格第一列就寫明：多 session「需要把 state 改成 map，超出這一步範圍」；「還沒做的事」第二項寫明「沒有歷史模擬交易紀錄保存……只存在畫面的 React state 裡」。

**兩種橋接手法已經並存。** `paper_trading.rs` 把 handle 搬進背景執行緒、另外留一份停止旗標；`testnet_trading.rs` 把 handle 留在共用狀態、背景執行緒短鎖輪詢。差別的理由是 kill switch 需要 `&TestnetTradingHandle`（`docs/steps/6.5` 的「一鍵停止跟停止用不同的橋接手法」）。**這份設計統一採用 testnet 的手法**（理由見 §4.4）。

**現有的持久化慣例有兩套，都不是給 session 紀錄用的。**

- `crates/core/src/bar_store.rs`：手刻的 6 欄逗號分隔純文字（`:105` `format_bars`、`:123` `write_bars_file`），**刻意不用 serde**（`docs/steps/1.6` 的設計決定第一條：「這個 workspace 到目前為止零第三方依賴」）。同一份文件也預先寫下了升級條件：「如果之後這個格式要版本演進（例如加欄位、要相容舊檔案）或存更複雜的巢狀結構，那時候再評估要不要引入 serde + 一個真正的序列化格式（例如 serde_json）」。
- `crates/account-sync/src/lib.rs:189`-`:226` 與 `rules.rs:225`-`:257`：`<base_dir>/fee_schedule_<SYMBOL>.json`，`serde_json::to_string_pretty` 寫、壞檔當作沒有（`.ok()?`）。**這個 crate 收 `base_dir: &Path`，自己不碰 Tauri**，桌面 App 傳 `app.path().app_data_dir()`（`app/src-tauri/src/testnet_trading.rs` 的 `start_testnet_trading` 就是這樣用的）。

**零資料庫。** 根 workspace 的 10 個 crate 裡只有 `at-account-sync`／`at-binance` 等少數有第三方依賴，沒有任何 SQL。`app/src-tauri` 是**獨立的 cargo workspace**（`app/src-tauri/Cargo.toml` 開頭的註解說明理由：`frontendDist` 指向 `../dist`，沒先 `npm run build` 這個 crate 編不起來），已經有 `serde` + `serde_json`。

### 這份 ADR 要回答的五個問題

1. 多 session 追蹤放在哪一層？
2. 歷史持久化存在哪裡、用什麼格式？
3. 一個 session 的共同資料模型長什麼樣？
4. Tauri command 介面怎麼改？
5. 留給風控 Phase 的查詢介面長什麼樣？

---

## 2. 決策摘要

| # | 決策 | 一句話理由 |
|---|---|---|
| D1 | **Session registry 放在 `app/src-tauri`**（新模組 `session_registry.rs`），不動 `at-paper-trading`／`at-testnet-trading` 的核心邏輯 | 兩個 crate 的單一 session 設計沒有錯，要的是外面包一層集合；它們的控制面（`stop`／`set_kill_switch`／`latest_snapshot`）已經夠用 |
| D2 | **持久化獨立成新 crate `at-session-store`**，收 `base_dir: &Path`，不碰 Tauri | 照 `at-account-sync` 的既有慣例；測試不必先 `npm run build` 才能編 |
| D3 | **不引入 SQLite。** 格式＝「一份 JSON 索引 + 每個 session 一個明細資料夾」 | 索引全量載入記憶體後，所有 wireframe 要的聚合查詢都是幾千筆 `Vec` 的 filter/fold（微秒級）；SQLite 是 🔴 Framework 級依賴，現在換不到任何能量測到的好處 |
| D4 | **session 種類重用 `at_core::RunMode`**，市場重用 `at_core::Market` | `crates/core/src/types.rs:46`／`:5` 已經有，而且 `sends_orders()` 是現成的安全護欄 |
| D5 | **既有 command 介面要改**（`stop_paper_trading` 等加 `sessionId`，`start_*` 改回傳 session id） | 不留「沒帶 id 就操作唯一那場」的相容層——那是假相容，會在有兩場的時候做錯事 |
| D6 | 總覽／風控用**新的精簡事件 `session-registry-changed`** 通知「去重新查一次」，既有兩個詳細事件只加 `sessionId` 欄位 | 不必發明一個跨種類的合併 payload，總覽也不用懂 paper/testnet 兩種快照形狀 |
| D7 | **並行上限 20 場**，超過回明確的中文錯誤 | 每場 3 條 OS 執行緒 + 1 條 WebSocket，誠實的桌面上限（見 §9.1） |

---

## 3. 問題一的方案比較：多 session 追蹤放哪一層

### 方案 A：App 層 registry（`app/src-tauri/src/session_registry.rs`）

Tauri app state 從 `Mutex<Inner>` 變成 `SessionRegistry`，內含 `RwLock<HashMap<SessionId, Arc<SessionEntry>>>`。每個 `SessionEntry` 持有該場的 handle、快照快取、狀態。`start_*` command 插一筆、`stop_*` 查一筆。底層兩個 crate 一行不改。

### 方案 B：改 `at-paper-trading`／`at-testnet-trading`，各自長出 `Pool`

在兩個 crate 內部加 `PaperTradingPool::spawn_session(...) -> SessionId` 之類的集合型別，App 層只拿 pool。

### 方案 C：新 crate `at-session-supervisor`，統管兩種 session，App 變薄殼

把「開一場交易」整件事（建策略、開行情、讀金鑰、同步費率、spawn、追蹤）搬進一個新 crate，App 只做 command 轉呼叫。

### 比較

| 項目 | 方案 A：App 層 registry | 方案 B：改兩個底層 crate | 方案 C：新 supervisor crate |
|---|---|---|---|
| 優點 | 改動最小；兩個已審查過的 crate 零風險；每場 session 的生命週期邏輯已經在 App 層了（`start_paper_trading` 已經在做建策略、開行情、收拾失敗），registry 只是把 `Option<T>` 換成 `HashMap<K,T>` | 「多場」這件事封在底層，App 不必理解並行；兩個 crate 自己的測試就能驗多場 | 可以不依賴 Tauri 單獨測試（不必先 `npm run build`）；未來若要做 headless CLI runner 或東京節點，邏輯已經在 crate 裡 |
| 缺點 | 並行/鎖的正確性責任留在 App 層，而 App 層的測試比較貴（要 `cargo test -p app`，需要 `../dist` 存在） | 兩個 crate 要各做一次一樣的事（它們沒有共同抽象）；`TestnetTradingHandle` 的 `updates` 是 `mpsc::Receiver`（非 `Sync`），pool 要處理「多個 receiver 誰來收」，等於把 App 層現有的轉發執行緒設計複製進去 | 要把 `build_strategy`（`app/src-tauri/src/backtest.rs:64`）、Keychain 讀取、`at-account-sync` 呼叫全部搬出 App，blast radius 最大；而且會變成「對兩個控制面不同的 handle 做一層泛型抽象」——一個實作兩種特例的抽象 |
| 風險 | 鎖設計錯了會卡住 UI（可控：鎖的持有範圍只有 HashMap 查找，見 §4.3） | 違反「不動已審查過的 crate」的傾向；6.4／6.5 是 money-critical 路徑，動它要重跑跨模型審查 | 一次搬太多東西，和 Phase C/D/E 平行開工衝突；為了「未來可能的 CLI」現在付代價（YAGNI） |

### 決定：方案 A

理由三條：

1. **單一 session 的設計本身沒有錯，錯的是容器。** 兩個 crate 的 handle 已經提供了 registry 需要的全部控制面：`stop()`、`set_kill_switch()`、`kill_switch()`、`latest_snapshot()`、`updates`。registry 要做的只是「一個變多個」，而「多個」是 App 層（UI 有幾個標籤頁）的概念，不是引擎層的概念。
2. **App 層已經是生命週期的擁有者。** `start_paper_trading` 現在就在做驗證、開行情、spawn、失敗時收拾旗標、開轉發執行緒；`start_testnet_trading` 還多做讀 Keychain、同步費率與規則。這些都無法搬進 `at-paper-trading`（它不依賴 `at-binance`，也不該依賴）。把 registry 放別的地方，等於把一件事切成兩半。
3. **方案 C 的唯一真實好處（獨立可測）可以只花在持久化上。** 真正值得獨立出 crate 的是 `at-session-store`（純函式 + 檔案 I/O，測試最有價值、也最不需要 Tauri），registry 的並行邏輯則緊貼 Tauri 的 `AppHandle`／`emit`。所以採 A + D2 的組合，而不是整塊搬走。

**不改底層 crate 的驗證點**：`crates/paper-trading`、`crates/testnet-trading`、`crates/core`、`crates/risk-control` 的 diff 必須是空的；根目錄 `cargo test` 的既有測試數量與結果不變。

---

## 4. 設計：Session Registry（App 層）

### 4.1 元件關係

```
┌─ app/src-tauri ─────────────────────────────────────────────────────┐
│                                                                     │
│  Tauri commands                    SessionRegistry (app state)       │
│  ├ start_paper_trading ──────────► insert(SessionEntry) ──► id       │
│  ├ stop_paper_trading(id) ───────► get(id).stop()                    │
│  ├ set_testnet_kill_switch(id) ──► get(id).set_kill_switch()         │
│  ├ list_live_sessions ───────────► snapshot_all()  (唯讀、不阻塞)     │
│  └ run_backtest_command ─────────────────────┐                       │
│                                               │                      │
│  每場一條轉發執行緒                            │                      │
│  forward(id) ─ recv_timeout(200ms) ─┬─► 更新 SessionEntry.latest     │
│                                     ├─► emit 詳細事件（含 sessionId）│
│                                     ├─► emit session-registry-changed│
│                                     └─► 收尾時寫 store               │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ base_dir = app_data_dir()
                   ┌───────────▼────────────┐
                   │ at-session-store       │  index.json（全部 session 摘要）
                   │ （新 crate，不碰 Tauri）│  sessions/<id>/curve.csv
                   └────────────────────────┘  sessions/<id>/fills.jsonl（選配）
```

### 4.2 資料結構草圖（Rust 風格的偽碼，非最終程式碼）

```rust
// app/src-tauri/src/session_registry.rs

/// 檔名／事件 key 都用它，所以必須是檔案系統安全的字串。
/// 產生規則：format!("{kind}-{started_at_ms}-{seq:03}")，例如 "paper-1790756100000-001"。
/// kind 用 RunMode 的小寫英文；seq 來自 registry 的 AtomicU32，解決同一毫秒開兩場的碰撞。
/// 不引入 uuid/ulid 依賴（爬階梯：一行 format! 就夠）。
pub struct SessionId(String);

/// 一場執行中 session 在 registry 裡的格位。
/// 三個鎖刻意分開，理由見 4.3。
struct SessionEntry {
    id: SessionId,
    /// 不可變的識別資訊，建立後不再改，所以不用鎖。
    meta: SessionMeta,
    /// handle 本體。只有該場的轉發執行緒與 stop/kill-switch command 會鎖它。
    /// 用 enum 而不是 trait object：兩種 handle 的控制面不同（kill switch 只有 testnet 有），
    /// 一個只有兩個實作、而且介面不一致的 trait 不值得（KISS）。
    control: Mutex<SessionControl>,
    /// 最新快照 + 執行狀態。讀多寫少，而且風控／總覽要高頻讀，所以用 RwLock。
    /// **這個鎖絕對不能和 control 同時持有**（見 4.3）。
    live: RwLock<LiveState>,
}

enum SessionControl {
    Paper(PaperTradingHandle),
    Testnet(TestnetTradingHandle),
}

struct SessionMeta {
    kind: RunMode,          // at_core::RunMode（Backtest / Paper / Testnet / Live）
    market: Market,         // at_core::Market
    symbol: Symbol,
    interval: Interval,
    strategy_id: String,
    strategy_name: String,
    params: BTreeMap<String, String>,   // BTreeMap 而非 HashMap：序列化順序穩定，diff 看得懂
    starting_capital: Fixed,
    started_at_ms: i64,
}

struct LiveState {
    status: SessionStatus,
    /// 兩種快照的共同子集，總覽／風控只需要這些。
    /// 種類專屬的欄位（testnet 的 blocked/fees_paid/kill_switch）留在詳細事件裡，
    /// 不塞進這個共同結構。
    equity: Option<Fixed>,
    position: Fixed,
    daily_pnl: Option<Fixed>,
    /// 最後一根收盤 K 線的 open_time。總覽／風控用它判斷資料是不是過期（見 9.2）。
    as_of_ms: Option<i64>,
    bars_seen: usize,
}

pub struct SessionRegistry {
    /// 鎖只用來查 HashMap 與 clone Arc，持有時間是奈秒級。
    sessions: RwLock<HashMap<SessionId, Arc<SessionEntry>>>,
    seq: AtomicU32,
    /// 寫 store 一律經過這裡，序列化所有索引寫入（見 5.4）。
    store: Arc<SessionStore>,
}
```

### 4.3 鎖的規則（這是整個設計最容易出錯的地方，所以寫成不變量）

四條不變量，實作時每一條都該有對應的測試或 `debug_assert`：

1. **`sessions` 的鎖只在「查找、插入、移除」期間持有**，絕不跨 `emit`、`recv`、檔案 I/O 或網路。拿到 `Arc<SessionEntry>` 就立刻放掉。
2. **`control` 鎖與 `live` 鎖不可同時持有。** 轉發執行緒的循環是：鎖 `control` → `recv_timeout(200ms)` → 放鎖 → （有東西才）鎖 `live` 寫快照 → 放鎖 → `emit`。這正是 `app/src-tauri/src/testnet_trading.rs:306` 現有的手法，只是從全域一個鎖變成每場一個鎖，所以 N 場之間不再互相排隊。
3. **`live` 只用 `RwLock`，而且讀取端一律 clone 出去再用**，不在持有讀鎖時呼叫任何回呼。這是風控能安全呼叫 `aggregate_exposure()` 的前提（§8）。
4. **轉發執行緒不碰 `sessions` 的寫鎖**，session 的移除由 `stop`／收尾流程做，而且只在轉發執行緒結束**之後**（或由它自己最後一步做，此時它已不持有任何 entry 鎖）。

鎖被下毒（poisoned）的處理照既有慣例：`app/src-tauri/src/paper_trading.rs:201` 是 `poisoned.into_inner()` 繼續做、不 panic；registry 沿用。

### 4.4 統一用 testnet 的橋接手法（handle 留在 registry）

`paper_trading.rs` 現在把 handle 整個搬進背景執行緒，只留一份停止旗標。多 session 之後這個手法不夠用：

- 總覽與風控要的是「每場現在的部位與權益」。搬走 handle 之後 `latest_snapshot()` 叫不到，所以 5.4 另外在 App 層存了一份（`Inner.latest`）。這部分 registry 照樣要存（`LiveState`），所以**不是**改用 testnet 手法的理由。
- 真正的理由是對稱性：testnet session 的 `set_kill_switch` 非得 `&TestnetTradingHandle`，handle 必須留下。如果 paper 用 A 手法、testnet 用 B 手法，registry 就要有兩種格位、兩種轉發循環、兩種關機路徑——而兩者的差別只是「有沒有 kill switch」。統一成 B 之後 `SessionControl` 是一個 enum，兩條路只在 `recv` 回來的 payload 型別上分岔。

**被否決的替代做法**：在 `at-testnet-trading` 加一個 `pub fn kill_switch_flag(&self) -> Arc<AtomicBool>`（仿 `MarketStreamHandle::stop_flag()`），這樣兩種 handle 都能搬進執行緒。只要三行，但 `docs/steps/6.5` 明確記載過「不多開一個 public getter」的決定，而且 B 手法在多 session 下本來就夠好（鎖變成每場一個，6.5 文件裡擔心的全域鎖競爭不存在了）。若日後實測出 200 毫秒輪詢的成本不可接受，這是第一個該做的改動。

### 4.5 回測不是「執行中 session」

回測是同步的（`run_backtest_command` 跑完才回），沒有 handle、沒有 stop。它**只寫 store、不進 registry**。`RunMode::Backtest` 的紀錄在索引裡和模擬／測試網並排，但 `list_live_sessions` 永遠不會回它。這讓「最近回測清單」與「執行中策略表格」用同一份資料模型、不同的查詢。

---

## 5. 問題二的方案比較：歷史持久化

### 需求量化（先算清楚資料量，再選工具）

Wireframe 要的查詢只有五種：

| 查詢 | 來源畫面 | 形狀 |
|---|---|---|
| 最近 N 筆回測（含策略、參數、年化、回撤） | 總覽「最近回測」 | 索引排序 + 取前 N |
| 某策略的回測次數與最近時間 | 策略庫卡片 | 索引 filter + count/max |
| 某策略近 30 天的實際報酬（模擬／測試網） | 策略庫卡片 | 索引 filter（時間範圍 + strategy_id）+ fold |
| 某策略的權益走勢 sparkline | 策略庫卡片 | 1 筆 session 的曲線，降採樣到 ~30 點 |
| 60 天權益曲線 | 總覽 | 時間範圍內所有 session 的曲線（§10.1 有未決問題） |

資料量上界（假設「使用者天天跑回測」= 每天 20 次回測 + 2 場模擬）：

- 索引紀錄：22 筆/天 × 365 = **約 8,000 筆/年**。一筆 pretty-print 的 JSON 約 400 位元組（含參數 map）→ **約 3.2 MB/年**。
- 全量載入 + 解析 3 MB JSON：約 20–40 毫秒（serde_json 的量級是 100+ MB/s）。只在 App 啟動時做一次。
- 「近 30 天某策略報酬」：對 8,000 筆的 `Vec` 做 filter + fold = **微秒級**，不需要索引結構。
- 曲線檔：1 分鐘 K 線跑一個月的模擬 = 43,200 點 × 約 25 位元組 = **約 1 MB/場**。回測一個月 1m 資料同量級。只在使用者打開那一場時才讀。

**結論：這是一個「幾千筆紀錄、聚合都在記憶體裡做」的規模，不是資料庫的規模。**

### 方案 P1：SQLite（`rusqlite` 或 `tauri-plugin-sql`）

| | |
|---|---|
| 優點 | 真正的聚合查詢（`GROUP BY strategy_id WHERE started_at > ?`）、索引、交易性寫入、資料量再大 100 倍也不用改設計；「某策略近 30 天報酬」是一行 SQL |
| 缺點 | 這是專案第一個資料庫，依 `CLAUDE.md` 與公司規則屬 🔴 **Framework 級依賴，必須 CEO 明確同意**；`rusqlite` 預設 bundle 一份 C SQLite（Windows 交叉編譯多一個變數）；schema migration 從此是一件要維護的事（目前零）；寫測試要管理暫存 DB 檔與 schema 初始化 |
| 風險 | 為了 §5 算出來的 3 MB/年、微秒級查詢，付出一個 C 依賴 + migration 機制 + 🔴 決策流程。真正的風險不是技術，是**用複雜度換不到可量測的好處**，而這個專案的每一步都明確拒絕這種交換（1.6 的「不加外部套件」、5.4 的「範圍取捨」） |

### 方案 P2：延伸 `at-core::bar_store` 的檔案式儲存

| | |
|---|---|
| 優點 | 零新依賴，完全沿用已驗證的 round-trip 保證（`Fixed`/`i64`/`f64` 的 `Display`/`FromStr`）；格式人眼可讀、可用 `grep`／`wc -l` 除錯 |
| 缺點 | `bar_store` 是**固定 6 欄純數字**的格式（`crates/core/src/bar_store.rs:70` 直接 `fields.len() != 6` 就報錯）。session 紀錄是巢狀的（參數 map、`Option<Metrics>`、狀態 enum 帶訊息字串），而且一定會長欄位。硬塞進逗號分隔格式就要自己做欄位版本管理、字串轉義（參數值可能含逗號）——那是在手刻一個序列化格式 |
| 風險 | `docs/steps/1.6` 自己就寫明這是升級觸發條件：「如果之後這個格式要版本演進（例如加欄位、要相容舊檔案）或存更複雜的巢狀結構，那時候再評估要不要引入 serde + 一個真正的序列化格式」。現在正是那個時候 |

### 方案 P3：JSON 索引 + 每場一個明細資料夾（**選定**）

把資料拆成「永遠要全部載入的小東西」和「只在被打開時才讀的大東西」：

- `<base_dir>/sessions/index.json`：**一個 JSON 陣列**，每個 session 一筆摘要（§6.1）。App 啟動時全量載入記憶體，之後所有列表／聚合查詢都在記憶體做。寫入時整份重寫（寫暫存檔 + `fs::rename` 原子替換）。
- `<base_dir>/sessions/<session_id>/curve.csv`：`open_time,equity` 一行一點，**沿用 `bar_store` 的風格**（純文字、`Fixed` 的 `Display`/`FromStr`、整份壞掉就報錯不跳過壞行），執行中可以邊跑邊 append。
- `<base_dir>/sessions/<session_id>/fills.jsonl`（選配，Phase D 的成交明細才需要）：一行一筆 JSON，append-only。

| | |
|---|---|
| 優點 | 零新依賴（`serde_json` 在 `app` 與 `at-account-sync` 都已經在用）；照 `at-account-sync` 既有慣例，看得懂一個就看得懂另一個；索引小且只在生命週期轉換時寫（每場最多 2–3 次），**曲線的高頻寫入完全不碰索引**；明細檔 append-only，當掉最多丟最後一行；人眼可讀、可用一般檔案工具備份／搬移 |
| 缺點 | 沒有交易性（用「寫暫存 + rename」補到足夠）；索引更新是整份重寫（8,000 筆 ≈ 3 MB ≈ 10 毫秒，一天 50 次，可忽略）；同一台機器開兩個 App 實例會互相覆蓋索引（§10.2）；真要做複雜聚合（多維度 group by）就得自己寫 |
| 風險 | 索引無上限成長。對策：§6.3 的保留策略 + 明確的升級觸發門檻（§5.4） |

### 決定：P3，並寫下升級到 SQLite 的觸發條件

選 P3 不是「SQLite 不好」，而是現在換不到任何能量測的好處，卻要付 🔴 決策 + C 依賴 + migration 機制。但這個決定必須是**可反悔的**，所以先寫下觸發條件與遷移路徑：

**升級觸發條件（任一成立就重新評估）**

1. `index.json` 超過 **10 MB** 或 **25,000 筆**。
2. App 啟動時載入索引超過 **100 毫秒**（實測，不是猜）。
3. 出現需要多維度聚合的新需求（例如「按策略 × 月份 × 幣種的報酬矩陣」），在記憶體裡寫起來超過 50 行。
4. 需要多個行程同時寫（例如 §10.2 的多實例，或未來的 headless runner）。

**遷移路徑**：§6.1 的 `SessionRecord` 刻意是扁平的（除了 `params` 與 `metrics` 兩個子結構），每個欄位都能一對一映射成一個資料表欄位；`params` 轉成 `session_params(session_id, key, value)` 子表或一個 JSON 欄位。遷移是「讀 index.json → 逐筆 INSERT」的一次性腳本，曲線檔原樣保留在檔案系統（不要塞進 BLOB）。

**中間升級台階**（在跳 SQLite 之前還有一階）：索引從 JSON 陣列改成 JSONL（一行一筆、新 session 直接 append、更新靠「同 id 後寫覆蓋前寫 + 啟動時壓實」）。只在觸發條件 1 或 2 成立、而 3 和 4 都不成立時才值得。現在不做——整份重寫在這個規模下是正確且無聊的做法，JSONL 是為了不存在的寫入負載做優化。

### 5.4 寫入的一致性規則

1. **索引寫入全部經過一個 `Mutex`**（`SessionStore` 內部）。桌面 App 是單一行程，多場 session 同時收尾時靠這個鎖序列化。
2. **原子替換**：寫 `index.json.tmp` → `fs::rename` 到 `index.json`。同一個資料夾內的 rename 在 macOS/Windows 都是原子的，不會留下半份檔案。
3. **曲線 append**：開檔用 append 模式，每根 K 線一行。不做 `fsync`（成本不值得），所以當機最多丟尾端幾行——這是可接受的，而且 §9.3 的啟動對帳會把這種 session 標成「中斷」而不是假裝完整。
4. **壞檔處理和 `at-account-sync` 不同**：費率快取壞了當作沒有（下次重新同步就好），但**使用者的歷史紀錄不能靜默丟棄**。索引解析失敗時改名為 `index.json.bad-<timestamp>`、以空索引啟動、回傳一個繁體中文警告給前端顯示。`schema_version` 比程式認得的新時同樣處理（不要用舊程式寫壞新格式）。

---

## 6. 資料模型

### 6.1 `SessionRecord`（索引裡的一筆）

一個 session 不論是回測、模擬還是測試網交易，共同欄位如下。金額一律用**字串**保存 `Fixed`（`at-core` 零依賴、沒有 `Serialize`，而且字串是這個專案跨邊界傳 `Fixed` 的既有慣例——見 `app/src-tauri/src/backtest.rs:124` 的 `BacktestSummary`）。

```jsonc
{
  "schemaVersion": 1,
  "id": "paper-1790756100000-001",
  "kind": "paper",                    // RunMode: backtest / paper / testnet /（將來）live
  "market": "spot",                   // at_core::Market: spot / usdmPerp
  "symbol": "BTCUSDT",
  "interval": "1m",
  "strategyId": "sma_cross",
  "strategyName": "均線交叉",
  "params": { "fastPeriod": "10", "slowPeriod": "50" },   // 排序穩定的 map

  "startedAtMs": 1790756100000,
  "endedAtMs": 1790762400000,         // null = 還在跑（或當機後沒收尾，見 9.3）
  "status": "stopped",                // running / stopped / failed / completed / interrupted
  "statusMessage": null,              // failed 時放引擎的中文錯誤訊息

  "startingCapital": "10000",
  "finalEquity": "10184.52",          // null = 一根都還沒收盤
  "barsSeen": 105,

  "metrics": {                        // null = 曲線太短算不出來；欄位語意同 at_core::Metrics
    "totalReturn": "0.018452",
    "annualizedReturn": null,         // 跨時間太短時 at_core 本來就回 None，照實存 null
    "maxDrawdown": "0.0071",
    "sharpe": "1.42",
    "spanYears": "0.000199"
  },

  "counters": {                       // 種類專屬的計數器，各自可為 null
    "trades": 4,                      // paper / backtest
    "liquidations": 0,                // paper / backtest
    "fills": null,                    // testnet
    "blocked": null,                  // testnet（被風控閘門擋下的次數）
    "feesPaid": null                  // testnet（估算累計手續費）
  },

  "costAssumptions": {                // 「這個數字是用什麼假設算出來的」，缺了就無法解釋報酬
    "feeModel": "spot_vip0",
    "slippage": "0.0005",
    "fundingRate": "0",
    "maintenanceMarginRate": "0.005"
  },

  "saved": false,                     // 使用者按過「儲存回測」才是 true（見 6.3）
  "dataSourcePath": "…/BTCUSDT/1m/2026-09.txt",   // 只有 backtest 有；null 表示即時行情
  "notes": null                       // 預留給使用者自己標註；現在前端不提供編輯入口
}
```

**設計決定與理由**

- **`kind` 重用 `at_core::RunMode`，不新定義 enum。** `crates/core/src/types.rs:46` 已經有，而且 `sends_orders()` 是現成的安全護欄：store 可以在寫入時 `debug_assert` 「`kind.sends_orders() == false` 的紀錄不該帶 testnet 專屬計數器」。
- **`counters` 用一個全 `Option` 的結構，而不是 `kind` 標籤的 enum。** 兩種 session 的計數器只有 5 個欄位、語意互不重疊，用 `Option` 讓「列表只讀共同欄位」的程式碼不必 match。代價是允許了不合法組合（paper 帶 `fills`）——用寫入時的 assert 擋，不用型別擋。這是刻意選簡單。
- **`metrics` 照 `at_core::Metrics`（`crates/core/src/metrics.rs:89`）一對一，含 `spanYears`。** `spanYears` 在那裡是為了讓年化與夏普可以被驗算，存下來的理由一樣：使用者質疑數字時要能重算。
- **`costAssumptions` 不能省。** 兩筆年化 30% 的紀錄，一筆零費率一筆 VIP0 吃單，在列表上長得一樣但不是同一件事。`app/src-tauri/src/backtest.rs:124` 的 `BacktestSummary` 已經在回顯這些，store 照存。
- **`status` 有五個值，`interrupted` 是獨立的一個。** 正常停止（帳本完整）、引擎失敗（帳本從某根起不可信）、當機中斷（曲線被截斷、最終指標不可信）是三件不同的事，在 UI 上必須能分辨。`completed` 專給回測（跑完整段資料，不是「被停止」）。
- **`params` 用排序穩定的 map。** 寫出來的 JSON 逐次相同，`git diff`／人眼比對才有意義。

### 6.2 檔案佈局

```
<app_data_dir>/
├── fee_schedule_BTCUSDT.json      ← 既有（at-account-sync）
├── symbol_rules_BTCUSDT.json      ← 既有（at-account-sync）
├── BTCUSDT/1m/2026-09.txt         ← 既有（at-downloader + bar_store）
└── sessions/                      ← 新增
    ├── index.json                 ← SessionRecord 陣列
    └── paper-1790756100000-001/
        ├── curve.csv              ← open_time,equity 一行一點
        └── fills.jsonl            ← 選配，Phase D 才寫
```

`session_id` 直接當資料夾名稱，所以 **id 的字元集必須受控**：`[a-z0-9-]`，由 registry 用 `format!` 產生。`at-session-store` 的每個公開函式都要在組路徑**之前**驗證 id（拒絕空字串、`.`、`..`、任何非白名單字元）——`read_session_curve(sessionId)` 的 id 來自前端，是信任邊界，不驗證就是路徑穿越漏洞（§9.6）。

### 6.3 保留策略（索引不能無上限長）

- `saved: true` 的紀錄**永不自動刪除**（使用者按過「儲存回測」，那是他的資料）。
- `saved: false` 的回測紀錄：保留最新 500 筆，更舊的連同曲線檔一起刪。500 筆足以撐起「最近回測清單」與「回測次數」兩個需求（§10.3 是這個數字的未決問題）。
- 模擬／測試網 session **一律不自動刪除**（它們是真的跑過即時行情的紀錄，不可重現），只提供手動 `delete_session`。
- 「回測次數」的語意因此是「保留窗口內的次數」而不是「史上總次數」。若要真正的累計次數，索引外另存一個 `strategy_stats.json` 的計數器——**現在不做**，等 UI 真的被這個差異絆到再說。

### 6.4 「每場都自動留紀錄」而不是「按儲存才留」

Wireframe 同時要「回測次數」（必須每次都記）和「儲存回測／加入比較」按鈕（使用者挑選要留的版本）。解法是一個 `saved: bool`：

- `run_backtest_command` 跑完**自動**寫一筆 `saved: false` 的紀錄 → 「回測次數」「最近回測清單」有資料。
- 「儲存回測」按鈕把該筆翻成 `saved: true` → 比較頁、策略庫的「保留版本」用這個過濾，而且不受 §6.3 的裁剪影響。

---

## 7. Tauri command 介面

### 7.1 既有 command 的改動（刻意破壞相容）

| Command | 現在 | 改成 | 為什麼 |
|---|---|---|---|
| `start_paper_trading(request)` | `Result<(), String>` | `Result<String, String>`（回 session id） | 前端後續所有操作都要用 id 定位 |
| `stop_paper_trading()` | 無參數 | `stop_paper_trading(sessionId)` | 有多場時「停止」必須指名 |
| `paper_trading_status()` | 無參數 | `paper_trading_status(sessionId)` | 同上 |
| `start_testnet_trading(request)` | `Result<(), String>` | `Result<String, String>` | 同上 |
| `stop_testnet_trading()` | 無參數 | `stop_testnet_trading(sessionId)` | 同上 |
| `set_testnet_kill_switch(on)` | 只有 `on` | `set_testnet_kill_switch(sessionId, on)` | 同上 |
| `testnet_trading_status()` | 無參數 | `testnet_trading_status(sessionId)` | 同上 |
| `run_backtest_command(request)` | `Result<BacktestSummary, String>` | 同簽名，`BacktestSummary` 多一個 `sessionId` 欄位；**副作用**：成功後寫一筆 store 紀錄 | 「最近回測」「回測次數」要靠它 |

**不保留「不帶 id 就操作唯一那場」的相容層。** 那種相容在只有一場的時候看起來對，在有兩場的時候會停錯人的 session——money-critical 路徑上這是不能接受的便利。改法是連同 `app/src/PaperTrading.tsx`、`app/src/TestnetTrading.tsx` 與它們的四份測試一起改，blast radius 已知且有限（Phase D 本來就要重寫模擬交易頁）。

不存在的 session id 一律回 `Err("找不到這場交易（可能已經停止）：<id>")`，不要靜默成功。

### 7.2 新增 command

```
list_live_sessions() -> Result<Vec<LiveSessionDto>, String>
  // 總覽「執行中策略」表格、TopBar「執行中策略數」、風控「跨策略加總」共用這一個。
  // 純記憶體讀取，不碰檔案、不碰網路。
  LiveSessionDto {
    sessionId, kind, market, symbol, interval,
    strategyId, strategyName,
    status,                 // running / stopped / failed（停止後仍短暫留在列表，見 7.4）
    startedAtMs,
    equity: String | null, position: String, dailyPnl: String | null,
    asOfMs: number | null, stale: boolean,   // stale 見 9.2
    killSwitch: boolean | null,              // 只有 testnet 有
    barsSeen,
  }

list_sessions(filter) -> Result<Vec<SessionSummaryDto>, String>
  // 最近回測清單、模擬交易「已停止」標籤頁、策略庫的回測次數／近 30 天報酬。
  // 一個查詢服務三個畫面，不要為每個畫面各開一個 command。
  filter {
    kinds?: ["backtest" | "paper" | "testnet"],
    strategyId?: string, symbol?: string,
    sinceMs?: number, untilMs?: number,
    savedOnly?: boolean,
    limit?: number,          // 預設 100，上限 1000
  }
  // 回傳 SessionRecord 的扁平 DTO（§6.1），依 startedAtMs 新到舊排序。

read_session_curve(sessionId, maxPoints?) -> Result<Vec<EquityPointDto>, String>
  // 明細頁的曲線、策略庫 sparkline、總覽 60 天曲線。
  // maxPoints 給就在 Rust 端等距降採樣（sparkline 要 30 點，不要把 43,000 點丟過 IPC）。
  // 重用既有的 EquityPointDto（app/src-tauri/src/backtest.rs）。

mark_session_saved(sessionId, saved) -> Result<(), String>
  // 「儲存回測」按鈕（§6.4）。

delete_session(sessionId) -> Result<(), String>
  // 刪索引紀錄 + 整個明細資料夾。執行中的 session 拒絕刪除（先停止）。

session_store_health() -> Result<StoreHealthDto, String>
  // { sessionCount, indexBytes, lastErrorZh: string | null }
  // §5.4 的壞檔警告、§5.3 的升級觸發條件都要有地方看。設定頁放一行就夠。
```

### 7.3 事件

| 事件名 | 變動 | 消費者 |
|---|---|---|
| `paper-trading-update`（既有名稱不改） | payload 加 `sessionId` 欄位 | 模擬交易明細頁，依 `sessionId` 過濾 |
| `testnet-trading-update`（既有名稱不改） | payload 加 `sessionId` 欄位 | 測試網明細頁，依 `sessionId` 過濾 |
| `session-registry-changed`（新增） | `{ sessionId, change: "started" \| "tick" \| "stopped" \| "failed" }` | 總覽、TopBar、風控頁：收到就重新呼叫 `list_live_sessions` |

**為什麼不合併成一個跨種類的事件：** 總覽只需要共同欄位（權益／部位／狀態），明細頁需要種類專屬的全部欄位（testnet 還有 `Order` 結果）。合併就得發明一個聯集 payload，而且逼總覽去理解兩種快照形狀。精簡的「去重新查一次」通知讓總覽只依賴 `list_live_sessions` 一個介面，兩個明細頁的事件處理也只需要加一個 `sessionId` 過濾。每場 session 每根 K 線多發一則空心事件（一分鐘一次）是可忽略的成本。

### 7.4 停止後的 session 停留在 registry 多久

按停止之後 session 不立刻從 `list_live_sessions` 消失——wireframe 的模擬交易頁有「已停止」標籤、而且使用者按停止後會想看最終數字。規則：

1. 收到 `Stopped`／`Failed` → 更新 `LiveState.status`、寫 store 的收尾紀錄（`endedAtMs`、`finalEquity`、`metrics`）、`emit`。
2. session 留在 registry 直到使用者在 UI 上關掉那個標籤（新 command 不需要——沿用 `delete_session` 太重；用 registry 的 `forget(sessionId)` 語意，可以合進 `stop_*` 的第二次呼叫，或由前端不再顯示即可）。
3. **上限保護**：registry 裡最多留 20 場已停止的 session，超過就丟最舊的（紀錄已經在 store 裡，不會遺失）。

> 這一條是刻意留的簡化：「已停止但還在記憶體」和「已停止且只在 store 裡」在前端看起來一樣（都靠 `list_sessions` 查得到），所以前端不需要知道差別。實作時 `list_live_sessions` 只回 `running`，已停止的從 `list_sessions` 查——這樣第 2、3 點都不需要存在。**建議採這個更簡單的版本**，上面兩點留作已知的替代方案。

---

## 8. 給風控 Phase（Phase E）預留的介面

風控要做跨策略全域加總（總部位上限、單日虧損上限、套利淨曝險），需要「現在有哪些 session 在跑、各自的部位與權益」。

### 8.1 Rust 層介面（風控引擎用）

```rust
impl SessionRegistry {
    /// 跨所有執行中 session 的曝險快照。
    ///
    /// 契約（Phase E 可以依賴這些保證）：
    /// 1. 純記憶體讀取，不碰檔案、不碰網路，不會阻塞超過一次 HashMap 走訪的時間。
    /// 2. 只拿 RwLock 的讀鎖，而且全部 clone 出來才回傳——呼叫端拿到的是值，
    ///    不是借用，所以持有回傳值的期間不會卡住任何 session 的轉發執行緒。
    /// 3. 絕不呼叫呼叫端提供的任何回呼，因此從 session 自己的執行緒呼叫它不會死鎖。
    /// 4. 每一筆都帶 as_of_ms。資料可能是過期的（行情斷線時會），
    ///    呼叫端必須自己決定「多舊就不可信」——風控不該假裝資料是新的。
    pub fn aggregate_exposure(&self) -> PortfolioExposure;
}

pub struct PortfolioExposure {
    /// 產生這份快照的時間（本機時鐘）。
    pub taken_at_ms: i64,
    pub sessions: Vec<SessionExposure>,
}

pub struct SessionExposure {
    pub session_id: SessionId,
    pub kind: RunMode,          // 風控可以據此只把 Testnet/Live 算進真錢曝險
    pub market: Market,
    pub symbol: Symbol,
    /// 帶正負號的持倉數量（正多負空），語意同 at_risk_control::AccountState.position。
    pub position: Fixed,
    pub equity: Option<Fixed>,
    pub daily_pnl: Option<Fixed>,
    pub kill_switch: Option<bool>,
    /// 最後一根收盤 K 線的 open_time。None = 一根都還沒收盤。
    pub as_of_ms: Option<i64>,
}
```

**刻意不做的事**：不提供「名目金額加總」或「幣種分組」之類的計算。那是風控的業務邏輯（要用哪個價格評價、要不要把模擬算進去、`None` 要當 0 還是當「不可信」都是風控的決定），registry 只負責誠實回報原始數字。`at_risk_control::AccountState`（`crates/risk-control/src/lib.rs:105`）本來就是「呼叫端給的快照，閘門不自己查帳本」——這個介面就是餵它的原料。

### 8.2 Tauri command（風控頁顯示用）

風控頁要顯示「目前全域用量 vs 上限」，用 §7.2 的 `list_live_sessions` 就夠，不另開 command。

### 8.3 要先講清楚的跨 Phase 風險

**Phase B 不需要動 `at-testnet-trading`，但 Phase E 可能要。** 現在的風控閘門在交易執行緒裡呼叫（`crates/testnet-trading/src/trader.rs` 的 `send_gated_order` → `RiskLimits::check`），它只看自己這場的 `AccountState`。要做「送單前檢查全域部位上限」，就得讓那個執行緒拿到 `aggregate_exposure()`，也就是在 `at-testnet-trading` 開一個注入點（例如 `spawn` 多收一個 `Box<dyn GlobalGate + Send>`）——**那是對已審查過的 money-critical crate 的改動**，要走 6.x 等級的跨模型審查。

Phase E 的替代方案（不改那個 crate）：全域檢查放在 App 層的「開新 session 之前」與「週期性監控」兩個時機，而不是「每一筆單之前」。這會讓全域上限變成**事後偵測 + 自動停止**而不是**事前阻擋**。兩者的安全性差很多，是 Phase E 必須明確裁決的事。這份 ADR 的責任只有一條：**`aggregate_exposure()` 的契約（§8.1 的四點）必須強到可以從交易執行緒裡同步呼叫**，所以不管 Phase E 選哪條路，registry 這邊都不用再改。

---

## 9. 魔鬼代言人：失效模式與攻擊清單

誠實列出撐得住的地方與撐不住的地方。

### 9.1 並行 100 倍（100 場同時跑）

**撐不住，而且必須明文設上限。** 每一場 session 吃 **3 條 OS 執行緒**：`at-market-stream` 的 WebSocket 執行緒、交易迴圈執行緒、App 層的轉發執行緒。還有一條獨立的 WebSocket 連線。

- 100 場 = 300 條執行緒 + 100 條 WebSocket。執行緒本身桌面撐得住（記憶體約 100 MB 棧），但 100 條到 Binance 的連線會踩到交易所的連線頻率限制，而且斷線重連風暴會互相加劇。
- 誠實的舒適上限是 **20 場左右**。
- **決策（D7）**：registry 硬上限 20 場執行中 session，超過回 `Err("同時執行的交易已達上限 20 場，請先停止其中一場")`。這不是推託，是「超過就會踩交易所限制」的物理事實。
- 升級路徑（**現在不做**）：同一個 symbol+interval 的多場 session 共用一條 WebSocket，由 `at-market-stream` 做扇出（它現在是一條連線對一個 receiver）。那是對 4.5 的實質改動，等真的有人要同時跑 20 場再說。

### 9.2 第三方掛掉／網路斷掉

- `at-market-stream`（4.5）本來就有自動重連，重連期間引擎收不到收盤 K 線，**registry 不會也不該把 session 標成 Failed**（只有引擎回錯誤才是 Failed）。這一點現有設計是對的，照抄。
- **但是**：連不回來的 session 會永遠停在 `running`、數字凍結，而總覽上看起來一切正常。這是真的會害人的狀況。
- **對策**：`LiveState.as_of_ms` + 衍生的 `stale` 旗標（超過 3 × K 線週期沒有新的收盤 K 線就是 stale）。總覽與風控頁必須把 stale 顯示出來，`aggregate_exposure()` 照實回 `as_of_ms` 讓風控自己判斷。Wireframe 的風控頁本來就有「行情中斷 > 3 秒 → 暫停高頻並撤單」，這個欄位就是那條規則的輸入。
- 測試網 REST 掛掉：`at-testnet-trading` 已經有送單後等回報的退避與 `StoppedWhileWaiting` 處理，registry 不改這條路。

### 9.3 App 當掉／斷電

**這是最容易漏掉的一條。** 索引裡會留下 `status: "running"`、`endedAtMs: null` 的孤兒紀錄，而曲線檔被截斷在最後一次 flush。

**對策：啟動時對帳。** App 啟動載入索引後，任何 `status == "running"` 的紀錄都不可能真的在跑（registry 是空的），一律改寫成 `status: "interrupted"`、`endedAtMs = 曲線最後一點的 open_time`、`metrics = 從現有曲線重算`，並在 UI 上和「正常停止」區分顯示。**不要**標成 `stopped`（會讓使用者相信一條被截斷的曲線是完整的），也不要標成 `failed`（引擎沒出錯）。

### 9.4 資料量 10 倍／100 倍

- 10 倍（每年 80,000 筆、32 MB 索引）：載入約 300–400 毫秒，開始有感但還能用；觸發 §5.3 的升級條件 1。
- 100 倍：必須換 SQLite。這就是為什麼 §5.3 的觸發條件要寫下來而不是口頭約定。
- 曲線檔不受影響（按需讀取，和總量無關）。
- 總覽的「60 天曲線」是唯一會隨使用量變慢的查詢：要讀窗口內所有重疊 session 的曲線檔。60 天內最多幾十個檔、每個 1 MB → 數十毫秒。可接受，但若實測超過 200 毫秒就該加一張「每日權益彙總」的小表。**現在不預先做**（§10.1 還沒決定那條曲線的語意，先做會做錯）。

### 9.5 回滾／格式演進

- `schemaVersion` 在每筆紀錄上（不只在檔頭）：混著新舊版紀錄的索引也能一筆一筆處理。
- 讀到比程式認得的更新的 `schemaVersion` → 不改寫那一筆、整個 store 進唯讀模式、`session_store_health` 回中文警告。**舊程式不准覆寫新格式**。
- 新增欄位用 `Option` + `#[serde(default)]`，舊檔案能被新程式讀。
- 回滾的實際操作：索引是人眼可讀的 JSON，使用者可以直接備份／編輯／刪除整個 `sessions/` 資料夾。這是檔案式方案一個真實（不是湊數）的好處。

### 9.6 安全／被攻擊面

- **路徑穿越**：`read_session_curve(sessionId)`／`delete_session(sessionId)` 的 id 來自前端。`at-session-store` 的每個公開函式都必須在組路徑之前驗證 id 的字元集（§6.2），不然 `../../..` 就能讀寫任意檔案。這是這份設計唯一的真實安全漏洞來源，必須有專門的測試。
- **金鑰**：store 永遠不寫憑證。`SessionRecord` 裡沒有任何欄位可能裝金鑰（參數是策略參數、路徑是 K 線檔路徑）。測試網 session 的紀錄只有本地帳本數字，不存交易所訂單 id／回應原文。
- **安全規則不受影響**：`kind` 用 `RunMode`，`sends_orders()` 對 `Backtest`／`Paper` 仍然是 `false`。registry 不新增任何送單路徑——它只呼叫 handle 既有的 `stop`／`set_kill_switch`。
- **檔案權限**：`app_data_dir()` 本來就是使用者範圍，不自己做 chmod。

### 9.7 單人維護

- `at-session-store`：約 200–300 行（serde 結構 + 原子寫入 + id 驗證 + 查詢 filter），純函式居多，測試用暫存目錄（照 `bar_store.rs:149` 與 `account-sync` 的既有做法）。
- `session_registry.rs`：約 300–400 行，最難的是 §4.3 的四條鎖不變量。這是整個 Phase B 唯一需要仔細審查的部分，要獨立審查。
- 對照 SQLite 方案要多維護的東西：C 依賴的跨平台建置、schema migration 機制、測試的 DB 初始化。**這一條是選 P3 的最實在理由之一。**

### 9.8 撐得住的地方（不為了湊數而捏造問題）

- **索引全量載入 + 記憶體聚合**：以 §5 算出的規模，所有 wireframe 查詢都在微秒級。這裡沒有隱藏的效能問題。
- **不動底層 crate**：`at-core`／`at-paper-trading`／`at-testnet-trading`／`at-risk-control` 的 diff 是空的，6.x 的 money-critical 審查結論仍然有效。這是真的零風險，不是「我們覺得風險低」。
- **原子替換 + append-only 明細**：這兩個機制在單行程下是正確的，不需要交易；撐不住的只有多行程（§10.2）。
- **既有事件名稱不改**：兩個明細頁的事件處理只需要加一個過濾條件，不是重寫。

---

## 10. 未決問題（需要決定才能完成下游 Phase）

### 10.1 🟡 總覽的「60 天權益曲線」到底是什麼曲線？（Phase C 要用）

Wireframe 的總覽頂部是「帳戶權益（USDT）」，暗示的是**真實 Binance 帳戶餘額**。但這個專案沒有查餘額的端點（`crates/testnet-trading/src/trader.rs:83` 的註解：「本地帳本，不是交易所餘額——這個專案沒有查餘額的端點」），而模擬 session 的「虛擬資金」互相加總在金融上沒有意義（兩場各 10,000 虛擬 USDT 加起來不是 20,000 的任何東西）。

**建議**：Phase C 把它定義成「已紀錄 session 的權益合計曲線」，並在圖上明確標示「模擬／測試網帳本合計，非交易所餘額」。計畫檔已經寫了「看實際累積多少資料顯示多少，不用硬湊 60 天假資料」，這個定義與之一致。需要 CEO 或 Phase C 確認後才知道要不要加每日彙總表（§9.4）。

### 10.2 🟡 同一台機器開兩個 App 實例

兩個行程同時寫 `index.json` 會互相覆蓋（原子替換保證不會壞檔，但後寫的會蓋掉前寫的紀錄）。三個選項：

- (a) 什麼都不做，在文件裡寫明「不要開兩個」。成本 0，風險是使用者真的開兩個然後丟紀錄。
- (b) 加 `tauri-plugin-single-instance`（🟡 小依賴，Tauri 官方）：第二個實例直接把焦點給第一個。
- (c) 自己做 advisory lock 檔：要處理殘留鎖檔（當機後）的判斷，比 (b) 麻煩。

**建議 (b)**，理由是它同時解掉「兩份 WebSocket 連線打同一組金鑰」這個更討厭的問題。但它是新依賴，照 CEO 權限表屬 🟡（一般 runtime 依賴、不觸及認證／付費／資料／安全層；Tauri 官方 plugin）。先做 (a) 也可以，但要寫進 README。

### 10.3 🟢 保留策略的具體數字（§6.3 的 500 筆）

未儲存的回測保留 500 筆是憑估計（約 25 天的量）。建議先實作成一個常數 + `session_store_health` 顯示現況，跑一兩週看實際累積速度再調。不需要事前決定。

### 10.4 🟢 Phase D 要不要寫逐筆成交明細

`fills.jsonl` 在這份設計裡是選配。`PaperSnapshot` 現在只有累計 `trades: usize`，要逐筆明細就得讓 `at-paper-trading` 多送一種更新事件——**那是對已審查 crate 的改動**，而且是 Phase D 的需求，不是 Phase B 的。這份設計只保證檔案佈局留了位置（`sessions/<id>/fills.jsonl`），不預先定義格式。

### 10.5 🔴 給 CEO 的唯一真正決策點

如果 CEO 傾向「一次把持久化做到位、直接用 SQLite」，那是 🔴 Framework 級依賴，需要明確同意。這份設計的立場是**現在不需要**，而且寫下了 §5.3 的量化升級觸發條件，所以之後要換不會變成重寫。除此之外，Phase B 的所有決策都在 🟡 以下（App 層新模組 + 一個照既有慣例的新 crate + 既有 command 加參數）。

---

## 11. 對現有程式碼的影響範圍

### 不改（diff 必須是空的）

- `crates/core/`（含 `bar_store.rs`：session 曲線用它的**風格**，不呼叫它的函式，因為欄位數不同——理由同 1.6 不重用 `kline_csv`）
- `crates/paper-trading/`
- `crates/testnet-trading/`
- `crates/risk-control/`
- `crates/market-stream/`、`crates/binance-client/`、`crates/account-sync/`、`crates/downloader/`、`crates/secret-store/`

驗收：根目錄 `cargo test` 的既有測試一題不少、一題不改。

### 新增

- `crates/session-store/`（crate 名 `at-session-store`）：`SessionRecord` 的序列化、索引讀寫（原子替換）、曲線 append／讀取／降採樣、id 驗證、查詢 filter、啟動對帳（§9.3）。依賴 `at-core` + `serde` + `serde_json`（和 `at-account-sync` 完全相同的依賴組合）。加進根 `Cargo.toml` 的 members。
- `app/src-tauri/src/session_registry.rs`：`SessionRegistry`、`SessionEntry`、轉發執行緒、§4.3 的鎖不變量。
- `app/src-tauri/src/sessions.rs`（或併進 registry）：§7.2 的新 command 與 DTO。

### 修改

- `app/src-tauri/src/lib.rs`：`manage(SessionRegistry)` 取代 `manage(PaperTradingState)`／`manage(TestnetTradingState)`；`invoke_handler` 註冊新 command。
- `app/src-tauri/src/paper_trading.rs`：`PaperTradingState`／`Inner`／`forward_updates` 移進 registry；`validate_request`、`PaperSnapshotDto`、`apply_update` 的**純函式部分與既有測試全部留下**（它們驗的是驗證規則與 DTO 轉換，和單一／多 session 無關）。
- `app/src-tauri/src/testnet_trading.rs`：同上。
- `app/src-tauri/src/backtest.rs`：`run_backtest_command` 成功後寫 store；`BacktestSummary` 加 `sessionId`。
- `app/src-tauri/Cargo.toml`：加 `at-session-store` path 依賴。
- `app/src/PaperTrading.tsx`、`app/src/TestnetTrading.tsx` 與它們的四份測試：command 簽名改了，事件 payload 多了 `sessionId` 要過濾。
- `app/src/paperTradingTypes.ts`、`testnetTradingTypes.ts`：DTO 型別跟著改。
- `docs/ROADMAP.md`／`docs/plans/2026-10-01-wireframe完整補齊.md`：勾選與開放決策更新。

### 實作順序建議（每一步都能獨立驗證）

1. `at-session-store`（純 crate，不碰 App）：`cargo test -p at-session-store` 綠。含 id 驗證、壞檔、原子替換、啟動對帳的測試。
2. `run_backtest_command` 接上 store（最小的真實使用者）：跑一次回測，紀錄出現在 `index.json`，`list_sessions` 查得到。此時還沒有 registry。
3. `SessionRegistry` + paper 單場走新路徑：行為和現在完全一樣，只是走了 registry。既有的 `PaperTrading.tsx` 測試改簽名後要全綠。
4. paper 多場 + 上限 20 + `list_live_sessions`。
5. testnet 接上 registry（含 kill switch 的 per-session 定位）。
6. `aggregate_exposure()` + 事件 `session-registry-changed`。

第 3 步是風險最高的一步（既有行為不能變），應該在那裡做獨立的 fresh-context 驗證。

---

## 12. 後果

**得到**：Phase C/D/E/H 四個 Phase 共同的地基；兩個已審查過的 money-critical crate 零改動；零新的第三方依賴；人眼可讀、可備份、可手動修復的歷史紀錄。

**付出**：既有六個 Tauri command 的簽名破壞性改動（連帶兩個前端頁面與四份測試）；App 層多了一塊需要仔細審查的並行程式碼（§4.3 的鎖不變量）；索引全量載入的設計有一個明確但有限的天花板（§5.3）。

**沒有解決**：真實帳戶權益（沒有餘額端點）；多行程並行（§10.2）；逐筆成交明細（§10.4）；每個 session 一條獨立 WebSocket 的浪費（§9.1）。這四件都有寫下來的升級路徑，不是被忽略。
