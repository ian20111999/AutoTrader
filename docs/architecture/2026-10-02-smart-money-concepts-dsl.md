# ADR-004：把「聰明錢」（Smart Money Concepts / ICT）的 Order Block、Fair Value
Gap、Break of Structure 接進 `at_core::strategy_dsl`

> **重建說明**：這份文件是對原始設計 agent 交付內容的忠實重建。原始 837 行文件在
> COO 清理 worktree 時被誤刪（未 commit 就被 `git worktree remove --force`
> 清掉），這份重建版本依據 agent 回報時的完整交付摘要重寫，保留所有數值、公式、
> 決策理由跟誠實揭露的限制；行號/章節編號可能跟原始文件不完全一致，但實質內容
> 一致。跟既有文件一樣，編號撞到既有的 `docs/architecture/` 清單（已經有兩份
> ADR-001、一份 ADR-002），這份取 ADR-004，沿用上一份（`2026-10-02-bar-order-flow-
> fields.md`）取 ADR-003 時的同一個理由：不回頭重編既有文件。

## 1. 背景

CEO 要求豐富策略庫，點名「聰明錢」（Smart Money Concepts / ICT 交易方法論）。核心
概念定義（已查證，不是憑記憶）：

- **Order Block（訂單塊）**：強勁突破走勢發生前，最後一根「方向相反」的K線所在的
  價格區間（例如一段強力上漲前的最後一根下跌K線，那根K線的高低點範圍之後常被視為
  支撐）。
- **Fair Value Gap（FVG，公允價值缺口）**：三根連續K線構成的缺口——中間那根走勢
  劇烈，導致第一根的高點跟第三根的低點之間（做多方向）或第一根低點跟第三根高點
  之間（做空方向）留下一段沒有真正雙向交易過的價格空白，之後價格常會回來「回補」
  這段空白。
- **Break of Structure（BOS，結構突破）**：趨勢延續訊號——上升趨勢中創出新的更高
  高點（Higher High），或下降趨勢中創出新的更低低點（Lower Low）。跟它相對的
  「Change of Character / CHoCH」才是反轉訊號，這份文件不處理 CHoCH。

這三個概念的共同前提都需要先找出「最近的擺動高點/擺動低點」（swing high/low，
局部極值），BOS 跟 Order Block 都是建立在擺動高低點之上的派生判斷。

## 2. 任務範圍與約束

設計怎麼把這三個概念接進現有的 `at_core::strategy_dsl`，**不重新設計整個 DSL**：
現有的 `Expr`/`Cond` AST、`compile()` 信任邊界（≤512 nodes、≤32 depth）、
`PriceField`/`price_field()` 讀取入口、`Smoothed`（EMA/Wilder 平滑通用結構）都已經
存在且經過審查，這是在這個既有架構上**新增節點類型**，不是重做。

範圍邊界：只涵蓋 Order Block、FVG、BOS 三個概念（Order Block 可縮小範圍不做，見
下文），不涵蓋 CHoCH、Liquidity Sweep、Premium/Discount Zone 等其他 ICT/SMC 概念；
不設計前端積木 UI；不涵蓋訂單簿/造市（平行的另一份工作）。

## 3. 決策摘要

**本次做：只有「擺動高低點」一個新東西。** 兩個 `IndicatorName`
（`swing_high`／`swing_low`），參數 `left`／`right`，`output: last | previous`，
一個約 45 行的 `PivotTracker` 狀態機。**新增 `Expr`／`Cond` variant：0 個。新增
依賴：0 個。新增 crate：0 個。**

**FVG 與 BOS 的引擎 diff 是空的** —— 這是本次最大的發現，兩者都不需要任何新節點：

- **FVG 今天就寫得出來**：做多缺口 = `gt(price(low, offset:0), price(high,
  offset:2))`；做空 = `lt(price(high, offset:0), price(low, offset:2))`。
  逐項核對過：`Delay::step`（`eval.rs:217-232`）在緩衝未滿時回 `None` → 前兩根
  自動空手；`warmups()` 的 `1 + offset` 規則（`eval.rs:671`）讓暖機自動等於 3 根；
  「中間那根走勢劇烈」在數學上已被 `low[t] > high[t-2]` 蘊含，不需額外條件。加一個
  `bullish_fvg` 指標等於為「換個名字」寫一個狀態機 + 暖機規則 + 序列化名稱 + 一組
  測試，而引擎已經算對了。
