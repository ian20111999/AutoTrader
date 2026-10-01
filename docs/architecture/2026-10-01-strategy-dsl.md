# ADR-001：使用者自訂策略的中介表示（Strategy DSL）與執行引擎

- **狀態**：Proposed（待 CEO 拍板；其中「at-core 加 serde 依賴」一項需要明確同意）
- **日期**：2026-10-01
- **決策者**：架構研發部
- **影響的 crate**：`at-core`（新增模組）、`app/src-tauri`（新增 command）、前端（新頁面）
- **不影響**：`at-engine`、`at-paper-trading`、`at-testnet-trading`、`at-risk-control`、`at-binance-client`

---

## 1. 背景

### 1.1 落差

設計稿的「策略編輯器」（`docs/design-reference.md` 第 43–51 行）要的是**使用者自己拖積木
組合策略條件**。現況是 `at-core::strategies` 裡四個寫死的 Rust 型別（`SmaCross`、
`Bollinger`、`Donchian`、`Rsi`），使用者只能改數值參數——`app/src-tauri/src/strategies.rs`
把它們的 `DEFAULT_*` 常數包成一組 `{key, label, kind, default}` 的輸入框 schema，
`app/src-tauri/src/backtest.rs:64` 的 `build_strategy()` 再用 `match strategy_id` 把
字串參數還原成那四個型別之一。

也就是說：**策略的「結構」寫在 Rust 裡，使用者只能動葉子上的數字。** 設計稿要的是
使用者能動結構。

### 1.2 不可妥協的前提

專案從 1.x 就有一條核心原則，寫在 `crates/core/src/strategy.rs` 的模組註解裡：

> 回測（2.2）、成交模擬（2.3）、模擬交易、實盤都透過這一個介面呼叫策略，
> 所以同一份策略程式在四種模式下的決策完全一樣。

所以「使用者組出來的策略」不能只是前端用 JS 算一算畫個預覽。它必須是一個**真正的
`Strategy` 實作**，被 `run_backtest`、`PaperEngine`、`at_testnet_trading::Trader` 呼叫的
是同一份程式碼、同一份狀態機。

好消息是三條執行路徑的介面已經統一好了，不需要改：

| 執行路徑 | 介面 | 位置 |
|---|---|---|
| 回測 | `&mut dyn Strategy` | `at_core::run_backtest` |
| 模擬交易 | `Box<dyn Strategy + Send>` | `at_paper_trading::spawn` |
| 測試網交易 | `Box<dyn Strategy + Send>` | `at_testnet_trading::spawn`（`src/lib.rs:283`） |

三者都只要求 `Strategy`（模擬與測試網多要一個 `Send`）。這是本 ADR 能做到「零侵入」的
全部原因。

### 1.3 現有四個策略的語意，決定了 DSL 必須長什麼樣

讀過四個內建策略的 `on_bar` 之後，有三個**非顯而易見**的既有慣例，DSL 若表達不出來就
無法重現現有策略（也就無法驗證 DSL 的正確性）：

1. **進／出場是帶記憶的狀態機（hysteresis），不是單一條件。**
   `Rsi`、`Bollinger`、`Donchian` 都有一個 `holding: bool` 欄位：RSI ≤ 30 進場、
   RSI ≥ 70 出場、**中間維持上一根的部位**。只有 `SmaCross` 是純比大小（無狀態），
   而它的註解明確說明這是刻意的特例。
   → DSL 的核心結構必須是「進場條件群組 + 出場條件群組」兩棵樹，不是一棵。
     設計稿的「做多進場／做多出場」正好就是這個形狀。

2. **出場條件先判斷，進場條件後判斷。**
   `bollinger.rs` 的註解寫得很清楚：標準差為 0 時三條線重疊，收盤同時滿足
   「≤ 下軌」與「≥ 中軌」，這時正確答案是空手，不是憑零波動開倉。
   → 執行引擎的判斷順序必須是「先出場、後進場」，而且要寫成測試鎖住。

3. **指標算不出來時，強制回到空手並清掉 `holding`。**
   三個有狀態的策略在 `let (Some(..), ..) = .. else` 分支裡都做 `self.holding = false`。
   → 三值邏輯（真／假／未知）的「未知」必須對應「空手 + 清狀態」，不是「維持原狀」。

另外，`Donchian::on_bar` 是**先讀通道、再把這根 K 線放進視窗**（註解：「否則會拿自己
跟自己比」）。也就是說它的通道是「前 N 根」而不是「含這根的 N 根」。
→ DSL 的指標節點需要一個 `offset`（往前位移幾根）欄位，才能精確重現。

### 1.4 明確排除：匯入自訂程式（WASM）

設計稿的策略庫頁面（`design-reference.md:41`）有一個「匯入自訂程式（WASM）」入口。

**這不在本 ADR 範圍內，而且是刻意排除的。**

本 ADR 的 DSL 是**純資料**：一棵只能表示「指標、比較、AND/OR」的樹，沒有迴圈、沒有
函式呼叫、沒有 I/O、沒有記憶體存取。它的最壞情況是算出一個錯的數字，然後因為
`TargetPosition` 這個出口（見第 7 節）而最多造成一筆錯誤的部位。

「執行使用者上傳的 WASM」是完全不同等級的能力：那是在一個能下單的程式裡跑第三方的
任意程式碼。它需要獨立評估沙箱邊界（WASI 要不要開？host function 暴露什麼？CPU/記憶體
/執行時間上限怎麼強制？使用者從論壇下載的 .wasm 怎麼辦？），那是另一份 ADR 的工作。

→ **UI 上這個入口在 Phase 1–3 期間必須是 `disabled` + 「需要安全評估，暫未開放」**，
  不可以是一個點了沒反應的按鈕（違反專案的「禁止假按鈕」原則）。

---

## 2. 決策摘要

1. 新增 `at-core::strategy_dsl` 模組，定義一棵 **JSON 可序列化的條件樹（AST）**：
   數值節點（`Expr`）回傳 `Option<Fixed>`，布林節點（`Cond`）回傳 `Option<bool>`。
2. 一份策略 = **四棵樹**（做多進場／做多出場／做空進場／做空出場）+ 部位設定。
3. 執行引擎是**直譯器**，但不是天真的遞迴直譯器：建構時把樹**攤平成後序節點陣列**，
   每根 K 線**由前往後、無短路、全部節點各算一次**。狀態存在與節點陣列平行的
   `Vec<NodeState>`，用 index 存取（不用 `HashMap<NodeId, _>`）。
4. `CustomStrategy` 直接 `impl Strategy`，所以 `run_backtest` / `PaperEngine` / `Trader`
   **一行都不用改**。
5. 新增三個指標計算：`EMA`（標準 α=2/(n+1)）、`MACD`、`ATR`（Wilder）。`WilderAverage`
   泛化成帶有理數平滑係數的 `Smoothed`，RSI 繼續用 α=1/n。
6. **合約資料積木（資金費率／標記價格／未平倉量／多空比）Phase 1 不實作**——
   `Strategy::on_bar(&Bar)` 這個介面根本沒有這些資料可以傳，詳見第 8.2 節。
7. **停損／移動停損不進 DSL**——策略層不知道成交價，停損屬於執行層／風控層，
   詳見第 8.3 節。
8. WASM 匯入 out of scope（第 1.4 節）。

---

## 3. DSL Schema（完整定義）

### 3.1 兩層型別

DSL 刻意分成兩種節點，用 Rust 型別分開（不是同一個 enum 混用），這樣「把一個數字
當成條件用」在編譯期就不可能發生：

| 層 | Rust 型別 | 求值結果 | 意義 |
|---|---|---|---|
| 數值 | `Expr` | `Option<Fixed>` | 一個價格／指標值。`None` = 暖機不足或算式溢位 |
| 條件 | `Cond` | `Option<bool>` | 一個是／否。`None` = 無法判定 |

序列化用 serde 的 internally-tagged enum，標籤欄位是 `kind`，值用 `snake_case`：
`#[serde(tag = "kind", rename_all = "snake_case")]`。

**所有數值字面值與門檻一律用「十進位字串」傳遞，不用 JSON number。**
理由跟 `app/src-tauri/src/strategies.rs` 既有的 `default: String` 一致：JSON number 在
JS 端是 f64，`0.1` 這種值會變成 `0.1000000000000000055…`，而這個數字會決定要不要下單。
字串經 `Fixed: FromStr` 解析，前端送什麼就是什麼。

### 3.2 `Expr`（數值節點）

#### 3.2.1 `price` — 價格與成交量

```json
{ "kind": "price", "field": "close", "offset": 0 }
```

| 欄位 | 型別 | 必填 | 說明 |
|---|---|---|---|
| `field` | `"open" \| "high" \| "low" \| "close" \| "volume"` | 是 | 對應 `at_core::Bar` 的欄位 |
| `offset` | `u16` | 否，預設 `0` | 往前位移幾根。`0` = 這根（已收盤），`1` = 前一根 |

- `offset` 讓「前一根收盤價」可以直接表示，也是重現 `Donchian`「前 N 根通道」的工具。
- `volume` 特別處理：`Bar::volume` 是 `f64`（`bar.rs:104`，註解說明只用在統計）。
  DSL 的比較一律在 `Fixed` 域裡做，所以成交量在進入 DSL 時做一次**確定性轉換**：
  `raw = (volume * 1e8)` 截尾成 `i64`。`Bar::validate()` 已擋掉負數與非有限值；
  **超過 `Fixed` 上限（約 9.22 × 10¹⁰）時回傳 `None`（視為指標算不出來），不做飽和**。
  同一個 `f64` 輸入永遠得到同一個 `Fixed`，所以回測與實盤的訊號一致性不受影響。
  > ponytail：成交量天花板 ≈ 922 億（基礎幣計）。SHIB 這類高供給量幣的日線成交量
  > 可能撞到。撞到時策略空手（安全方向），若之後真的要支援就在轉換時先除以一個
  > 固定比例（例如 1e3）並在 UI 標示單位，不要改成 f64 比較（會破壞確定性）。

