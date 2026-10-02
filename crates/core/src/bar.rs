//! K 線與週期。
//!
//! 一根 K 線記錄一段時間內的開盤、最高、最低、收盤價與成交量。
//! 回測引擎只吃 K 線，所以 K 線本身一定要正確：這裡負責檢查
//! 「單根是否合理」與「一串 K 線的時間順序是否正確」。
//!
//! 時間一律用 UTC 毫秒（和 Binance API 相同），不處理時區。

use crate::fixed::Fixed;
use std::fmt;
use std::str::FromStr;

/// K 線週期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Interval {
    S1,
    M1,
    M5,
    M15,
    H1,
    H4,
    D1,
}

/// 無法辨識的週期字串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIntervalError(pub String);

impl fmt::Display for ParseIntervalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "不支援的週期：{:?}（可用：1s、1m、5m、15m、1h、4h、1d；注意大寫 1M 是一個月）",
            self.0
        )
    }
}

impl std::error::Error for ParseIntervalError {}

impl Interval {
    pub const ALL: [Interval; 7] = [
        Interval::S1,
        Interval::M1,
        Interval::M5,
        Interval::M15,
        Interval::H1,
        Interval::H4,
        Interval::D1,
    ];

    /// 一根 K 線有幾毫秒。
    pub const fn millis(self) -> i64 {
        const SEC: i64 = 1_000;
        const MIN: i64 = 60 * SEC;
        match self {
            Interval::S1 => SEC,
            Interval::M1 => MIN,
            Interval::M5 => 5 * MIN,
            Interval::M15 => 15 * MIN,
            Interval::H1 => 60 * MIN,
            Interval::H4 => 4 * 60 * MIN,
            Interval::D1 => 24 * 60 * MIN,
        }
    }

    /// Binance 使用的代號，例如 `"4h"`。
    pub const fn as_str(self) -> &'static str {
        match self {
            Interval::S1 => "1s",
            Interval::M1 => "1m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
            Interval::D1 => "1d",
        }
    }
}

impl FromStr for Interval {
    type Err = ParseIntervalError;

    /// 區分大小寫：Binance 的 `1m` 是一分鐘、`1M` 是一個月，不能混用。
    fn from_str(s: &str) -> Result<Interval, ParseIntervalError> {
        Interval::ALL
            .into_iter()
            .find(|i| i.as_str() == s.trim())
            .ok_or_else(|| ParseIntervalError(s.to_string()))
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一根 K 線。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// 開盤時間，UTC 毫秒。
    pub open_time: i64,
    pub open: Fixed,
    pub high: Fixed,
    pub low: Fixed,
    pub close: Fixed,
    /// 成交量（以基礎幣計）。只用在統計，所以用 `f64`。
    pub volume: f64,
}

/// 單根 K 線不合理的原因。
#[derive(Debug, Clone, PartialEq)]
pub enum BarError {
    /// 開高低收有一個 ≤ 0。
    NonPositivePrice,
    /// 最高價低於開盤、收盤或最低價。
    HighTooLow,
    /// 最低價高於開盤或收盤價。
    LowTooHigh,
    /// 成交量是負數或不是有限數字。
    BadVolume,
}

impl fmt::Display for BarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BarError::NonPositivePrice => write!(f, "開高低收必須大於 0"),
            BarError::HighTooLow => write!(f, "最高價低於開盤、收盤或最低價"),
            BarError::LowTooHigh => write!(f, "最低價高於開盤或收盤價"),
            BarError::BadVolume => write!(f, "成交量必須是 ≥ 0 的有限數字"),
        }
    }
}

impl std::error::Error for BarError {}

impl Bar {
    /// 檢查這根 K 線本身是否合理。
    pub fn validate(&self) -> Result<(), BarError> {
        let prices = [self.open, self.high, self.low, self.close];
        if prices.iter().any(|p| *p <= Fixed::ZERO) {
            return Err(BarError::NonPositivePrice);
        }
        if self.high < self.open || self.high < self.close || self.high < self.low {
            return Err(BarError::HighTooLow);
        }
        if self.low > self.open || self.low > self.close {
            return Err(BarError::LowTooHigh);
        }
        if !self.volume.is_finite() || self.volume < 0.0 {
            return Err(BarError::BadVolume);
        }
        Ok(())
    }

    /// 這根 K 線的收盤時間（下一根的開盤時間）。
    pub fn close_time(&self, interval: Interval) -> i64 {
        self.open_time + interval.millis()
    }
}

/// 一串 K 線的時間順序錯誤。`index` 是出錯那根在陣列中的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesError {
    /// 開盤時間沒有對齊週期（例如 4 小時 K 線卻在 01:00 開盤）。
    Misaligned { index: usize, open_time: i64 },
    /// 和前一根時間相同。
    Duplicate { index: usize, open_time: i64 },
    /// 比前一根還早。
    OutOfOrder { index: usize, open_time: i64 },
}

impl fmt::Display for SeriesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SeriesError::Misaligned { index, open_time } => {
                write!(f, "第 {index} 根的開盤時間 {open_time} 沒有對齊週期")
            }
            SeriesError::Duplicate { index, open_time } => {
                write!(f, "第 {index} 根的開盤時間 {open_time} 和前一根重複")
            }
            SeriesError::OutOfOrder { index, open_time } => {
                write!(f, "第 {index} 根的開盤時間 {open_time} 比前一根早")
            }
        }
    }
}

impl std::error::Error for SeriesError {}

/// 一段缺少的 K 線：從 `from` 開始（含），共缺 `missing` 根。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub from: i64,
    pub missing: i64,
}