- **BOS 也不需要新節點**，而且有兩種公式：
  - ① `gt(close, swing_high(last))`（**推薦**，延遲只有既有的 1 根）
  - ② `gt(swing_high(last), swing_high(previous))`（任務書定義的 HH>HH，延遲
    `right`+1 根）

  推薦 ①，它更接近交易者實際說的 BOS、而且明顯更早——②必須等**新的**轉折在舊
  高點上方成形並確認，系統性地晚。兩個都免費提供，`output: previous` 的存在理由
  就是公式 ②（表達「結構在創更高高點」這個**狀態**）。

**延後：Order Block**（任務書允許的範圍縮小）。理由不是「太難」，而是一條清楚的
架構界線：本次交付的每一個節點都是「由一個**有界的已收盤 K 線視窗**完全決定的
事實」——可手算、可寫死測試向量、沒有生命週期、不可能有陳舊狀態。Order Block 與
「未填補缺口的回補進場」需要的是**同一個**被延後的東西：**價格區間的生命週期**
（追蹤幾個／什麼事件讓它失效／重疊怎麼合併），而且它還需要邊緣事件（「突破發生
在哪一根」）——這是三個概念裡唯一業界沒有共識的部分。下一個 Phase 用獨立 ADR
一次把 Order Block、未填補 FVG 回補（可能還有 Liquidity Sweep）設計完。縮小後
仍能組出完整可回測的 SMC 策略（§6.3：10 個節點、深度 3，見下文範例）。

## 4. 擺動高低點（Swing High/Low）設計

### 4.1 延遲不等於新狀態——本文件最重要的設計決定

一根K線「是不是擺動高點」只有在看到它右側足夠的根數（`right` 參數）之後才能
確定——這個判斷天生有延遲，不可能在這根剛收盤那一刻就知道。

解法：節點語意定成「**最近一個已確認的擺動點價位**」，而不是「這根是不是擺動
點」。於是節點對最近 `right` 根**不作任何宣稱** → 沒有「待確認」狀態需要表示／
傳播／讓下游判斷 → 直接複用既有 `None` → Kleene 三值邏輯 → `step_held` 清狀態 →
空手。

這條路徑跟 ADR-003（訂單流欄位）處理「這個指標在某些情況下沒有意義」的三值邏輯
模式完全一致，但連新型別都不需要（ADR-003 要加 1 個 `OrderFlow` 型別，這次 0 個）。
形狀等同既有 `donchian` 配 `offset: 1`（「落後的事實」），不是新概念。

### 4.2 `output: last | previous`

- `last`：最近一個已確認的擺動點價位——BOS 公式①（推薦）用這個。
- `previous`：上一個已確認的擺動點價位——BOS 公式②（HH>HH）用這個，也是這個
  參數存在的唯一理由。

### 4.3 嚴格不等式

兩側都用 `>`／`<`（不是 `>=`/`<=`）。平盤序列永不產生擺動點 → 恆 `None` → 空手
（安全方向，跟既有「訊號矛盾／不明確時空手」立場一致）。

### 4.4 不接受 `source` 參數——這是正確性需求，不是慣例

`eval.rs:472-486` 在 source 為 `None` 時**不餵指標**，若 swing 接受 source，
「左右各 N 根」會悄悄變成「左右各 N 根**有值的**K 線」——節點的根數計數和真實
K 線序列脫鉗，和 ADR-001（strategy-dsl 原始設計）§4.1 力求避免的短路求值 bug 是
同一類：不 panic、不報錯，只算出時間軸錯位的訊號。

### 4.5 跟 Donchian/`Window` 的親緣評估（任務書點名要求）

結論：**不共用程式碼，只共用模式**。既有 `Window` 沒有索引存取（pivot 要讀
`buf[left]`）、也沒有「保持上一個結論」的概念；加這兩件事只為一個呼叫端，等於把
`Window` 變成兩個指標的聯集。真正共用的祖先是「有界的 `VecDeque<Fixed>`」，`std`
已提供。

附帶記錄一個**今天就能用的近似**：`gte(price(high, offset=R), highest(period=
L+R+1, source=price(high)))` 已能表達「R 根前那根是視窗最高點」，但它是非嚴格的、
而且拿不到「最近確認的擺動高點**價位**」（BOS 要比較的正是那個數字），所以不能
取代真正的 swing 節點。

