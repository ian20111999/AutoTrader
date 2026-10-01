# 2026-10-01 對照 wireframe 補齊介面與功能落差 — 執行計畫

## 背景

使用者要求核對桌面 App 是否完整照著 wireframe（claude.ai Design canvas
`NUreRyfxXWXz8m53onzHG3`）做。派設計部門逐頁核對後，結果是 9 個有對應設計稿的畫面裡
**0 個完全吻合、2 個大致吻合有出入（SideNav、Compare）、7 個明顯落差**。使用者已明確
同意：「有動架構的也可以動，我要的是你做到好再來找我」——授權涵蓋架構層級變動，且不需要
過程中逐步回報，完成後才一次報告。

這跟 ROADMAP 1-8 的「一次一個小步驟」不是同一種任務：wireframe 本身描繪的是功能完整的
專業量化終端，現有實作停在「單一策略、單一交易對」的早期階段。這份計畫把落差拆成可以
獨立驗證的分期，沿用全專案一致的紀律（先寫測試、`cargo fmt`/`clippy`/`test`、前端
`npm test`/`lint`/`build`、money-critical 或架構層級的東西要獨立審查），但不走
「做完一步就停下來等確認」——做完整個計畫才回報。

## 落差全貌（設計部門核對結果）

| 畫面 | 吻合度 | 需要新架構？ |
|---|---|---|
| 總覽 Main | 明顯落差（整頁不存在） | 是（session registry + 歷史持久化） |
| TopBar | 明顯落差（刻意延後，有註解） | 依賴總覽的資料來源 |
| SideNav | 大致吻合有出入 | 否（補導覽項，指向未完成頁面要用「即將推出」佔位） |
| 策略庫 Strategies | 明顯落差 | 部分（分頁/篩選可做；範本庫/新增策略流程需要使用者自訂策略，見策略編輯器） |
| 策略編輯器 StrategyEditor | 落差最大（完全不同範式） | 是（策略 DSL + 編譯/執行引擎 + 積木 UI） |
| 回測 Backtest | 明顯落差 | 部分（槓桿/方向/保證金控制可做，引擎 2.5 已支援；參數掃描/基準比較/熱力圖需要新的編排邏輯，但不算核心架構變動） |
| 比較 Compare | 大致吻合有出入 | 否（年度欄位/基準列/洞察卡都是現有資料的呈現強化） |
| 模擬交易 PaperTrading | 明顯落差 | 是（多策略並行需要 session registry） |
| 設定 Settings | 明顯落差 | 否（環境顯示/權限清單/測試連線用現有資料） |
| 風控 Risk（無獨立頁面） | 絕大部分未做 | 是（跨策略全域限制 + 熔斀規則引擎，依賴 session registry） |

## 分期

### Phase A：不需要新架構的 UI 強化（可以立刻平行派工）

這些都是用現有的 Rust 引擎能力或現有資料做呈現層強化，不涉及新的架構決策：

- A1. 回測頁：槓桿／方向（多空）／保證金模式控制 + 多幣種選擇（`at-core` 的
  `backtest.rs` 2.5 已經支援合約做空槓桿，只是沒有接到 Tauri command／前端表單）
- A2. 比較頁：年度欄位、BTC 基準列、洞察卡、回撤切換
- A3. 設定頁：環境顯示（唯讀/測試網分開顯示目前狀態）、API 權限檢查清單、測試連線按鈕
- A4. SideNav：補齊缺的導覽項；還沒做的頁面（部署、即時交易、高頻監控）用
  `disabled` + 「即將推出」文字佔位，不能讓它們可以點但沒反應（專案鐵律）

### Phase B：核心新架構 — Session Registry + 歷史持久化

這是 Phase C（總覽）、Phase D（模擬交易多策略並行）、Phase E（風控跨策略）、策略庫的
回測次數/sparkline 共同的地基，必須先做。需要架構部門先設計：

- 怎麼同時追蹤多個執行中的 paper-trading / testnet-trading session（目前兩者都是
  5.4／6.5 刻意限定的單一 session 設計）
- 本地怎麼持久化回測結果、session 歷史（SQLite？本地檔案？用 1.6 既有的本機儲存機制
  延伸？）
- 持久化層要不要影響 `at-core`／`at-paper-trading`／`at-testnet-trading` 這幾個已經
  審查過的 crate，還是在 App 層（`app/src-tauri`）疊一層管理，不動底層 crate

### Phase C：總覽頁面（依賴 Phase B）

統計卡、60 天權益曲線（看實際累積多少資料顯示多少，不用硬湊 60 天假資料）、連線額度
面板、執行中策略表格、最近回測清單。

### Phase D：模擬交易多策略並行監控（依賴 Phase B）

多策略分頁、模擬 vs 回測對比圖、差距歸因面板、部位表格、成交明細表。

### Phase E：風控獨立頁（依賴 Phase B）

