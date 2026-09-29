//! ROADMAP 2.8 對照驗證：整條鏈（下載 → 存檔 → 讀回 → 回測 → 指標）跑真實資料，
//! 結果和一份獨立重寫的 Python 回測逐格比對。
//!
//! 期望值全部來自 `docs/reference/manual_verification_sma_cross.py`——那支腳本
//! 只照規格（成交時點、SmaCross 的行為宣告、2.6 的指標定義、`Fixed` 的算術契約）
//! 從頭算，沒有參照本專案回測引擎的實作，所以這裡的斷言是外部對照，
//! 不是把現有行為抄成測試。比對過程與已知差異寫在 `docs/steps/2.8-對照驗證.md`。
//!
//! 資料：BTCUSDT 2024-01 日線 31 根（`tests/fixtures/`，由下面的 `#[ignore]`
//! 測試從 data.binance.vision 重新下載並比對）。

use std::path::{Path, PathBuf};

use at_core::backtest::DEFAULT_MAINTENANCE_MARGIN_RATE;
use at_core::{
    find_gaps, read_bars_file, run_backtest, BacktestConfig, Bar, FeeModel, Fixed, Interval,
    Metrics, SmaCross, Symbol,
};
use at_downloader::download_and_store_monthly_klines;

const FAST: usize = 3;
const SLOW: usize = 8;
const YEAR: u32 = 2024;
const MONTH: u32 = 1;

/// 成交筆數：進場、出場、再進場。Python 腳本逐筆列出了成交價、數量與手續費。
const EXPECTED_TRADES: usize = 3;

/// 情境 A（零成本）的權益曲線：（K 線開盤時間, 權益）。
const FRICTIONLESS_CURVE: [(i64, &str); 31] = [
    (1_704_067_200_000, "10000"),
    (1_704_153_600_000, "10000"),
    (1_704_240_000_000, "10000"),
    (1_704_326_400_000, "10000"),
    (1_704_412_800_000, "10000"),
    (1_704_499_200_000, "10000"),
    (1_704_585_600_000, "10000"),
    (1_704_672_000_000, "10000"),
    (1_704_758_400_000, "9820.86871546"),
    (1_704_844_800_000, "9936.73196510"),
    (1_704_931_200_000, "9869.67700658"),
    (1_705_017_600_000, "9112.20069834"),
    (1_705_104_000_000, "9126.10028412"),
    (1_705_190_400_000, "9126.10028412"),
    (1_705_276_800_000, "9126.10028412"),
    (1_705_363_200_000, "9126.10028412"),
    (1_705_449_600_000, "9126.10028412"),
    (1_705_536_000_000, "9126.10028412"),
    (1_705_622_400_000, "9126.10028412"),
    (1_705_708_800_000, "9126.10028412"),
    (1_705_795_200_000, "9126.10028412"),
    (1_705_881_600_000, "9126.10028412"),
    (1_705_968_000_000, "9126.10028412"),
    (1_706_054_400_000, "9126.10028412"),
    (1_706_140_800_000, "9126.10028412"),
    (1_706_227_200_000, "9126.10028412"),
    (1_706_313_600_000, "9126.10028412"),
    (1_706_400_000_000, "9106.69352752"),
    (1_706_486_400_000, "9382.21443815"),
    (1_706_572_800_000, "9303.86808125"),
    (1_706_659_200_000, "9225.63005726"),
];