#### 3.2.2 `number` — 常數

```json
{ "kind": "number", "value": "30" }
```

| 欄位 | 型別 | 說明 |
|---|---|---|
| `value` | `String` | 十進位字串，用 `Fixed: FromStr` 解析。建構時解析失敗就報錯，不等到 `on_bar` |

#### 3.2.3 `indicator` — 指標

```json
{
  "kind": "indicator",
  "name": "sma",
  "source": { "kind": "price", "field": "close", "offset": 0 },
  "params": { "period": 10 },
  "output": null,
  "offset": 0
}
```

| 欄位 | 型別 | 必填 | 說明 |
|---|---|---|---|
| `name` | `IndicatorName`（見下表） | 是 | |
| `source` | `Box<Expr>` | 看指標 | 餵給指標的數列。`null`／省略 = 預設來源 |
| `params` | 物件（整數） | 是 | 週期一類的參數，**整數用 JSON number**（週期是根數，不是價格） |
| `output` | `String \| null` | 看指標 | 多輸出指標要選哪一路（例：MACD 的 `line`/`signal`/`histogram`） |
| `offset` | `u16` | 否，預設 `0` | 取這個指標**幾根之前**的值 |

支援的指標（Phase 1 全部）：

| `name` | 預設 `source` | `params` | `output` | 說明 |
|---|---|---|---|---|
| `sma` | `close` | `period` | — | 簡單移動平均。重用現有 `Window::mean()` |
| `ema` | `close` | `period` | — | 指數移動平均，α=2/(period+1)，SMA 種子。第 5.1 節 |
| `rsi` | `close` | `period` | — | Wilder RSI，0–100。重用現有 `Rsi::value()` 的算法 |
| `macd` | `close` | `fast`, `slow`, `signal` | `line` \| `signal` \| `histogram`（**必填**） | 第 5.2 節 |
| `bb` | `close` | `period`, `mult_num`, `mult_den` | `upper` \| `middle` \| `lower`（**必填**） | 布林通道。重用 `Window::stddev()` |
| `atr` | 固定吃整根 K 線（不可指定 `source`） | `period` | — | Wilder ATR。第 5.3 節 |
| `donchian` | 固定吃 `high`/`low`（不可指定 `source`） | `period` | `high` \| `low`（**必填**） | 唐奇安通道 |
| `highest` | `close` | `period` | — | 視窗最大值。重用 `Window::highest()` |
| `lowest` | `close` | `period` | — | 視窗最小值 |

布林通道的倍數為什麼拆成 `mult_num`/`mult_den` 兩個整數：現有 `Bollinger::new()` 收
一個 `Fixed` 倍數，而 `Fixed` 乘法會截尾。設計稿同時出現 1.5 倍與 2 倍，用
「分子/分母」兩個整數（`3/2`、`2/1`）在 `i128` 裡先乘後除，比先把 1.5 變成
`150000000` 再乘再除精確，而且省掉一個「倍數字串解析」的失敗路徑。
> ponytail：如果實作時發現現有 `Bollinger` 的截尾行為必須被完全重現（2.8 的對照驗證
> 可能依賴它），就改用 `Fixed` 倍數並在這裡記一筆；兩種都可以，但要跟現有策略算出
> 一樣的值，以測試為準。

#### 3.2.4 不實作的 `Expr`：算術

設計稿的積木庫沒有「加減乘除」積木，所以 DSL **Phase 1 沒有 `arith` 節點**。
`close > sma(20) × 1.02` 這種「偏移門檻」寫不出來。

需要的時候才加，形狀已經想好（`{"kind":"arith","op":"mul","left":…,"right":…}`），
加它不會破壞既有 schema（serde 的 tagged enum 加 variant 是前向相容的方向：
舊引擎讀到新 variant 會報「不支援的節點」，而不是誤解）。

### 3.3 `Cond`（條件節點）

| `kind` | 欄位 | 語意 |
|---|---|---|
| `gt` | `left: Expr`, `right: Expr` | `left > right` |
| `gte` | 同上 | `left >= right` |
| `lt` | 同上 | `left < right` |
| `lte` | 同上 | `left <= right` |
| `cross_above` | 同上 | 向上穿越：**前一根** `left <= right` **且這根** `left > right` |
| `cross_below` | 同上 | 向下穿越：前一根 `left >= right` 且這根 `left < right` |
| `all` | `children: Vec<Cond>`（≥1） | AND |
| `any` | `children: Vec<Cond>`（≥1） | OR |
| `sustained` | `inner: Box<Cond>`, `bars: u16`（≥1） | `inner` 連續成立 `bars` 根（含這根） |

`gte`/`lte` 不在設計稿的積木清單裡（它只列了大於／小於／穿越／持續N根），但**必須
存在**：`Bollinger` 的出場是 `close >= middle`、進場是 `close <= lower`，`Rsi` 是
`rsi >= 70` / `rsi <= 30`。沒有 `gte`/`lte` 就無法重現現有策略，也就無法用「DSL 版本
和 Rust 版本跑出一樣的訊號」這個最有力的驗證方法。UI 可以用同一顆積木的下拉選單
（「大於」／「大於或等於」）呈現，不必多一個積木分類。

**沒有 `not`**。設計稿的做空條件是「預設鏡像多單」，那是 UI 在生成 JSON 時把
`gt` 換成 `lt`、`cross_above` 換成 `cross_below`，不是在樹上包一層 NOT。
少一個節點種類就少一組三值邏輯的邊界案例要測。

#### 三值邏輯（Kleene）

暖機期間指標是 `None`，比較無法判定。規則：

- 比較節點（`gt`/`lt`/`gte`/`lte`）：任一邊 `None` → `None`。
- `cross_above`/`cross_below`：這根或前一根的比較結果是 `None` → `None`。
- `all`：任一子節點 `Some(false)` → `Some(false)`；否則有 `None` → `None`；否則 `Some(true)`。
- `any`：任一子節點 `Some(true)` → `Some(true)`；否則有 `None` → `None`；否則 `Some(false)`。
- `sustained`：視窗內任一根是 `None` → `None`；全部 `Some(true)` → `Some(true)`；否則 `Some(false)`。

`all`/`any` 的規則是「資訊足夠就下結論」：`any` 裡只要有一個已經成立，其他還在暖機
也不影響答案。這比「有 None 就整棵 None」更合理，也不會不安全（它只在答案確定時
才確定）。

### 3.4 策略根節點

`at-core` 吃的是這個（只有引擎需要的部分）：

```json
{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry":  { "...Cond..." },
  "longExit":   { "...Cond..." },
  "shortEntry": null,
  "shortExit":  null
}
```

| 欄位 | 型別 | 說明 |
|---|---|---|
| `schemaVersion` | `u16` | 目前固定 `1`。不等於 1 → 建構時報「這份策略來自不同版本」 |
| `direction` | `"long_only" \| "long_short"` | `long_only` 時 `shortEntry`/`shortExit` **必須**是 `null`（否則報錯，不是靜默忽略） |
| `sizing.positionPct` | `String` | 每次進場投入的權益百分比，`"100"` = 滿倉。範圍 `(0, 100]` |
| `sizing.leverage` | `String` | 槓桿倍數，範圍 `[1, 125]`。現貨必須是 `"1"` |
| `longEntry` / `longExit` | `Cond` | 兩棵都必填 |
| `shortEntry` / `shortExit` | `Cond \| null` | `long_short` 時兩棵都必填 |

`TargetPosition` 的大小 = `positionPct / 100 × leverage`，做多為正、做空為負。
（`TargetPosition` 的語意本來就是「配置資金的比例，可大於 1 表示槓桿」——
見 `strategy.rs:24`。）

App 層另外有一個外層文件，`at-core` 不該知道它的存在：

```json
{
  "id": "usr_01J…", "name": "我的均線策略", "version": 3,
  "market": "spot", "interval": "1h", "symbols": ["BTCUSDT", "ETHUSDT"],
  "marginMode": "isolated",
  "risk": { "exchangeStop": "...", "trailingStop": "..." },
  "ast": { "...上面那個物件... " }
}
```

`name`／`version`／「未儲存標記」／交易對清單／保證金模式／停損設定全部是 App 層的事。
`at-core` 只看 `ast`。這條界線要守住，否則 `at-core` 會開始依賴 UI 的概念。

### 3.5 範例一：10/50 均線交叉（重現 `SmaCross`）

現有語意（`sma_cross.rs:78`）：快線 > 慢線 → 滿倉做多，其他（含相等、暖機不足）→ 空手。
它是無狀態的「比大小」，所以出場條件就是進場條件的反面：

```json
{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "gt",
    "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
    "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
  },
  "longExit": {
    "kind": "lte",
    "left":  { "kind": "indicator", "name": "sma", "params": { "period": 10 } },
    "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } }
  },
  "shortEntry": null,
  "shortExit": null
}
```

驗證點（必須寫成測試）：這份 JSON 建出來的 `CustomStrategy` 餵同一串 K 線，
每一根的 `TargetPosition` 要跟 `SmaCross::new(10, 50)` **完全相同**，包含暖機期。
相等時（`fast == slow`）兩邊都是空手：`longExit` 的 `lte` 成立 → 出場先判斷 → 空手。✓

