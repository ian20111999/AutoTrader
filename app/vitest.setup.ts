import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup, configure } from "@testing-library/react";

// vitest.config.ts 沒開 test.globals，Testing Library 的自動 cleanup 偵測不到
// 全域 afterEach，同一個檔案裡多個 it() 的 render() 會疊在同一個 DOM 上。
afterEach(cleanup);

// `findBy*`/`waitFor` 預設逾時是 1000ms。這個套件現在有 23 個測試檔、每個檔
// 用自己的 jsdom 平行跑（vmThreads），CPU 競爭重的時候某些測試（尤其是
// render → emit 事件 → 等重新渲染這種鏈路）偶爾會剛好卡在 1000ms 邊緣逾時
// ——已經實測抓到兩次（PaperTrading.test.tsx 的「模擬交易結果」「帳本不可信
// 中止」兩個斷言），不是邏輯錯誤，是測試本身的計時器太緊。拉長這個全域預設值
// 一次處理，不要每次 flaky 了才回頭補一個檔案裡的一行逾時參數。
configure({ asyncUtilTimeout: 5000 });
