# ADR-002：全域（跨策略／跨 session）風控引擎 `at-portfolio-risk`

- **日期**：2026-10-01
- **狀態**：Proposed（等 CEO 裁決；未寫任何實作程式碼）
- **範圍**：設計稿 Risk（風控）頁的「全域上限 / 合約風控 / 熔斷規則 / 一鍵停止行為 / 觸發紀錄」
- **相關**：`docs/steps/6.3-風控層.md`（ADR-001 等級的既有決定）、`docs/steps/6.4-接上測試網下單.md`、
  `docs/steps/6.5-測試網交易頁面.md`、`docs/design-reference.md` 第 93–102 行
- **相依設計（尚未存在）**：`docs/architecture/2026-10-01-session-registry.md`
  —— 撰寫本文時這份文件在任何 worktree 與主 checkout 都還不存在，所以本文第 8 節是
  **本引擎對 registry 提出的介面契約草案**，需要和那份設計對齊後才定稿。

---

## 1. 背景

### 1.1 設計稿要什麼

設計稿的 Risk 頁是一個**獨立的全域風控頁**，管的是「這台機器上所有策略加起來」的風險：

| 分組 | 項目 |
|------|------|
| 全域上限 | 單筆金額上限、單一幣種部位上限、總部位上限、單日虧損上限（USDT）、價格偏離保護、下單頻率自我上限（% 交易所限制）、嚴格模式開關 |
| 合約風控 | 槓桿上限（預設 2 倍，超過拒絕下單）、保證金模式、強平距離下限（低於拒絕加倉、低於一半自動減倉）、資金費率預算（超過警示並減倉）、交易所端停損（強制開啟）、套利淨曝險上限 |
| 熔斷規則 | 連續虧損 5 筆→暫停策略、行情中斷 >3 秒→暫停高頻並撤單、1 分鐘拒絕率 >20%→暫停策略、對帳不一致→暫停下單（強制開啟）、單策略回撤超過上限→停止並撤單 |
| 一鍵停止行為 | 預設「停止並撤銷掛單、保留部位」；另一選項「停止、撤單並市價平倉」；失聯保護（斷線 >10 秒交易所自動撤合約掛單） |
| 觸發紀錄 | 列表 |

### 1.2 現狀（實測過的事實，不是推測）

- **風控目前只有 2 個數字，而且是 session 內的。**
  `at-risk-control` 的 `RiskLimits` 只有 `max_daily_loss` 與 `max_order_notional`
  （`crates/risk-control/src/lib.rs:80-83`）。
- **設定不持久化、綁在表單裡。** 前端是 `useState("50")` / `useState("200")`
  （`app/src/TestnetTrading.tsx:78-79`），隨 `StartTestnetTradingRequest` 送進後端，
  關掉 App 就沒了。沒有「風控頁」這個東西。
- **送單路徑只有一條，而且第一件事就是過閘門。**
  `Trader::send_gated_order`（`crates/testnet-trading/src/trader.rs:297`）是全專案唯一
  呼叫 `place_market_order` 的地方。
- **只有市價單，而且沒有撤單能力。** `OrderGateway` 刻意只有
  `place_market_order` / `query_order`（`crates/testnet-trading/src/trader.rs:38-47`），
  doc comment 明寫「撤單不在 6.4 的路徑上」。
- **只有現貨、只有單一交易對、只有一個 session。**
  現貨做空目標被當成空手（`trader.rs:250` 與測試 `a_short_target_is_treated_as_flat_on_spot`）；
  `TestnetConfig` doc 明寫「單一交易對、單一 session」（`crates/testnet-trading/src/lib.rs:119`）；
  App 層 `TestnetTradingState(Mutex<Inner>)` 一次只有一個 handle。
- **合約在引擎裡有、在下單路徑上沒有。** 2.5 做了合約回測（做空/槓桿/資金費/強平），
  但第 6 步的下單路徑是現貨市價單，沒有任何合約下單、改槓桿、設保證金模式的程式碼。
- **持久化的既有慣例**：`at-account-sync` 用 `serde_json` 寫
  `<app_data_dir>/fee_schedule_<SYMBOL>.json`，best-effort 寫入、讀不到就當沒有
  （`crates/account-sync/src/lib.rs:189-225`）。workspace 其他 crate 幾乎零依賴。

### 1.3 問題陳述

要在「不破壞 6.3 已經審查通過的單一 session 閘門」的前提下，加上一層
**跨策略、跨 session、可持久化、可被 UI 開關與調參**的風控，並把熔斷規則、
一鍵停止的兩種行為、觸發紀錄都容納進去。

---

## 2. 跟既有 `at-risk-control` 的關係

### 2.1 延續哪些原則（這些不重新發明，也不推翻）

| 原則 | 出處 | 新層怎麼延續 |
|------|------|--------------|
| **判不出來就擋（fail closed）** | `risk-control/src/lib.rs:31-34` | 查不到 portfolio 快照、快照過期、某個 session 的權益是 `None`、算術溢位 → 一律擋，**不當成 0**。見 8.3。 |
| **不讀系統時鐘、不碰檔案系統** | `risk-control/src/lib.rs:11-15` | 新 crate 同樣零 I/O：`now_ms` 由呼叫端傳入；觸發紀錄由引擎**回傳**、App 層負責寫檔。 |
| **不相信下單方自己貼的標籤** | `risk-control/src/lib.rs:109-111` | 「這是平倉」「這是降風險」一律由引擎從部位正負號自己算，不收下單方的 flag。一鍵停止不開「平倉例外」的洞（見 6.2）。 |
| **一鍵停止連平倉都擋** | `risk-control/src/lib.rs:19-24` | `set_kill_switch` 的語意一字不改。新的「緊急處理」疊在它之上，不是改它（見第 6 節）。 |
| **虧損上限只擋增加曝險，不自動強制平倉** | `risk-control/src/lib.rs:27-29` | 全域單日虧損上限用完全相同的語意；設計稿的「低於一半自動減倉」「超過警示並減倉」這類自動減倉動作**不實作**（見 7.2）。 |
| **一個閘門、一條送單路徑** | `trader.rs:99-101` | 新層塞進**同一個** `send_gated_order`，不另開第二條送單路徑。 |

### 2.2 新增的是哪一層

