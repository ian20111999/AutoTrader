//! K 線本機儲存：把 `Vec<Bar>` 存成本機文字檔、讀回來要和存之前完全一樣。
//!
//! 目的是給 1.7 下載器、之後的回測引擎重複讀取歷史資料用，不用每次都
//! 重新解析原始 CSV。
//!
//! 格式跟 1.5 的 Binance CSV 類似（逗號分隔、無 header、每行一根），
//! 但只存 `Bar` 真正有的 6 個欄位：
//!
//! ```text
//! open_time(ms), open, high, low, close, volume
//! ```
//!
//! 這是這個專案自己的格式，不是 Binance 的格式，所以不重用
//! `kline_csv` 的解析器（欄位數不同：6 欄不是 12 欄）。

use crate::bar::{Bar, BarError};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

/// 儲存或讀取本機 K 線檔失敗的原因。`line` 是第幾行（從 1 起算）。
#[derive(Debug)]
pub enum BarStoreError {
    /// 欄位數不是 6。
    WrongFieldCount { line: usize, found: usize },
    /// 某一欄不是合法數字。
    BadNumber { line: usize, field: &'static str },
    /// 數字都解析成功，但湊出來的 K 線不合理（例如最高價低於最低價）。
    /// 格式合法但數值不合理的檔案可能是手動編輯或程式中途崩潰寫壞的。
    InvalidBar { line: usize, source: BarError },
    /// 讀寫檔案失敗。
    Io(io::Error),
}

impl fmt::Display for BarStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BarStoreError::WrongFieldCount { line, found } => {
                write!(f, "第 {line} 行欄位數不對：應該有 6 欄，實際有 {found} 欄")
            }
            BarStoreError::BadNumber { line, field } => {
                write!(f, "第 {line} 行的 {field} 欄不是合法數字")
            }
            BarStoreError::InvalidBar { line, source } => {
                write!(f, "第 {line} 行解析出的 K 線不合理：{source}")
            }
            BarStoreError::Io(err) => write!(f, "讀寫本機 K 線檔失敗：{err}"),
        }
    }
}

impl std::error::Error for BarStoreError {}

/// 解析單一欄位，失敗時回報是第幾行、哪一欄。
fn parse_field<T: FromStr>(
    field: &str,
    line: usize,
    name: &'static str,
) -> Result<T, BarStoreError> {
    field
        .trim()
        .parse::<T>()
        .map_err(|_| BarStoreError::BadNumber { line, field: name })
}

fn parse_line(line: &str, line_no: usize) -> Result<Bar, BarStoreError> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() != 6 {
        return Err(BarStoreError::WrongFieldCount {
            line: line_no,
            found: fields.len(),
        });
    }
    let bar = Bar {
        open_time: parse_field(fields[0], line_no, "open_time")?,
        open: parse_field(fields[1], line_no, "open")?,
        high: parse_field(fields[2], line_no, "high")?,
        low: parse_field(fields[3], line_no, "low")?,
        close: parse_field(fields[4], line_no, "close")?,
        volume: parse_field(fields[5], line_no, "volume")?,
    };
    bar.validate().map_err(|source| BarStoreError::InvalidBar {
        line: line_no,
        source,
    })?;
    Ok(bar)
}

/// 把儲存格式的文字內容解析成 `Vec<Bar>`。空白行會被跳過；空輸入回傳空陣列。
pub fn parse_bars(content: &str) -> Result<Vec<Bar>, BarStoreError> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| parse_line(line, idx + 1))
        .collect()
}

/// 把 `Vec<Bar>` 轉成儲存格式的文字內容。
///
/// 用 `Fixed`、`i64`、`f64` 各自的 `Display` 序列化，對應 `parse_bars`
/// 用它們的 `FromStr` 還原，兩邊都已經測過會 round-trip（見 1.1、1.2）。
pub fn format_bars(bars: &[Bar]) -> String {
    let mut out = String::new();
    for bar in bars {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            bar.open_time, bar.open, bar.high, bar.low, bar.close, bar.volume
        ));
    }
    out
}

