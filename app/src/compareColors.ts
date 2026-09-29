// CompareChart 的折線顏色跟 Compare.tsx 的圖例/表格色塊共用同一份調色盤，避免
// 圖表上的顏色跟圖例對不起來。單獨拆檔也是為了讓 CompareChart.tsx 只匯出元件，
// 符合 React Fast Refresh 的要求（react-refresh/only-export-components）。
export const PALETTE = ["#F2B33D", "#5B9BFF", "#A78BFA", "#22C38E", "#F4555E", "#7F8898"];

export function colorForIndex(index: number): string {
  return PALETTE[index % PALETTE.length];
}