### 4.6 暖機下界計算（修正過一次，避免實作者抄錯）

初稿把 `output: previous` 的暖機下界寫成 `L+R+1+max(L,R)`，複查後是錯的。

**正確公式：`left + right + min(left, right) + 2`**。

推導：兩個擺動點的間距 `d` 下界是 `min(L,R)+1`，不是 `max(L,R)+1`——只有
`d ≤ L` **且** `d ≤ R` 時才矛盾；`L=3, R=1` 時 `d=2` 是合法的（`d ≤ R=1` 不成立，
所以不矛盾）。

範例：`L=R=2` → 8 根（A 最早在第 3 根確認／第 5 根其實是第一個轉折，B 最早第 6
根／第 8 根確認）。實作時要用「最緊」序列實測這個下界，並另測 `L=3, R=1` 驗證
是 `min` 不是 `max`。

### 4.7 暖機只是下界，不是正確性需求——務必確認

這是 DSL 第一個「暖機聲明不足只會少空手幾根、不會算錯訊號」的節點。已查證：
`warmup_bars()` 在 `on_bar` 裡沒有用途、`run_backtest` 完全不呼叫它（grep 實測），
只影響即時模式（paper/testnet）要抓多少根歷史暖機。宣告不足的後果是「啟動後多
空手幾根」，不是錯訊號。既有 `WARMUP_SAFETY_FACTOR = 5` 已是緩衝（`L=R=2` → 宣告
8 → 實際抓 40 根）。大設定（例如 `L=R=20` → 宣告 62 → 抓 310 根）的殘餘風險列為
🟡，**建議不要為了這一個節點改動 `WARMUP_SAFETY_FACTOR` 這個全域常數**——那是錯的
槓桿，會影響所有既有策略的暖機行為。

## 5. FVG 設計

三根K線的缺口判斷不像擺動高低點，不需要「等未來確認」的延遲問題——三根都收盤了
就能立刻判斷。直接用既有的 `gt`/`lt` + `price(..., offset:N)` 組合表達，不新增
任何 `Expr`/`Cond` variant（見第 3 節的公式）。

**誠實限制**：這個版本是「影線版」（用 high/low），沒有缺口大小濾網（例如「缺口
至少要有收盤價的 0.1% 寬」這種門檻）。兩者都需要 `Expr::Arith`（算術運算節點），
而那是 ADR-001（原始 strategy-dsl 設計）§3.2.4 **已經記錄的既有缺口**——`Expr::Arith`
的價值遠大於只為 FVG 一個用例而加（還能解開 `close > sma(20) × 1.02` 這種常見寫法、
以及 ADR-003 §10.2 提過的 CVD 累積成交量差），應當作獨立的事評估，不要被 SMC
夾帶進來這份設計。

## 6. BOS 設計

建立在擺動高低點之上，兩個公式（見第 3 節）：

- 公式①（推薦）：`gt(close, swing_high(last))`——延遲只有既有的 1 根（訊號在
  收盤產生、下一根開盤成交，這是全專案既有的延遲，沒有額外疊加）。
- 公式②：`gt(swing_high(last), swing_high(previous))`——延遲是 `right + 1` 根
  （預設參數下約 3 根／3 小時，視週期而定）。

**BOS 延遲是必然的，不可優化**：在候選轉折確認前就宣稱＝偷看未來，回測會在
實盤重現不了。UI 必須標示「右側根數＝確認延遲根數」。禁止用「尚未確認的候選
轉折」換即時性，要寫進程式碼註解，不能只寄望使用者自己看懂。

**BOS 不是 CHoCH，分不出延續與反轉**：機械定義沒有趨勢狀態概念，下降趨勢中
100→90→95 會被判成「更高高點」（技術上沒錯，但交易者說的 BOS 通常隱含「原有
趨勢方向」的上下文，這個版本沒有）。CHoCH（結構反轉判斷）明確排除在這份文件
範圍外。