> 設計稿的積木庫只有「大於／小於」，所以 UI 的實務作法是：使用者只拖「做多進場：
> SMA10 大於 SMA50」，UI 在「出場條件留空」時自動填入進場條件的邏輯反面並標示
> 「出場 = 進場條件不成立」。這是 UI 的便利功能，DSL 本身維持兩棵樹都明確。

### 3.6 範例二：RSI 14 / 30 / 70（重現 `Rsi`）

現有語意（`rsi.rs:66-70`）：RSI ≤ 30 進場、RSI ≥ 70 出場、中間維持上一根。
這是帶記憶的狀態機，所以 DSL 的兩棵樹不是互補的：

```json
{
  "schemaVersion": 1,
  "direction": "long_only",
  "sizing": { "positionPct": "100", "leverage": "1" },
  "longEntry": {
    "kind": "lte",
    "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
    "right": { "kind": "number", "value": "30" }
  },
  "longExit": {
    "kind": "gte",
    "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
    "right": { "kind": "number", "value": "70" }
  },
  "shortEntry": null,
  "shortExit": null
}
```

注意兩棵樹各自有一個 `rsi(14)` 節點，它們是兩個獨立的狀態槽。**餵的是同一串收盤價，
所以算出的值永遠相同**（指標是輸入序列的純函數），不會漂移——前提是執行引擎每根
K 線都餵每一個指標，見第 4.2 節。

### 3.7 範例三：唐奇安突破（示範 `offset`）

現有語意（`donchian.rs`）：通道用**前 N 根**的高低價算（不含這根），收盤突破前 20 根
最高價進場、跌破前 10 根最低價出場。`offset: 1` 就是「前一根算出來的通道值」，而
`donchian(period)` 本身是「含這根的 N 根」，兩者組合起來等於「前 N 根」：

```json
{
  "longEntry": {
    "kind": "gt",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "donchian", "output": "high",
               "params": { "period": 20 }, "offset": 1 }
  },
  "longExit": {
    "kind": "lt",
    "left":  { "kind": "price", "field": "close" },
    "right": { "kind": "indicator", "name": "donchian", "output": "low",
               "params": { "period": 10 }, "offset": 1 }
  }
}
```

### 3.8 範例四：多空＋巢狀＋持續N根（示範 DSL 真正的新能力）

「MACD 柱狀體向上穿越 0，且（收盤價在 50 均線之上 或 RSI 連續 3 根高於 55）」：

```json
{
  "schemaVersion": 1,
  "direction": "long_short",
  "sizing": { "positionPct": "50", "leverage": "2" },
  "longEntry": {
    "kind": "all",
    "children": [
      { "kind": "cross_above",
        "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
                   "params": { "fast": 12, "slow": 26, "signal": 9 } },
        "right": { "kind": "number", "value": "0" } },
      { "kind": "any",
        "children": [
          { "kind": "gt",
            "left":  { "kind": "price", "field": "close" },
            "right": { "kind": "indicator", "name": "sma", "params": { "period": 50 } } },
          { "kind": "sustained", "bars": 3,
            "inner": { "kind": "gt",
                       "left":  { "kind": "indicator", "name": "rsi", "params": { "period": 14 } },
                       "right": { "kind": "number", "value": "55" } } }
        ] }
    ]
  },
  "longExit": {
    "kind": "cross_below",
    "left":  { "kind": "indicator", "name": "macd", "output": "histogram",
               "params": { "fast": 12, "slow": 26, "signal": 9 } },
    "right": { "kind": "number", "value": "0" }
  },
  "shortEntry": { "...longEntry 的鏡像（cross_above→cross_below、gt→lt）..." },
  "shortExit":  { "...longExit 的鏡像..." }
}
```

目標部位大小 = 50% × 2 = `1.0`（做多 `TargetPosition::new(1.0)`、做空 `-1.0`）。

---

## 4. 執行引擎設計

### 4.1 核心選擇：攤平的後序節點陣列，由前往後一次掃完

**決策：直譯器，但樹在建構時攤平成陣列，每根 K 線無短路地從頭算到尾。**

建構時（`StrategyAst::compile()`）做一次後序（post-order）走訪，把四棵樹的所有節點
攤成一個 `Vec<Node>`，每個節點的子節點用**比自己小的 index** 表示：

```
nodes[0] = Sma { period: 10, source: PriceClose }
nodes[1] = Sma { period: 50, source: PriceClose }
nodes[2] = Gt  { left: 0, right: 1 }        ← longEntry 的根 = index 2
nodes[3] = Sma { period: 10, source: PriceClose }
nodes[4] = Sma { period: 50, source: PriceClose }
nodes[5] = Lte { left: 3, right: 4 }        ← longExit 的根 = index 5
```

搭配一個等長的 `Vec<NodeState>` 與一個等長的 `Vec<Value>`（本根的求值結果）。
每根 K 線：

```
for i in 0..nodes.len():
    values[i] = eval(nodes[i], &mut states[i], values[..i], bar)
```

因為子節點的 index 一定小於自己，單一次由前往後的掃描就保證「用到的值都已經算好」。
不需要遞迴、不可能有環、不需要 `HashMap`。

#### 為什麼不是天真的遞迴直譯器

遞迴直譯器有一個具體而危險的 bug：**短路會讓有狀態的節點漏掉 K 線。**

```
any([ cross_above(sma10, sma50),   ← 需要記住前一根的比較結果
      close > sma200 ])
```

如果用 `||` 短路求值，而第二個子條件先成立，第一個 `cross_above` 節點這根就不會被
求值，它記的「前一根比較結果」就停在更早的一根。下一根它再被求值時，會拿兩根之前的
狀態來判斷穿越——**算出一個根本沒發生過的穿越訊號**。更糟的是這個 bug 會依求值順序
而異，所以回測與實盤如果節點順序不同（例如前端重新排列了積木），訊號會不一樣，
正好打破本專案最在意的那條原則。

防法有兩種：(a) 寫一個「保證不短路」的遞迴求值器，然後靠 code review 守住它；
(b) 讓「每個節點每根都算一次」成為資料結構的性質。選 (b)。它的程式碼量不比 (a) 多，
而且把一個需要靠紀律維持的不變量換成了結構上的保證。

#### 為什麼不是編譯成 bytecode VM 或生成 Rust/WASM

節點陣列 + 一個 `match` 已經是「扁平的指令序列 + 直譯器迴圈」了，再往下做一層
bytecode 只是把 `match nodes[i]` 換成 `match ops[i]`，不會更快也不會更清楚。
生成程式碼要帶編譯器（或 WASM runtime）進來，是本專案最不需要的依賴。

效能的真實數字：一份複雜策略約 50 個節點，每個節點每根約 10–100 ns。
5 年 1h 資料 ≈ 44,000 根 → 單次回測約 2 × 10⁶ 次節點求值，數十毫秒以內。
對比之下，讀 K 線檔案與算績效指標才是主要成本。

真正會痛的是**參數掃描**（設計稿的「參數穩定度熱力圖」要雙參數網格）：
10×10 網格 × 10 個幣 × 5 年 = 100 次完整回測 × 10 幣 ≈ 4.4 × 10⁸ 次節點求值。
這**不應該**靠「讓直譯器更快」解決，而是靠「每組參數各建一個 `CustomStrategy`、
用 `std::thread::scope` 分到各核心跑」（K 線資料唯讀共享，`CustomStrategy` 之間
完全獨立，所以這是最乾淨的並行形狀）。不需要新依賴。

### 4.2 狀態管理

`Vec<NodeState>` 與節點陣列平行，用 index 存取。每種節點的狀態：

| 節點 | `NodeState` 內容 |
|---|---|
| `price`（offset=0） | 無狀態 |
| `price`（offset>0） | 長度 `offset+1` 的環狀緩衝 |
| `number` | 無狀態 |
| `sma` / `highest` / `lowest` / `bb` | 現有的 `Window`（`VecDeque<Fixed>` + 移動總和） |
| `ema` | 一個 `Smoothed`（種子累加 + 當前值），見 5.1 |
| `rsi` | 兩個 `Smoothed`（漲幅／跌幅）+ `prev_close` |
| `macd` | 三個 `Smoothed`（fast、slow、signal） |
| `atr` | 一個 `Smoothed` + `prev_close` |
| `indicator` 的 `offset>0` | 上述狀態 + 長度 `offset+1` 的值環狀緩衝 |
| `gt`/`lt`/`gte`/`lte` | 無狀態 |
| `cross_above`/`cross_below` | 前一根的比較結果 `Option<bool>` |
| `all`/`any` | 無狀態 |
| `sustained(n)` | 長度 `n` 的環狀緩衝（存 `Option<bool>`） |

**為什麼用 `Vec` + index，不用 `HashMap<NodeId, IndicatorState>`：**

| | `Vec<NodeState>` + 攤平 index | `HashMap<NodeId, State>` + 前端發 id |
|---|---|---|
| id 從哪來 | 建構時自己編（後序走訪順序） | 前端送（UUID） |
| id 重複怎麼辦 | 不可能 | 要驗證、要報錯、要寫測試 |
| 前端漏送 id | 不可能 | 執行期才發現 |
| 每根 K 線成本 | index 存取，無 hash、無配置 | 每個節點一次 hash 查找 |
| 「每個節點都算到」 | 結構保證 | 靠求值器紀律 |

前端當然有自己的 block id（拖拉 UI 需要 key），但那是 UI 的事，不該穿透到引擎。
**引擎的狀態槽身分由樹的結構決定，不由前端的字串決定。**