```
                    ┌─────────────────────────────────────────────┐
                    │ App 層（Tauri）                              │
  Risk 頁 ────────► │  · 風控設定的唯一持有者 + 持久化（JSON）        │
  觸發紀錄列表 ◄──── │  · 熔斷累加器（吃 TestnetUpdate 事件流）         │
                    │  · 觸發紀錄寫檔                                │
                    │  · 緊急處理程序（EmergencyAction）              │
                    └───────────────┬─────────────────────────────┘
                                    │ 注入 Arc<dyn PortfolioGate>
                                    │ （跟 OrderGateway 同一種注入手法）
                    ┌───────────────▼─────────────────────────────┐
                    │ at-testnet-trading                           │
                    │  Trader::send_gated_order                    │
                    │   ① RiskLimits::check      ← 6.3，不動        │
                    │   ② PortfolioGate::check   ← 本 ADR 新增      │
                    │   ③ place_market_order                       │
                    └───────────────┬─────────────────────────────┘
                                    │
       ┌────────────────────────────▼──────────────┐   ┌──────────────────────┐
       │ at-portfolio-risk（新 crate，零 I/O）       │──►│ session registry      │
       │  · GlobalLimits  全域上限                   │   │ （另一份設計）          │
       │  · BreakerRules  熔斷規則（含強制開啟）        │   │ PortfolioSnapshot     │
       │  · RiskEvent     觸發紀錄（回傳，不寫檔）       │◄──│                      │
       │  · 依賴 at-core::Fixed，**不依賴 at-risk-control** │   └──────────────────────┘
       └───────────────────────────────────────────┘
```

**決策：`at-risk-control` 的原始碼一行都不改。** 新 crate 不是它的 wrapper、也不是它的
上層型別，兩者是**並列的兩道閘門**，由 `send_gated_order` 依序呼叫。

為什麼不是 wrapper：wrapper 代表新 crate 要依賴 `at-risk-control` 的型別
（`AccountState` / `ProposedOrder` / `Blocked`），而這三個型別**沒有一個適合跨 session**
——`AccountState.position` 是單一交易對的部位、`Blocked` 的訊息都是 session 內語氣。
並列的兩道閘門讓兩邊各自演進，`Blocked` 與新的 `GlobalBlocked` 是兩個獨立 enum。

### 2.3 哪些欄位要不要塞進 `AccountState` / `ProposedOrder`

逐項評估（這是本 ADR 最關鍵的一組判斷）：

| 設計稿欄位 | 塞進 6.3 的型別？ | 理由 |
|-----------|-----------------|------|
| 單筆金額上限 | **不需要新增，直接重用** | `RiskLimits::max_order_notional` 就是它。Risk 頁的這個欄位應該**餵進既有的 `RiskLimits`**，不要在新層再做一份——兩份上限、兩個擋單理由、使用者看到兩種訊息，是純粹的重複。 |
| 單日虧損上限 | **型別重用，不改 API** | 設計稿要的是「帳戶層單日虧損」，既有的是「session 層」。但 `DailyPnl` 本身就是一個純粹的「餵我時間與權益、我回傳今日損益」的型別（`risk-control/src/lib.rs:266-299`），**再開第二個 instance、餵進 registry 加總出來的總權益就是帳戶層的版本**。一行原始碼都不用改。這是「重用型別、新增 instance」勝過「改 API」最強的一個例子。 |
| 單一幣種部位上限 / 總部位上限 | **不塞 `ProposedOrder`** | 這兩項需要 `symbol`，而 `ProposedOrder` 沒有 symbol 欄位。加上去會動到 6.3 的 doctest（`lib.rs:57-61`）與 ~8 個測試的建構點，**而單一 session 的閘門沒有任何一條規則需要 symbol**——加一個那一層用不到的欄位，是把成本付在錯的地方。新層用自己的 `GlobalOrder { symbol, side, quantity, reference_price }`，由送單點用同一批 local 變數建出來（多 3 行）。**刻意接受 3 個欄位的重複**，換「不動已審查過的穩定 API」。 |
| 價格偏離保護 | 新層 | 需要「下單參考價 vs 可信價（最新 tick）」兩個價格 + tick 的時間。6.3 的 `ProposedOrder` 只有一個價格，而且它刻意只做算術、不知道行情。 |
| 下單頻率自我上限 | 新層 | 需要時間窗內的送單計數（有狀態）+ 交易所限制值。6.3 是無狀態純函式，塞進去會破壞它「只有算數與比較」的性質。 |
| 嚴格模式 | 新層 | 它是個 modifier（把 Warn 升級成 Block），不是一條上限。 |
| 合約風控全部 | 新層，而且多數只建模型 | 見第 7 節。 |

**結論：`at-risk-control` 的 `AccountState` / `ProposedOrder` / `RiskLimits` / `Blocked`
四個型別完全不動，6.3 的 20 個測試與 doctest 完全不受影響。**

---

## 3. 決策一：新 crate `at-portfolio-risk`

```
crates/portfolio-risk/
  Cargo.toml        # 依賴：at-core（Fixed/Side/Symbol）、serde + serde_json（設定與紀錄的序列化）
  src/lib.rs        # GlobalLimits、GlobalBlocked、PortfolioGate trait
  src/breaker.rs    # BreakerRule / BreakerTrigger / BreakerSwitch / BreakerAction / BreakerState
  src/event.rs      # RiskEvent（觸發紀錄的型別，不含寫檔）
```

**為什麼是新 crate 而不是 App 層的邏輯**：跟 6.3 獨立成 crate 的理由一字不改
（`risk-control/src/lib.rs:3-9`）——(a) 這是政策不是 UI；(b) `Cargo.toml` 上看得見
送單路徑一定依賴它，少一條繞過閘門的暗路；(c) 可以在不開 Tauri、不連網路的情況下
用純單元測試驗完每一條規則與每一個邊界。

**為什麼可以收 `serde`**：6.3 不收是因為它零 I/O 且沒有設定檔；本 crate 的設定要
存成 JSON、觸發紀錄要存成 JSON，而 `serde` + `serde_json` **已經在 workspace 的
依賴樹裡**（`at-account-sync`、Tauri App 都在用），不是新引入的框架級依賴。
序列化的型別定義放在型別旁邊，比在 App 層再寫一份 DTO 少一份會不同步的平行結構。
`serde` 只負責型別轉字串；**讀寫檔案仍然在 App 層**，crate 本身沒有 `std::fs`。

---

## 4. 決策二：檢查順序與注入方式

### 4.1 順序：`RiskLimits::check` 先，`PortfolioGate::check` 後

```
send_gated_order:
  ① RiskLimits::check(&AccountState, &ProposedOrder)   // 6.3，完全不變
  ② PortfolioGate::check(&GlobalOrder, now_ms)         // 新增
  ③ place_market_order
```

兩道都必須回 `Ok` 才送單，所以**順序只影響「擋下時回報哪一個理由」，不影響安不安全**。
選這個順序的理由：

1. **對既有測試是零行為變動。** 6.4 所有斷言 `Blocked::*` 的測試，擋單理由與順序
   一字不變（因為第一道閘門先講話）。如果反過來，就得逐一重新論證每個測試的期望值。
