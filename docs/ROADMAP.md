# 開發順序

原則：先做不碰錢、結果可以驗證的部分，最後才接真實下單。
每個子步驟都很小（通常一個檔案、一組測試），做完就停下來讓你驗證。
每個子步驟的說明在 `docs/steps/`。

## 進度

### 0. 專案骨架
- [x] 0.1 Cargo workspace、引擎執行檔、環境檢查腳本

### 1. 核心資料
- [x] 1.1 定點數 `Fixed`：價格、數量、金額不用浮點數
- [x] 1.2 K 線 `Bar`：開高低收量、時間，並檢查資料合理（最高 ≥ 最低…）
- [x] 1.3 交易規則 `SymbolRules`：價格跳動、數量級距、最小金額，把下單數量取整到合法值
- [x] 1.4 手續費模型 `FeeSchedule`：掛單/吃單費率、BNB 抵扣，算出每筆成交的手續費
- [x] 1.5 讀取 Binance 歷史 K 線 CSV（先用專案內附的小樣本檔）
- [x] 1.6 本機儲存：把 K 線存成檔案、再讀回來
- [x] 1.7 下載器：從 data.binance.vision 下載指定幣種與週期的歷史 K 線

### 2. 回測引擎
- [x] 2.1 策略介面：`on_bar` 收到 K 線、回傳目標部位
- [x] 2.2 最簡單的回測迴圈：只做多、不計成本
- [x] 2.3 成交模擬：訊號在收盤、下一根開盤成交
- [x] 2.4 加上手續費與滑價（用 1.4 的模型）
- [x] 2.5 合約：做空、槓桿、資金費（強制平倉只在開盤／收盤檢查，不看盤中最高最低價，
      高槓桿結果偏樂觀；維持保證金率沒有分級。要更準的爆倉模型時再補）
- [x] 2.6 績效指標：總報酬、年化、最大回撤、夏普、交易次數（夏普的取樣頻率跟著 K 線週期，
      年化用時間戳算、一年當 365 天、無風險利率 0；標準差用母體除以 n，
      和 pandas 預設的樣本標準差差一個 √(n÷(n−1)) 倍，2.8 對照時要先知道）
- [x] 2.7 內建策略：均線交叉、布林通道、唐奇安突破、RSI
- [x] 2.8 對照驗證：和之前 Python 回測結果比對（原始 Python 對照資料在這個 repo 裡不存在，
      改用一份獨立重寫的 Python 回測對照真實資料——整條鏈的權益曲線逐點相等，
      細節、已知差異與「找到原始資料要重做」的建議見 `docs/steps/2.8-對照驗證.md`）

### 3. 桌面 App：Tauri + React
- [x] 3.1 Tauri 專案骨架：`app/` 目錄、Tauri 2 + React + TS + Vite 初始化，能開一個空白視窗
- [x] 3.2 Rust↔前端橋接：第一個 Tauri command（列出內建策略與參數 schema），前端能呼叫並顯示
- [x] 3.3 版面殼與導覽：TopBar、SideNav、四個分頁路由（策略庫／回測／比較／設定）
- [x] 3.4 策略庫頁面：列出四個內建策略卡片，可選擇
- [x] 3.5 策略調參頁面：依參數 schema 動態產生表單，可調整並驗證
- [x] 3.6 回測頁面：選資料＋策略＋參數，執行回測，顯示權益曲線與績效指標
- [x] 3.7 回測比較頁面：多次回測結果並排比較（表格＋疊圖權益曲線）

明確不做（對應功能還沒做到，先不建空殼 UI）：PaperTrading／LiveTrading／GoLive／Risk／HFTMonitor 畫面，等 ROADMAP 第 5-8 步做到對應功能才回頭做。詳細計畫見 `docs/plans/2026-09-29-tauri-desktop-app.md`。

### 4. Binance 唯讀連線
使用者已於 2026-09-29 確認：API 金鑰為唯讀（交易/提領權限已關閉），Key/Secret 標籤正確。詳細計畫見 `docs/plans/2026-09-29-binance-readonly-connection.md`。
- [x] 4.1 Keychain 存取模組：Rust 讀寫 OS 安全儲存區，不連網路（讀取「既有」憑證在 headless
      環境會卡在 macOS 的互動授權對話框，要在有畫面的終端機跑過一次才能放行）
- [x] 4.2 Binance REST client 骨架＋簽名機制：HMAC-SHA256，打一個唯讀端點驗證（簽名演算法
      已用官方範例離線驗證；真實 API 呼叫已對正式環境的 `/api/v3/account/commission` 打通，
      真的拿到帳戶手續費率資料）