**重複節點不去重。** 同一個 `sma(10)` 出現在進場與出場兩棵樹裡就算兩次。
去重要在建構時建一張 `(指標, 參數, 來源, offset) → slot` 的表，是一次性成本、
不影響每根的效能，但在正確性上沒有任何好處（指標是輸入序列的純函數，兩個槽餵同樣
的 K 線必然算出同樣的值）。所以是純效能優化：
> ponytail：不去重。50 個節點裡重複算三四個 SMA 是幾十奈秒。等參數掃描實測發現
> 這裡是瓶頸，再在 `compile()` 裡加一張 HashMap 把相同的指標節點指向同一個 slot
> （`Node::Indicator` 多一個 `slot: usize` 欄位即可，不動求值邏輯）。

### 4.3 每根 K 線的求值流程

```
fn on_bar(&mut self, bar: &Bar) -> TargetPosition:
    1. for i in 0..nodes.len():            // 無短路，全部算
           values[i] = eval(nodes[i], &mut states[i], &values, bar)
       （price 的環狀緩衝與 offset 的推進也在這一步，順序與現有
         Donchian「先讀舊值、再推入新值」一致——offset 讀的是推入前的歷史）

    2. 依 direction 取出四個（或兩個）根節點的 Option<bool>

    3. // 先出場、後進場（重現 Bollinger / Rsi / Donchian 的既有順序）
       if long_exit  == None || long_entry  == None:  self.long_held  = false
       else:
           if long_exit  == Some(true):  self.long_held  = false
           if long_entry == Some(true):  self.long_held  = true
       （做空同理，用 short_entry / short_exit / short_held）

    4. match (long_held, short_held):
           (true,  false) => long(size)
           (false, true ) => short(size)
           (false, false) => FLAT
           (true,  true ) => FLAT   // 多空同時成立 = 互相抵銷，見下
```

幾個刻意的決定：

- **步驟 3 的「任一棵樹 `None` → 清掉 `long_held`」**精確對應現有三個策略的
  `let (Some(..)) = .. else { self.holding = false; return FLAT; }`。暖機期間不只是
  不開倉，而是連狀態都不留。
- **步驟 4 的多空同時成立 → 空手**，而不是「誰先誰贏」。理由跟 `Bollinger` 處理
  標準差為 0 的理由一樣：矛盾的訊號代表沒有資訊，不該憑矛盾開倉。
  這個行為要寫成測試（使用者完全有可能拖出矛盾的條件）。
- `on_bar` **絕不 panic**，所有算術走 `checked_*`，任何溢位 → 該節點 `None`
  → 傳播成「無法判定」→ 空手。這是 `Strategy` trait 的既有契約。

### 4.4 建構時的驗證（信任邊界）

策略 JSON 來自使用者的輸入框，也可能來自使用者從別處拿到的檔案。
**`compile()` 是信任邊界，驗證不可以省。** 全部在 `compile()` 做完，`on_bar` 裡
沒有任何驗證（跑在熱路徑上，而且錯誤無處可回報）：

| 檢查 | 上限／規則 | 為什麼 |
|---|---|---|
| `schemaVersion` | 必須 == 1 | 未來版本不要被誤解 |
| 節點總數 | ≤ 512 | 資源耗盡 |
| 樹深度 | ≤ 32 | 遞迴走訪的堆疊深度 |
| 指標 `period` | 1 ≤ period ≤ 2000 | `period` 直接決定 `VecDeque` 長度。512 節點 × 2000 × 8 bytes ≈ 8 MB 上限，可接受 |
| `sustained.bars` | 1 ≤ bars ≤ 2000 | 同上 |
| `offset` | ≤ 500 | 同上 |
| MACD | `fast < slow`，三者皆 ≥ 1 | 否則 MACD 線沒有意義 |
| 多輸出指標 | `output` 必填且合法 | 不預設成 `line`，避免使用者以為選了別的 |
| `all`/`any` | `children` 非空 | 空的 AND 在邏輯上是「真」，使用者絕對不是這個意思 |
| `number.value` | 解析得出 `Fixed` | |
| `direction` 一致性 | `long_only` 時做空樹必須是 `null`；`long_short` 時兩棵都必填 | 不靜默忽略 |
| `sizing.positionPct` | `(0, 100]` | |
| `sizing.leverage` | `[1, 125]`，現貨必須 == 1 | 真正的槓桿上限由 `at-risk-control` 把關（預設 2 倍），這裡只擋明顯荒謬的值 |
| `source` 合法性 | `atr`/`donchian` 不接受 `source` | 它們吃整根 K 線，給 `source` 是使用者誤解 |

另外一個**在 `compile()` 之前**的邊界：JSON 字串本身的深度。
Tauri command 的參數是 serde_json 先反序列化好才進到我們手上，所以「10000 層巢狀的
JSON」會在 serde_json 階段處理掉（它有預設遞迴上限）。
**這件事要寫一個測試確認是「回傳錯誤」而不是「堆疊溢位」**，不要只是相信文件。

錯誤型別 `DslError`，`Display` 用繁體中文，而且**要帶路徑**
（例如「做多進場條件 → 第 2 個子條件 → 左側：SMA 週期必須在 1 到 2000 之間，收到 0」），
否則使用者在一棵 30 個積木的樹裡收到「週期錯誤」會不知道要改哪一顆。

### 4.5 `warmup_bars()`

在 `compile()` 時算好存起來（`on_bar` 不重算）。各節點的暖機根數：

| 節點 | warmup |
|---|---|
| `price` | `1 + offset` |
| `number` | `0` |
| `sma(n)` / `bb(n)` / `highest(n)` / `lowest(n)` / `donchian(n)` | `n + offset` |
| `ema(n)` | `n + offset` |
| `rsi(n)` | `n + 1 + offset` |
| `atr(n)` | `n + 1 + offset` |
| `macd(f, s, sig)` | `s + sig - 1 + offset` |
| 比較節點 | `max(左, 右)` |
| `cross_above` / `cross_below` | `max(左, 右) + 1` |
| `all` / `any` | `max(子節點)` |
| `sustained(n)` | `inner + n - 1` |
| 策略根 | `max(四棵樹)` |

有 `source` 的指標要把 source 的 warmup 疊上去（`sma(10)` 吃 `ema(20)` → 20 + 10 − 1）。
`macd` 的 `s + sig - 1`：MACD 線從第 `s` 根開始有值，signal 是它的 EMA(sig)，
再需要 `sig − 1` 根才有第一個值。第 5.2 節的測試向量會驗證這個數字。

---

## 5. 新指標的計算規格

這三個指標目前專案裡沒有。規格寫到可以直接照著實作與寫測試的程度。
**全部用 `Fixed`（8 位小數、i64 raw）在 `i128` 中間值裡算，除法往零的方向截尾**，
理由跟 `strategies/mod.rs` 的模組註解一樣：`f64` 的加總順序會讓同一份資料在回測與
實盤落在門檻的兩邊。

### 5.0 共用零件：`Smoothed`（泛化現有的 `WilderAverage`）

`strategies/rsi.rs` 已經有一個 `WilderAverage`，用 α = 1/n 的平滑：

```
種子期：前 n 筆取算術平均
之後：新值 = 舊值 + (這筆 − 舊值) ÷ n
```

註解已經論證了「這個寫法的中間值一定落在舊值與這筆之間，所以不可能溢位」——
這個論證對帶正負號的輸入（MACD 線可以是負的）同樣成立。

**把它泛化成有理數係數 α = num/den：**

```
新值 = 舊值 + (這筆 − 舊值) × num ÷ den      // 在 i128 裡先乘後除，截尾往零
```

| 指標 | α | 建構子 |
|---|---|---|
| RSI 的漲跌幅平均（Wilder） | `1/n` | `Smoothed::wilder(n)` |
| ATR（Wilder） | `1/n` | `Smoothed::wilder(n)` |
| EMA / MACD（標準） | `2/(n+1)` | `Smoothed::ema(n)` |

種子都是「前 n 筆的算術平均」。這一步把 RSI 現有的程式碼重用掉，不是新寫一份。
**改完 `WilderAverage` 之後 `Rsi` 的所有既有測試必須不動就通過**（這是 refactor，
不是改行為）。

> 註：Wilder 的 α=1/n 等於標準 EMA 的 n′ = 2n−1。所以 `rsi(14)` 的平滑強度相當於
> `ema(27)`。這不是錯誤，是兩個指標的歷史定義不同；UI 上不需要解釋，但文件要寫，
> 否則下一個維護者會以為其中一個算錯了。

### 5.1 EMA（指數移動平均）

- α = 2 / (period + 1)
- 種子：前 `period` 筆收盤價的算術平均，第 `period` 根產生第一個值
- 之後：`EMA_t = EMA_{t−1} + (close_t − EMA_{t−1}) × 2 ÷ (period + 1)`

這是 TradingView／ta-lib 的 EMA 定義（種子用 SMA）。另一種常見做法是用第一筆
直接當種子，兩者只在前期有差異，但會差到第幾十根，所以**必須選定一種並寫進測試**。
選 SMA 種子，跟 `WilderAverage` 現有的做法一致（少一種慣例）。

### 5.2 MACD

```
macd_line  = EMA(close, fast) − EMA(close, slow)          // 預設 12, 26
signal     = EMA(macd_line, signal_period)                 // 預設 9
histogram  = macd_line − signal
```

三個注意點：