2. **便宜的先跑。** 第一道是純算術；第二道可能要拿 registry 的鎖、檢查快照新鮮度。
3. **代價與補償**：帳戶已經被熔斷暫停、同時這筆單又超過單筆金額上限時，使用者看到的是
   「單筆金額超過上限」而不是「熔斷已觸發」。補償是：熔斷觸發**在觸發當下**就寫進
   觸發紀錄、並在 UI 顯示為橫幅狀態，使用者不是靠下一筆單的拒絕訊息才知道被暫停了。

### 4.2 注入方式：`trait PortfolioGate`，比照 `OrderGateway`

```rust
// at-portfolio-risk/src/lib.rs（型別草案，非實作）
pub trait PortfolioGate {
    /// 送單前的全域檢查。`now_ms` 由呼叫端提供（延續「不讀系統時鐘」）。
    fn check(&self, order: &GlobalOrder, now_ms: i64) -> Result<(), GlobalBlocked>;
}

pub struct GlobalOrder {
    pub session: SessionId,
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: Fixed,
    pub reference_price: Fixed,
}
```

`Trader::new` / `spawn` 多一個 `Arc<dyn PortfolioGate + Send + Sync>` 參數，
**不是 `Option`**：`Option` 等於在型別上開一條「沒有全域風控」的合法路徑，
正是 6.3 的 crate 邊界論證要防的東西。測試自己寫 3 行的 fake（`trader.rs` 的
`FakeExchange` 已經立下這個慣例），production 的唯一接線點在 App 層。

### 4.3 被否決的方案

| 方案 | 否決理由 |
|------|---------|
| **A. 把全域上限塞進 `RiskLimits`，`check` 多收一個 portfolio 參數** | 動到 6.3 的核心 API：20 個測試 + doctest 全部要改，而且會把「有狀態、要查 registry、要時間窗」的東西塞進一個刻意無狀態的純函式。6.3 的 crate 說明寫得很清楚它為什麼是那個形狀。 |
| **B. 用 decorator 包住 `OrderGateway`（`GatedGateway::place_market_order` 裡做全域檢查）** | **最誘人也最錯的那個「聰明」方案**：零簽名變動。但全域擋單會變成 `BinanceError` → `TradingError::PlaceFailed` → 使用者看到「訂單可能已經送達交易所，請到測試網後台確認實際部位」。那是一句謊話（什麼都沒送出去），而且 `PlaceFailed` 是**致命錯誤會停掉整個 session**。`OrderOutcome::Blocked`（非致命、明確沒送出）和 `PlaceFailed`（致命、下場不明）的區別是 6.4 設計的核心，不能為了省簽名變動而糊掉。 |
| **C. 把 gate 放成 `TestnetConfig` 的欄位** | `TestnetConfig` derive 了 `Debug, Clone`，`dyn Trait` 不是 `Debug`；要手刻 `Debug` 或給 trait 加 `Debug` bound。而且 5 處 struct literal（含 `lib.rs:58` 的 doctest）都要改，比加參數更吵。 |
| **D. 全域檢查放在 App 層，啟動時與每 N 秒檢查一次** | App 層看不到個別的單——單是 session 迴圈在 K 線收盤時自己產生的。做不到「這一筆會不會超過總部位上限」。 |

---

## 5. 決策三：熔斷規則的資料結構

### 5.1 要解的三個問題

1. 5 條規則的輸入型態完全不同（次數 / 時間 / 比率 / 布林 / 百分比）。
2. 要能開關、要能調參數 → 參數必須是**資料**，不是寫死的常數。
3. 「對帳不一致」強制開啟**不能靠 UI 約定**，要在資料結構上就關不掉。

### 5.2 設計：條件（trigger）／啟用（switch）／動作（action）三者分離

```rust
// at-portfolio-risk/src/breaker.rs（型別草案）

pub struct BreakerRule {
    pub trigger: BreakerTrigger,
    pub switch:  BreakerSwitch,
    pub action:  BreakerAction,
}

/// 條件 + 它自己的參數。閉集合、exhaustive match、可序列化。
pub enum BreakerTrigger {
    /// 連續虧損 N 筆 → 暫停策略（設計稿預設 5）
    ConsecutiveLosses { count: u32 },
    /// 行情中斷超過 N 毫秒（設計稿 3 秒）
    MarketStalled { ms: i64 },
    /// 時間窗內的拒絕率超過門檻（設計稿 1 分鐘 / 20%）
    RejectRate { window_ms: i64, ratio: Fixed },
    /// 對帳不一致。沒有參數——「不一致」就是不一致。
    ReconciliationMismatch,
    /// 單一策略回撤超過上限（相對該 session 的權益高點）
    StrategyDrawdown { ratio: Fixed },
}

/// 啟用狀態。**關鍵：`Mandatory` 沒有 payload，所以「關掉」在型別上不存在。**
pub enum BreakerSwitch {
    Mandatory,
    Optional(bool),
}

/// 觸發後做什麼。範圍（單一策略 / 全帳戶）由動作本身表達，不另開 scope 欄位。
pub enum BreakerAction {
    /// 暫停這一個策略（session），其他策略照跑。
    PauseStrategy,
    /// 暫停整個帳戶的下單（= 等效於自動按下 kill switch）。
    PauseAllOrders,
    /// 停止這個策略並撤單（目前只有市價單 → 撤單是 no-op，見 6.3）。
    StopAndCancel,
}
```

**「強制開啟」的兩道保證**（兩道都要，缺一就能被繞過）：

1. **型別層**：`BreakerSwitch::Mandatory` 是無 payload 的 variant，
   `is_armed()` 對它永遠回 `true`。沒有任何值能表達「強制規則被關閉」。
2. **集合層**：規則集只能用 `BreakerRules::new(optional: Vec<BreakerRule>)` 建立，
   建構子**永遠把強制規則加進去**。所以「手動改 JSON 把對帳不一致那條刪掉」也沒用
   ——反序列化走同一個建構子，少掉的強制規則會被補回來。
   這一條比第 1 條重要：型別層防得住「設成 false」，集合層防得住「整條刪掉」。

設計稿點名強制開啟的有兩項：**對帳不一致→暫停下單**、**交易所端停損**。
前者是本層的 `Mandatory` 規則；後者是合約下單參數，沒有合約路徑可以掛（見 7.2）。

### 5.3 評估機制：一個觀測結構 + 一個評估點

讓這件事「不是每條規則一個 if-else 散落在送單路徑上」的，不是 enum 本身，而是
**五條規則都是同一個觀測結構的函式**：

```rust
/// 評估熔斷用的一次觀測。全部由 App 層的累加器算好餵進來。
pub struct BreakerObservation {
    pub now_ms: i64,
    /// 最後一次收到行情事件的時間（交易所時間）。`None` = 還沒收到過任何行情。
    pub last_market_event_ms: Option<i64>,
    /// 各 session 的連續虧損筆數。
    pub consecutive_losses: /* per-session */ u32,
    /// 時間窗內的 (送出筆數, 被交易所拒絕筆數)。
    pub recent_orders: (u32, u32),
    /// 相對權益高點的回撤比例。`None` = 算不出來 → fail closed。
    pub drawdown: Option<Fixed>,
    /// 對帳結果。`Unknown` 跟 `Mismatch` 一樣擋。
    pub reconciliation: Reconciliation, // Ok | Mismatch | Unknown
}
```

