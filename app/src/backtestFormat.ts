// Rust 端的比例指標是「0.25 = 25%」這種比例字串，不是已經乘過 100 的數字；
// 這裡統一轉成畫面用的百分比文字。這裡的 Number() 轉換只用來「顯示」（四捨五入
// 到 1～3 位小數），不是下單相關計算，符合專案規則「f64 只能用在統計與畫圖」。

function toPercent(raw: string): number {
  return Number(raw) * 100;
}

// total_return / annualized_return 這種可正可負的指標：null 代表 Rust 端算不出來
// （曲線太短、除以 0…），顯示「—」而不是假造一個 0，跟 Rust 端「算不出來就回
// None，不回假數字」的原則一致。
export function formatSignedPercent(raw: string | null, digits = 1): string {
  if (raw === null) return "—";
  const pct = toPercent(raw);
  if (pct === 0) return "0%";
  const sign = pct > 0 ? "+" : "−";
  return `${sign}${Math.abs(pct).toFixed(digits)}%`;
}

// max_drawdown 永遠是「跌了多少」的正數量級（0.19 代表跌 19%），畫面上一律顯示成負的。
export function formatDrawdown(raw: string, digits = 1): string {
  const pct = toPercent(raw);
  if (pct === 0) return "0%";
  return `−${Math.abs(pct).toFixed(digits)}%`;
}

export function formatSharpe(raw: string | null, digits = 2): string {
  if (raw === null) return "—";
  const value = Number(raw);
  const formatted = Math.abs(value).toFixed(digits);
  return value < 0 ? `−${formatted}` : formatted;
}

// 滑價這種永遠是正數的成本假設，不需要正負號，只需要百分比。
export function formatPercentMagnitude(raw: string, digits = 2): string {
  return `${Math.abs(toPercent(raw)).toFixed(digits)}%`;
}