/// 情境 B（滑價 0.05% + 現貨 VIP0 吃單費 0.1%）的權益曲線。
const WITH_COSTS_CURVE: [(i64, &str); 31] = [
    (1_704_067_200_000, "10000"),
    (1_704_153_600_000, "10000"),
    (1_704_240_000_000, "10000"),
    (1_704_326_400_000, "10000"),
    (1_704_412_800_000, "10000"),
    (1_704_499_200_000, "10000"),
    (1_704_585_600_000, "10000"),
    (1_704_672_000_000, "10000"),
    (1_704_758_400_000, "9806.15458085"),
    (1_704_844_800_000, "9921.84423784"),
    (1_704_931_200_000, "9854.88974473"),
    (1_705_017_600_000, "9098.54832886"),
    (1_705_104_000_000, "9112.42708952"),
    (1_705_190_400_000, "9098.76300496"),
    (1_705_276_800_000, "9098.76300496"),
    (1_705_363_200_000, "9098.76300496"),
    (1_705_449_600_000, "9098.76300496"),
    (1_705_536_000_000, "9098.76300496"),
    (1_705_622_400_000, "9098.76300496"),
    (1_705_708_800_000, "9098.76300496"),
    (1_705_795_200_000, "9098.76300496"),
    (1_705_881_600_000, "9098.76300496"),
    (1_705_968_000_000, "9098.76300496"),
    (1_706_054_400_000, "9098.76300496"),
    (1_706_140_800_000, "9098.76300496"),
    (1_706_227_200_000, "9098.76300496"),
    (1_706_313_600_000, "9098.76300496"),
    (1_706_400_000_000, "9065.81113143"),
    (1_706_486_400_000, "9340.09515598"),
    (1_706_572_800_000, "9262.10051655"),
    (1_706_659_200_000, "9184.21372370"),
];

/// Python 算出的四個指標 + 期間，字串照它印出來的值抄。
struct ExpectedMetrics {
    total_return: &'static str,
    /// 用 f64 的 `pow` 算的，引擎用連續開根號展開，最後一兩位會差。
    annualized_return: &'static str,
    max_drawdown: &'static str,
    /// 用 f64 算的；引擎全程走整數（截尾的平均、floor 的開根號），最後幾位會差。
    sharpe: &'static str,
    span_years: &'static str,
}

const FRICTIONLESS_METRICS: ExpectedMetrics = ExpectedMetrics {
    total_return: "-0.07743699",
    annualized_return: "-0.62492473",
    max_drawdown: "0.08933065",
    sharpe: "-3.14010202",
    span_years: "0.08219178",
};

const WITH_COSTS_METRICS: ExpectedMetrics = ExpectedMetrics {
    total_return: "-0.08157863",
    annualized_return: "-0.64490537",
    max_drawdown: "0.09341889",
    sharpe: "-3.31328131",
    span_years: "0.08219178",
};

/// 年化與夏普容許的誤差：這兩個指標要開根號，引擎為了「同一份輸入永遠得到同一個
/// 數字」全程用整數算（見 `metrics` 與 `Fixed::isqrt` 的說明），Python 用 f64，
/// 所以最後幾位一定不同（實測 2×10⁻⁶ 與 1×10⁻⁵ 量級）。
///
/// 為什麼 2×10⁻⁵ 還算嚴格：真正會出錯的地方（標準差用樣本而不是母體、
/// 每年期數寫死 365、年化用根數而不是時間戳）都會造成 10⁻² 以上的差，
/// 這個門檻抓得到。逐點的權益曲線與總報酬、最大回撤仍然要求 8 位小數完全相等。
const TOLERANCE: &str = "0.00002";

fn fx(s: &str) -> Fixed {
    s.parse().expect("測試常數格式錯誤")
}

fn fixture_path() -> PathBuf {
    at_downloader::local_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        &Symbol::new("BTCUSDT").unwrap(),
        Interval::D1,
        YEAR,
        MONTH,
    )
}

fn frictionless() -> BacktestConfig {
    BacktestConfig::frictionless(fx("10000"))
}

/// 滑價 0.05%、現貨 VIP0 吃單費 0.1%、無資金費。
fn with_costs() -> BacktestConfig {
    BacktestConfig {
        initial_capital: fx("10000"),
        fees: Some(FeeModel::spot_vip0()),
        slippage: fx("0.0005"),
        funding_rate: Fixed::ZERO,
        maintenance_margin_rate: Some(DEFAULT_MAINTENANCE_MARGIN_RATE),
    }
}

fn assert_close(actual: Fixed, expected: &str, what: &str) {
    let expected = fx(expected);
    let diff = actual.checked_sub(expected).unwrap().abs();
    assert!(
        diff <= fx(TOLERANCE),
        "{what}：引擎 {actual} vs 獨立推導 {expected}，差 {diff}"
    );
}