`BreakerRules::evaluate(&self, obs) -> Vec<BreakerTrip>`：一個 `match self.trigger`，
每個 arm 一次比較。設計上買到的是四件事，不是「消滅 match」：
(a) 唯一一個評估點；(b) 參數是資料、可從 JSON 調；(c) 啟用狀態是資料、而且強制的關不掉；
(d) 五條規則吐出**同一個形狀**的 `BreakerTrip`，所以觸發紀錄、UI 列表、
暫停動作的套用都只要寫一份。

**為什麼不用 `Box<dyn BreakerRule>`**：5 條封閉的規則、輸入相同、沒有第三方擴充需求。
trait object 換來的「可擴充性」是沒人要的；換掉的是 exhaustive match（加第六條規則時
編譯器會逼你處理每一個地方）、免費的 `Serialize`/`Deserialize`、以及不用 mock 的測試。
這裡 enum 是比較懶也比較對的選擇。

### 5.4 累加器放哪：App 層，不是引擎層

連續虧損筆數、拒絕率的時間窗、權益高點——這些都是**從事件流累加出來的狀態**。
它們放在 App 層，因為 App 層**已經**在收 `TestnetUpdate`（`Order(OrderOutcome)` /
`Bar(TestnetSnapshot)`，`crates/testnet-trading/src/lib.rs:208-219`），拿到的資訊剛好夠：

- 拒絕筆數 ← `OrderOutcome::Filled(FillReport { status: Rejected, .. })`
- 送出筆數 ← 每一則 `OrderOutcome::Filled`
- 權益與高點 ← `TestnetSnapshot::point.equity`
- 行情中斷 ← 最後一則 `Bar(..)` 的 `point.open_time`（**交易所時間**，不是本機時鐘）

於是分工是：**檢查在 `Trader` 裡（必須在送單路徑上）、累加與記錄在 App 層（已經有事件流）**，
中間用 `Arc<Mutex<BreakerState>>` 共享，而 `Trader` 只認得 `PortfolioGate` trait。
引擎 crate 不多一個 trait 方法、不多碰一次 I/O。

### 5.5 「連續虧損 5 筆」的定義問題（實作前必須先定下來）

**現在的帳本算不出「一筆交易的損益」。** `Trader` 只維護 `cash` 與 `position`
（`trader.rs:113-114`），沒有成本基礎（cost basis），所以沒辦法對單一賣單說
「這一筆賺還是虧」。

三個選項：

| 選項 | 代價 |
|------|------|
| **a. 用「回到空手」當一筆交易的邊界**：一筆交易的損益 = `equity(現在) − equity(上次部位為 0 時)` | **推薦。** 只需要 App 層已經收到的權益序列，零帳本改動，對「單一交易對的現貨來回」是精確的。限制：策略如果永遠不回到空手，就永遠不會累積「筆數」——要在 UI 上說明這一點（顯示「本 session 已完成 N 筆」）。 |
| b. 在 `Trader` 加成本基礎追蹤 | 動 6.4 已審查過的帳本邏輯（`apply_fill`），為了一條熔斷規則去改全專案最敏感的那段程式碼，風險不對價。 |
| c. 用「每一根 K 線權益下降」近似 | 定義和設計稿的「筆」差太遠，會在不該觸發的時候觸發，使用者會不信任熔斷。 |

**決策：選 a**，並在 ADR 與 UI 上寫清楚「一筆 = 一次從空手到空手的來回」。

### 5.6 觸發後怎麼解除

設計稿沒說。**決策：觸發狀態是黏著的（sticky），只有使用者手動清除。**
理由和一鍵停止同一個：自動恢復代表系統要自己判斷「問題已經解決了」，而會誤判的
系統剛好就是被熔斷的那個。對帳不一致尤其不能自動恢復。

唯一值得討論的例外是「行情中斷」——行情重連之後自動恢復很誘人。但中斷期間的
本地帳本可能已經和交易所不一致（6.4 的 `OrderNotFinal` 就是這個情況），
自動恢復等於假設「斷線沒有造成任何遺漏」。**不實作自動恢復**，記錄在此。

---

## 6. 決策四：「一鍵停止兩種行為」

### 6.1 判斷：這**不是**擴充現有 kill switch，是疊在它之上的另一個機制

| | 現有 `set_kill_switch` | 設計稿的「一鍵停止行為」 |
|---|---|---|
| 性質 | **狀態**（旗標，可反覆開關） | **程序**（按一次、跑一遍、有順序） |
| 對 session 的影響 | 不停 session，行情與快照照跑（`lib.rs:232-234`） | 明確要「停止」 |
| 對既有部位 | 什麼都不做（連平倉單都擋） | 選項一保留部位、選項二市價平倉 |
| 可逆 | 設成 `false` 就恢復 | 跑完就是跑完了 |

兩者語意不同層級，**硬塞進同一個 bool 會把兩件事都弄壞**。所以：

- `TestnetTradingHandle::set_kill_switch` 的語意、簽名、測試**一字不改**。
- 新增 App 層的緊急處理程序 `EmergencyAction`，它**使用** kill switch 當第一步。

```rust
pub enum EmergencyAction {
    /// 設計稿的預設：停止並撤銷掛單、保留部位。
    StopAndCancel,
    /// 設計稿的另一選項：停止、撤單並市價平倉。**目前不實作**，見 6.3。
    StopCancelAndFlatten,
}
```

程序（順序本身就是 fail-closed 的設計）：

1. `set_kill_switch(true)` —— 先讓後面的步驟期間「一張新單都不會出去」。
   這裡正好用到它「連平倉都擋」的性質：程序進行中不該有任何自發的單。
2. `stop()` —— 停掉 session 迴圈。
3. 撤銷掛單 —— **目前是 no-op**（見 6.3）。
4. （僅 `StopCancelAndFlatten`）市價平倉 —— **目前不實作**。
5. 寫一筆 `RiskEvent`（含選了哪個動作、每一步的結果）。

### 6.2 為什麼不給平倉開 kill switch 的洞

第 4 步和第 1 步直接衝突：kill switch 開著的時候，平倉單也送不出去。
三條路：

- **(a) 讓 kill switch 放行「平倉」單。** 直接推翻 6.3 的核心論證
  （「閘門不該相信下單方自己對『這是平倉』的分類；會把加碼誤判成平倉的程式，
  剛好就是最需要按下停止鍵的那個程式」）。**否決。**