/// 檢查一串 K 線的時間順序，並列出中間缺了哪些。
///
/// - 沒對齊、重複、時間倒退：回傳錯誤，因為資料本身有問題。
/// - 中間缺幾根：只列出來，不算錯誤。交易所維護時真的會缺資料。
pub fn find_gaps(bars: &[Bar], interval: Interval) -> Result<Vec<Gap>, SeriesError> {
    let step = interval.millis();
    let mut gaps = Vec::new();
    for (index, bar) in bars.iter().enumerate() {
        if bar.open_time.rem_euclid(step) != 0 {
            return Err(SeriesError::Misaligned {
                index,
                open_time: bar.open_time,
            });
        }
        let Some(prev) = index.checked_sub(1).map(|i| bars[i]) else {
            continue;
        };
        if bar.open_time == prev.open_time {
            return Err(SeriesError::Duplicate {
                index,
                open_time: bar.open_time,
            });
        }
        if bar.open_time < prev.open_time {
            return Err(SeriesError::OutOfOrder {
                index,
                open_time: bar.open_time,
            });
        }
        let missing = (bar.open_time - prev.open_time) / step - 1;
        if missing > 0 {
            gaps.push(Gap {
                from: prev.open_time + step,
                missing,
            });
        }
    }
    Ok(gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    /// 2024-01-01 00:00 UTC
    const T0: i64 = 1_704_067_200_000;

    fn bar(open_time: i64, o: &str, h: &str, l: &str, c: &str) -> Bar {
        Bar {
            open_time,
            open: fx(o),
            high: fx(h),
            low: fx(l),
            close: fx(c),
            volume: 12.5,
        }
    }

    fn series(times: &[i64]) -> Vec<Bar> {
        times
            .iter()
            .map(|&t| bar(t, "100", "110", "90", "105"))
            .collect()
    }

    #[test]
    fn interval_millis() {
        assert_eq!(Interval::S1.millis(), 1_000);
        assert_eq!(Interval::M1.millis(), 60_000);
        assert_eq!(Interval::H4.millis(), 14_400_000);
        assert_eq!(Interval::D1.millis(), 86_400_000);
    }

    #[test]
    fn interval_parses_and_displays() {
        for i in Interval::ALL {
            assert_eq!(i.as_str().parse::<Interval>(), Ok(i));
            assert_eq!(i.to_string(), i.as_str());
        }
    }

    #[test]
    fn capital_m_is_a_month_not_a_minute() {
        assert!("1M".parse::<Interval>().is_err());
        assert!("1H".parse::<Interval>().is_err());
        assert!("2h".parse::<Interval>().is_err());
    }

    #[test]
    fn valid_bar_passes() {
        assert_eq!(bar(T0, "100", "110", "90", "105").validate(), Ok(()));
        // 一字線：開高低收都一樣也合理
        assert_eq!(bar(T0, "100", "100", "100", "100").validate(), Ok(()));
    }

    #[test]
    fn high_below_close_is_rejected() {
        assert_eq!(
            bar(T0, "100", "104", "90", "105").validate(),
            Err(BarError::HighTooLow)
        );
    }

    #[test]
    fn low_above_open_is_rejected() {
        assert_eq!(
            bar(T0, "100", "110", "101", "105").validate(),
            Err(BarError::LowTooHigh)
        );
    }

    #[test]
    fn zero_or_negative_price_is_rejected() {
        assert_eq!(
            bar(T0, "0", "110", "0", "105").validate(),
            Err(BarError::NonPositivePrice)
        );
    }

    #[test]
    fn bad_volume_is_rejected() {
        let mut b = bar(T0, "100", "110", "90", "105");
        b.volume = -1.0;
        assert_eq!(b.validate(), Err(BarError::BadVolume));
        b.volume = f64::NAN;
        assert_eq!(b.validate(), Err(BarError::BadVolume));
    }

    #[test]
    fn close_time_is_next_open() {
        let b = bar(T0, "100", "110", "90", "105");
        assert_eq!(b.close_time(Interval::H4), T0 + 14_400_000);
    }

    #[test]
    fn continuous_series_has_no_gaps() {
        let h = Interval::H1.millis();
        let bars = series(&[T0, T0 + h, T0 + 2 * h]);
        assert_eq!(find_gaps(&bars, Interval::H1), Ok(vec![]));
        assert_eq!(find_gaps(&[], Interval::H1), Ok(vec![]));
    }

    #[test]
    fn gaps_are_reported_not_rejected() {
        let h = Interval::H1.millis();
        let bars = series(&[T0, T0 + 3 * h, T0 + 4 * h, T0 + 6 * h]);
        assert_eq!(
            find_gaps(&bars, Interval::H1),
            Ok(vec![
                Gap {
                    from: T0 + h,
                    missing: 2
                },
                Gap {
                    from: T0 + 5 * h,
                    missing: 1
                },
            ])
        );
    }

    #[test]
    fn duplicate_is_an_error() {
        let bars = series(&[T0, T0]);
        assert_eq!(
            find_gaps(&bars, Interval::H1),
            Err(SeriesError::Duplicate {
                index: 1,
                open_time: T0
            })
        );
    }

    #[test]
    fn going_backwards_is_an_error() {
        let h = Interval::H1.millis();
        let bars = series(&[T0 + h, T0]);
        assert_eq!(
            find_gaps(&bars, Interval::H1),
            Err(SeriesError::OutOfOrder {
                index: 1,
                open_time: T0
            })
        );
    }

    #[test]
    fn misaligned_open_time_is_an_error() {
        // 4 小時 K 線應該在 00、04、08… 點開盤，01:00 開盤表示資料有問題
        let bars = series(&[T0 + Interval::H1.millis()]);
        assert_eq!(
            find_gaps(&bars, Interval::H4),
            Err(SeriesError::Misaligned {
                index: 0,
                open_time: T0 + Interval::H1.millis()
            })
        );
    }
}
