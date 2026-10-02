//! K 線本機儲存：把 `Vec<Bar>` 存成本機文字檔、讀回來要和存之前完全一樣。
//!
//! 目的是給 1.7 下載器、之後的回測引擎重複讀取歷史資料用，不用每次都
//! 重新解析原始 CSV。
//!
//! 格式跟 1.5 的 Binance CSV 類似（逗號分隔、無 header、每行一根），
//! 存 `Bar` 的欄位：
//!
//! ```text
//! open_time(ms), open, high, low, close, volume[, trades, taker_buy_volume]
//! ```
//!
//! 這是這個專案自己的格式，不是 Binance 的格式，所以不重用
//! `kline_csv` 的解析器（欄位數不同：6/8 欄不是 12 欄）。
//!
//! **版本用欄位數自我描述**（不加版本號那一行）：6 欄是舊格式（沒有訂單流，
//! 讀成 `order_flow: None`），8 欄是新格式（多了成交筆數、主動買方成交量）。
//! 同一個檔案裡所有行的欄位數必須一致——第一個非空行決定這個檔案是 6 欄還是
//! 8 欄，之後任何一行欄位數不同都是 `WrongFieldCount` 錯誤，不會把「半欄數」
//! 的檔案靜默讀成一段有訂單流、一段沒有。
//! 寫檔：只有全部 `Bar` 都有 `order_flow` 才寫 8 欄，否則寫 6 欄
//! （所以舊的 golden 測試不用改也會綠）。

use crate::bar::{Bar, BarError, OrderFlow};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

/// 儲存或讀取本機 K 線檔失敗的原因。`line` 是第幾行（從 1 起算）。
#[derive(Debug)]
pub enum BarStoreError {
    /// 欄位數不是 6 也不是 8，或者和同一檔案裡前面的行欄位數不一致。
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
                write!(
                    f,
                    "第 {line} 行欄位數不對：應該是 6 欄（舊格式）或 8 欄（含訂單流），\
且同一檔案內欄位數必須一致，實際有 {found} 欄"
                )
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

/// 解析一行，`expected_fields` 是這個檔案（由第一個非空行決定）的欄位數，
/// 6 或 8。行的實際欄位數和它不同就是 `WrongFieldCount`。
fn parse_line(line: &str, line_no: usize, expected_fields: usize) -> Result<Bar, BarStoreError> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() != expected_fields {
        return Err(BarStoreError::WrongFieldCount {
            line: line_no,
            found: fields.len(),
        });
    }
    let order_flow = if expected_fields == 8 {
        Some(OrderFlow {
            trades: parse_field(fields[6], line_no, "trades")?,
            taker_buy_volume: parse_field(fields[7], line_no, "taker_buy_volume")?,
        })
    } else {
        None
    };
    let bar = Bar {
        open_time: parse_field(fields[0], line_no, "open_time")?,
        open: parse_field(fields[1], line_no, "open")?,
        high: parse_field(fields[2], line_no, "high")?,
        low: parse_field(fields[3], line_no, "low")?,
        close: parse_field(fields[4], line_no, "close")?,
        volume: parse_field(fields[5], line_no, "volume")?,
        order_flow,
    };
    bar.validate().map_err(|source| BarStoreError::InvalidBar {
        line: line_no,
        source,
    })?;
    Ok(bar)
}

/// 把儲存格式的文字內容解析成 `Vec<Bar>`。空白行會被跳過；空輸入回傳空陣列。
///
/// 欄位數由第一個非空行決定（6 或 8），其餘行欄位數必須和它一致。
pub fn parse_bars(content: &str) -> Result<Vec<Bar>, BarStoreError> {
    let mut lines = content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());

    let Some((first_idx, first_line)) = lines.next() else {
        return Ok(Vec::new());
    };
    let expected_fields = first_line.split(',').count();
    if expected_fields != 6 && expected_fields != 8 {
        return Err(BarStoreError::WrongFieldCount {
            line: first_idx + 1,
            found: expected_fields,
        });
    }

    let mut bars = vec![parse_line(first_line, first_idx + 1, expected_fields)?];
    for (idx, line) in lines {
        bars.push(parse_line(line, idx + 1, expected_fields)?);
    }
    Ok(bars)
}