全域上限、合約風控、5 條可各自開關的熔斷規則、觸發紀錄稽核。需要架構部門先設計熔斷
規則怎麼表示、怎麼跟現有的 `at-risk-control`（目前是單一 session 的 2 個數字）整合或
取代。

### Phase F：策略編輯器 — 視覺化積木策略建構器（獨立大工程）

設計稿要的是使用者自己拖積木組合策略條件（指標/合約資料/價格/比較運算子 + 巢狀
AND/OR）。現有引擎的策略是 4 個寫死的 Rust 實作，只能調數值參數。這個 Phase 需要：

- 定義一個策略定義的中介表示（DSL：JSON/類似 AST 的結構）
- 一個能把這個中介表示變成可執行邏輯的直譯器或編譯器（在 Rust 引擎裡，不是前端 JS
  算，因為回測/模擬/測試網都要共用同一套執行邏輯，這是全專案從 1.x 就有的原則）
- 積木式的前端 UI，把使用者拖拉的積木序列化成上面的中介表示

這是整個計畫裡風險最高、最獨立的一塊，架構部門要先出一份 ADR 等級的設計文件。

### Phase G：回測進階功能（部分依賴 Phase B）

參數掃描、健檢摘要、vs BTC 基準比較、參數穩定度熱力圖、逐年表現表、各幣種貢獻圖。

### Phase H：策略庫 UI 強化

分頁（我的策略/內建範本）、篩選、卡片詳細資訊（狀態、回測次數、sparkline、4 個動作
按鈕）。「新增策略」流程依賴 Phase F 的策略編輯器存在後才有意義；分頁/篩選/卡片資訊
部分依賴 Phase B 的回測歷史持久化（回測次數、sparkline 需要歷史資料）。

## 執行順序與模型分配

1. Phase A 四項可以立刻平行派工給 `web-frontend`／`desktop-engineering`，彼此獨立，
   不互相依賴。
2. Phase B、Phase F（策略 DSL）、Phase E（風控熔斷規則）需要 `architecture` 部門先各自
   出一份設計文件（ADR 等級），這三個可以平行設計，但 Phase B 的設計要先定案，Phase C/D/E
   才能真的動工（他們都依賴 Phase B 的 session registry 介面）。
3. Phase B 設計定案後開始實作（`backend-engineering`／`desktop-engineering`，視落在
   Rust 引擎層還是 App 層而定），完成並審查過後，Phase C、D、E 才平行展開。
4. Phase F 的 DSL 設計定案後，先做 Rust 端的執行引擎（`investment` 或
   `backend-engineering`，這是會影響回測/模擬/測試網共用邏輯的地基，風險等級比照 6.x
   的money-critical 邏輯，需要跨模型（Claude/Codex）審查），執行引擎通過審查後才做
   積木 UI（`design` + `web-frontend`）。
5. Phase G、Phase H 視 Phase B 完成進度交錯進行。

## 驗證方式

- 所有 Rust 改動：先寫測試、`cargo fmt --all`／`cargo clippy --all-targets -- -D
  warnings`／`cargo test`（根目錄跟 `app/src-tauri` 兩個 workspace 都要跑）。
- 所有前端改動：`npm test`／`npm run lint`／`npm run build`。
- 架構層級變動（Phase B、E、F 的設計與落地）：比照 6.x 的審查強度，獨立 QA
  fresh-context 驗證 + 高風險邏輯加一輪 Codex 審查。
- 每個 Phase 完成都走既有的 worktree → 驗證 → merge → re-verify → push 流程，不等全部
  做完才一次驗證（累積風險太高）。
- 完成一個 Phase 就更新這份計畫檔的勾選狀態，方便中斷後接手。

## 已知的開放決策（架構部門設計時要處理）

- [ ] Phase B：持久化層選型（SQLite vs 檔案）、要不要動到已審查過的底層 crate
- [ ] Phase F：策略 DSL 的表達能力邊界（支援哪些指標/運算子/巢狀深度）、Rust 執行引擎
  的介面怎麼跟現有 `Strategy` trait 共存
- [ ] Phase E：熔斷規則的資料結構、怎麼跟現有單一 session 的 `at-risk-control` 整合

## 進度

- [ ] Phase A1 回測槓桿/方向/保證金/多幣種
- [ ] Phase A2 比較頁強化
- [ ] Phase A3 設定頁強化
- [ ] Phase A4 SideNav 補齊
- [ ] Phase B 設計（architecture）
- [ ] Phase B 實作
- [ ] Phase C 總覽頁面
- [ ] Phase D 模擬交易多策略並行
- [ ] Phase E 設計（architecture）
- [ ] Phase E 實作
- [ ] Phase F 設計（architecture，策略 DSL ADR）
- [ ] Phase F Rust 執行引擎
- [ ] Phase F 積木 UI
- [ ] Phase G 回測進階功能
- [ ] Phase H 策略庫 UI 強化
