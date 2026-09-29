# 2026-09-29 桌面 App（ROADMAP 第 3 步）執行計畫

CEO 已同意「不用等我確認，照著 wireframe 做」（2026-09-29），跳過原本 Framework 級依賴的 🔴 確認流程，直接開始。

## 範圍

只做 ROADMAP 第 3 步字面上點名的四件事：**策略庫、調參數、回測頁、回測比較**，加上讓這四頁能跑起來所需的殼（視窗、導覽、Rust↔前端橋接）。

**明確不做**（那些畫面對應的功能還沒做到,提前建 UI 只是空殼,違反「不要為了以後才需要的東西現在就做」）：
- PaperTrading／LiveTrading／GoLive／Risk／HFTMonitor 這五個畫面——分別對應 ROADMAP 第 5-8 步，等那些步驟的底層功能做出來才回頭做對應畫面。

## 技術選型（已由專案 CLAUDE.md 定案，不重新選型）

Tauri 2 + React + TypeScript。前端建置工具用 Vite（公司預設：SPA/內部工具不需要 SSR 時用 Vite+React，Tauri 應用正是這個例外情況，不用 Next.js）。套件管理用 npm（公司預設）。測試：Vitest（單元）+ Playwright（E2E，必驗實際資料流不只驗元素存在）。TypeScript strict、ESLint+Prettier。

## 畫面參考

`docs/design-reference.md`（已讀過設計稿摘要）＋原始 Design canvas artifact（`https://claude.ai/artifact/NUreRyfxXWXz8m53onzHG3`，需要哪一頁的細節就叫負責那頁的 agent 直接讀對應的 `project/*.dc.html`，不要只憑摘要猜版面）。

## 子步驟、檔案、順序、驗收條件

### 3.1 Tauri 專案骨架
- 新增 `app/`：Tauri 2 + React + TS + Vite 初始化，`src-tauri/` 是 Rust 殼。
- `app/src-tauri/Cargo.toml` 加 `at-core`（path 依賴），確認能從 Tauri 殼呼叫 core 的型別。
- 更新 `scripts/check-env.sh`／`.ps1` 加 Node/npm/Tauri CLI 前置檢查。
- 更新根 `Cargo.toml` workspace members（如果 `src-tauri` 要納入 cargo workspace）。
- 驗收：`npm run tauri dev`（或等價指令）能開出一個空白視窗，標題正確；`cargo test`／`npm test` 都至少有一個真測試通過；README 補一段「桌面 App 開發」的啟動說明。

### 3.2 Rust↔前端橋接
- 第一個 Tauri command：`list_builtin_strategies`，回傳 2.7 那四個策略的名稱＋參數 schema（週期/門檻等，含預設值），直接重用 `at_core::strategies` 裡已有的型別，不要重新定義一份。
- 前端呼叫這個 command，先用最陽春的方式印出結果（還沒有版面設計），證明橋接是通的。
- 驗收：前端能拿到並顯示四個策略的名稱與參數，改動 Rust 那邊參數預設值、前端顯示要跟著變（證明是真的呼叫過去，不是寫死資料）。

### 3.3 版面殼與導覽
- 照 `design-reference.md`／`project/TopBar.dc.html`、`SideNav.dc.html`、`Main.dc.html` 做 TopBar＋SideNav＋路由，先切出四個空頁面（策略庫／回測／比較／設定，設定頁可以先留空或只放版本資訊）。
- 驗收：能在四個分頁間切換，畫面結構（不要求像素級一致，但主要區塊/導覽項目要對上設計稿）跟 wireframe 大致一致，跑 `npm run build` 不報錯。

### 3.4 策略庫頁面（Strategies）
- 照 `project/Strategies.dc.html`，列出 3.2 拿到的四個內建策略卡片，可點選進入 3.5 的調參頁。
- 驗收：四張卡片資訊跟 Rust 那邊回傳的一致，點選後能導到對應策略的調參頁並帶對正確的策略 id。

### 3.5 策略調參頁面（StrategyEditor）
- 照 `project/StrategyEditor.dc.html`，依 3.2 的參數 schema 動態產生表單（週期、門檻等），數值可調整、有基本驗證（例如週期不能是 0 或負數，重用 Rust 那邊已有的 `StrategyParamError` 語意，不要在前端重新發明一套驗證規則、只做基本的型別/範圍檢查）。
- 驗收：調整參數後有即時回饋（不合法值要擋、有錯誤訊息，繁體中文）；表單狀態能傳到 3.6 的回測頁。

### 3.6 回測頁面（Backtest）
- 照 `project/Backtest.dc.html`：選資料（幣種/週期/區間，串接 1.6/1.7 的本機儲存與下載器）＋策略＋參數，執行按鈕呼叫 Rust 整條 `run_backtest`＋`metrics`，畫出權益曲線圖與四個績效指標＋交易次數／強平次數。
- 這是整個 App 第一個「真的會跑很久」的操作（下載+回測可能要等），要有 loading 狀態，不能讓 UI 卡死。
- 驗收：對一組真實下載的資料跑一次回測，畫面數字要跟 Rust 端測試（2.2-2.6 已驗證過的邏輯）算出來的一致；用 Playwright 走一次「選策略→調參數→選資料區間→執行→看到權益曲線與指標」的完整路徑，不是只驗證按鈕存在。

### 3.7 回測比較頁面（Compare）
- 照 `project/Compare.dc.html`：能把 3.6 跑出來的多次回測結果存起來，並排比較（表格＋疊圖權益曲線）。
- 驗收：至少跑兩次不同參數的回測、進比較頁能同時看到兩者的指標與曲線疊圖。

## 驗證方式

每個子步驟：對應部門 agent 實作 → fresh-context agent 用 Playwright 實際打開 dev server 走一遍(不只讀程式碼)驗證 → 通過才 commit、進下一步。前端這塊我（協調端）不自己動手寫程式，只讀驗證報告與截圖/console log。

## 已知會遇到的環境問題（沿用回測引擎階段的教訓）

多 agent 同時寫這個repo會被 harness 自動導進共用 git worktree（跟 2.x 階段一樣的機制）——沿用同一套「先讀最新內容再改共用檔案」的安全編輯方式，完成後我來統一 commit、最後 merge 回 main。
