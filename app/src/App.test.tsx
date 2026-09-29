import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "./App";

describe("App", () => {
  it("顯示應用程式標題", () => {
    render(<App />);
    expect(screen.getByRole("heading", { name: "自動交易台" })).toBeInTheDocument();
  });
});
