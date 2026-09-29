#!/usr/bin/env python3
"""ROADMAP 2.8 對照驗證：完全獨立於 Rust 實作，只照規格重算一次均線交叉回測。

這支腳本是「第二套實作」，用途是抓 Rust 引擎的錯，所以刻意不看
`backtest.rs` / `metrics.rs` / `strategies/sma_cross.rs` 的程式碼，只依據：

1. `CLAUDE.md`：訊號在 K 線收盤產生、下一根開盤成交；滑價與手續費分開記帳。
2. `docs/ROADMAP.md` 2.6：夏普取樣頻率跟著 K 線週期、年化用時間戳算、
   一年 365 天、無風險利率 0、標準差用母體（除以 n）。
3. `SmaCross` 的行為宣告：兩條 SMA 都用收盤價；快線 > 慢線滿倉做多，
   其餘（含相等、暖機不足）空手；暖機 = 慢線週期。
4. `Fixed` 的算術契約（1.1）：8 位小數，乘除在第 9 位四捨五入（0.5 遠離零）。
5. `FeeModel::fee`：手續費 = 價格 × 數量 × 費率，以報價幣計。

用法：python3 docs/reference/manual_verification_sma_cross.py
"""

from __future__ import annotations

import math
from pathlib import Path

SCALE = 10**8
MS_PER_YEAR = 365 * 24 * 60 * 60 * 1000

REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "crates/downloader/tests/fixtures/BTCUSDT/1d/BTCUSDT-1d-2024-01.csv"

FAST, SLOW = 3, 8
INITIAL = 10_000


# --- Fixed：8 位小數定點數，乘除四捨五入（0.5 遠離零）-------------------------


def fx(s: str) -> int:
    """把十進位字串轉成 raw（放大 10^8 的整數）。"""
    neg = s.startswith("-")
    s = s.lstrip("+-")
    whole, _, frac = s.partition(".")
    frac = (frac + "0" * 8)[:8]
    raw = int(whole or "0") * SCALE + int(frac or "0")
    return -raw if neg else raw


def show(raw: int) -> str:
    sign = "-" if raw < 0 else ""
    raw = abs(raw)
    return f"{sign}{raw // SCALE}.{raw % SCALE:08d}"


def fmul(a: int, b: int) -> int:
    wide = a * b
    sign = -1 if wide < 0 else 1
    q, r = divmod(abs(wide), SCALE)
    return sign * (q + 1 if 2 * r >= SCALE else q)


def fdiv(a: int, b: int) -> int:
    wide = a * SCALE
    sign = -1 if (wide < 0) != (b < 0) else 1
    q, r = divmod(abs(wide), abs(b))
    return sign * (q + 1 if 2 * r >= abs(b) else q)


def from_int(n: int) -> int:
    return n * SCALE


def to_float(raw: int) -> float:
    return raw / SCALE


# --- 資料 ---------------------------------------------------------------------


class Bar:
    __slots__ = ("open_time", "open", "high", "low", "close")

    def __init__(self, line: str):
        f = line.split(",")
        self.open_time = int(f[0])
        self.open = fx(f[1])
        self.high = fx(f[2])
        self.low = fx(f[3])
        self.close = fx(f[4])


def load_bars() -> list[Bar]:
    text = FIXTURE.read_text()
    return [Bar(line) for line in text.splitlines() if line.strip()]


# --- 策略：均線交叉 -----------------------------------------------------------


class SmaCross:
    """快線 > 慢線 → 目標部位 1（滿倉做多）；其餘 → 0。"""

    def __init__(self, fast: int, slow: int):
        self.fast, self.slow = fast, slow
        self.closes: list[int] = []

    def sma(self, period: int) -> int | None:
        if len(self.closes) < period:
            return None
        window = self.closes[-period:]
        return fdiv(sum(window), from_int(period))

    def on_bar(self, bar: Bar) -> int:
        """回傳目標部位比例（raw）。"""
        self.closes.append(bar.close)
        f, s = self.sma(self.fast), self.sma(self.slow)
        if f is None or s is None:
            return 0
        return SCALE if f > s else 0


# --- 回測：訊號在收盤產生、下一根開盤成交 -------------------------------------


