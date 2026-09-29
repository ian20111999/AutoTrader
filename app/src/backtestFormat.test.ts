import { describe, expect, it } from "vitest";
import {
  formatDrawdown,
  formatPercentMagnitude,
  formatSharpe,
  formatSignedPercent,
} from "./backtestFormat";

describe("formatSignedPercent", () => {
  it("正值加上正號", () => {
    expect(formatSignedPercent("0.198")).toBe("+19.8%");
  });

  it("負值用全形負號、取絕對值後的百分比", () => {
    expect(formatSignedPercent("-0.183")).toBe("−18.3%");
  });

  it("剛好 0 不加正負號", () => {
    expect(formatSignedPercent("0")).toBe("0%");
  });

  it("null 顯示「—」，不是假造的 0（呼應 Rust 端算不出來就回 None）", () => {
    expect(formatSignedPercent(null)).toBe("—");
  });
});

describe("formatDrawdown", () => {
  it("正數量級永遠顯示成負的", () => {
    expect(formatDrawdown("0.183")).toBe("−18.3%");
  });

  it("0 顯示 0%，不是 −0%", () => {
    expect(formatDrawdown("0")).toBe("0%");
  });
});

describe("formatSharpe", () => {
  it("格式化成兩位小數", () => {
    expect(formatSharpe("0.9")).toBe("0.90");
  });

  it("可以是負值，用全形負號跟其他指標一致", () => {
    expect(formatSharpe("-1.234")).toBe("−1.23");
  });

  it("null 顯示「—」", () => {
    expect(formatSharpe(null)).toBe("—");
  });
});

describe("formatPercentMagnitude", () => {
  it("轉成百分比，不帶正負號", () => {
    expect(formatPercentMagnitude("0.0005")).toBe("0.05%");
  });
});