**BOS 與 FVG 都是「狀態」不是「事件」**：一旦條件成立，會持續成立好幾根（直到
下一個相反的擺動點出現）。在既有「進場樹＋出場樹＋hysteresis」模型下可以運作，
但「只在突破發生的那一根進場」這種寫法目前寫不出來——**缺的不是 BOS 節點，是一個
通用的 `Cond::RisingEdge`**（偵測「這一刻才從否轉是」的邊緣，一個新 variant、
一個狀態槽、跟既有 `NodeState::Cross` 同形狀，對 BOS/FVG 以外的每一種條件都有
通用價值）。列為未決問題，建議跟下一個 Phase 的「區間生命週期」ADR 一起決定要
不要做，不要為了這份文件單獨加。

## 7. Order Block（延後，不在這次範圍內）

Order Block 的定義建立在 BOS 之上（「強勁突破走勢」= BOS 發生、「最後一根方向
相反的K線」= BOS 發生前，往回找最近一根跟突破方向相反的K線）。

延後理由（不是工程難度，是架構界線）：本次交付的每個節點都是「有界已收盤K線
視窗完全決定的事實」——沒有生命週期，不會過期。Order Block 需要的是一個全新
類別的東西：**追蹤一個價格區間，決定什麼事件讓它失效、重疊的區間怎麼合併**，
這跟「未填補 FVG 的回補進場」是同一個被延後的概念（兩者都需要「區間存續期」）。
而且 Order Block 還需要邊緣事件判斷（「突破發生在哪一根」），這是三個 SMC 概念
裡業界對精確定義最沒有共識的一個。

建議下一個 Phase 用獨立 ADR，把 Order Block、未填補 FVG 回補、（可能還有
Liquidity Sweep）一次設計完——它們共用同一套「區間生命週期」基礎設施，分開做
會重複發明。

縮小範圍後，這次交付的三個概念（擺動高低點 + FVG + BOS）仍能組出一個完整可回測
的 SMC 風格策略。範例（做多進場）：

```json
{
  "kind": "all",
  "children": [
    { "kind": "gt",
      "left": { "kind": "price", "field": "close" },
      "right": { "kind": "indicator", "name": "swing_high",
                 "params": { "left": 2, "right": 2, "output": "last" } } },
    { "kind": "gt",
      "left": { "kind": "price", "field": "low", "offset": 0 },
      "right": { "kind": "price", "field": "high", "offset": 2 } }
  ]
}
```

（BOS 確認收盤價突破最近擺動高點 + FVG 確認缺口存在，約 10 個節點、深度 3，
在既有 `compile()` 信任邊界內游刃有餘，見第 8 節。）

## 8. 信任邊界：不需要調整

實測算式：BOS 3 個節點、FVG 3 個節節點、上面範例完整策略 10 個節點、刻意寫一個
極端多空都用的策略約 40 個節點——離 `MAX_NODES=512`／`MAX_DEPTH=32` 有 12 倍以上
margin。擺動高低點歸入 `takes_whole_bar` 類別所以**不展開子節點**，甚至比 `sma`
（會多推一個預設的 `price(close)` 子節點）還省一個節點。

唯一要改的是 `mod.rs:83-87` 的記憶體上限 doc comment（8 MB → 16 MB，因為 swing
的內部緩衝區是 `left + right + 1` 個 `Fixed`，比既有節點稍大，但仍是常數級）。

**三處必須同步改的參數拒絕清單**（漏了任何一處都會讓非法參數靜默通過）：
- `only_period()`（`mod.rs:661-670`）
- `bb`（布林通道）分支
- `macd` 分支

這三處目前應該都是「只接受 `period` 參數,其餘一律拒絕」的檢查邏輯，加入
`swing_high`/`swing_low` 的 `left`/`right`/`output` 參數後要確認這三處不會讓
類似 `sma { period: 10, left: 3 }`（多塞一個不相關的 `left` 參數）靜默通過
驗證。**這一條必須寫成測試**，不能只憑人工檢查。

## 9. 影響範圍

### 9.1 不改（diff 必須是空的）

- `crates/core/src/bar.rs`（`Bar` 型別本身不受影響）
- `Strategy` trait（不新增方法，這次全部在 DSL 層完成）
- 任何認證/付費/下單路徑
- `SCHEMA_VERSION`
- 信任邊界的**數值**（`MAX_NODES`/`MAX_DEPTH` 不變，只有記憶體上限的*doc comment*
  數字變）

### 9.2 修改（Rust，都只是加 match arm／加欄位，沒有函式簽名或控制流改動）