class Fill:
    def __init__(self, i, open_time, side, price, qty, fee):
        self.i, self.open_time = i, open_time
        self.side, self.price, self.qty, self.fee = side, price, qty, fee

    def __str__(self):
        return (
            f"  第 {self.i:2d} 根 {self.side} 價 {show(self.price)} "
            f"數量 {show(self.qty)} 手續費 {show(self.fee)}"
        )


def backtest(
    bars,
    strategy,
    slippage=0,
    taker_rate=0,
    fee_aware_sizing=False,
    rebalance_every_bar=False,
):
    """回傳 (權益曲線, 成交明細, 每根的訊號與均線)。

    每根 K 線的順序：先用上一根收盤產生的目標部位在這根開盤調倉，
    再用這根的收盤價把權益標記到曲線上，最後把這根餵給策略拿下一個目標。

    `rebalance_every_bar`：目標部位沒變時要不要也重新算一次數量。規格沒有寫，
    零成本時兩種做法完全一樣（權益 ÷ 開盤價剛好等於手上的數量）；含滑價時
    每根重算會賣掉一小片碎屑（買價含滑價 > 開盤價），白付手續費。
    預設關掉，只在目標改變時調倉。
    """
    cash = from_int(INITIAL)
    qty = 0
    target = 0  # 上一根收盤產生、待在這根開盤執行的目標比例
    held = 0  # 手上這個部位是照哪個目標建的
    curve, fills, rows = [], [], []

    for i, bar in enumerate(bars):
        # 1. 開盤調倉
        equity_at_open = cash + fmul(qty, bar.open)
        want_qty = qty
        if rebalance_every_bar or target != held:
            want_qty = 0
            if target != 0:
                # 買價含滑價（買貴），賣價含滑價（賣賤）
                buy_price = fmul(bar.open, SCALE + slippage)
                denom = fmul(buy_price, SCALE + taker_rate) if fee_aware_sizing else buy_price
                want_qty = fdiv(fmul(equity_at_open, target), denom)
        if want_qty != qty:
            if want_qty > qty:
                price = fmul(bar.open, SCALE + slippage)
                delta = want_qty - qty
                side = "買進"
            else:
                price = fmul(bar.open, SCALE - slippage)
                delta = qty - want_qty
                side = "賣出"
            notional = fmul(price, delta)
            fee = fmul(notional, taker_rate)
            cash += -notional - fee if side == "買進" else notional - fee
            qty = want_qty
            fills.append(Fill(i, bar.open_time, side, price, delta, fee))
        held = target

        # 2. 收盤標記權益
        equity = cash + fmul(qty, bar.close)
        curve.append((bar.open_time, equity))

        # 3. 產生下一根要執行的目標
        prev_target = target
        target = strategy.on_bar(bar)
        rows.append(
            (
                i,
                strategy.sma(FAST),
                strategy.sma(SLOW),
                target,
                prev_target,
                equity,
                cash,
                qty,
            )
        )

    return curve, fills, rows


# --- 績效指標 -----------------------------------------------------------------


def sharpe_all_integer(curve):
    """同一條夏普公式，但全部用整數算，用來解釋和 Rust 的最後幾位差在哪。

    引擎為了「同一份輸入永遠得到同一個數字」不碰 f64：平均值是 i128 截尾除法、
    標準差是 floor 的整數開根號、每年期數 = 報酬個數 ÷ 年數（都是 8 位小數）。
    這裡照同一條路徑重算一次，就能證明差異是量化誤差而不是公式不同。
    """
    eq = [e for _, e in curve]
    ts = [t for t, _ in curve]
    rets = [fdiv(eq[i] - eq[i - 1], eq[i - 1]) for i in range(1, len(eq))]
    n = len(rets)
    total = sum(rets)
    mean = total // n if total >= 0 else -((-total) // n)  # 截尾，不四捨五入
    var = sum((r - mean) ** 2 for r in rets) // n
    std = math.isqrt(var)  # floor
    if std == 0:
        return None
    span_years = (ts[-1] - ts[0]) * SCALE // MS_PER_YEAR
    ppy = fdiv(from_int(n), span_years)
    scaled = ppy * SCALE
    root = math.isqrt(scaled)
    if scaled > root * root + root:
        root += 1
    return fmul(fdiv(mean, std), root)


