//! 讀取 Binance 官方歷史 K 線 CSV。
//!
//! 檔案來源：data.binance.vision（例如 `BTCUSDT-1m-2024-01.csv`）。
//! 格式：無 header、逗號分隔，每行 12 欄：
//!
//! ```text
//! open_time(ms), open, high, low, close, volume, close_time(ms),
//! quote_asset_volume, number_of_trades,
//! taker_buy_base_asset_volume, taker_buy_quote_asset_volume, ignore
//! ```
//!
//! 除了前 6 欄（開高低收量＋開盤時間）以外，也讀第 9 欄（成交筆數）與
//! 第 10 欄（主動買方成交量，基礎幣計）組成 `OrderFlow`。
//! 不收報價幣成交額（第 8、11 欄）與 `ignore`（第 12 欄）：
//! 理由見 `docs/architecture/2026-10-02-bar-order-flow-fields.md` §4。

use crate::bar::{Bar, BarError, OrderFlow};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

/// 解析 K 線 CSV 失敗的原因。`line` 是第幾行（從 1 起算）。
#[derive(Debug)]
pub enum KlineCsvError {
    /// 欄位數不是 12。
    WrongFieldCount { line: usize, found: usize },
    /// 某一欄不是合法數字。
    BadNumber { line: usize, field: &'static str },
    /// 解析出的 K 線本身不合理（例如最高價低於最低價）。
    InvalidBar { line: usize, source: BarError },
    /// 讀取檔案失敗。
    Io(io::Error),
}

impl fmt::Display for KlineCsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KlineCsvError::WrongFieldCount { line, found } => {
                write!(f, "第 {line} 行欄位數不對：應該有 12 欄，實際有 {found} 欄")
            }
            KlineCsvError::BadNumber { line, field } => {
                write!(f, "第 {line} 行的 {field} 欄不是合法數字")
            }
            KlineCsvError::InvalidBar { line, source } => {
                write!(f, "第 {line} 行解析出的 K 線不合理：{source}")
            }
            KlineCsvError::Io(err) => write!(f, "讀取檔案失敗：{err}"),
        }
    }
}

impl std::error::Error for KlineCsvError {}

/// 解析單一欄位，失敗時回報是第幾行、哪一欄。
fn parse_field<T: FromStr>(
    field: &str,
    line: usize,
    name: &'static str,
) -> Result<T, KlineCsvError> {
    field
        .trim()
        .parse::<T>()
        .map_err(|_| KlineCsvError::BadNumber { line, field: name })
}

fn parse_line(line: &str, line_no: usize) -> Result<Bar, KlineCsvError> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() != 12 {
        return Err(KlineCsvError::WrongFieldCount {
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
        order_flow: Some(OrderFlow {
            trades: parse_field(fields[8], line_no, "number_of_trades")?,
            taker_buy_volume: parse_field(fields[9], line_no, "taker_buy_base_asset_volume")?,
        }),
    };
    bar.validate().map_err(|source| KlineCsvError::InvalidBar {
        line: line_no,
        source,
    })?;
    Ok(bar)
}

/// 解析 CSV 內容字串。空白行會被跳過；空輸入回傳空陣列（不是錯誤）。
pub fn parse_klines_csv(content: &str) -> Result<Vec<Bar>, KlineCsvError> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| parse_line(line, idx + 1))
        .collect()
}

/// 從檔案讀取並解析歷史 K 線 CSV。
pub fn read_klines_csv(path: impl AsRef<Path>) -> Result<Vec<Bar>, KlineCsvError> {
    let content = fs::read_to_string(path).map_err(KlineCsvError::Io)?;
    parse_klines_csv(&content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::Fixed;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    #[test]
    fn empty_input_returns_empty_vec() {
        assert_eq!(parse_klines_csv("").unwrap(), vec![]);
        assert_eq!(parse_klines_csv("\n\n").unwrap(), vec![]);
    }

    #[test]
    fn parses_single_line() {
        // Binance 官方文件範例行
        let line = "1601510340000,4.15070000,4.15870000,4.15060000,4.15540000,\
539.23000000,1601510399999,2240.39860900,13,401.82000000,1669.98121300,0";
        let bars = parse_klines_csv(line).unwrap();
        assert_eq!(
            bars,
            vec![Bar {
                open_time: 1_601_510_340_000,
                open: fx("4.15070000"),
                high: fx("4.15870000"),
                low: fx("4.15060000"),
                close: fx("4.15540000"),
                volume: 539.23,
                order_flow: Some(OrderFlow {
                    trades: 13,
                    taker_buy_volume: 401.82,
                }),
            }]
        );
    }

    #[test]
    fn parses_multiple_lines_in_order() {
        let content = "\
1704067200000,42000.00000000,42050.00000000,41980.00000000,42020.00000000,12.34500000,1704067259999,518000.00000000,150,6.00000000,252000.00000000,0
1704067260000,42020.00000000,42100.00000000,42000.00000000,42080.00000000,15.67800000,1704067319999,660000.00000000,200,8.00000000,336000.00000000,0";
        let bars = parse_klines_csv(content).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open_time, 1_704_067_200_000);
        assert_eq!(bars[1].open_time, 1_704_067_260_000);
        assert_eq!(bars[1].open, fx("42020"));
    }

    #[test]
    fn wrong_field_count_is_rejected() {
        let err = parse_klines_csv("1,2,3,4,5").unwrap_err();
        assert!(matches!(
            err,
            KlineCsvError::WrongFieldCount { line: 1, found: 5 }
        ));
    }

    #[test]
    fn non_numeric_field_is_rejected() {
        let line = "1601510340000,abc,4.15870000,4.15060000,4.15540000,\
539.23000000,1601510399999,2240.39860900,13,401.82000000,1669.98121300,0";
        let err = parse_klines_csv(line).unwrap_err();
        assert!(matches!(
            err,
            KlineCsvError::BadNumber {
                line: 1,
                field: "open"
            }
        ));
    }

    #[test]
    fn invalid_bar_reuses_bar_validation() {
        // high(4.0) 比 open、low 都低 → 重用 Bar::validate 的 HighTooLow
        let line = "1601510340000,4.15070000,4.00000000,4.15060000,4.15540000,\
539.23000000,1601510399999,2240.39860900,13,401.82000000,1669.98121300,0";
        let err = parse_klines_csv(line).unwrap_err();
        assert!(matches!(
            err,
            KlineCsvError::InvalidBar {
                line: 1,
                source: BarError::HighTooLow
            }
        ));
    }

    #[test]
    fn reads_fixture_file() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample_klines.csv"
        );
        let bars = read_klines_csv(path).unwrap();
        assert_eq!(bars.len(), 4);
        assert_eq!(bars[0].open_time, 1_704_067_200_000);
        assert_eq!(bars[0].open, fx("42000"));
        assert_eq!(bars[3].close, fx("42180"));
    }
}
