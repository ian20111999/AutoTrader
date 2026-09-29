import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// 獨立於 vite.config.ts：那份是給 `tauri dev`/`tauri build` 用的，
// 固定 1420 埠、忽略 src-tauri，測試不需要也不該受這些限制。
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    setupFiles: ["./vitest.setup.ts"],
  },
});