def metrics(curve):
    first, last = curve[0][1], curve[-1][1]
    total_return = fdiv(last - first, first)

    span_ms = curve[-1][0] - curve[0][0]
    span_years_raw = fdiv(from_int(span_ms), from_int(MS_PER_YEAR))
    span_years = to_float(span_years_raw)

    growth = to_float(last) / to_float(first)
    annualized = growth ** (1.0 / span_years) - 1.0

    peak, max_dd = first, 0
    for _, e in curve:
        peak = max(peak, e)
        dd = fdiv(peak - e, peak)
        max_dd = max(max_dd, dd)

    rets = [to_float(curve[i][1]) / to_float(curve[i - 1][1]) - 1.0 for i in range(1, len(curve))]
    n = len(rets)
    mean = sum(rets) / n
    var = sum((r - mean) ** 2 for r in rets) / n  # 母體標準差：除以 n
    std = math.sqrt(var)
    interval_ms = curve[1][0] - curve[0][0]
    periods_per_year = MS_PER_YEAR / interval_ms
    sharpe = mean / std * math.sqrt(periods_per_year) if std > 0 else None

    return {
        "total_return": total_return,
        "annualized_return": annualized,
        "max_drawdown": max_dd,
        "sharpe": sharpe,
        "span_years": span_years_raw,
        "periods_per_year": periods_per_year,
        "sharpe_all_integer": sharpe_all_integer(curve),
    }


def report(name, bars, **kwargs):
    strategy = SmaCross(FAST, SLOW)
    curve, fills, rows = backtest(bars, strategy, **kwargs)
    m = metrics(curve)

    print(f"\n{'=' * 78}\n{name}\n{'=' * 78}")
    print(f"{'#':>2} {'收盤':>12} {'快線SMA3':>14} {'慢線SMA8':>14} {'訊號':>4} {'權益':>16}")
    for i, f, s, t, _prev, equity, _cash, _qty in rows:
        print(
            f"{i:2d} {show(bars[i].close):>12} "
            f"{show(f) if f is not None else '—':>14} "
            f"{show(s) if s is not None else '—':>14} "
            f"{'多' if t else '空手':>4} {show(equity):>16}"
        )
    print(f"\n成交 {len(fills)} 筆：")
    for f in fills:
        print(f)
    print("\n指標：")
    print(f"  總報酬     {show(m['total_return'])}")
    print(f"  年化報酬   {m['annualized_return']:.10f}")
    print(f"  最大回撤   {show(m['max_drawdown'])}")
    print(f"  夏普       {m['sharpe']:.10f}（f64）")
    print(f"             {show(m['sharpe_all_integer'])}（全整數路徑，同公式）")
    print(f"  期間（年） {show(m['span_years'])}  每年 {m['periods_per_year']:.0f} 期")
    print(f"  期末權益   {show(curve[-1][1])}")
    print("\n權益曲線（給 Rust 測試斷言用）：")
    for t, e in curve:
        print(f"  {t} {show(e)}")
    return curve, fills, m


def main():
    bars = load_bars()
    print(f"資料：{FIXTURE.relative_to(REPO)}，{len(bars)} 根")
    print(f"參數：SmaCross(fast={FAST}, slow={SLOW})，起始資金 {INITIAL}")

    report("情境 A：零成本（frictionless）", bars)
    report(
        "情境 B：滑價 0.05% + 現貨 VIP0 吃單費 0.1%（部位不預留手續費）",
        bars,
        slippage=fx("0.0005"),
        taker_rate=fx("0.001"),
    )
    report(
        "情境 B'：同上，但部位預留手續費（qty = 權益 / (價格 × (1+費率))）",
        bars,
        slippage=fx("0.0005"),
        taker_rate=fx("0.001"),
        fee_aware_sizing=True,
    )
    report(
        "情境 B''：同 B，但目標沒變時也每根重新平衡（示範碎單成本）",
        bars,
        slippage=fx("0.0005"),
        taker_rate=fx("0.001"),
        rebalance_every_bar=True,
    )


if __name__ == "__main__":
    main()