- `crates/core/src/strategy_dsl/mod.rs`：`IndicatorName` 加 `SwingHigh`/`SwingLow`
  兩個 variant；三處參數拒絕清單（見第 8 節）同步更新；記憶體上限 doc comment。
- `crates/core/src/strategy_dsl/indicators.rs`：新增 `PivotTracker` 狀態機
  （約 45 行）。
- `crates/core/src/strategy_dsl/eval.rs`：`IndicatorName::SwingHigh`/`SwingLow`
  的求值邏輯接進既有的指標求值分支；暖機下界公式（第 4.6 節）。
- `crates/core/src/strategy_dsl/serialization.rs`：新增的兩個 `IndicatorName`
  的 JSON `name` 字串。
- `crates/core/src/strategy_dsl/tests.rs`：新增測試（見第 9.4 節）。

（FVG、BOS 不需要任何引擎層改動，純粹是使用者在前端組出既有節點的組合——見下方
前端 Phase 的範圍。）

### 9.3 前端（下一個 Phase 的範圍，這份文件不實作）

- `app/src/strategyDslTypes.ts`：`IndicatorName` union 加兩個新值。
- `app/src/StrategyEditor.tsx`：積木庫加「擺動高點」「擺動低點」指標選項；可能
  提供 FVG/BOS 的「複合積木」捷徑（直接插入上面範例的 JSON 片段，而不是要使用者
  自己手動拼 `gt`/`offset`），這個 UX 決定交給下一個 Phase 判斷。
- `app/src/strategyEditorModel.ts`：**必須修正一個既有 bug**（見第 10 節），
  不是這次新增的範圍,但是這次新節點會讓它第一次真正發作,必須在前端 Phase 裡
  一起修。
- `app/src/StrategyEditor.test.tsx`：對應測試。

### 9.4 新增測試（驗收條件）

1. `PivotTracker` 基本案例：單一明確的局部極值被正確偵測跟確認。
2. 延遲驗證：擺動點在確認前的 `right` 根回 `None`，確認後才回值。
3. 嚴格不等式：平盤序列（連續相同高點）永遠不產生擺動點。
4. `output: last` vs `output: previous` 行為差異的對照測試。
5. 暖機下界：用 `L=R=2`（期望 8 根）與 `L=3, R=1`（驗證是 `min` 不是 `max`）
   兩組「最緊」序列實測第 4.6 節的公式。
6. 三處參數拒絕清單（第 8 節）：用一個混了 `swing` 專屬參數的 `sma`/`bb`/`macd`
   去 `compile()`，驗證全部被拒絕，不是靜默通過。
7. FVG 用既有節點組合（第 3 節公式）正確判斷做多/做空缺口存在與否的案例。
8. BOS 公式①、②分別驗證延遲行為符合第 6 節的描述。
9. 信任邊界：驗證一個 ~40 節點的極端策略仍在 `MAX_NODES`/`MAX_DEPTH` 內通過
   `compile()`。
10. **因果性測試（最重要）**：完整序列餵進去逐根求值的輸出，必須跟「每次只餵
    一個遞增前綴」逐根求值在共同重疊的部分完全相等——這是結構性證明沒有
    look-ahead（偷看未來），不能只靠人工檢查邏輯「看起來」正確。
11. 不接受 `source` 參數：嘗試給 `swing_high` 傳 `source` 應該在 `compile()`
    階段被拒絕（或忽略，視實作決定，但要有明確測試覆蓋這個邊界）。
12. 序列化往返：新增的 `IndicatorName` JSON 字串能正確序列化/反序列化。

### 9.5 實作順序（每一步都能獨立驗證）

1. `IndicatorName` 加兩個 variant + 三處參數拒絕清單同步更新（先讓 `compile()`
   認得新節點、正確拒絕非法參數組合）。
2. `PivotTracker` 狀態機（純邏輯單元，可以完全獨立於 DSL 引擎寫測試）。
3. 接進 `eval.rs` 的求值分支（`output: last`）。
4. 補 `output: previous`（依賴上一步，驗證暖機公式）。
5. 因果性測試 + FVG/BOS 組合策略的端對端驗證（第 9.4 節第 10 條）。

## 10. 魔鬼代言人：發現一個前端既有 bug（會造成錯訊號，不只是 UX）