- **(b) 平倉排在開 kill switch 之前**，走正常閘門送出去，之後才開旗標。
  技術上可行，但平倉單要過 `max_order_notional`——而
  `notional_cap_applies_to_closing_orders_too`（`risk-control/src/lib.rs:477`）
  證明大額平倉**會被擋下**。也就是「緊急平倉」可能在最需要的時候失敗。
  要做的話必須拆成多筆、處理部分成交、處理交易所拒絕——那是一條全新的送單邏輯。
- **(c) 現在不做平倉，只保留型別與 UI 位置。**

### 6.3 決策：`StopAndCancel` 做，`StopCancelAndFlatten` 只建模型並標注未實作

誠實評估的結論，三個理由：

1. **撤單在目前範圍內真的是 no-op。** 只有市價單，市價單送出去就結束了，沒有掛單可撤
   （6.2 文件已記錄，`OrderGateway` 刻意沒有 cancel 方法）。所以選項一的「撤單」
   和選項二的「撤單」在今天完全沒有差別——兩個選項唯一真正的差異就是平倉。
2. **市價平倉是一條新的送單程式碼路徑**，而這個 codebase 裡風險最高的改動種類就是
   「新增一條會送單的路徑」。
3. **它自動化的，剛好是 6.3 明確決定要留給人工的事**：
   「既有部位請用交易所自己的 App 手動處理——那條路不依賴這份程式碼正確」。
   在沒有新需求的情況下，先去推翻一個已經接受的決定，方向是錯的。

**具體做法**：UI 上兩個選項都顯示，選項二是 `disabled` + 說明文字
「需要撤單與平倉路徑；目前只支援市價單，尚未實作」（遵守「不放假按鈕」的規則）。
後端若收到 `StopCancelAndFlatten` 回明確錯誤，不要默默降級成選項一
——默默做了另一件事比拒絕更糟。

### 6.4 失聯保護（斷線 >10 秒交易所自動撤合約掛單）

這是**交易所端**的功能（Binance 合約的「斷線自動撤單」倒數端點），不是本機檢查。
需要合約下單權限與合約 API 才能設定。**只建模型**：
`GlobalLimits.disconnect_protection_seconds: Option<u32>`，標注「需要合約下單路徑（第 7 步之後）」。
不要在本機寫一個「偵測斷線然後自己撤單」的替代品——本機版在斷線時正好也連不上交易所，
那正是交易所端功能存在的理由。

---

## 7. 合約風控：哪些檢查真的做、哪些只建模型

### 7.1 前提

下單路徑今天是**現貨市價單**。沒有任何程式碼會設槓桿、設保證金模式、讀保證金率、
讀強平價、讀資金費。2.5 的合約數學只存在於回測引擎。

### 7.2 逐項

| 項目 | 做法 |
|------|------|
| 槓桿上限（預設 2 倍，超過拒絕下單） | **做，但在部署時檢查，不在送單時檢查。** 槓桿是部署參數（GoLive 頁選的），不是每筆單的屬性。設計稿第 55 行也是在調參頁標示「>2x 標示超過風控上限」。送單時沒有「這筆單的槓桿」這種東西。 |
| 保證金模式（逐倉／全倉） | **只存設定。** 本機沒有東西可以檢查，要送到交易所才生效。 |
| 強平距離下限（低於拒絕加倉、低於一半自動減倉） | **只建模型 + 標注未實作。** 需要保證金率，而保證金率要查合約帳戶端點。另外「自動減倉」違反 6.3 的「不自動強制平倉」原則，等真的有合約路徑時要另開一份 ADR 重新論證。 |
| 資金費率預算（超過警示並減倉） | **只建模型 + 標注未實作。**「警示」可以做（那只是一則 `RiskEvent`），「減倉」同上。 |
| 交易所端停損（強制開啟） | **只存設定、標注為下單參數。** 它是下合約單時帶的參數，不是本機閘門。在資料結構上標成強制，等合約路徑接上時由送單點讀取。 |
| 套利淨曝險上限 | **不建模型。** 這個專案沒有套利策略、沒有多腿訂單、沒有「一組腿」的概念。為一個不存在的策略型態留欄位是純粹的投機性複雜度。等真的做套利時再加。 |

**這是本 ADR 最大的一刀 YAGNI，必須明說**：合約風控六項裡，只有一項（槓桿上限）
今天有地方可以執行；四項只存設定／標注未實作；一項不做。
Risk 頁可以把它們都顯示出來（使用者想先填好設定是合理的），但**不能讓它們看起來
已經在保護他**——未生效的項目要在 UI 上明確標成「需要合約交易（第 7 步之後）」。

---

## 8. 決策五 & 六：資料模型、觸發紀錄、跨策略加總

### 8.1 全域上限

```rust
pub struct GlobalLimits {
    /// 單一幣種部位上限（以報價幣計價的名目價值）。
    pub max_symbol_exposure: Fixed,
    /// 總部位上限（所有幣種的**毛**曝險加總，見 8.2）。
    pub max_total_exposure: Fixed,
    /// 帳戶層單日虧損上限（正數）。餵給第二個 `DailyPnl` instance。
    pub max_daily_loss: Fixed,
    /// 價格偏離保護：下單參考價與最新可信價的最大容許偏離比例。
    pub max_price_deviation: Fixed,
    /// 下單頻率自我上限：交易所限制的百分比 + 交易所限制本身。
    pub order_rate: OrderRateLimit,
    /// 快照新鮮度預算。超過就擋（預設 3000ms，和行情中斷熔斷同一個量級）。
    pub max_snapshot_age_ms: i64,
    /// 嚴格模式：把可警示的項目升級成硬擋。
    pub strict_mode: bool,
    /// 合約設定（多數只存不執行，見第 7 節）。
    pub futures: FuturesRiskSettings,
    /// 失聯保護秒數（交易所端功能，目前無路徑可設）。
    pub disconnect_protection_seconds: Option<u32>,
}
```

和 6.3 的關係：**「單筆金額上限」不在這裡**——它是 `RiskLimits::max_order_notional`。
Risk 頁的那個欄位寫進設定檔，啟動 session 時餵進 `RiskLimits::new`。一個數字一個主人。

`GlobalBlocked` 是一個獨立 enum（`SymbolExposureExceeded` / `TotalExposureExceeded` /
`DailyLossReached` / `PriceDeviation` / `RateLimited` / `BreakerTripped(BreakerTrigger)` /
`PortfolioUnknown` / `SnapshotStale` / `Overflow`），全部實作 `Display` 回繁體中文，
比照 `Blocked` 的做法（錯誤訊息可直接顯示給使用者）。

### 8.2 跨策略加總：毛曝險 vs 淨曝險

- **單一幣種部位上限 → 用「淨」。** 兩個策略同時做多 BTC 是 2 倍曝險；一個多一個空
  在同一個交易對上是真的互相抵銷。