1. **`macd_line` 只在 `EMA(slow)` 有值之後才存在**（第 `slow` 根）。
   在那之前什麼都不餵給 signal 的 `Smoothed`——不是餵 0。餵 0 會把 signal 的種子
   汙染成一堆零，算出一條假的線。
2. **signal 的種子是「前 `signal_period` 個 macd_line 值的算術平均」**，
   所以第一個 signal 值在第 `slow + signal_period − 1` 根。
3. `macd_line` 可以是負數，`Smoothed` 要能處理帶正負號的輸入（見 5.0）。

#### 測試向量（小參數，可手算驗證）

`fast=2, slow=3, signal=2`，收盤價 `10, 12, 14, 13, 15`：

| 根 | close | EMA2（fast） | EMA3（slow） | macd_line | signal | histogram |
|---|---|---|---|---|---|---|
| 1 | 10 | — | — | — | — | — |
| 2 | 12 | 11（種子 =(10+12)/2） | — | — | — | — |
| 3 | 14 | 11+(14−11)×2/3 = **13** | 12（種子 =(10+12+14)/3） | **1** | — | — |
| 4 | 13 | 13+(13−13)×2/3 = **13** | 12+(13−12)×1/2 = **12.5** | **0.5** | 0.75（種子 =(1+0.5)/2） | **−0.25** |
| 5 | 15 | 13+(15−13)×2/3 = **14.33333333** | 12.5+(15−12.5)/2 = **13.75** | **0.58333333** | 0.75+(0.58333333−0.75)×2/3 = **0.63888889** | **−0.05555556** |

逐步核對第 5 根的 signal（raw 單位）：
`(58333333 − 75000000) = −16666667`；`× 2 = −33333334`；`÷ 3 = −11111111.33`
→ 往零截尾 `−11111111`；`75000000 − 11111111 = 63888889` = `0.63888889`。✓

warmup 檢查：`slow + signal − 1 = 3 + 2 − 1 = 4`，signal 的第一個值確實在第 4 根。✓

### 5.3 ATR（Average True Range，Wilder）

真實波幅（True Range）：

```
TR_t = max( high_t − low_t,
            |high_t − close_{t−1}|,
            |low_t  − close_{t−1}| )
```

- **第一根沒有 `close_{t−1}`，所以沒有 TR。** TR 從第 2 根開始。
  （有些實作把第一根的 TR 定成 `high − low`；這裡選「沒有」，跟 ta-lib 一致，
  而且跟現有 `Rsi::on_bar` 第一根 `return FLAT` 的處理方式同一個風格。）
- ATR = `Smoothed::wilder(period)` 餵 TR，種子是前 `period` 個 TR 的算術平均。
- 所以**第一個 ATR 值在第 `period + 1` 根**，`warmup_bars` = `period + 1`。
- `high − low` 兩個正數相減不會溢位；`checked_sub` 仍然要用（契約要求），失敗就
  這根不餵 TR 並回傳 `None`。

#### 測試向量

`period = 3`：

| 根 | high | low | close | prev close | TR | ATR |
|---|---|---|---|---|---|---|
| 1 | 10 | 8 | 9 | — | — | — |
| 2 | 11 | 9 | 10.5 | 9 | max(2, 2, 0) = **2** | — |
| 3 | 12 | 10.5 | 11 | 10.5 | max(1.5, 1.5, 0) = **1.5** | — |
| 4 | 11 | 9 | 9.5 | 11 | max(2, 0, 2) = **2** | 種子 (2+1.5+2)/3 = **1.83333333** |
| 5 | 10 | 9.5 | 10 | 9.5 | max(0.5, 0.5, 0) = **0.5** | **1.38888889** |

第 5 根核對（raw）：`(50000000 − 183333333) = −133333333`；`÷ 3 = −44444444.33`
→ 截尾 `−44444444`；`183333333 − 44444444 = 138888889` = `1.38888889`。✓
（第 4 根種子：`5.5 / 3 = 1.8333…` → `183333333`，截尾。）

### 5.4 要跟現有策略對齊的指標

`sma`、`rsi`、`bb`、`donchian`、`highest`、`lowest` **不重新實作**，重用
`strategies/mod.rs` 的 `Window` 與 `rsi.rs` 的平滑平均。

驗證方式最有力：**同一份 K 線，DSL 版本與 Rust 版本的每一根 `TargetPosition` 必須
逐根相等**，四個內建策略各一個測試。這同時驗證了指標值、三值邏輯、出場優先順序、
暖機行為。這四個測試是整個 Phase 1 的驗收核心。

---

## 6. 跟 `Strategy` trait 的整合

**決策：`CustomStrategy` 直接 `impl at_core::strategy::Strategy`。不改 trait、
不改三條執行路徑的任何一行。** 同意任務書提出的方向。

```
pub struct CustomStrategy {
    nodes:  Vec<Node>,          // compile() 產生，之後唯讀
    states: Vec<NodeState>,     // 跟 nodes 等長，on_bar 會改
    values: Vec<Value>,         // 本根的暫存，on_bar 會改
    roots:  Roots,              // 四棵樹的根 index
    size:   Fixed,              // positionPct/100 × leverage，compile() 算好
    long_held:  bool,
    short_held: bool,
    warmup: usize,
}

impl Strategy for CustomStrategy {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition { /* 4.3 節的流程 */ }
    fn warmup_bars(&self) -> usize { self.warmup }
}
```

為什麼這是對的（而不只是最省事的）：

- `Strategy` 的契約是「看完這根 K 線，我想持有多少部位」，`CustomStrategy` 完全
  符合這個描述。它不是一個新種類的東西，它就是一個策略。
- 三條執行路徑要的是 `Box<dyn Strategy + Send>` / `&mut dyn Strategy`。
  `CustomStrategy` 的欄位全是 `Vec`／`Fixed`／`bool`／`VecDeque`，**自動滿足 `Send`**
  （沒有 `Rc`、沒有 `RefCell`），所以 `app/src-tauri/src/backtest.rs:64` 的
  `build_strategy()` 回傳型別 `Result<Box<dyn Strategy + Send>, String>` 不用改——
  只要在那個 `match` 裡多一個分支。
- 任何「為了自訂策略而擴充 `Strategy` trait」的設計，都會讓四個內建策略也要跟著
  實作新方法，而且會把 DSL 的概念洩漏到核心介面裡。

唯一的接點（`backtest.rs` 的 `build_strategy`）要怎麼長：

現在的簽名是 `build_strategy(strategy_id: &str, params: &HashMap<String, String>)`，
而自訂策略的「參數」是一整棵樹，塞不進 `HashMap<String, String>`。
最小的改法是在 `BacktestRequest` 加一個 `Option<StrategyAst>` 欄位（前端只在
`strategyId == "custom"` 時填），`build_strategy` 多收一個 `ast: Option<&StrategyAst>`：

```
"custom" => {
    let ast = ast.ok_or("自訂策略缺少條件定義")?;
    Ok(Box::new(ast.compile().map_err(|e| format!("策略定義錯誤：{e}"))?))
}
```

這樣回測與模擬交易（`paper_trading.rs` 重用同一個 `build_strategy`）同時支援自訂策略，
測試網路徑（`roadmap 6.x` 的 `Trader`）也一樣，因為它們都只是在要一個
`Box<dyn Strategy + Send>`。

---

## 7. 安全邊界（不可違反）

**`CustomStrategy` 的唯一輸出是 `TargetPosition`。它沒有、也永遠不會有任何下單能力。**

這條界線怎麼被結構性地保證（不是靠紀律）：

1. **`at-core::strategy_dsl` 不依賴任何會下單的東西。**
   `at-core` 現在的依賴是**空的**；加了 serde 之後仍然不依賴
   `at-binance-client`、`at-testnet-trading`、`reqwest`、`tokio`。
   DSL 模組連「有一個交易所」這件事都不知道。Rust 的 crate 依賴方向就是這條界線的
   強制機制：`at-core` 在依賴圖的最底層，上面的 crate 可以用它，它不能回頭用上面的。
2. **`Node` 的求值函式簽名裡沒有任何可以產生副作用的東西。**
   `eval(&Node, &mut NodeState, &[Value], &Bar) -> Value`：輸入是一根 K 線與自己的
   狀態，輸出是一個數字或布林。沒有 `&mut Account`、沒有 client、沒有 channel、
   沒有 `std::fs`、沒有 `std::net`。它連「現在幾點」都不知道
   （只有 `bar.open_time`，而那是資料的一部分）。
3. **「目標部位 → 訂單」的轉換在別的地方，而且已經有風控把關。**
   `run_backtest` 只做記帳；`PaperEngine` 永遠不送單；只有
   `at_testnet_trading::Trader` 會送單，而它在送之前要過 `at-risk-control`
   的 `check()`（單筆金額、單一幣種部位、槓桿上限預設 2 倍、單日虧損上限…）。
   使用者在積木裡填 `leverage: 125` 不會變成 125 倍的實際下單——
   風控層會擋下來。**DSL 的驗證上限（第 4.4 節）是防手滑，風控層才是防線。**
4. **`RunMode::sends_orders()` 的既有測試保護不受影響。**
   自訂策略沒有引入任何新的執行路徑，所以回測／模擬永不送單的保證原封不動。

**給實作者的明確禁令：**

- `strategy_dsl` 模組**不可以** `use` 任何 `at_binance_client` / `at_testnet_trading` /
  `at_paper_trading` 的東西，也不可以新增 `reqwest`／`tokio`／`std::fs`／`std::net`
  的用法。（這一條可以用一個測試鎖住：檢查 `at-core` 的 `Cargo.toml` 依賴清單只有
  serde。）
