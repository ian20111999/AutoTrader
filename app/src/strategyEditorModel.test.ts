import { beforeEach, describe, expect, it } from "vitest";
import type { Cond } from "./strategyDslTypes";
import {
  defaultAst,
  defaultCond,
  deleteSavedStrategy,
  loadSavedStrategies,
  mirrorCond,
  needsOrderFlowData,
  upsertSavedStrategy,
} from "./strategyEditorModel";

describe("needsOrderFlowData", () => {
  it("trades／taker_buy_ratio 需要訂單流資料，其餘欄位不需要", () => {
    expect(needsOrderFlowData("trades")).toBe(true);
    expect(needsOrderFlowData("taker_buy_ratio")).toBe(true);
    expect(needsOrderFlowData("open")).toBe(false);
    expect(needsOrderFlowData("close")).toBe(false);
    expect(needsOrderFlowData("volume")).toBe(false);
  });
});

describe("mirrorCond", () => {
  it("把大於換成小於、向上穿越換成向下穿越，巢狀結構不變", () => {
    const cond = defaultCond(); // { kind: "gt", ... }
    expect(mirrorCond(cond).kind).toBe("lt");

    const crossAbove = { ...cond, kind: "cross_above" as const };
    expect(mirrorCond(crossAbove).kind).toBe("cross_below");
  });

  it("all/any/sustained 遞迴鏡像子節點，kind 本身不變", () => {
    const nested = {
      kind: "all" as const,
      children: [
        { kind: "gte" as const, left: defaultCond().left, right: defaultCond().right },
        { kind: "sustained" as const, bars: 3, inner: defaultCond() },
      ],
    };
    const mirrored = mirrorCond(nested);
    expect(mirrored.kind).toBe("all");
    if (mirrored.kind === "all") {
      expect(mirrored.children[0].kind).toBe("lte");
      const second = mirrored.children[1];
      expect(second.kind).toBe("sustained");
      if (second.kind === "sustained") {
        expect(second.inner.kind).toBe("lt");
      }
    }
  });

  // ADR-004 第 10 節：既有 bug——只翻轉運算子，不動 Expr 本身，會讓 FVG／BOS
  // 的做空版本產生錯誤訊號。這兩個測試驗證「鏡像後語意正確」，不只是
  // 「鏡像後能編譯」。
  it("FVG 做多鏡像成做空：high/low 要互換，不是只翻轉運算子", () => {
    // 做多 FVG：gt(low[0], high[2])
    const longFvg: Cond = {
      kind: "gt",
      left: { kind: "price", field: "low", offset: 0 },
      right: { kind: "price", field: "high", offset: 2 },
    };
    // 正確的做空版本（使用者直接手動組出來的）：lt(high[0], low[2])
    const expectedShortFvg: Cond = {
      kind: "lt",
      left: { kind: "price", field: "high", offset: 0 },
      right: { kind: "price", field: "low", offset: 2 },
    };
    expect(mirrorCond(longFvg)).toEqual(expectedShortFvg);
  });

  it("BOS 做多鏡像成做空：swing_high 要換成 swing_low，不是只翻轉運算子", () => {
    // 做多 BOS（公式①）：gt(close, swing_high(last))
    const longBos: Cond = {
      kind: "gt",
      left: { kind: "price", field: "close" },
      right: {
        kind: "indicator",
        name: "swing_high",
        params: { left: 2, right: 2 },
        output: "last",
      },
    };
    // 正確的做空版本：lt(close, swing_low(last))
    const expectedShortBos: Cond = {
      kind: "lt",
      left: { kind: "price", field: "close" },
      right: {
        kind: "indicator",
        name: "swing_low",
        params: { left: 2, right: 2 },
        output: "last",
      },
    };
    expect(mirrorCond(longBos)).toEqual(expectedShortBos);
  });

  it("非方向性的指標（sma）鏡像時維持原樣，不受這次修復影響", () => {
    const cond = defaultCond(); // gt(close, sma(10))
    const mirrored = mirrorCond(cond);
    expect(mirrored).toEqual({ kind: "lt", left: cond.left, right: cond.right });
  });
});

describe("defaultAst", () => {
  it("long_only 時做空兩棵樹是 null", () => {
    const ast = defaultAst("long_only", defaultCond(), defaultCond());
    expect(ast.shortEntry).toBeNull();
    expect(ast.shortExit).toBeNull();
  });

  it("long_short 時做空兩棵樹是做多的鏡像", () => {
    const longEntry = defaultCond();
    const ast = defaultAst("long_short", longEntry, defaultCond());
    expect(ast.shortEntry?.kind).toBe("lt");
  });
});

describe("本機存檔（localStorage）", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("儲存、讀取、刪除一份策略", () => {
    const ast = defaultAst("long_only", defaultCond(), defaultCond());
    const saved = upsertSavedStrategy("我的策略", ast);

    const list = loadSavedStrategies();
    expect(list).toHaveLength(1);
    expect(list[0].name).toBe("我的策略");
    expect(list[0].id).toBe(saved.id);

    deleteSavedStrategy(saved.id);
    expect(loadSavedStrategies()).toHaveLength(0);
  });

  it("用同一個 id 再存一次是更新，不是新增一筆", () => {
    const ast = defaultAst("long_only", defaultCond(), defaultCond());
    const first = upsertSavedStrategy("A", ast);
    upsertSavedStrategy("A 改名", ast, first.id);

    const list = loadSavedStrategies();
    expect(list).toHaveLength(1);
    expect(list[0].name).toBe("A 改名");
  });

  it("儲存空間裡是壞掉的 JSON 時安全回退成空陣列", () => {
    localStorage.setItem("strategyEditor.savedStrategies", "{ not json");
    expect(loadSavedStrategies()).toEqual([]);
  });
});