- **總部位上限 → 用「毛」（各幣種淨曝險取絕對值再相加）。** BTC 多單和 ETH 空單
  互相抵銷是假的——它們是兩個獨立的風險。把它們淨掉會讓「總部位上限」在
  最分散（也最容易一起爆）的組合下形同失效。

檢查的是「**這筆單成交之後**的曝險」：把這筆單的帶號 delta 加到該交易對的淨部位上再比較。

### 8.3 對 session registry 的介面契約（草案，待對齊）

```rust
/// 本引擎需要 registry 提供的全部資訊。一次呼叫、一次鎖。
pub trait PortfolioView {
    fn snapshot(&self) -> Option<PortfolioSnapshot>;
}

pub struct PortfolioSnapshot {
    /// 快照時間（交易所時間優先）。用來判斷新鮮度。
    pub taken_at_ms: i64,
    pub sessions: Vec<SessionExposure>,
}

pub struct SessionExposure {
    pub session: SessionId,
    pub symbol: Symbol,
    /// 帶號部位數量（正多負空）。
    pub position: Fixed,
    /// 評價用的參考價。
    pub mark_price: Fixed,
    /// 該 session 的權益。`None` = 算不出來。
    pub equity: Option<Fixed>,
    /// 已送出、還沒拿到終態的數量（帶號）。見 12.3。
    pub in_flight: Fixed,
}
```

**fail-closed 規則（這是整個加總設計裡最重要的一段）：**

| 情況 | 行為 |
|------|------|
| `snapshot()` 回 `None` | 擋下所有下單（`PortfolioUnknown`），對應 6.3 的 `DailyPnlUnknown`。 |
| `taken_at_ms` 比 `now_ms − max_snapshot_age_ms` 舊 | 擋（`SnapshotStale`）。 |
| 任何一個 session 的 `equity` 是 `None` | 擋。**不可以當成 0 跳過那個 session**——漏掉一個 session 會讓加總**低估**曝險，而低估是危險的那一邊。 |
| `position × mark_price` 溢位 | 擋（`Overflow`）。 |
| 快照裡有 session 但 registry 說不確定清單是否完整 | 擋。寧可擋一筆合法的單。 |

**需要和 session registry 那份設計對齊的三件事**（本文無法單方面決定）：

1. `SessionId` 的型別與生成方式——**應該由 registry 定義，本 crate 引用**，不要兩邊各自定義。
2. `in_flight` 欄位是否存在。如果 registry 不提供，就有一個 under-count 的競態（見 12.3）。
3. 持久化位置：本引擎只要求「和 registry 用同一個 `app_data_dir`、用同一個 `SessionId`」。
   **不共用儲存層**——兩份互相獨立的清單共用一個抽象，是為了對稱而付的成本。
   如果 registry 那邊剛好做了一個通用的 append-only JSON 檔工具，那就重用它。

### 8.4 觸發紀錄

```rust
pub struct RiskEvent {
    /// 單調遞增的序號（同一毫秒內多筆也能排序）。
    pub seq: u64,
    pub at_ms: i64,
    /// 這個時間戳是交易所時間還是本機時間。混在一起而不說，之後查問題會被誤導。
    pub clock: ClockSource,       // Exchange | Local
    pub session: Option<SessionId>,
    pub symbol: Option<Symbol>,
    pub cause: RiskEventCause,
}

pub enum RiskEventCause {
    /// 全域閘門擋下一筆單。
    GlobalBlock(GlobalBlocked),
    /// session 內閘門擋下一筆單（重用 6.3 的 `Blocked`，不另定一套理由）。
    SessionBlock(/* at_risk_control::Blocked 的序列化鏡像 */),
    /// 熔斷觸發，以及套用的動作。
    BreakerTrip { trigger: BreakerTrigger, action: BreakerAction },
    /// 使用者按了緊急處理，以及每一步的結果。
    Emergency { action: EmergencyAction, steps: Vec<StepResult> },
    /// 一鍵停止被開啟／關閉。
    KillSwitch { on: bool },
    /// 風控設定被修改（只記「哪一項」，不記完整設定快照）。
    LimitsChanged { field: &'static str },
}
```

**不存 `detail_zh: String`。** `Blocked` 已經有 `Display`、新的型別也會有；
顯示字串在渲染時產生。存結構化資料才能篩選、才能寫測試。
代價：之後改了 `Display` 的字句，舊紀錄讀起來會跟著變——對一個本機工具可以接受，記錄在此。

**存哪裡**：`<app_data_dir>/risk_events.json`，照 `at-account-sync` 的既有慣例
（`serde_json` + `to_string_pretty` + best-effort 寫入）。環形上限 **500 筆**，
超過丟最舊的。

- **為什麼不是 SQLite／sled**：引入新資料庫是 🔴 框架級依賴，而需求是一個 500 筆上限、
  只會「附加一筆」和「全部讀出來顯示」的清單。整檔重寫 500 筆 JSON 的成本可以忽略。
- **為什麼不是 append-only 的 JSON Lines**：環形上限需要刪最舊的，
  JSON Lines 要另外做截斷邏輯。整檔重寫一個 `Vec` 更短。
  （如果 registry 那邊已經做了 JSON Lines + 截斷，重用它，不要堅持自己這份。）
- **何時寫**：每產生一則就立刻寫，不要等關閉時再寫。
  「對帳不一致」這種紀錄必須在 App 可能死掉之前就落地。
- **誰寫**：**App 層。** `at-portfolio-risk` 只**回傳** `Vec<RiskEvent>`，
  不碰 `std::fs`，延續 6.3「沒有網路、沒有鑰匙圈、沒有時鐘」的性質。

### 8.5 風控設定的持久化

`<app_data_dir>/risk_settings.json`，內容是 `GlobalLimits` + `BreakerRules` +
單筆金額上限（餵給 `RiskLimits`）。

- 讀不到 / 解析失敗 → 用一組保守的預設值，並記一筆 `RiskEvent`。
  **不可以**「解析失敗就當沒有限制」。
- 反序列化後一律經過 `BreakerRules::new`，強制規則被補回（見 5.2）。
- 設定檔裡**不可以有任何 API 金鑰相關的東西**（金鑰在 Keychain，見 CLAUDE.md 安全規則）。

---

## 9. 對現有程式碼的影響範圍

### 9.1 不受影響（零原始碼改動）

| crate | 說明 |
|-------|------|
| `at-core` | 零改動。新 crate 只用它的 `Fixed` / `Side` / `Symbol`。 |
| **`at-risk-control`** | **零改動。6.3 的 20 個測試與 doctest 全部不受影響。** |
| `at-binance` / `at-market-stream` / `at-account-sync` / `at-paper-trading` / `at-engine` / `at-downloader` / `at-secret-store` | 零改動。 |

### 9.2 會被改到的地方（都已核對過實際位置）

