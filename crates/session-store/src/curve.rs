//! `sessions/<id>/curve.csv`：`open_time,equity` 一行一點。
//!
//! 風格比照 `at_core::bar_store`（純文字、人看得懂、append 友善、整份壞掉
//! 就報錯不跳過壞行），但這是全新的格式（2 欄不是 6 欄），所以不呼叫
//! `bar_store` 的函式，只延續它的做法。點的型別直接重用
//! `at_core::EquityPoint`（`{ open_time, equity }` 正好是這裡要的欄位）。

use at_core::{EquityPoint, Fixed};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// 讀寫 / 解析曲線檔失敗的原因。
#[derive(Debug)]
pub enum CurveError {
    /// 欄位數不是 2。
    WrongFieldCount {
        line: usize,
        found: usize,
    },
    /// 某一欄不是合法數字。
    BadNumber {
        line: usize,
        field: &'static str,
    },
    Io(io::Error),
}

impl fmt::Display for CurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CurveError::WrongFieldCount { line, found } => {
                write!(
                    f,
                    "曲線檔第 {line} 行欄位數不對：應該有 2 欄，實際有 {found} 欄"
                )
            }
            CurveError::BadNumber { line, field } => {
                write!(f, "曲線檔第 {line} 行的 {field} 欄不是合法數字")
            }
            CurveError::Io(err) => write!(f, "讀寫曲線檔失敗：{err}"),
        }
    }
}

impl std::error::Error for CurveError {}

fn parse_line(line: &str, line_no: usize) -> Result<EquityPoint, CurveError> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() != 2 {
        return Err(CurveError::WrongFieldCount {
            line: line_no,
            found: fields.len(),
        });
    }
    let open_time = fields[0]
        .trim()
        .parse::<i64>()
        .map_err(|_| CurveError::BadNumber {
            line: line_no,
            field: "open_time",
        })?;
    let equity = fields[1]
        .trim()
        .parse::<Fixed>()
        .map_err(|_| CurveError::BadNumber {
            line: line_no,
            field: "equity",
        })?;
    Ok(EquityPoint { open_time, equity })
}

/// 把曲線檔文字內容解析成 `Vec<EquityPoint>`。空白行會被跳過；空輸入回傳空陣列。
pub fn parse_curve(content: &str) -> Result<Vec<EquityPoint>, CurveError> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| parse_line(line, idx + 1))
        .collect()
}

/// 把 `Vec<EquityPoint>` 轉成曲線檔文字內容。
pub fn format_curve(points: &[EquityPoint]) -> String {
    let mut out = String::new();
    for p in points {
        out.push_str(&format!("{},{}\n", p.open_time, p.equity));
    }
    out
}

/// 從本機檔案讀取並解析整條曲線。檔案不存在時回傳空陣列（還沒有任何一根
/// 收盤 K 線時是正常狀態，不是錯誤）。
pub fn read_curve_file(path: impl AsRef<Path>) -> Result<Vec<EquityPoint>, CurveError> {
    match fs::read_to_string(path) {
        Ok(content) => parse_curve(&content),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(CurveError::Io(err)),
    }
}

/// 把整條曲線覆寫到本機檔案（用於啟動對帳等需要整份重寫的場合）。
pub fn write_curve_file(points: &[EquityPoint], path: impl AsRef<Path>) -> Result<(), CurveError> {
    if let Some(dir) = path.as_ref().parent() {
        fs::create_dir_all(dir).map_err(CurveError::Io)?;
    }
    fs::write(path, format_curve(points)).map_err(CurveError::Io)
}

/// 把一個點 append 到曲線檔末尾（執行中 session 每根收盤 K 線呼叫一次）。
///
/// 不做 `fsync`（成本不值得，ADR §5.4 第 3 條）：當機最多丟尾端幾行，
/// 啟動對帳會把這種 session 標成「中斷」而不是假裝完整。
pub fn append_curve_point(point: &EquityPoint, path: impl AsRef<Path>) -> Result<(), CurveError> {
    let path = path.as_ref();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(CurveError::Io)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(CurveError::Io)?;
    file.write_all(format!("{},{}\n", point.open_time, point.equity).as_bytes())
        .map_err(CurveError::Io)
}

/// 等距降採樣到最多 `max_points` 點，給 sparkline／總覽曲線用
/// （不要把幾萬點丟過 IPC，ADR §7.2）。`max_points == 0` 回空陣列；
/// 點數本來就不超過上限時原樣回傳。
pub fn downsample(points: &[EquityPoint], max_points: usize) -> Vec<EquityPoint> {
    if max_points == 0 || points.is_empty() {
        return Vec::new();
    }
    if points.len() <= max_points {
        return points.to_vec();
    }
    if max_points == 1 {
        return vec![points[points.len() - 1]];
    }
    let step = (points.len() - 1) as f64 / (max_points - 1) as f64;
    (0..max_points)
        .map(|i| {
            let idx = ((i as f64) * step).round() as usize;
            points[idx.min(points.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    fn point(open_time: i64, equity: &str) -> EquityPoint {
        EquityPoint {
            open_time,
            equity: fx(equity),
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "at_session_store_curve_test_{}_{name}",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_through_a_real_file() {
        let path = temp_path("roundtrip.csv");
        let points = vec![point(1_000, "10000"), point(2_000, "10184.52")];
        write_curve_file(&points, &path).unwrap();
        let read_back = read_curve_file(&path).unwrap();
        assert_eq!(read_back, points);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_empty_not_an_error() {
        let path = temp_path("does_not_exist.csv");
        assert_eq!(read_curve_file(&path).unwrap(), Vec::new());
    }

    #[test]
    fn append_builds_up_the_file_one_line_at_a_time() {
        let path = temp_path("append.csv");
        fs::remove_file(&path).ok();
        append_curve_point(&point(1_000, "10000"), &path).unwrap();
        append_curve_point(&point(2_000, "10100"), &path).unwrap();
        let read_back = read_curve_file(&path).unwrap();
        assert_eq!(
            read_back,
            vec![point(1_000, "10000"), point(2_000, "10100")]
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_line_is_an_error_not_silently_skipped() {
        let path = temp_path("corrupt.csv");
        fs::write(&path, "not,a,valid,line\n").unwrap();
        let err = read_curve_file(&path).unwrap_err();
        assert!(matches!(
            err,
            CurveError::WrongFieldCount { line: 1, found: 4 }
        ));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn downsample_keeps_first_and_last_point() {
        let points: Vec<_> = (0..100).map(|i| point(i, "10000")).collect();
        let sampled = downsample(&points, 10);
        assert_eq!(sampled.len(), 10);
        assert_eq!(sampled.first(), points.first());
        assert_eq!(sampled.last(), points.last());
    }

    #[test]
    fn downsample_is_a_no_op_when_already_within_the_limit() {
        let points = vec![point(1, "1"), point(2, "2")];
        assert_eq!(downsample(&points, 30), points);
    }
}
