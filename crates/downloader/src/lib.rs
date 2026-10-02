//! 從 `data.binance.vision` 下載歷史 K 線：組 URL → HTTP 下載 zip → 解壓 → 用
//! `at_core::parse_klines_csv` 解析成 `Vec<Bar>` → 用 `at_core::write_bars_file` 存本機。
//!
//! 這是公開資料（不需要 API 金鑰），目前只支援現貨市場的月線檔
//! （`data/spot/monthly/klines/...`）。daily 版本與合約市場的路徑格式類似，
//! 只是網址前綴不同，等真的需要再加，不先幫還沒用到的東西打地基。

use at_core::{
    parse_klines_csv, write_bars_file, Bar, BarStoreError, Interval, KlineCsvError, Symbol,
};
use std::fmt;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

const BASE_URL: &str = "https://data.binance.vision";

/// 下載、解壓、解析、存檔任一步驟失敗的原因。
#[derive(Debug)]
pub enum DownloadError {
    /// 月份不在 1~12 之間。
    InvalidMonth(u32),
    /// HTTP 下載失敗（連線錯誤、逾時、4xx/5xx 狀態碼）。
    Http(String),
    /// zip 格式壞掉，或內容不是合法的 zip 檔。
    Zip(String),
    /// zip 內的檔案數不是預期的 1 個。
    UnexpectedEntryCount(usize),
    /// 解壓出來的 CSV 內容解析失敗。
    Csv(KlineCsvError),
    /// 存成本機檔案失敗。
    Store(BarStoreError),
    /// 建立本機資料夾失敗。
    Io(io::Error),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::InvalidMonth(m) => write!(f, "月份必須是 1 到 12，收到 {m}"),
            DownloadError::Http(msg) => write!(f, "下載失敗：{msg}"),
            DownloadError::Zip(msg) => write!(f, "解壓 zip 失敗：{msg}"),
            DownloadError::UnexpectedEntryCount(n) => {
                write!(f, "zip 內應該剛好有 1 個檔案，實際有 {n} 個")
            }
            DownloadError::Csv(e) => write!(f, "解析 K 線 CSV 失敗：{e}"),
            DownloadError::Store(e) => write!(f, "存本機檔案失敗：{e}"),
            DownloadError::Io(e) => write!(f, "建立本機資料夾失敗：{e}"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<KlineCsvError> for DownloadError {
    fn from(e: KlineCsvError) -> Self {
        DownloadError::Csv(e)
    }
}

impl From<BarStoreError> for DownloadError {
    fn from(e: BarStoreError) -> Self {
        DownloadError::Store(e)
    }
}

/// 組出 Binance 現貨月線 zip 的下載網址，例如：
/// `https://data.binance.vision/data/spot/monthly/klines/BTCUSDT/1m/BTCUSDT-1m-2024-01.zip`
pub fn monthly_kline_url(
    symbol: &Symbol,
    interval: Interval,
    year: u32,
    month: u32,
) -> Result<String, DownloadError> {
    if !(1..=12).contains(&month) {
        return Err(DownloadError::InvalidMonth(month));
    }
    let symbol = symbol.as_str();
    let interval = interval.as_str();
    Ok(format!(
        "{BASE_URL}/data/spot/monthly/klines/{symbol}/{interval}/{symbol}-{interval}-{year:04}-{month:02}.zip"
    ))
}

/// Binance 官方的月線 zip 裡剛好只有一個 CSV 檔，解壓、讀成字串。
fn extract_single_csv(zip_bytes: &[u8]) -> Result<String, DownloadError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| DownloadError::Zip(e.to_string()))?;
    if archive.len() != 1 {
        return Err(DownloadError::UnexpectedEntryCount(archive.len()));
    }
    let mut file = archive
        .by_index(0)
        .map_err(|e| DownloadError::Zip(e.to_string()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| DownloadError::Zip(e.to_string()))?;
    Ok(content)
}

/// 打 HTTP GET，把整個回應內容讀成 bytes。抽成獨立函式方便之後替換/測試。
fn fetch_bytes(url: &str) -> Result<Vec<u8>, DownloadError> {
    ureq::get(url)
        .call()
        .map_err(|e| DownloadError::Http(e.to_string()))?
        .body_mut()
        .read_to_vec()
        .map_err(|e| DownloadError::Http(e.to_string()))
}

/// 下載一個月的歷史 K 線並解析成 `Vec<Bar>`（不存檔）。
pub fn download_monthly_klines(
    symbol: &Symbol,
    interval: Interval,
    year: u32,
    month: u32,
) -> Result<Vec<Bar>, DownloadError> {
    let url = monthly_kline_url(symbol, interval, year, month)?;
    let zip_bytes = fetch_bytes(&url)?;
    let csv = extract_single_csv(&zip_bytes)?;
    Ok(parse_klines_csv(&csv)?)
}

/// 這個月的 K 線存在本機的哪個路徑：
/// `<base_dir>/<SYMBOL>/<interval>/<SYMBOL>-<interval>-<year>-<month>.csv`
pub fn local_path(
    base_dir: impl AsRef<Path>,
    symbol: &Symbol,
    interval: Interval,
    year: u32,
    month: u32,
) -> PathBuf {
    let symbol = symbol.as_str();
    let interval = interval.as_str();
    base_dir
        .as_ref()
        .join(symbol)
        .join(interval)
        .join(format!("{symbol}-{interval}-{year:04}-{month:02}.csv"))
}

/// 下載一個月的歷史 K 線並存到本機檔案，回傳解析出的 K 線與存檔路徑。
pub fn download_and_store_monthly_klines(
    symbol: &Symbol,
    interval: Interval,
    year: u32,
    month: u32,
    base_dir: impl AsRef<Path>,
) -> Result<(Vec<Bar>, PathBuf), DownloadError> {
    let bars = download_monthly_klines(symbol, interval, year, month)?;
    let path = local_path(base_dir, symbol, interval, year, month);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(DownloadError::Io)?;
    }
    write_bars_file(&bars, &path)?;
    Ok((bars, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    fn sym(s: &str) -> Symbol {
        Symbol::new(s).unwrap()
    }

    /// 建一個記憶體中的 zip，裡面放指定的檔案（名稱、內容）。
    /// 用 `Stored`（不壓縮）：測試只關心解壓邏輯本身，不需要真的壓縮，
    /// 也不需要為了寫測試 fixture 多啟用一個壓縮後端的 cargo feature。
    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn builds_expected_monthly_url() {
        let url = monthly_kline_url(&sym("BTCUSDT"), Interval::M1, 2024, 1).unwrap();
        assert_eq!(
            url,
            "https://data.binance.vision/data/spot/monthly/klines/BTCUSDT/1m/BTCUSDT-1m-2024-01.zip"
        );
    }

    #[test]
    fn builds_expected_monthly_url_for_1s_interval() {
        // 1 秒 K 線歷史資料的 URL 模板跟其他週期完全一樣，只是把 "1m" 換成 "1s"。
        let url = monthly_kline_url(&sym("BTCUSDT"), Interval::S1, 2024, 1).unwrap();
        assert_eq!(
            url,
            "https://data.binance.vision/data/spot/monthly/klines/BTCUSDT/1s/BTCUSDT-1s-2024-01.zip"
        );
    }

    #[test]
    fn local_path_layout_for_1s_interval() {
        let path = local_path("data", &sym("BTCUSDT"), Interval::S1, 2024, 1);
        assert_eq!(
            path,
            PathBuf::from("data/BTCUSDT/1s/BTCUSDT-1s-2024-01.csv")
        );
    }

    /// Binance 開始提供 1 秒歷史資料的時間比 1 分鐘晚，太早的月份會 404。
    /// 實際打一個鐵定不存在的月份，驗證 `download_monthly_klines` 回清楚的
    /// `DownloadError::Http`，不是 panic、也不是靜默回傳空陣列。
    ///
    /// 需要網路，預設不跑。手動驗證：`cargo test -p at-downloader -- --ignored`
    #[test]
    #[ignore]
    fn a_month_with_no_1s_history_yet_is_reported_as_a_clear_http_error() {
        // 2019-01 早於 Binance 開始提供 1 秒 K 線歷史資料的時間。
        let err = download_monthly_klines(&sym("BTCUSDT"), Interval::S1, 2019, 1).unwrap_err();
        assert!(
            matches!(err, DownloadError::Http(_)),
            "找不到的月份要回清楚的 Http 錯誤，不是 panic 或空陣列：{err:?}"
        );
    }

    #[test]
    fn month_is_zero_padded() {
        let url = monthly_kline_url(&sym("ETHUSDT"), Interval::H4, 2023, 9).unwrap();
        assert!(url.ends_with("/ETHUSDT-4h-2023-09.zip"));
    }

    #[test]
    fn rejects_invalid_month() {
        assert!(matches!(
            monthly_kline_url(&sym("BTCUSDT"), Interval::M1, 2024, 0),
            Err(DownloadError::InvalidMonth(0))
        ));
        assert!(matches!(
            monthly_kline_url(&sym("BTCUSDT"), Interval::M1, 2024, 13),
            Err(DownloadError::InvalidMonth(13))
        ));
    }

    #[test]
    fn extracts_the_single_csv_entry() {
        let csv_line = "1704067200000,42000,42050,41980,42020,12.345,1704067259999,0,0,0,0,0\n";
        let zip_bytes = make_zip(&[("BTCUSDT-1m-2024-01.csv", csv_line.as_bytes())]);
        assert_eq!(extract_single_csv(&zip_bytes).unwrap(), csv_line);
    }

    #[test]
    fn rejects_zip_with_more_than_one_entry() {
        let zip_bytes = make_zip(&[("a.csv", b"x"), ("b.csv", b"y")]);
        assert!(matches!(
            extract_single_csv(&zip_bytes),
            Err(DownloadError::UnexpectedEntryCount(2))
        ));
    }

    #[test]
    fn rejects_zip_with_no_entries() {
        let zip_bytes = make_zip(&[]);
        assert!(matches!(
            extract_single_csv(&zip_bytes),
            Err(DownloadError::UnexpectedEntryCount(0))
        ));
    }

    #[test]
    fn rejects_bytes_that_are_not_a_zip() {
        assert!(matches!(
            extract_single_csv(b"not a zip file at all"),
            Err(DownloadError::Zip(_))
        ));
    }

    #[test]
    fn download_monthly_klines_parses_the_zip_end_to_end() {
        // 串起「解壓 → 解析」但不打真正的網路：直接把記憶體 zip 丟給
        // extract_single_csv + parse_klines_csv，驗證兩段接得起來。
        let csv_line = "1704067200000,42000,42050,41980,42020,12.345,1704067259999,0,0,0,0,0\n\
                         1704067260000,42020,42100,42000,42080,15.678,1704067319999,0,0,0,0,0\n";
        let zip_bytes = make_zip(&[("BTCUSDT-1m-2024-01.csv", csv_line.as_bytes())]);
        let csv = extract_single_csv(&zip_bytes).unwrap();
        let bars = parse_klines_csv(&csv).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open_time, 1_704_067_200_000);
        assert_eq!(bars[1].open_time, 1_704_067_260_000);
    }

    #[test]
    fn local_path_layout() {
        let path = local_path("data", &sym("BTCUSDT"), Interval::M1, 2024, 1);
        assert_eq!(
            path,
            PathBuf::from("data/BTCUSDT/1m/BTCUSDT-1m-2024-01.csv")
        );
    }

    #[test]
    fn download_and_store_writes_a_readable_file() {
        // 只測「存檔 + 讀回」這一段，不牽涉下載：直接呼叫 write_bars_file
        // 走一遍 download_and_store_monthly_klines 存檔用的同一條路徑邏輯。
        let base =
            std::env::temp_dir().join(format!("at_downloader_test_{}_store", std::process::id()));
        let symbol = sym("BTCUSDT");
        let path = local_path(&base, &symbol, Interval::D1, 2024, 1);
        let bars = vec![Bar {
            open_time: 1_704_067_200_000,
            open: "42000".parse().unwrap(),
            high: "42050".parse().unwrap(),
            low: "41980".parse().unwrap(),
            close: "42020".parse().unwrap(),
            volume: 12.345,
            order_flow: None,
        }];
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_bars_file(&bars, &path).unwrap();

        let read_back = at_core::read_bars_file(&path).unwrap();
        assert_eq!(read_back, bars);
        fs::remove_dir_all(&base).ok();
    }

    /// 實際打 data.binance.vision，不在 `cargo test` 預設跑（會因為沒網路而失敗）。
    /// 手動驗證：`cargo test -p at-downloader -- --ignored`
    #[test]
    #[ignore]
    fn downloads_a_real_month_from_binance() {
        // 用 D1（日線）而不是 1m：一個月只有 ~30 行，下載量小、跑得快。
        let symbol = sym("BTCUSDT");
        let bars = download_monthly_klines(&symbol, Interval::D1, 2024, 1).unwrap();
        assert_eq!(bars.len(), 31, "2024-01 有 31 天");
        assert_eq!(bars[0].open_time, 1_704_067_200_000); // 2024-01-01 00:00 UTC
    }
}