| 檔案 | 改動 | 會不會動到已通過的測試 |
|------|------|----------------------|
| `Cargo.toml`（workspace） | `members` 加 `crates/portfolio-risk` | 不會 |
| `crates/testnet-trading/Cargo.toml` | 加依賴 | 不會 |
| `crates/testnet-trading/src/trader.rs:102-119`（`Trader` struct） | 多一個 `gate` 欄位 | 編譯層，不改斷言 |
| `crates/testnet-trading/src/trader.rs:122`（`Trader::new`） | 多一個參數 | **會**：3 個測試呼叫點（`trader()` helper at :665、:682、:697）要加一個 fake 參數。**斷言一字不改。** |
| `crates/testnet-trading/src/trader.rs:297`（`send_gated_order`） | `RiskLimits::check` 之後加第二道閘門 | 現有擋單測試全部仍通過（第一道先講話，見 4.1）。**需要新增**「全域上限擋下」的測試。 |
| `crates/testnet-trading/src/trader.rs:50-58`（`OrderOutcome`） | 需要表達「被全域閘門擋下」。**建議新增 variant `GloballyBlocked(GlobalBlocked)`**，不要把它塞進既有的 `Blocked(..)`——那會讓「哪一道閘門擋的」消失，而觸發紀錄要這個資訊 | **會**：`OrderOutcome` 的 `match` 不再 exhaustive。實際影響：`app/src-tauri/src/testnet_trading.rs` 的 DTO 轉換 + 前端 `testnetTradingTypes.ts` 的 union type。這是**刻意**的編譯錯誤——新增一種結果就該逼每個消費端處理。 |
| `crates/testnet-trading/src/lib.rs:280`（`spawn`） | 多一個參數 | **會**：doctest（`lib.rs:58` 與 `:79` 的範例）要補幾行，以及 `lib.rs:421`、`:424`、`:738` 的測試建構點。`cargo test` 會抓到 doctest 失敗，所以不會被漏掉。 |
| `app/src-tauri/src/testnet_trading.rs:366` | 建 `TestnetConfig` 並接線真實的 gate | 該檔測試的 `TestnetConfig` 建構點要跟著改 |
| `app/src-tauri/src/` 新增 `risk.rs` | 風控設定讀寫、熔斷累加器、觸發紀錄讀寫、緊急處理指令 | 新檔，不會 |
| `app/src/` 新增 Risk 頁 + `SideNav` / `nav.ts` 加一頁 | 前端 | `app/src/App.test.tsx`、`SideNav` 相關測試會因為多一頁而需要更新（既有測試可能斷言分頁數量／清單） |
| `app/src/TestnetTrading.tsx:441-445` 的 3 個風控欄位 | 改成唯讀顯示 + 「到風控頁調整」，設定來源變成設定檔 | **會**：`TestnetTrading.integration.test.tsx:70-71` 餵 `maxDailyLoss`/`maxOrderNotional` 的測試要改 |
| `docs/ROADMAP.md` | 新增子步驟 | 不會 |

### 9.3 影響範圍的總結論

- **6.3（`at-risk-control`）**：完全不受影響。這是本設計最主要的目標，達成了。
- **6.4（`at-testnet-trading`）**：**會被改到**，但改動的性質是
  「加一個參數、加一個 enum variant」——**沒有一條既有斷言的期望值需要改變**，
  只有建構點要補參數、`match` 要補 arm。所有既有的風控／帳本／輪詢行為測試照原樣通過。
- **6.5（App 層 + 前端）**：改動最大的一塊，因為風控設定的**所有權**從表單搬到設定檔。
  這是設計稿要求的（獨立風控頁）必然後果，不是本設計多做的。

**建議的落地順序**（每一步都能獨立通過 `cargo test` + `npm test`，照專案「一次一個子步驟」的規矩）：

1. 新 crate `at-portfolio-risk`：`GlobalLimits` + `GlobalBlocked` + `PortfolioGate` trait，
   先只做「上限比較」這一半，registry 用 trait 注入、測試用 fake 快照。
2. 熔斷規則資料結構 + `evaluate`（純單元測試，不碰任何 session）。
3. 接進 `send_gated_order`（第二道閘門）+ `OrderOutcome` 新 variant。
4. App 層：設定檔讀寫 + 觸發紀錄讀寫。
5. Risk 頁前端（含「未實作」項目的明確標示）。
6. 緊急處理 `StopAndCancel`。
7. （等 session registry 落地後）接真實的 `PortfolioView`，單一幣種／總部位上限才真的生效。

**步驟 1–6 在單一 session 下就有價值**（單日虧損、價格偏離、下單頻率、熔斷、觸發紀錄
都不需要 registry）。只有「單一幣種部位上限」和「總部位上限」需要等 registry。
這代表這份設計**不被 registry 阻擋**。

---

## 10. 魔鬼代言人挑戰與回應

### 10.1 撐得住的部分（誠實地說，不是為了湊數）

| 挑戰 | 回應 |
|------|------|
| **「流量 100 倍？」** 撐得住，而且這個問題在這裡沒什麼意義。檢查發生在 K 線收盤，1 分鐘 K 線 = 每分鐘幾次整數比較。即使 100 個 session × 每秒一次，也是幾千次 `i64` 比較。真正的瓶頸會是 registry 快照的鎖競爭，而那是 registry 的設計問題，不是本層的。 |
| **「單人維護？」** 撐得住。沒有新的框架級依賴（`serde`/`serde_json` 已在樹裡）、沒有資料庫、沒有背景執行緒（累加器跑在 App 已有的事件處理裡）、沒有 trait object 的動態分派迷宮（只有一個 `PortfolioGate` 和一個 `PortfolioView`，都比照既有的 `OrderGateway` 慣例）。熔斷規則是閉集合 enum，加第六條規則時編譯器會指出每一個要改的地方。 |
| **「資料量 10 倍？」** 撐得住。觸發紀錄有 500 筆硬上限；設定檔是固定大小；快照是 O(session 數)。 |
| **「回滾？」** 撐得住。步驟 1–2 是純新增（刪掉 crate 即可回滾）。步驟 3 的回滾是「把第二道閘門的呼叫拿掉」——因為兩道閘門是並列而非嵌套，拿掉一道不會破壞另一道。這是 2.2 節選「並列」而非「wrapper」的額外好處。 |
| **「被攻擊面？」** 撐得住。本 crate 不連網路、不讀金鑰、不執行外部程式。唯一的外部輸入是兩個 JSON 檔（使用者自己機器上的），而它們的解析失敗路徑是 fail-closed（用保守預設值、記一筆事件），且強制熔斷規則經由建構子補回，手改 JSON 關不掉。 |

### 10.2 真正的弱點（必須記錄）