/// 從本機檔案讀取並解析歷史 K 線。
pub fn read_bars_file(path: impl AsRef<Path>) -> Result<Vec<Bar>, BarStoreError> {
    let content = fs::read_to_string(path).map_err(BarStoreError::Io)?;
    parse_bars(&content)
}

/// 把 `Vec<Bar>` 存成本機檔案。
pub fn write_bars_file(bars: &[Bar], path: impl AsRef<Path>) -> Result<(), BarStoreError> {
    fs::write(path, format_bars(bars)).map_err(BarStoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::Fixed;
    use std::env;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn bar(open_time: i64, o: &str, h: &str, l: &str, c: &str, volume: f64) -> Bar {
        Bar {
            open_time,
            open: fx(o),
            high: fx(h),
            low: fx(l),
            close: fx(c),
            volume,
        }
    }

    /// 每個測試用不同檔名，避免平行跑測試時互相覆寫。
    fn temp_path(name: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "at_core_bar_store_test_{}_{name}",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_through_a_real_file() {
        let path = temp_path("roundtrip.txt");
        let bars = vec![
            bar(
                1_704_067_200_000,
                "42000",
                "42050",
                "41980",
                "42020",
                12.345,
            ),
            bar(
                1_704_067_260_000,
                "42020",
                "42100",
                "42000",
                "42080",
                15.678,
            ),
        ];

        write_bars_file(&bars, &path).unwrap();
        let read_back = read_bars_file(&path).unwrap();

        assert_eq!(read_back, bars);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_vec_round_trips_to_empty_vec() {
        let path = temp_path("empty.txt");
        write_bars_file(&[], &path).unwrap();
        let read_back = read_bars_file(&path).unwrap();
        assert_eq!(read_back, vec![]);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn parse_bars_empty_input_is_empty_vec() {
        assert_eq!(parse_bars("").unwrap(), vec![]);
        assert_eq!(parse_bars("\n\n").unwrap(), vec![]);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let path = temp_path("does_not_exist.txt");
        let err = read_bars_file(&path).unwrap_err();
        assert!(matches!(err, BarStoreError::Io(_)));
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_panic() {
        let path = temp_path("corrupt.txt");
        fs::write(&path, "not,a,valid,bar,line\n").unwrap();

        let err = read_bars_file(&path).unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::WrongFieldCount { line: 1, found: 5 }
        ));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_number_of_fields_is_rejected() {
        let err = parse_bars("1,2,3,4,5").unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::WrongFieldCount { line: 1, found: 5 }
        ));
    }

    #[test]
    fn non_numeric_field_is_rejected() {
        let err = parse_bars("1704067200000,abc,42050,41980,42020,12.345").unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::BadNumber {
                line: 1,
                field: "open"
            }
        ));
    }

    #[test]
    fn well_formed_but_unreasonable_bar_is_rejected() {
        // 欄位格式合法（6 欄、都是合法數字），但 high(90) < low(95)——
        // 這種檔案可能是手動編輯或程式中途崩潰寫壞的，不能靜默接受。
        let path = temp_path("invalid_bar.txt");
        fs::write(&path, "1704067200000,100,90,95,100,12.345\n").unwrap();

        let err = read_bars_file(&path).unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::InvalidBar {
                line: 1,
                source: BarError::HighTooLow
            }
        ));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn format_is_stable_and_predictable() {
        let bars = vec![bar(
            1_704_067_200_000,
            "42000",
            "42050",
            "41980",
            "42020",
            12.345,
        )];
        assert_eq!(
            format_bars(&bars),
            "1704067200000,42000,42050,41980,42020,12.345\n"
        );
    }

    #[test]
    fn one_line_per_bar() {
        let bars = vec![
            bar(1_704_067_200_000, "1", "1", "1", "1", 0.0),
            bar(1_704_067_260_000, "2", "2", "2", "2", 0.0),
            bar(1_704_067_320_000, "3", "3", "3", "3", 0.0),
        ];
        let text = format_bars(&bars);
        assert_eq!(text.lines().count(), 3);
    }
}