- [x] 4.3 帳戶實際費率同步：串接 1.4 `FeeSchedule`，每 24h 更新、失敗沿用舊值（新 crate
      `at-account-sync`；回傳值帶 `Freshness`，過期資料一定標記不會假裝是新的；已用真實帳戶
      驗證拿到 0.075% 吃單費率 = 標準 0.1% × BNB 折扣 0.75）
- [x] 4.4 下單規則同步：串接 1.3 `SymbolRules`，從 `exchangeInfo` 拿真實規則（`at-account-sync`
      新增 `rules` 模組；`exchangeInfo` 是公開端點，`at-binance` 另開不帶金鑰的 `public_get`，
      不讀 Keychain；已對正式環境驗證拿到 BTCUSDT 的 tick 0.01、step 0.00001、最小金額 5 USDT）
- [x] 4.5 即時行情：WebSocket 連線（新 crate `at-market-stream`；同步 `tungstenite` + 背景
      執行緒 + `mpsc` channel，沒有引入 tokio；斷線用指數退避重連 1s→30s；kline 訊息用
      `k.x` 欄位區分收盤／未收盤，避免把還沒收盤的 K 線誤存成歷史資料；已對
      `wss://stream.binance.com:9443/ws/btcusdt@ticker` 真實連線收到即時報價）
- [x] 4.6 桌面 App「連線設定」畫面：App 內輸入/更新金鑰，顯示連線狀態（提前完成，只依賴
      4.1；4.2-4.5 尚未開始，不影響這一步——完全不連 Binance 網路）

### 5. 模擬交易
`RunMode`（1.x 就有）從一開始就設計成四種模式共用同一份策略程式，`Paper`/`Backtest` 的
`sends_orders()` 都回 `false`——這一步完全不會、也不需要碰送單程式碼。詳細計畫見
`docs/plans/2026-09-30-paper-trading.md`。
- [x] 5.1 回測引擎改成「逐根餵」：`PaperEngine::on_bar` 一次吃一根 `Bar` 並自己維持帳本狀態，
      `run_backtest` 變成逐根呼叫它的薄殼（財務邏輯只剩一份，回測與模擬交易不可能分岔）；
      既有 195 個測試逐項比對零差異，另外用重構前的實作跑 4000 組隨機情境差分對照全部一致
- [x] 5.2 接上 4.5 即時行情：只在 K 線收盤（`is_closed`）時才餵給引擎（新 crate
      `at-paper-trading` 只負責接線，`at-core` 與 `at-market-stream` 都不依賴對方；未收盤 K 線
      與 ticker 一律路過，引擎回錯誤就往外通報一次並停止餵；已對真實 BTCUSDT 1m 串流跑通，
      同一批 K 線走即時管線和走 `run_backtest` 權益曲線逐點相等）
- [x] 5.3 執行生命週期管理：`PaperTradingHandle::stop()`／`latest_snapshot()`，以及順手把 4.5
      留的伏筆補上（`MarketStreamHandle::stop()`）；兩層共用**同一個**停止旗標，按一次停止
      WebSocket 執行緒也收工、不留孤兒連線。三處阻塞都改成看得到旗標：模擬交易迴圈用
      `recv_timeout`、WebSocket 讀取設 500ms TCP 讀取逾時（`tungstenite` 明訂 `WouldBlock`
      不是致命錯誤，半截訊框留在它的緩衝區）、斷線退避等待切成 100ms 一段；新增
      `PaperUpdate::Stopped`（正常停止，和 `Failed` 的「帳本不可信」語意分開）。實機驗證：
      真實連線按停止後模擬交易迴圈 207ms 結束、WebSocket 執行緒 502ms 結束