- `Node` enum **不可以**有任何帶有「動作」語意的 variant（`place_order`、
  `cancel_all`、`notify`…）。DSL 是一個算式求值器，不是腳本語言。
  使用者想要的任何「動作」都必須表達成「目標部位是多少」。
- 停損**不可以**用「DSL 直接送一張停損單」來實作（見 8.3）。

---

## 8. 範圍邊界與已知缺口

### 8.1 Phase 1 做得到的

指標積木：SMA、EMA、RSI、MACD、布林通道、ATR、唐奇安高低點、成交量 ✓（設計稿點名的 8 個全部）
價格積木：開、高、低、收 ✓
比較運算子：大於、小於、向上穿越、向下穿越、持續N根（+ 大於等於／小於等於）✓
邏輯：AND／OR／可巢狀 ✓
做多進場／做多出場／做空進場／做空出場 ✓
部位大小：每幣部位 % 權益 × 槓桿 ✓

### 8.2 合約資料積木（資金費率／標記價格／未平倉量／多空比）：Phase 1 不做

這不是偷懶，是**資料根本不在**：

| 積木 | 現狀 |
|---|---|
| 資金費率 | `BacktestConfig.funding_rate` 是**全程固定的一個常數**（`backtest.rs:159`），不是逐根的歷史序列 |
| 標記價格 | 回測只有 K 線的收盤價，沒有標記價格序列 |
| 未平倉量 | 專案裡完全沒有這個資料的下載器或儲存格式 |
| 多空比 | 同上 |

而且更根本的：`Strategy::on_bar(&mut self, bar: &Bar)` 這個簽名**只傳一根 K 線**。
`Bar` 有 `open_time / open / high / low / close / volume`，就這樣。要讓 DSL 讀到合約
資料，必須改介面。兩條路：

- **(a) 擴充 `Bar`**：加 `funding_rate`、`mark_price`、`open_interest`…
  → 會動到 `bar_store` 的檔案格式、`kline_csv` 的解析、`downloader`、以及所有
  現存的 `Bar { .. }` 建構處與測試。代價很大，而且讓每一根現貨 K 線都背著四個
  用不到的欄位。
- **(b) 加一個有預設實作的 trait 方法**：
  `fn on_context(&mut self, _ctx: &MarketContext) {}`，由執行層在 `on_bar` 之前呼叫。
  四個內建策略不用改（有預設實作），`CustomStrategy` 覆寫它。
  → **推薦這條**，但它需要先解決「回測時這些序列從哪來」（要新的下載器與儲存格式），
  那是獨立的一塊工作量。

**Phase 1 的處理：UI 上這一整個積木分類顯示為 `disabled` + 「需要合約歷史資料，
即將推出」。** 不可以讓使用者拖得進去、組得出來，然後回測結果是錯的或默默空手。

### 8.3 停損／移動停損：不進 DSL

設計稿的編輯器中間欄有「停損與部位設定（交易所端停損、移動停損、每幣部位%權益、槓桿）」。
其中**部位 % 權益與槓桿進 DSL**（它們就是 `TargetPosition` 的大小），
**停損與移動停損不進 DSL**。理由：

**策略層不知道自己的成交價。** 這是 1.x 就定下的設計（`strategy.rs` 的模組註解：
「策略只說『我想要半倉做多』，至於要送幾張單、成交價是多少…是成交模擬的事」）。
而且回測的成交規則是「訊號在 K 線收盤產生，下一根開盤成交」，所以在 `on_bar` 回傳
目標部位的那一刻，成交價在未來、還不存在。一個不知道進場價的東西無法算出
「虧 2% 了」。

所以停損的正確歸屬是：

- **交易所端停損**：下單層（`at-testnet-trading` 送單時一併送 STOP_MARKET），
  設計稿的風控頁也把它列在「合約風控」而不是策略邏輯裡。
- **移動停損**：執行層（持倉管理），它知道成交價與目前標記價。
- **策略回撤停止 %**：已經在 `at-risk-control` 的範疇（設計稿 GoLive 頁的部署設定）。

**實作上的處理**：這些欄位存在 App 層的 `StrategyDoc.risk` 裡（UI 照設計稿顯示、
可以編輯、會存檔），但**不傳給 `compile()`**，Phase 1 也不被任何東西消費。
UI 必須標示「部署時生效」或先 `disabled`，不可以讓使用者以為回測結果已經含停損。
> 這是整份設計裡最容易被誤解的一點：使用者在編輯器裡設了 5% 停損，看回測結果，
> 以為回測含停損。必須在回測結果頁明確回顯「本次回測未計入停損」。

### 8.4 其他已知不做

- 算術運算節點（3.2.4）
- 多時間週期（在 1h 策略裡引用 4h 的 SMA）——需要第二條 K 線流，介面改動大
- 多交易對互動條件（「BTC 漲時買 ETH」）——`Strategy` 是單一交易對的介面
- 掛單簿／Tick 級條件（設計稿的「造市」、「掛單簿失衡」範本）——完全不同的資料頻率
- 匯入 WASM（第 1.4 節）

---

## 9. 方案比較

### 9.1 中介表示：方案 A（JSON AST）vs 方案 B（線性規則表）

| 項目 | 方案 A：巢狀 JSON AST（**採用**） | 方案 B：扁平規則表（`WHEN sma10 > sma50 AND rsi < 70 THEN LONG`） |
|---|---|---|
| 優點 | 結構與 UI 的巢狀積木一對一；AND/OR 任意巢狀；加節點種類不破壞舊檔；serde derive 直接搞定 | schema 極簡（一張表）；人眼可讀；可以直接存進 SQLite 的 rows |
| 缺點 | JSON 巢狀深時人眼難讀；需要深度/大小上限 | 設計稿明確要求「可巢狀」的 AND/OR，扁平表做不到；要巢狀就得發明一套群組 id 欄位，等於自己手刻一棵樹 |
| 風險 | 前端送來的樹結構錯誤 → 靠 `compile()` 驗證 | 需求一來就要改 schema，而策略檔已經存在使用者硬碟上 |
| 判決 | 設計稿要巢狀，A 直接支援 | 被「可巢狀」這一條要求直接排除 |

### 9.2 執行引擎：方案 A（攤平陣列）vs 方案 B（遞迴直譯器 + HashMap）vs 方案 C（編譯）

| 項目 | A：攤平後序陣列（**採用**） | B：遞迴走 AST + `HashMap<NodeId, State>` | C：編譯成 bytecode／Rust／WASM |
|---|---|---|---|
| 優點 | 「每節點每根都算一次」是結構保證；index 存取無 hash；不可能有環；狀態槽身分不依賴前端 | 程式碼最直觀，跟 JSON 結構一對一；好下中斷點 | 理論上最快 |
| 缺點 | 多一個攤平步驟（約 60 行）；debug 時要把 index 對回原樹 | 短路會讓有狀態節點漏根（4.1 節的具體 bug）；前端 id 要驗重複；每節點一次 hash | 要帶編譯器或 runtime 依賴；WASM 等於 1.4 節排除掉的攻擊面 |
| 風險 | 攤平邏輯本身要寫對（有測試：攤平後的 roots 能還原原樹的求值結果） | 一個依求值順序而異的訊號差異，而且**只在某些策略形狀下才出現**——最難查的那種 bug，而且正好打破「回測=實盤」這條核心原則 | 過度設計；K 線級根本不需要 |
| 判決 | 把需要紀律維持的不變量換成結構保證 | 風險不可接受 | YAGNI |

### 9.3 `at-core` 的 serde 依賴：三個選項

`at-core` 目前的 `[dependencies]` 是**空的**。這是一個刻意維持的性質。

| 項目 | A：直接加 `serde = { version="1", features=["derive"] }`（**推薦**） | B：optional feature `serde` | C：at-core 不加，在 `app/src-tauri` 寫一份平行的 DTO + 轉換 |
|---|---|---|---|
| 優點 | 最少程式碼；derive 一行搞定；`serde` 已經在 `Cargo.lock` 裡（`binance-client`、`market-stream`、`account-sync`、`src-tauri` 都在用），**沒有新的下載或編譯成本** | 保住「at-core 預設零依賴」 | at-core 完全不變 |
| 缺點 | 破壞「at-core 零依賴」這個性質 | 每個 DSL 型別上一堆 `#[cfg_attr(feature="serde", derive(...))]`；要測 feature 開關兩種組合 | 要手寫兩套型別（約 15 個 enum variant × 2）+ 轉換函式 + 轉換測試，而且兩邊會慢慢不同步——這正是最該避免的 boilerplate |
| 判決 | 乾淨，成本為零 | 為了一個美學性質付 cfg 噪音 | 最糟：最多程式碼、最容易腐爛 |

`serde_json` **不加進 at-core**。反序列化由 Tauri（已經用 serde_json）在 command 邊界
做，`at-core` 只需要 derive。測試裡要 JSON 字串時用 `dev-dependencies` 的 `serde_json`。

> 🔴 **這一項需要 CEO 明確同意**：它改變 `at-core` 的依賴性質，而且專案的
> `CLAUDE.md` 要求「新增外部套件前先說明理由」。理由就是上表 A 欄；若 CEO 傾向
> 保守，退到方案 B（optional feature），**不要**退到方案 C。

---

## 10. 魔鬼代言人：攻擊與回應

誠實標註哪些是真問題、哪些是這個設計本來就撐得住。

### 10.1 「流量 100 倍呢？」 → 單次回測沒問題，參數掃描是真的會痛

