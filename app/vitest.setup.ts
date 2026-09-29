import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// vitest.config.ts 沒開 test.globals，Testing Library 的自動 cleanup 偵測不到
// 全域 afterEach，同一個檔案裡多個 it() 的 render() 會疊在同一個 DOM 上。
afterEach(cleanup);