`app/src/strategyEditorModel.ts` 的 `mirrorCond()`（約第 120-131 行）只翻轉
比較運算子（`gt`↔`lt`），**不動 `Expr` 本身**。

- 做多 FVG `gt(low[0], high[2])` 鏡像成做空時變成 `lt(low[0], high[2])`——這是
  **錯的**，正確應該是 `lt(high[0], low[2])`（左右兩個 `Expr` 也要互換，不只是
  運算子）。
- BOS 同理：做多 `gt(close, swing_high(last))` 鏡像會得到
  `lt(close, swing_high(last))`，而正確應該是 `lt(close, swing_low(last))`
  （連指標本身——`swing_high`→`swing_low`——都要換，不只是比較運算子）。

既有的 `sma`/`rsi` 等指標因為左右兩側通常是「指標 vs 數字常數」這種不對稱結構，
鏡像時剛好不需要換 `Expr`，所以這個 bug 到現在都沒有發作過。**這次新增的節點會
讓它第一次真正產生錯誤訊號**，必須在前端 Phase 的驗收條件裡明確列出並修正，
不能被當成「順便」的小事忽略。

## 11. 未決問題

### 11.1 🟡 `Cond::RisingEdge`（邊緣偵測）要不要做

BOS/FVG 都是「狀態」不是「事件」，缺的通用解法是一個新的 `Cond` variant。建議
跟下一個 Phase 的「區間生命週期」ADR（Order Block）一起評估，不要為這份文件
單獨加——這個決定可能會影響那份 ADR 的設計方向。

### 11.2 🟡 FVG 要不要加缺口大小濾網

需要 `Expr::Arith`（算術運算節點），這是 ADR-001 已記錄的既有缺口，建議獨立
評估（價值不只在 FVG），不要被這份文件夾帶決定。

### 11.3 🟡 大 `left`/`right` 設定的暖機殘餘風險

見第 4.7 節，建議不處理（不改全域 `WARMUP_SAFETY_FACTOR`），只在文件/UI 提示
使用者「`left`/`right` 設太大時啟動後會多空手一段」。

### 11.4 給 CEO 的決策點

**🔴 決策點：0 個。** 全部落在 🟡：不新增依賴、不新增 crate、不碰 `Bar`、不碰
`Strategy` trait、不碰認證/付費/下單路徑、不動任何檔案格式、不改
`SCHEMA_VERSION`、不改任何信任邊界的**數值**（只改 doc comment 的敘述數字）。

需要**報告但不需等同意**的兩件事：
1. 範圍縮小——Order Block 本次不做，留給下一個獨立 Phase。
2. 結構類訊號（BOS）比均線類指標多 `right` 根延遲，這是定義本身造成的、不可
   優化，不是實作疏漏。

## 12. 後果

### 得到什麼

- 使用者可以在策略編輯器組出 FVG、BOS 兩種聰明錢概念驅動的策略,完全用既有
  DSL 節點,沒有新增任何引擎層的 `Expr`/`Cond` variant。
- 擺動高低點（`swing_high`/`swing_low`）是一個通用的新指標,除了支撐 BOS,
  使用者也可以直接拿來做其他用途（例如傳統的 ZigZag / 轉折點交易邏輯）。
- 信任邊界、暖機機制、三值邏輯模型全部沿用既有基礎設施,沒有引入新的正確性
  風險類別。

### 付出什麼

- 兩個新 `IndicatorName`、一個 ~45 行的狀態機、三處參數拒絕清單要同步維護。
- 記憶體上限文件需要更新（數字變大,但仍是常數級,非結構性風險）。
- 前端要修一個既有的 `mirrorCond()` bug（不修的話,FVG/BOS 的「做空」版本
  會產生錯誤訊號,比沒有這個功能更糟）。

### 不做什麼（明確劃掉）

- Order Block——延後到獨立 Phase（需要「區間生命週期」基礎設施）。
- CHoCH（結構反轉判斷）——明確排除,不是這份文件的範圍。
- Liquidity Sweep、Premium/Discount Zone——不在這次的三個概念清單內。
- `Cond::RisingEdge`（邊緣偵測）——列為未決問題,不在這次做。
- FVG 缺口大小濾網（需要 `Expr::Arith`）——列為未決問題,獨立評估。
- 前端積木 UI 怎麼呈現——下一個 Phase 的範圍。