單次回測（50 節點 × 44,000 根 ≈ 2 × 10⁶ 次求值）在數十毫秒內，比讀檔還便宜。
**但設計稿的「參數穩定度熱力圖」要雙參數網格 × 10 個幣**，等於 100–1000 次完整回測，
那是 10⁸–10⁹ 次節點求值，會從「瞬間」變成「幾十秒到幾分鐘」。

**回應**：這不是直譯器的問題，也不該用「讓直譯器更快」解決。每組參數建一個獨立的
`CustomStrategy`（它們之間零共享狀態），K 線資料 `&[Bar]` 唯讀共享，用
`std::thread::scope` 分核心跑。8 核就是 8 倍。不需要新依賴，也不需要改 DSL。
**這一點要寫進 Phase 3 的驗收條件（參數掃描必須並行），不要等使用者抱怨。**

### 10.2 「第三方掛了呢？網路斷了呢？」 → 這塊真的撐得住，不用湊問題

DSL 求值是純計算：沒有網路、沒有檔案、沒有時鐘。網路斷線影響的是 K 線的供給
（`market-stream` 的重連邏輯，已經是既有範圍），不影響 DSL。
`CustomStrategy` 在沒有新 K 線時就是不被呼叫，狀態原封不動。

唯一需要確認的是：**斷線重連後補餵的 K 線有沒有重複或缺漏**。
`Strategy` 的契約寫明「同一個交易對與週期、按開盤時間遞增、每根只餵一次」，
這個保證由執行層負責，`CustomStrategy` 跟四個內建策略一樣依賴它。
DSL 沒有讓這件事變難，但它讓後果變嚴重（使用者的策略可能有 200 根的視窗，
重複餵一根會讓狀態偏掉更久）。→ 列入未決問題。

### 10.3 「資料不一致呢？回測和實盤算出不一樣的訊號呢？」 → **最嚴重的真問題，而且是既有的**

三個來源，依嚴重度排：

**(1) 冷啟動：模擬／測試網交易沒有暖機回放。**
我 grep 過 `crates/paper-trading` 與 `crates/testnet-trading`，**完全沒有 warmup 相關
的程式碼**。策略從第一根即時 K 線開始餵。所以一個 200 根視窗的策略在 1h 週期下
要空手 8 天才會出第一個訊號，而且更糟的是 Wilder 平滑（RSI、ATR）理論上記得所有
歷史（`rsi.rs:80-83` 的註解已經承認這件事），**從不同的起點開始餵會算出不同的值，
也就是不同的訊號**。

這是既有缺口，不是 DSL 造成的——但 DSL 讓它從「四個策略最多 50 根」變成
「使用者可以組出 2000 根」。**→ 列為最高優先的未決問題（11.1）。**

**(2) `f64` 成交量轉 `Fixed`。** 已處理：單一確定性的轉換函式，同一個 `f64` 永遠得到
同一個 `Fixed`，超範圍回傳 `None`。只要回測與實盤的 `Bar.volume` 位元相同，訊號相同。
（真正的風險是上游：REST 下載的 CSV 與 WebSocket 推的 `volume` 位元是否一致？
這是既有問題，不是 DSL 的。）

**(3) 短路造成的狀態漂移。** 已經用攤平陣列結構性消除（4.1 節）。這是這個設計的
主要賣點之一。

### 10.4 「被攻擊面呢？」 → 攻擊面是「資源耗盡」，不是「任意程式碼執行」

威脅模型：使用者從論壇／朋友那裡拿到一份策略 JSON 匯入。

| 攻擊 | 防法 |
|---|---|
| 任意程式碼執行 | 結構上不可能：DSL 是純資料，沒有迴圈、沒有函式、沒有 I/O（第 7 節）。**這正是把 WASM 排除在外的理由** |
| 記憶體耗盡（`period: 4294967295`） | `compile()` 的上限：period ≤ 2000、節點 ≤ 512、offset ≤ 500 → 最壞 ≈ 8 MB |
| CPU 耗盡（512 個 2000 週期節點） | 同上的上限；最壞情況一次回測慢個幾秒，不會掛 |
| 堆疊溢位（10000 層巢狀 JSON） | serde_json 的遞迴上限先擋；`compile()` 的深度 ≤ 32 再擋一次。**要寫測試證明是回錯誤不是 crash** |
| 惡意的 125 倍槓桿策略 | `at-risk-control` 的槓桿上限（預設 2 倍）會擋下單。DSL 的 `[1,125]` 只是防手滑 |
| 偷資料 | DSL 讀不到檔案、讀不到金鑰、連不了網 |

### 10.5 「一個人維護得動嗎？」 → 節點種類是唯一的複雜度來源，要守住

節點種類數 = 要維護的 `match` 分支 × 要寫的測試 × UI 要畫的積木 × 三值邏輯的邊界案例。
所以這份設計刻意**砍掉**了：`not`、算術節點、多時間週期、自訂 id。
Phase 1 是 9 個指標 + 9 個條件 + 2 個數值 = 20 種節點。

守門規則（寫進文件，之後要加節點時引用）：**新增一個節點種類，必須有一個現有
策略或使用者明確要求的策略組不出來。** 「以後可能會用到」不算理由。

### 10.6 「回滾呢？」 → 乾淨，因為是純新增

`CustomStrategy` 是新增，四個內建策略一行都不動。`build_strategy` 多一個 `match`
分支。回滾 = 前端不顯示策略編輯器入口，引擎的程式碼留著也不會被呼叫。
使用者已經存的自訂策略 JSON 不會壞（`schemaVersion` 在那裡）。

唯一有風險的是 5.0 的 `WilderAverage` → `Smoothed` refactor，它會動到 `Rsi`。
緩解：refactor 和新功能分成兩個 commit，refactor 的驗收條件是
**`Rsi` 的既有測試一個字都不改就通過**。

### 10.7 「資料量 10 倍呢？」 → 記憶體與時間都是線性的，算得出上限

K 線根數 10 倍（5 年 1h → 5 年 15m）= 時間 10 倍，記憶體不變
（節點狀態只跟 period 有關，不跟總根數有關）。單次回測從數十毫秒到數百毫秒。
這塊真的撐得住。

---

## 11. 風險與未決問題

### 11.1 🔴 模擬／測試網交易沒有暖機回放（既有缺口，DSL 讓它變嚴重）

**現況**：`at-paper-trading` 與 `at-testnet-trading` 都沒有 warmup 相關程式碼，
策略從第一根即時 K 線冷啟動。

**為什麼 DSL 讓它變嚴重**：內建策略最長 50 根視窗；DSL 允許到 2000 根。
而且 Wilder 平滑（RSI/ATR）從不同起點餵會收斂到不同的值，所以不只是「慢幾天才有
訊號」，是「訊號跟回測不一樣」。

**建議的解法**（需要獨立的一小步，不在這份 ADR 的實作範圍）：
`spawn` 時先用 `warmup_bars() × 2`（Wilder 需要額外收斂空間，`rsi.rs:80-83` 已說明）
根的歷史 K 線餵進策略，丟棄它產生的所有目標部位，然後才接即時流。
歷史 K 線的來源已經有了（`at-downloader` + `bar_store`）。

**這件事應該在 Phase 1 之前或同時處理，否則 DSL 做完也不能真的拿去模擬交易。**

### 11.2 🟡 布林通道倍數的表示法要以「重現現有 Bollinger」為準

3.2.3 節選了 `mult_num`/`mult_den` 兩個整數。但如果現有 `Bollinger`（收 `Fixed` 倍數）
的截尾行為必須被完全重現（2.8 的 Python 對照驗證可能依賴它），就改用 `Fixed` 字串。
**以「DSL 版與 Rust 版逐根相等」的測試結果為準**，實作時第一個寫這個測試。

### 11.3 🟡 UI 怎麼防止使用者組出無意義的策略

DSL 的驗證只擋「結構錯誤」與「資源耗盡」，不擋「邏輯上沒意義」：
- `rsi(14) > 200`（永遠不成立，RSI 上限 100）
- `longEntry` 與 `longExit` 同時成立 → 永遠空手（引擎行為是對的，但使用者不懂為什麼）
- 進場條件與出場條件完全相同 → 退化成無狀態的比大小

**建議**：這些是 UI 的「健檢提示」而不是引擎的錯誤（設計稿的回測頁本來就有
「健檢摘要（良好/注意項目列表）」）。引擎可以多提供一個
`fn lint(&self) -> Vec<Warning>` 給 UI 用，但這是 Phase 3 的事，不阻擋 Phase 1。

### 11.4 🟡 策略檔的儲存位置與格式

`StrategyDoc` 存哪裡？Phase 1 的最小可行解：`app_data_dir/strategies/<id>.json`
（Tauri 的 `app_data_dir`），一個策略一個檔。
不需要 SQLite（策略數量是幾十個，不是幾萬個）。
版本管理（設計稿有「策略名稱/版本」）用檔名或檔內的 `version` 欄位遞增即可，
不需要 git-like 的歷史。這部分屬於前端/App 層的設計，列在這裡是為了避免有人
以為需要先建資料庫。

### 11.5 🟢 未決但不阻擋：合約資料積木的資料來源

8.2 節的 `MarketContext` 方案（選項 b）要先有資金費率／標記價格的歷史序列。
Binance 有 funding rate 的歷史 API。這是一塊獨立的工作（下載器 + 儲存格式 +
回測時的對齊），列入 roadmap 但不阻擋 Phase 1。

---

## 12. 對現有程式的影響範圍