/// 跑一次回測，和 Python 的獨立推導逐點、逐指標比對。
fn assert_matches_manual_derivation(
    bars: &[Bar],
    config: &BacktestConfig,
    expected_curve: &[(i64, &str)],
    expected: &ExpectedMetrics,
) {
    let mut strategy = SmaCross::new(FAST, SLOW).unwrap();
    let result = run_backtest(bars, &mut strategy, config).unwrap();

    assert_eq!(result.trades, EXPECTED_TRADES, "成交筆數");
    assert_eq!(result.liquidations, 0, "只做多不用槓桿，不該有強制平倉");

    assert_eq!(result.curve.len(), expected_curve.len(), "權益曲線長度");
    for (point, &(open_time, equity)) in result.curve.iter().zip(expected_curve) {
        assert_eq!(point.open_time, open_time, "曲線時間戳");
        assert_eq!(point.equity, fx(equity), "{open_time} 的權益");
    }

    let metrics = Metrics::from_curve(&result.curve);
    assert_eq!(
        metrics.total_return.unwrap(),
        fx(expected.total_return),
        "總報酬"
    );
    assert_eq!(metrics.max_drawdown, fx(expected.max_drawdown), "最大回撤");
    assert_eq!(
        metrics.span_years.unwrap(),
        fx(expected.span_years),
        "期間（年）"
    );
    assert_close(
        metrics.annualized_return.unwrap(),
        expected.annualized_return,
        "年化報酬",
    );
    assert_close(metrics.sharpe.unwrap(), expected.sharpe, "夏普");
}

fn load_fixture() -> Vec<Bar> {
    let bars = read_bars_file(fixture_path()).expect("讀不到 2.8 的對照樣本");
    assert_eq!(bars.len(), FRICTIONLESS_CURVE.len(), "K 線根數");
    assert!(
        find_gaps(&bars, Interval::D1).unwrap().is_empty(),
        "2024-01 的日線不該有缺口"
    );
    bars
}

/// 零成本：每一點權益、成交筆數、四個指標都對得上獨立推導。
#[test]
fn frictionless_backtest_matches_independent_derivation() {
    let bars = load_fixture();
    assert_matches_manual_derivation(
        &bars,
        &frictionless(),
        &FRICTIONLESS_CURVE,
        &FRICTIONLESS_METRICS,
    );
}

/// 含滑價與手續費：同一份資料、同一個策略，成本也要對得上獨立推導。
#[test]
fn backtest_with_fees_and_slippage_matches_independent_derivation() {
    let bars = load_fixture();
    assert_matches_manual_derivation(&bars, &with_costs(), &WITH_COSTS_CURVE, &WITH_COSTS_METRICS);
}

/// 成本只會讓結果變差，不會變好——方向性的健全檢查，和上面兩組數字互相印證。
#[test]
fn costs_can_only_make_the_result_worse() {
    let bars = load_fixture();
    let run = |config: &BacktestConfig| {
        let mut strategy = SmaCross::new(FAST, SLOW).unwrap();
        let result = run_backtest(&bars, &mut strategy, config).unwrap();
        result.curve.last().unwrap().equity
    };
    assert!(run(&with_costs()) < run(&frictionless()));
}

/// 手動驗證整條鏈含下載：`cargo test -p at-downloader -- --ignored`
///
/// 重新下載同一個月的資料、確認和倉庫裡的樣本逐根相同，再跑一次同樣的比對。
#[test]
#[ignore]
fn downloaded_data_reproduces_the_fixture_and_the_derivation() {
    let dir = std::env::temp_dir().join("at-roadmap-2-8-validation");
    let (bars, path) = download_and_store_monthly_klines(
        &Symbol::new("BTCUSDT").unwrap(),
        Interval::D1,
        YEAR,
        MONTH,
        &dir,
    )
    .expect("下載失敗（需要網路）");

    let committed = load_fixture();
    assert_eq!(bars, committed, "重新下載的資料和倉庫裡的樣本不一樣");
    assert_eq!(
        read_bars_file(&path).unwrap(),
        committed,
        "存檔再讀回不一致"
    );

    assert_matches_manual_derivation(
        &bars,
        &frictionless(),
        &FRICTIONLESS_CURVE,
        &FRICTIONLESS_METRICS,
    );
    assert_matches_manual_derivation(&bars, &with_costs(), &WITH_COSTS_CURVE, &WITH_COSTS_METRICS);
    std::fs::remove_dir_all(&dir).ok();
}