| 挑戰 | 回應／緩解 |
|------|-----------|
| **「registry 掛了呢？」** 擋下所有下單（`PortfolioUnknown`）。這是正確的失效模式，但副作用要講明：**registry 變成送單路徑上的硬依賴**。registry 的一個 bug 會表現為「什麼單都送不出去」。緩解：`max_snapshot_age_ms` 可調；UI 必須把「因為查不到全域部位而擋單」顯示成一個清楚的狀態（不是一堆看不懂的擋單紀錄），讓使用者知道該修的是 registry 不是策略。 |
| **「兩個 session 同時下單呢？」** 見 12.3——這是本設計目前最真實的漏洞。 |
| **「資料不一致呢？」** 「對帳不一致」被列為強制熔斷，但**誰來做對帳、怎麼做**本文沒有設計。目前 `Trader` 的 `OrderNotFinal` / `StoppedWhileWaiting` 已經是「本地帳本可能和交易所不一致」的明確訊號，可以當對帳不一致的第一個來源；真正的「查交易所部位 vs 本地帳本」需要一個查部位的端點，那是另一份設計。**不要假裝這條熔斷規則現在就有完整的輸入。** 見 12.1。 |
| **「嚴格模式到底做什麼？」** 設計稿只說它會擋住部署、且 Risk 頁有這個開關。本設計把它窄化成「把可警示項目升級成硬擋」。這是一個**猜測**，需要 CEO 確認（見 12.4）。 |
| **「熔斷自己誤觸發呢？」** 誤觸發 = 策略被不必要地停掉，代價是錯過行情，不是賠錢。可接受的方向。但「連續虧損」的定義（5.5）如果使用者理解成別的意思，會導致不信任。所以 UI 必須把定義寫在旁邊。 |

---

## 11. 不做什麼（明確的 YAGNI 清單）

1. **套利淨曝險上限**——沒有套利策略、沒有多腿訂單的概念。
2. **自動減倉**（強平距離／資金費預算觸發的）——違反 6.3 的既有決定，且沒有合約路徑。
3. **市價平倉**（一鍵停止的選項二）——建型別、不實作，見 6.3。
4. **本機版的失聯自動撤單**——交易所端功能的本機替代品在斷線時正好也不能用。
5. **熔斷規則的 trait object / plugin 機制**——5 條封閉規則，enum 就夠。
6. **熔斷自動恢復**——見 5.6。
7. **資料庫**——500 筆上限的清單不需要。
8. **成本基礎（cost basis）追蹤**——用「回到空手」的權益差替代，見 5.5。
9. **第二份「單筆金額上限」**——重用 `RiskLimits::max_order_notional`。
10. **把全域風控做成可以關閉**（`Option<gate>`）——那是一條繞過閘門的暗路。

---

## 12. 未決問題（需要決定才能開工）

### 12.1 「對帳不一致」的輸入從哪來？（阻擋性）

這條規則是強制開啟的，但目前沒有一個明確的「對帳」動作。
建議的最小版本：把 `TradingError::OrderNotFinal` 與 `StoppedWhileWaiting`
當成「不一致」的訊號（這兩個錯誤的 doc 已經明寫「本地帳本可能和交易所不一致」），
真正的「查交易所部位比對本地帳本」留給之後（需要一個查部位的端點，`OrderGateway`
目前沒有）。**需要 CEO／架構確認這個窄化版本可以接受**，否則這條強制規則會是
一個永遠不會觸發的裝飾品——那比沒有它更糟。

### 12.2 session registry 的介面（阻擋步驟 7，不阻擋 1–6）

8.3 的契約是單方面草案。需要對齊：`SessionId` 型別歸屬、`in_flight` 是否提供、
快照是否能保證「清單完整」。**`SessionId` 應該由 registry 定義，本 crate 引用。**

### 12.3 跨 session 的在途訂單競態（真實漏洞，需要決定接受或修）

兩個 session 同時在 K 線收盤送單時，各自看到的快照都還沒包含對方那筆在途的單，
於是總部位可能**同時**被兩筆單推過上限。超額的上界是「每個 session 一筆最大金額的單」。

三個選項：
(a) registry 在快照裡回報 `in_flight`（推薦，成本在 registry 那邊）；
(b) 全域引擎自己做「預約」：檢查通過時先把這筆單的曝險記上，拿到終態後沖銷
（引擎變有狀態，而且要處理「永遠拿不到終態」的沖銷洩漏）；
(c) 接受超額，把上界寫進文件，把全域上限設得保守一點。

今天只有一個 session，所以這個競態**現在不存在**。但它會在 registry 落地的同一天出現。
**建議 (a)**，並在 registry 設計裡要求這個欄位。

### 12.4 嚴格模式的確切語意

設計稿只說「嚴格模式會擋住部署」。本設計額外假設它會「把警示升級成硬擋」。
需要確認：嚴格模式是只管 GoLive 頁的部署前檢查，還是也改變執行期的行為？

### 12.5 全域單日虧損 vs session 單日虧損的互動

兩層都有「單日虧損上限」，兩層都用 UTC 日界。**不是重複**（一個管單一策略、
一個管整個帳戶），但兩個數字在 UI 上要分得很清楚，否則使用者會以為設了一個就夠。
建議：Risk 頁放帳戶層的，GoLive／部署設定放單一策略的（設計稿第 90 行的
「單日虧損上限」出現在部署設定裡，支持這個分法）。

### 12.6 多 session 時「一鍵停止」的範圍

現在 kill switch 是一個 session 一個旗標。多 session 之後，Risk 頁的一鍵停止
顯然應該是**全帳戶**的。需要決定：是一個全域旗標 + 每個 session 一個，
還是只保留全域的？建議前者（熔斷的 `PauseStrategy` 需要 session 層級的旗標），
但這要和 session registry 的設計一起定。

---

## 13. 後果

### 13.1 好的

- 6.3 的程式碼與測試完全不動，6.4 只有機械性的參數／variant 改動。
- 全域風控可以在 session registry 完成之前就開始做（步驟 1–6）。
- 熔斷規則的參數與開關是資料，可調、可存、可測；強制規則在型別與建構子兩層都關不掉。
- 風控設定終於會持久化（目前關掉 App 就沒了）。
- 引擎層仍然零 I/O、零時鐘，所以每條規則都能用純單元測試驗到邊界。

### 13.2 要承受的

- `send_gated_order` 從一道閘門變兩道。它是全專案最敏感的函式，每次改動都該
  要求跨模型審查（6.3 已有這個先例）。
- Risk 頁會顯示一批**還沒生效**的合約設定。如果 UI 沒把「未生效」標清楚，
  使用者會以為自己受到保護——這是本設計最可能造成實際傷害的地方，
  比任何技術風險都嚴重。
- registry 成為送單路徑的硬依賴（fail-closed 的必然代價）。
- 「一鍵停止兩種行為」只做一種，設計稿與實作之間留著一個明確、有文件的落差。
