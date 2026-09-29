# 自動交易台（AutoTrader）

自己調參數、回測策略有沒有賺錢，有的話再模擬、再上線交易的桌面工具。
市場：Binance 現貨與 U 本位永續合約。

- 架構設計、畫面設計：見 Claude 裡的「自動交易系統架構設計」文件與設計稿
- 開發順序：[docs/ROADMAP.md](docs/ROADMAP.md)

## 目前進度

回測引擎（ROADMAP 第 1、2 節）已全部完成：核心資料型別、歷史 K 線讀取/儲存/下載、策略介面、回測迴圈（含下一根開盤成交、手續費滑價、合約做空/槓桿/資金費/強制平倉）、績效指標、四個內建策略（均線交叉、布林通道、唐奇安突破、RSI），並用獨立重新推導的方式對照驗證過整條鏈。還沒有連線交易所，不會下任何單。
第 3 步（Tauri 桌面 App）進行中：`app/` 骨架已建立，能開出空白視窗、Rust 殼能呼叫 `at-core`。
每個子步驟的說明在 `docs/steps/`。

## 專案結構

```
AutoTrader/
├─ app/         Tauri 2 + React + TypeScript 桌面介面（src-tauri/ 是 Rust 殼）
├─ crates/
│  ├─ core/       共用型別（定點數、K 線、交易規則、手續費、市場、執行模式…），四種模式共用
│  ├─ downloader/ 從 data.binance.vision 下載歷史 K 線
│  └─ engine/     交易引擎執行檔
├─ docs/        開發順序（ROADMAP.md）與每一步的說明（steps/）
└─ scripts/     輔助腳本（環境檢查）
```

## 第一次使用

1. 在終端機進到這個資料夾，執行環境檢查，缺什麼它會告訴你去哪裡裝：

   - **macOS**：`bash scripts/check-env.sh`
   - **Windows**（PowerShell）：`powershell -ExecutionPolicy Bypass -File scripts\check-env.ps1`

2. 編譯並執行測試：

   ```
   cargo build
   cargo test
   cargo run -p engine
   ```

   最後一行會印出引擎狀態與四種執行模式。

## 桌面 App 開發

`app/` 是 Tauri 2 + React + TypeScript + Vite 的桌面介面（第一次執行前先跑過上面的環境檢查，
第 3 步開始需要 Node.js／npm）。

```
cd app
npm install        # 只需執行一次
npm run tauri dev  # 開發模式：熱重載，會開出桌面視窗
```

其他常用指令（都在 `app/` 目錄下執行）：

```
npm test            # Vitest 單元測試
npm run lint         # ESLint
npm run build        # tsc 型別檢查 + Vite 產出前端靜態檔（dist/）
npm run tauri build  # 打包成可安裝的桌面應用程式
```

`app/src-tauri` 是獨立的 Rust package（有自己的 `[workspace]`，不在根目錄的 cargo workspace
裡），因為 `tauri.conf.json` 的 `frontendDist` 指向 `../dist`，這個 crate 要能編譯得先跑過
`npm run build` 產生前端產物，跟根目錄的 `cargo build`/`cargo test`（回測引擎）分開跑互不影響。
它已經加了 `at-core`（path 依賴）,之後 Rust↔前端橋接的 Tauri command 會寫在
`app/src-tauri/src/lib.rs`。

## 安全原則

- API 金鑰不寫進程式碼、不放在這個資料夾，之後會存在作業系統的安全儲存區（macOS 鑰匙圈、Windows 認證管理員）。
- `.gitignore` 已排除 `data/`、`.env`、`*.key` 等檔案。
- 只有「實盤」模式會動用真實資金；回測與模擬交易永遠不送單。