| 檔案／模組 | 動作 | 大小估計 |
|---|---|---|
| `crates/core/Cargo.toml` | 加 `serde`（derive）依賴 + `serde_json` dev-dependency | 2 行（🔴 需 CEO 同意，見 9.3） |
| `crates/core/src/strategy_dsl/mod.rs` | **新增**：AST 型別、`compile()`、`DslError` | ~400 行 |
| `crates/core/src/strategy_dsl/eval.rs` | **新增**：節點求值 + `CustomStrategy impl Strategy` | ~300 行 |
| `crates/core/src/strategy_dsl/indicators.rs` | **新增**：`Smoothed`、EMA、MACD、ATR | ~200 行 |
| `crates/core/src/strategies/rsi.rs` | **改**：`WilderAverage` → 用新的 `Smoothed` | ~20 行（行為必須不變） |
| `crates/core/src/strategies/mod.rs` | **改**：`Window` 目前是 `strategies` 模組私有的（`struct Window`，`mod.rs:91`），要連它的方法一起改成 `pub(crate)` 才能給同層的 `strategy_dsl` 用。`non_zero` 同理 | ~8 行 |
| `crates/core/src/lib.rs` | **改**：`pub mod strategy_dsl;` + re-export | ~3 行 |
| `app/src-tauri/src/backtest.rs` | **改**：`BacktestRequest` 加 `ast` 欄位；`build_strategy` 多一個 `"custom"` 分支 | ~15 行 |
| `app/src-tauri/src/paper_trading.rs` | **改**：把 `ast` 一起傳給 `build_strategy` | ~5 行 |
| `app/src-tauri/src/lib.rs` | **改**：新增 `validate_strategy_ast` command（前端即時驗證用） | ~20 行 |
| `app/src/` 前端 | **新增**：策略編輯器頁面 + DSL 的 TypeScript 型別 | Phase 3，獨立估算 |
| `crates/engine`、`paper-trading`、`testnet-trading`、`risk-control`、`binance-client` | **不動** | 0 行 |
| `crates/core/src/strategy.rs`（`Strategy` trait） | **不動** | 0 行 |

---

## 13. 前端 ↔ DSL 的資料交換（簡述，Phase 3 細化）

### 13.1 格式

JSON，就是第 3 節的 schema。Tauri 的 command 參數已經走 serde_json，所以前端送
一個 JS 物件、Rust 端收到 `StrategyAst`，中間不需要自己寫任何解析。

TypeScript 型別手寫一份與 Rust 對應的 discriminated union（`kind` 當 discriminant）：

```ts
type Expr =
  | { kind: "price"; field: "open"|"high"|"low"|"close"|"volume"; offset?: number }
  | { kind: "number"; value: string }
  | { kind: "indicator"; name: IndicatorName; source?: Expr | null;
      params: Record<string, number>; output?: string | null; offset?: number };

type Cond =
  | { kind: "gt"|"gte"|"lt"|"lte"|"cross_above"|"cross_below"; left: Expr; right: Expr }
  | { kind: "all"|"any"; children: Cond[] }
  | { kind: "sustained"; bars: number; inner: Cond };
```

**不要自動生成**（ts-rs／schemars 等於為兩個 enum 引入一個 codegen 依賴與一個
build step）。手寫一份 ~40 行的型別，再用一個**契約測試**鎖住兩邊一致：
Rust 端輸出一份「每種節點各一個的範例 JSON」到檔案，前端的 vitest 讀這個檔案
並用 TS 型別驗證它。不同步時測試會紅。

> 專案的既有陷阱 #1（前後端命名風格不一致）在這裡要注意：Rust 端用
> `#[serde(rename_all = "camelCase")]` 處理欄位名（`schemaVersion`、`longEntry`、
> `positionPct`、`mult_num` → `multNum`），而 `kind` 的**值**用 `snake_case`
> （`cross_above`），因為它們是識別字不是欄位名。兩個規則不一樣，要在型別上
> 註解清楚，並用序列化測試鎖住。

### 13.2 三個 Tauri command

| command | 用途 |
|---|---|
| `validate_strategy_ast(ast)` | 使用者每拖一顆積木就呼叫一次；回傳 `Ok { warmupBars }` 或 `Err { 路徑 + 繁中訊息 }`。讓編輯器即時紅字，不用等按回測 |
| `run_backtest_command(request)` | 既有 command，`request` 多一個 `ast` 欄位 |
| `save/load_strategy_doc` | `app_data_dir/strategies/<id>.json` 的讀寫（11.4） |

前端**不自己實作任何指標計算**。設計稿右側的「即時預覽 K 線圖 + 快速回測結果卡」
要的指標線與回測數字，一律呼叫 `run_backtest_command`（短區間）拿回來畫。
在前端用 JS 算一條 SMA 畫在圖上，就等於有了第二套實作，兩套一定會不一致——
這正是這份 ADR 最想避免的事。
> 如果「每拖一顆積木就跑一次短回測」在實測中太慢，解法是 debounce + 縮短預覽區間
> （例如最近 500 根），**不是**在前端算一份。

---

## 14. 分階段實作建議

每一階段結束都停下來讓使用者驗證（照 `CLAUDE.md` 的子步驟流程）。
**Phase 1 與 Phase 2 之間要過一輪跨模型審查**（這是風險最高的一塊，而且錯誤會
直接表現成「回測與實盤訊號不同」這種最難查的問題）。

### Phase 0：暖機回放（前置，獨立一小步）

11.1 節的問題。`at-paper-trading` / `at-testnet-trading` 在 `spawn` 時先餵
`warmup_bars() × 2` 根歷史 K 線並丟棄其輸出。
**驗收**：一個測試證明「先餵 100 根歷史再接即時流」與「一次餵完 100+N 根」
對同一個策略產生相同的後 N 個目標部位。

> 為什麼排在最前面：不做這一步，Phase 1 做完的自訂策略在模擬交易裡的行為跟回測
> 不一樣，而那是本專案唯一不能妥協的性質。而且它跟 DSL 完全獨立，可以先做。

### Phase 1：Rust 執行引擎（本 ADR 的核心，不碰 UI）

1. `Smoothed`（泛化 `WilderAverage`）+ EMA / MACD / ATR，各自帶第 5 節的測試向量。
   **`Rsi` 的既有測試不改一字就要通過。**
2. AST 型別 + serde derive + `compile()` 的全部驗證（4.4 節的表逐項一個測試）。
3. 攤平 + 求值 + `CustomStrategy impl Strategy`。
4. **驗收核心（四個等價測試）**：用 DSL 寫出 `SmaCross(10,50)`、`Bollinger(20,2)`、
   `Donchian(20,10)`、`Rsi(14,30,70)`，餵同一串 K 線（含極端值，重用
   `strategies/mod.rs` 的 `test_util::extreme_bars`），**每一根的 `TargetPosition`
   逐根相等**。
5. 安全邊界測試：`at-core` 的依賴清單只有 serde；超深 JSON 回錯誤不 crash；
   超大 period 被擋。
6. `app/src-tauri` 接上 `"custom"` 分支 + `validate_strategy_ast` command。
7. 手刻一份 JSON（第 3.8 節那個）跑一次真實回測，貼出輸出。

**停下來。** 這一步結束時使用者可以用手寫 JSON 跑自訂策略回測，沒有 UI。

### Phase 2：跨模型審查（不寫程式）

把 Phase 1 的程式碼交給另一個模型／另一個 agent 審，重點問三件事：

1. 有沒有任何路徑讓某個有狀態的節點漏掉一根 K 線？
2. 四個等價測試真的覆蓋了「出場優先」「暖機清狀態」「多空同時成立」三個邊界嗎？
3. `compile()` 的驗證有漏掉哪個會讓 `on_bar` panic 或吃爆記憶體的輸入？

### Phase 3：積木 UI

積木庫（合約資料分類 `disabled`）、巢狀條件群組、即時驗證紅字、
部位設定、預覽圖（呼叫後端短回測）、策略存檔／讀檔、WASM 入口 `disabled`。

### Phase 4：之後（各自獨立評估）

- 參數掃描的並行（10.1）
- `MarketContext` + 合約資料積木（8.2）
- 停損／移動停損在執行層的實作（8.3）
- 算術節點、多時間週期（8.4）
- WASM 匯入（需要獨立的安全 ADR，1.4）

---

## 15. 後果

**好的**

- 使用者能組自己的策略，而且組出來的東西在回測／模擬／測試網跑的是**同一份
  執行引擎**——本專案最核心的性質被保住了。
- `Strategy` trait、四個內建策略、三條執行路徑**零改動**。
- 多了 EMA／MACD／ATR 三個指標，內建策略之後想用也可以。
- 「使用者能表達什麼」被一個小而明確的節點清單界定住了，不是一個會長成腳本語言的
  開放式系統。
- 攻擊面被結構性地限制在「資源耗盡」，而且有明確上限。

**要付的代價**

- `at-core` 不再是零依賴（serde）。
- 多 ~900 行 Rust 要維護，其中最微妙的是三值邏輯與「出場優先」的順序——這些靠
  四個等價測試鎖住。
- 「使用者組出來的策略不會賺錢」會變成一個新的支援負擔。這不是技術問題，但設計稿
  的「部署前檢查」清單正是為了這個而存在。
- 設計稿的編輯器有一部分功能 Phase 1 做不到（合約資料積木、停損），UI 必須誠實地
  `disabled` + 說明，不可以放假按鈕。

**不做這件事的後果**

策略編輯器頁面只能是四個內建策略的參數調整器，跟設計稿差距最大的一塊永遠補不上。
而如果用「前端 JS 算一算」的捷徑做出視覺效果，就等於有兩套策略邏輯，
回測與實盤必然不一致——那會是比沒做更糟的結果。
