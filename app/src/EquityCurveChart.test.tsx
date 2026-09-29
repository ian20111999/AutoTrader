import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { EquityCurveChart } from "./EquityCurveChart";

describe("EquityCurveChart", () => {
  it("畫出折線與可存取的圖表說明", () => {
    const curve = [
      { openTime: 0, equity: "10000" },
      { openTime: 3600000, equity: "10500" },
      { openTime: 7200000, equity: "10200" },
    ];

    render(<EquityCurveChart curve={curve} startingCapital="10000" />);

    const chart = screen.getByRole("img");
    expect(chart).toBeInTheDocument();
    const polyline = chart.querySelector("polyline");
    expect(polyline).toBeInTheDocument();
    expect(polyline?.getAttribute("points")?.split(" ")).toHaveLength(3);
  });

  it("資料點少於 2 個時顯示文字說明，不畫空圖表", () => {
    render(<EquityCurveChart curve={[{ openTime: 0, equity: "10000" }]} startingCapital="10000" />);

    expect(screen.getByText("資料點太少，無法畫出權益曲線。")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("空曲線也顯示文字說明", () => {
    render(<EquityCurveChart curve={[]} startingCapital="10000" />);

    expect(screen.getByText("資料點太少，無法畫出權益曲線。")).toBeInTheDocument();
  });
});