- [x] 5.4 桌面 App「模擬交易」頁面：新分頁重用 3.6 回測頁面的表單/結果版面（`.backtest-*`
      CSS 類別直接沿用）；Rust 端 `start_paper_trading`／`stop_paper_trading`／
      `paper_trading_status` 三個 command，開始時開一條背景執行緒吃下整個
      `PaperTradingHandle`、逐則轉成 `app.emit("paper-trading-update", …)` 事件（Tauri
      command 本身是請求/回應式，沒辦法直接把持續的 channel 回傳給前端）；「停止」不呼叫
      `PaperTradingHandle::stop`（handle 已經整個搬進背景執行緒），而是在交給
      `at_paper_trading::spawn` 之前先跟 `MarketStreamHandle::stop_flag()` 要一份共用旗標的
      複本，跟 5.3 的設計一樣，兩層本來就共用同一個旗標；前端用
      `@tauri-apps/api/event` 的 `listen` 累積權益曲線、顯示部位/現金/成交/強平統計，
      頁面掛載時額外查一次 `paper_trading_status` 補上最後已知狀態；收到 `Failed` 顯示
      紅色警告（帳本不可信），收到 `Stopped` 顯示一般狀態文字。範圍取捨（對照設計稿
      `PaperTrading.dc.html`）：砍掉多重模擬 session 分頁、模擬 vs 回測疊圖比較、
      多幣別部位表、逐筆成交紀錄表、資金費/手續費/滑價分項——底層 `PaperTradingHandle`
      目前只有單一 session、單一交易對、只回累計後的成交筆數，不支援這些。
      驗證：Rust 端新增 `validate_request`/`apply_update` 純函式單元測試（輸入驗證＋狀態機）
      ＋ 一個 `#[ignore]` 真實 BTCUSDT 連線測試（已手動跑過，收到真實快照、按停止乾淨收工）；
      前端用 Vitest + Testing Library，一份手刻 `vi.mock` 的元件測試、
      另一份改用 Tauri 官方 `@tauri-apps/api/mocks`（`mockIPC` + `shouldMockEvents`）走真正的
      `invoke`/`listen`/`emit`，驗證開始→即時更新→停止與 Failed 警告兩條路徑；
      專案沒有 Playwright（3.1-4.6 都只用 Vitest），沿用同一套工具做全流程驗證，不多引入一個
      新框架

### 6. 測試網下單＋風控
使用者已同意跨過「第一次出現送單程式碼」這條安全線，僅限測試網；正式環境下單是第7步，
需要另一次明確同意。詳細計畫見 `docs/plans/2026-10-01-testnet-trading.md`。
- [x] 6.1 Testnet 金鑰獨立存放：Keychain 新增一組 testnet 專用 service name，跟 4.1 正式環境
      唯讀金鑰分開
- [x] 6.2 Testnet REST client＋下單簽名：`BinanceTestnetClient` 完全沒有 `base_url` 欄位，
      編譯期就不可能指向正式環境；簽名過的市價單下單＋查詢訂單狀態＋撤單都走同一個
      `signed_url()` 入口；New Order（RESULT／FULL）、Query Order、Cancel Order 四個官方範例
      全部離線驗證過（Query Order 範例原本有欄位被改過，已修正成逐字複製版本）
- [x] 6.3 風控層：新 crate `at-risk-control`，每日虧損上限（`DailyPnl` 按 UTC 日界自動歸零）、
      一鍵熔斷（`kill_switch`，連平倉單都擋，無例外）、單筆下單金額上限，下單前的檢查閘門
      （`RiskLimits::check`）；只有「增加曝險」的單才受虧損上限約束，過零（多翻空／空翻多）
      一律視為增加曝險；所有算術用 `checked_*`，資料缺失時 fail closed（擋單而非放行）
- [x] 6.4 把策略執行接上測試網下單：新 crate `at-testnet-trading`，送單→等回報（有限次數、
      有退避、會檢查停止旗標的輪詢）→用交易所回報的成交量與成交金額更新本地帳本，不是模擬成交；
      唯一的送單路徑是私有的 `Trader::send_gated_order`，第一行就是 `RiskLimits::check`；
      一鍵停止（只擋送單）與 `stop()`（連行情一起收工）是兩個獨立開關；手續費用 4.3 的帳戶
      費率估算（測試網回報 0 手續費不可信）。真實測試網驗證留給 6.6
- [x] 6.5 桌面 App「測試網交易」頁面＋風控設定：測試網專用 Keychain 金鑰管理（跟 4.6 正式
      環境金鑰分開）、起始資金／每日虧損上限／單筆金額上限表單、一鍵停止（只擋送單）與
      停止（連行情一起收工）是兩個視覺與文字都明顯區分的按鈕、帳本不可信時顯示明顯警告。
      手續費/下單規則透過 4.3/4.4 的 `at-account-sync` 首次接進 App（用正式環境唯讀金鑰
      同步，不信任測試網回報）
- [ ] 6.6 真實測試網端到端驗證（需要使用者提供的測試網 API 金鑰）

### 之後的大步驟（到時再拆小）
- [ ] 7. 小額實盤（需要開交易權限、不開提領的 API 金鑰）
- [ ] 8. 高頻（視前面結果決定要不要做）

## 為什麼這樣排

- **回測先做**：它是這個工具的核心（調參 → 看有沒有賺），而且可以和已經跑過的 Python 回測逐一比對，最容易驗證寫得對不對。
- **成交模擬與手續費只寫一次**：回測、模擬、測試網、實盤共用。
- **唯讀連線在下單之前**：先確定行情、費率、規則都讀對，再讓程式送單。
- **真錢最後**：第 7 步之前，程式不可能動用你的資金。
