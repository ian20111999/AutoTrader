import { beforeEach, describe, expect, it } from "vitest";
import {
  defaultAst,
  defaultCond,
  deleteSavedStrategy,
  loadSavedStrategies,
  mirrorCond,
  upsertSavedStrategy,
} from "./strategyEditorModel";

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