/// 把 `Vec<Bar>` 轉成儲存格式的文字內容。
///
/// 用 `Fixed`、`i64`、`f64` 各自的 `Display` 序列化，對應 `parse_bars`
/// 用它們的 `FromStr` 還原，兩邊都已經測過會 round-trip（見 1.1、1.2）。
///
/// 只有全部 `Bar` 都有 `order_flow` 才寫 8 欄，否則寫 6 欄（不混欄數）。
pub fn format_bars(bars: &[Bar]) -> String {
    let with_order_flow = !bars.is_empty() && bars.iter().all(|b| b.order_flow.is_some());
    let mut out = String::new();
    for bar in bars {
        out.push_str(&format!(
            "{},{},{},{},{},{}",
            bar.open_time, bar.open, bar.high, bar.low, bar.close, bar.volume
        ));
        if with_order_flow {
            let flow = bar.order_flow.expect("checked by with_order_flow above");
            out.push_str(&format!(",{},{}", flow.trades, flow.taker_buy_volume));
        }
        out.push('\n');
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
            order_flow: None,
        }
    }

    fn bar_with_flow(
        open_time: i64,
        o: &str,
        h: &str,
        l: &str,
        c: &str,
        volume: f64,
        flow: (u64, f64),
    ) -> Bar {
        Bar {
            order_flow: Some(OrderFlow {
                trades: flow.0,
                taker_buy_volume: flow.1,
            }),
            ..bar(open_time, o, h, l, c, volume)
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
    fn round_trips_order_flow_through_eight_column_file() {
        let path = temp_path("roundtrip_flow.txt");
        let bars = vec![
            bar_with_flow(
                1_704_067_200_000,
                "42000",
                "42050",
                "41980",
                "42020",
                12.345,
                (150, 6.0),
            ),
            bar_with_flow(
                1_704_067_260_000,
                "42020",
                "42100",
                "42000",
                "42080",
                15.678,
                (200, 8.0),
            ),
        ];

        write_bars_file(&bars, &path).unwrap();
        let read_back = read_bars_file(&path).unwrap();

        assert_eq!(read_back, bars);
        assert_eq!(
            format_bars(&bars)
                .lines()
                .next()
                .unwrap()
                .split(',')
                .count(),
            8
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mixed_field_counts_in_one_file_is_rejected() {
        let path = temp_path("mixed.txt");
        fs::write(
            &path,
            "1704067200000,42000,42050,41980,42020,12.345,150,6.0\n\
             1704067260000,42020,42100,42000,42080,15.678\n",
        )
        .unwrap();

        let err = read_bars_file(&path).unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::WrongFieldCount { line: 2, found: 6 }
        ));
        fs::remove_file(&path).ok();
    }

    /// 跟上面相反的方向：第一行先定出 6 欄，第二行卻是 8 欄——一樣要擋，不能因為
    /// 「多出來的欄位」看起來無害就放過。
    #[test]
    fn mixed_field_counts_six_then_eight_is_rejected() {
        let path = temp_path("mixed_six_then_eight.txt");
        fs::write(
            &path,
            "1704067200000,42000,42050,41980,42020,12.345\n\
             1704067260000,42020,42100,42000,42080,15.678,150,6.0\n",
        )
        .unwrap();

        let err = read_bars_file(&path).unwrap_err();
        assert!(matches!(
            err,
            BarStoreError::WrongFieldCount { line: 2, found: 8 }
        ));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn all_none_order_flow_writes_six_columns() {
        let bars = vec![bar(1_704_067_200_000, "1", "1", "1", "1", 0.0)];
        assert_eq!(
            format_bars(&bars)
                .lines()
                .next()
                .unwrap()
                .split(',')
                .count(),
            6
        );
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
