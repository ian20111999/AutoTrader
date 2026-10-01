//! 3.6 回測頁面的 Rust↔前端橋接：「取得資料→建立策略→跑回測→算績效」串成一個
//! Tauri command。只做格式轉換，不重寫 `at_core::run_backtest`/`metrics`/策略建構邏輯。
//!
//! 資料來源：本機已下載過就直接讀（[`at_core::read_bars_file`]），
//! 沒有就用 [`at_downloader`] 下載現貨月線（1.7 已驗證過的公開資料下載，
//! 跟「第 6/7 步之前不可下單」的安全規則無關）。
//!
//! ponytail: 滑價目前仍寫死成 [`DEFAULT_SLIPPAGE`]（0.05%），還沒有對應的 UI
//! 控制項；手續費依市場自動選 `FeeModel::spot_vip0()`／`FeeModel::futures_vip0()`。
//! 資金費率全程固定 0（還沒有歷史資金費率這個資料來源，`useRealFunding` 開關
//! 在前端是 disabled + 「即將推出」）。回傳的 [`BacktestSummary`] 把用了什麼
//! 假設完整回顯，之後要開放使用者調整就在這裡加欄位。
//!
//! 槓桿／方向怎麼接進只會回傳「多/空手」的四個內建策略：見
//! [`at_core::LeveragedStrategy`] 的文件註解。

use at_core::{
    read_bars_file, run_backtest, BacktestConfig, Bar, DirectionMode, FeeModel, Fixed, Interval,
    LeveragedStrategy, Market, Metrics, Strategy, Symbol, TargetPosition,
};
use at_downloader::{download_and_store_monthly_klines, local_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tauri::Manager;

/// 回測不進 session registry（ADR §4.5：它是同步的，沒有 handle、沒有
/// stop），所以用自己的一個計數器產生 id，不走 `SessionRegistry::generate_id`。
static BACKTEST_SESSION_SEQ: AtomicU32 = AtomicU32::new(0);

fn generate_backtest_session_id() -> String {
    let seq = BACKTEST_SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("backtest-{}-{seq:03}", crate::session_registry::now_ms())
}

/// 這一步的預設成本假設：Binance 現貨 VIP0（吃單 0.1%）+ 0.05% 滑價。
/// 沒有槓桿（strategies 目前全部只做多/空手），所以強制平倉、資金費用不到，
/// 沿用 [`BacktestConfig::frictionless`] 的維持保證金率、資金費率 0 即可。
///
/// 5.4 的模擬交易沿用同一組假設（`paper_trading.rs` 也會用到），所以是 `pub(crate)`。
pub(crate) const DEFAULT_SLIPPAGE: Fixed = Fixed::from_raw(50_000); // 0.0005

/// `run_backtest_command` 的輸入：symbol/interval/區間/策略/參數/起始資金
/// 一次送進來。跟 `strategyTypes.ts` 的 `StrategyConfig` 對應（`strategyId` + `params`）。
///
/// 前端的多幣種選擇不在這個結構裡：UI 對每個選到的交易對各呼叫一次這個
/// command，一次只測一個交易對，跟原本單一交易對的行為一致，不需要後端
/// 另外做批次結構。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BacktestRequest {
    pub symbol: String,
    pub interval: String,
    pub year: u32,
    pub month: u32,
    pub strategy_id: String,
    pub params: HashMap<String, String>,
    pub starting_capital: String,
    /// 市場：`"spot"`（現貨）或 `"usdm_perp"`（U 本位合約）。
    pub market: String,
    /// 方向：`"long_only"`（只做多）或 `"long_short"`（多空）。
    pub direction: String,
    /// 槓桿倍數，字串保留 `Fixed` 精確度。現貨市場必須是 `"1"`。
    pub leverage: String,
    /// 保證金模式：`"isolated"`（逐倉）或 `"cross"`（全倉）。現貨市場不適用，傳 `None`。
    pub margin_mode: Option<String>,
}

/// 保證金模式。目前這一版的回測引擎整場只會同時持有一個交易對的一個倉位，
/// 帳本就是「一份現金＋一份倉位」，逐倉與全倉在算出來的數字上沒有差異
/// （差異要等到一個帳戶同時跑多個倉位、彼此共用或不共用保證金時才會出現）。
/// 這裡還是把選項收下、原樣回顯，是為了接上之後的模擬交易/實盤帳戶風控，
/// 不是沒作用的假選項。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginMode {
    Isolated,
    Cross,
}

impl MarginMode {
    fn label_zh(self) -> &'static str {
        match self {
            MarginMode::Isolated => "逐倉",
            MarginMode::Cross => "全倉",
        }
    }
}

fn parse_market(raw: &str) -> Result<Market, String> {
    match raw {
        "spot" => Ok(Market::Spot),
        "usdm_perp" => Ok(Market::UsdmPerp),
        other => Err(format!("不支援的市場：{other}")),
    }
}

fn parse_direction(raw: &str) -> Result<DirectionMode, String> {
    match raw {
        "long_only" => Ok(DirectionMode::LongOnly),
        "long_short" => Ok(DirectionMode::LongShort),
        other => Err(format!("不支援的方向：{other}")),
    }
}

fn parse_margin_mode(raw: &str) -> Result<MarginMode, String> {
    match raw {
        "isolated" => Ok(MarginMode::Isolated),
        "cross" => Ok(MarginMode::Cross),
        other => Err(format!("不支援的保證金模式：{other}")),
    }
}

fn parse_usize(params: &HashMap<String, String>, key: &str) -> Result<usize, String> {
    let raw = params.get(key).ok_or_else(|| format!("缺少參數：{key}"))?;
    raw.trim()
        .parse::<usize>()
        .map_err(|_| format!("參數 {key} 必須是不小於 0 的整數，收到：{raw}"))
}

fn parse_fixed(params: &HashMap<String, String>, key: &str) -> Result<Fixed, String> {
    let raw = params.get(key).ok_or_else(|| format!("缺少參數：{key}"))?;
    raw.trim()
        .parse::<Fixed>()
        .map_err(|e| format!("參數 {key} 不是合法數字（{raw}）：{e}"))
}

/// 依策略 id 與字串參數建立策略物件。只做「字串→型別」的轉換與轉呼叫，
/// 驗證規則（`StrategyParamError`）完全交給 `at_core::strategies` 既有的建構子。
///
/// 回傳的 trait object 要求 `Send`：四個內建策略都是純值型別（沒有 `Rc`/`RefCell`），
/// 加這個界限不影響回測（`run_backtest` 只需要 `&mut dyn Strategy`），但讓
/// `paper_trading.rs` 可以直接重用這個函式建策略丟進背景執行緒
/// （`at_paper_trading::spawn` 要求 `Box<dyn Strategy + Send>`），不必另外寫一份。
pub(crate) fn build_strategy(
    strategy_id: &str,
    params: &HashMap<String, String>,
) -> Result<Box<dyn Strategy + Send>, String> {
    match strategy_id {
        "sma_cross" => {
            let fast = parse_usize(params, "fastPeriod")?;
            let slow = parse_usize(params, "slowPeriod")?;
            let strategy =
                at_core::SmaCross::new(fast, slow).map_err(|e| format!("均線交叉參數錯誤：{e}"))?;
            Ok(Box::new(strategy))
        }
        "bollinger" => {
            let period = parse_usize(params, "period")?;
            let multiplier = parse_fixed(params, "multiplier")?;
            let strategy = at_core::Bollinger::new(period, multiplier)
                .map_err(|e| format!("布林通道參數錯誤：{e}"))?;
            Ok(Box::new(strategy))
        }
        "donchian" => {
            let entry = parse_usize(params, "entryPeriod")?;
            let exit = parse_usize(params, "exitPeriod")?;
            let strategy = at_core::Donchian::new(entry, exit)
                .map_err(|e| format!("唐奇安突破參數錯誤：{e}"))?;
            Ok(Box::new(strategy))
        }
        "rsi" => {
            let period = parse_usize(params, "period")?;
            let buy_below = parse_fixed(params, "buyBelow")?;
            let exit_above = parse_fixed(params, "exitAbove")?;
            let strategy = at_core::Rsi::new(period, buy_below, exit_above)
                .map_err(|e| format!("RSI 參數錯誤：{e}"))?;
            Ok(Box::new(strategy))
        }
        other => Err(format!("不支援的策略代號：{other}")),
    }
}

/// 用策略 id 找內建清單裡的顯示名稱，跟 3.2 的 `strategies::builtin_strategies`
/// 共用同一份名稱，不在這裡另外寫一份中文字串。
pub(crate) fn strategy_display_name(strategy_id: &str) -> Result<String, String> {
    crate::strategies::builtin_strategies()
        .into_iter()
        .find(|info| info.id == strategy_id)
        .map(|info| info.name)
        .ok_or_else(|| format!("不支援的策略代號：{strategy_id}"))
}

/// 權益曲線上的一點，`equity` 用字串保留 `Fixed` 的精確表示。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EquityPointDto {
    pub open_time: i64,
    pub equity: String,
}

/// 回測頁面要的完整結果：權益曲線＋四個指標＋交易統計＋這次跑用了什麼設定的回顯，
/// 方便 3.7 比較頁面直接把整個結構存起來比較。
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BacktestSummary {
    pub session_id: String,
    pub symbol: String,
    pub interval: String,
    pub year: u32,
    pub month: u32,
    pub strategy_id: String,
    pub strategy_name: String,
    pub params: HashMap<String, String>,
    pub starting_capital: String,
    pub bar_count: usize,
    pub curve: Vec<EquityPointDto>,
    pub trades: usize,
    pub liquidations: usize,
    pub total_return: Option<String>,
    pub annualized_return: Option<String>,
    pub max_drawdown: String,
    pub sharpe: Option<String>,
    pub span_years: Option<String>,
    /// 這次回測實際用的成本假設，給前端顯示「這是簡化過的設定」。
    pub fee_model: String,
    pub slippage: String,
    pub market: String,
    pub direction: String,
    pub leverage: String,
    pub margin_mode: Option<String>,
    /// 資料來源的本機檔案路徑，方便除錯「橋接有沒有把資料讀對」。
    pub data_source_path: String,
}

/// 純邏輯：拿到 K 線跟「已經建好的策略」之後，跑回測→算績效→組 DTO。
/// 不碰檔案系統或網路，方便直接餵假資料測試。
///
/// 抽出 `strategy`/`strategy_id`/`strategy_name` 三個參數（不是直接用
/// `BacktestRequest.strategy_id` 查表），是因為 3.7 比較頁的 BTC 買入持有基準
/// （[`run_buy_hold_baseline_command`]）要跑同一套「建 config→跑回測→算績效→組
/// DTO」流程，但它的策略不經過 [`build_strategy`] 查表（基準策略不開放給使用者
/// 在策略庫選），兩邊共用這個函式就不必各寫一份 `BacktestSummary` 組裝邏輯。
///
/// 市場／方向／槓桿／保證金模式的解析、驗證、`LeveragedStrategy` 包裝都留在
/// 呼叫端（[`summarize`]）做，這個函式只收「已經決定好的」成本模型與顯示字串
/// ——BTC 買入持有基準固定是現貨/只做多/1 倍槓桿，不需要走一次市場驗證。
#[allow(clippy::too_many_arguments)]
fn summarize_with_strategy(
    session_id: &str,
    bars: &[Bar],
    symbol: &str,
    interval: &str,
    year: u32,
    month: u32,
    params: HashMap<String, String>,
    starting_capital: &str,
    strategy: &mut dyn Strategy,
    strategy_id: &str,
    strategy_name: &str,
    data_source_path: &Path,
    fee_model: FeeModel,
    fee_model_label: &str,
    maintenance_margin_rate: Option<Fixed>,
    market_label: &str,
    direction_label: &str,
    leverage_label: &str,
    margin_mode_label: Option<&str>,
) -> Result<BacktestSummary, String> {
    let capital = starting_capital
        .trim()
        .parse::<Fixed>()
        .map_err(|e| format!("起始資金不是合法數字（{starting_capital}）：{e}"))?;

    let config = BacktestConfig {
        initial_capital: capital,
        fees: Some(fee_model),
        slippage: DEFAULT_SLIPPAGE,
        // ponytail: 資金費全程固定 0（還沒有歷史資金費率這個資料來源）。
        // 「計入真實歷史資金費率」開關在前端是 disabled + 即將推出，這裡先不接。
        funding_rate: Fixed::ZERO,
        maintenance_margin_rate,
    };

    let result = run_backtest(bars, strategy, &config).map_err(|e| format!("回測執行失敗：{e}"))?;
    let metrics = Metrics::from_curve(&result.curve);

    Ok(BacktestSummary {
        session_id: session_id.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        year,
        month,
        strategy_id: strategy_id.to_string(),
        strategy_name: strategy_name.to_string(),
        params,
        starting_capital: capital.to_string(),
        bar_count: bars.len(),
        curve: result
            .curve
            .iter()
            .map(|p| EquityPointDto {
                open_time: p.open_time,
                equity: p.equity.to_string(),
            })
            .collect(),
        trades: result.trades,
        liquidations: result.liquidations,
        total_return: metrics.total_return.map(|v| v.to_string()),
        annualized_return: metrics.annualized_return.map(|v| v.to_string()),
        max_drawdown: metrics.max_drawdown.to_string(),
        sharpe: metrics.sharpe.map(|v| v.to_string()),
        span_years: metrics.span_years.map(|v| v.to_string()),
        fee_model: fee_model_label.to_string(),
        slippage: DEFAULT_SLIPPAGE.to_string(),
        market: market_label.to_string(),
        direction: direction_label.to_string(),
        leverage: leverage_label.to_string(),
        margin_mode: margin_mode_label.map(|s| s.to_string()),
        data_source_path: data_source_path.display().to_string(),
    })
}

/// 純邏輯：拿到 K 線之後的「建立策略→驗證市場設定→包槓桿→跑回測→算績效→
/// 組 DTO」，不碰檔案系統或網路，方便直接餵假資料測試。
fn summarize(
    bars: &[Bar],
    request: &BacktestRequest,
    data_source_path: &Path,
) -> Result<BacktestSummary, String> {
    let market = parse_market(&request.market)?;
    let direction = parse_direction(&request.direction)?;
    let leverage = request
        .leverage
        .trim()
        .parse::<Fixed>()
        .map_err(|e| format!("槓桿倍數不是合法數字（{}）：{e}", request.leverage))?;
    if leverage <= Fixed::ZERO {
        return Err("槓桿倍數必須大於 0".to_string());
    }
    let margin_mode = match (market, &request.margin_mode) {
        (Market::UsdmPerp, Some(raw)) => Some(parse_margin_mode(raw)?),
        (Market::UsdmPerp, None) => return Err("合約市場必須選擇保證金模式".to_string()),
        (Market::Spot, _) => None,
    };
    if market == Market::Spot {
        if direction == DirectionMode::LongShort {
            return Err("現貨市場不支援做空，請把方向切換成「只做多」".to_string());
        }
        if leverage != Fixed::ONE {
            return Err("現貨市場不支援槓桿，請把槓桿設為 1 倍".to_string());
        }
    }

    let strategy = build_strategy(&request.strategy_id, &request.params)?;
    let strategy_name = strategy_display_name(&request.strategy_id)?;
    let mut strategy = LeveragedStrategy::new(strategy, leverage, direction);

    let fee_model = match market {
        Market::Spot => FeeModel::spot_vip0(),
        Market::UsdmPerp => FeeModel::futures_vip0(),
    };
    let fee_model_label = match market {
        Market::Spot => "spot_vip0（現貨 VIP0，吃單 0.1%）",
        Market::UsdmPerp => "futures_vip0（合約 VIP0，吃單 0.05%）",
    };
    let maintenance_margin_rate = match market {
        Market::Spot => None,
        Market::UsdmPerp => Some(at_core::DEFAULT_MAINTENANCE_MARGIN_RATE),
    };

    let session_id = generate_backtest_session_id();
    summarize_with_strategy(
        &session_id,
        bars,
        &request.symbol,
        &request.interval,
        request.year,
        request.month,
        request.params.clone(),
        &request.starting_capital,
        &mut strategy,
        &request.strategy_id,
        &strategy_name,
        data_source_path,
        fee_model,
        fee_model_label,
        maintenance_margin_rate,
        &request.market,
        &request.direction,
        &leverage.to_string(),
        margin_mode.map(|m| m.label_zh()),
    )
}

/// 把一筆成功的回測寫進 session store（ADR §6.4：每場都自動留紀錄，使用者
/// 另外按「儲存回測」才把 `saved` 翻成 `true`）。寫入失敗不擋回測結果顯示——
/// 使用者已經等到這個結果了，不該因為歷史紀錄寫不進去就整個失敗。
fn record_backtest_session(store: &at_session_store::SessionStore, summary: &BacktestSummary) {
    let params: BTreeMap<String, String> = summary.params.clone().into_iter().collect();
    let record = at_session_store::SessionRecord {
        schema_version: at_session_store::CURRENT_SCHEMA_VERSION,
        id: summary.session_id.clone(),
        kind: at_core::RunMode::Backtest,
        market: match summary.market.as_str() {
            "usdm_perp" => Market::UsdmPerp,
            _ => Market::Spot,
        },
        symbol: summary.symbol.clone(),
        interval: summary.interval.clone(),
        strategy_id: summary.strategy_id.clone(),
        strategy_name: summary.strategy_name.clone(),
        params,
        started_at_ms: crate::session_registry::now_ms(),
        ended_at_ms: Some(crate::session_registry::now_ms()),
        status: at_session_store::SessionStatus::Completed,
        status_message: None,
        starting_capital: summary.starting_capital.clone(),
        final_equity: summary.curve.last().map(|p| p.equity.clone()),
        bars_seen: summary.bar_count as u64,
        metrics: Some(at_session_store::SessionMetrics {
            total_return: summary.total_return.clone(),
            annualized_return: summary.annualized_return.clone(),
            max_drawdown: Some(summary.max_drawdown.clone()),
            sharpe: summary.sharpe.clone(),
            span_years: summary.span_years.clone(),
        }),
        counters: at_session_store::SessionCounters {
            trades: Some(summary.trades as u64),
            liquidations: Some(summary.liquidations as u64),
            ..Default::default()
        },
        cost_assumptions: at_session_store::CostAssumptions {
            fee_model: Some(summary.fee_model.clone()),
            slippage: Some(summary.slippage.clone()),
            funding_rate: Some("0".to_string()),
            maintenance_margin_rate: None,
        },
        saved: false,
        data_source_path: Some(summary.data_source_path.clone()),
        notes: None,
    };
    if let Err(e) = store.upsert_session(record) {
        eprintln!("寫入回測紀錄失敗：{e}");
    }
}

/// 永遠全倉做多，不需要暖機、不需要參數。只給 3.7 比較頁內部用來跑「BTC 買入
/// 持有」基準曲線——刻意不透過 [`build_strategy`]／`strategies::builtin_strategies()`
/// 查表註冊，才不會讓它出現在使用者看得到的策略庫。
struct BuyAndHold;

impl Strategy for BuyAndHold {
    fn on_bar(&mut self, _bar: &Bar) -> TargetPosition {
        TargetPosition::FULL_LONG
    }
}

/// 基準策略固定用這個代號／顯示名稱，比較頁前端用代號判斷「這是基準列，不是
/// 使用者自己跑的回測」。
pub const BUY_HOLD_STRATEGY_ID: &str = "_buy_hold_baseline";
const BUY_HOLD_STRATEGY_NAME: &str = "BTC 買入持有";

/// [`run_buy_hold_baseline_command`] 的輸入：固定抓 BTCUSDT，只需要跟使用者
/// 要比較的那幾筆回測一樣的週期／年月／起始資金。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BuyHoldBaselineRequest {
    pub interval: String,
    pub year: u32,
    pub month: u32,
    pub starting_capital: String,
}

/// 3.7 比較頁用的 Tauri command：跑一次「全程持有 BTC」當比較基準，回傳格式
/// 跟 `run_backtest_command` 一樣的 [`BacktestSummary`]，前端不用另外處理。
#[tauri::command]
pub async fn run_buy_hold_baseline_command(
    app: tauri::AppHandle,
    request: BuyHoldBaselineRequest,
) -> Result<BacktestSummary, String> {
    let symbol = Symbol::new("BTCUSDT").expect("BTCUSDT 是合法交易對代號");
    let interval: Interval = request
        .interval
        .parse()
        .map_err(|e: at_core::ParseIntervalError| e.to_string())?;

    let base_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("找不到應用程式資料目錄：{e}"))?
        .join("klines");

    let (bars, data_source_path) =
        load_bars(base_dir, symbol, interval, request.year, request.month).await?;

    let mut strategy = BuyAndHold;
    // 基準策略刻意不寫進 session store：它不是使用者自己跑的回測，不該出現
    // 在「最近回測清單」「回測次數」裡（strategy_id 查表一節的理由同一個：
    // 不讓它出現在使用者看得到的策略庫）。id 只是為了跟 BacktestSummary
    // 其他欄位一樣有個值可以回顯，前端用 strategy_id 判斷不是真正的回測。
    let session_id = generate_backtest_session_id();
    summarize_with_strategy(
        &session_id,
        &bars,
        "BTCUSDT",
        &request.interval,
        request.year,
        request.month,
        HashMap::new(),
        &request.starting_capital,
        &mut strategy,
        BUY_HOLD_STRATEGY_ID,
        BUY_HOLD_STRATEGY_NAME,
        &data_source_path,
        FeeModel::spot_vip0(),
        "spot_vip0（現貨 VIP0，吃單 0.1%）",
        None,
        "spot",
        "long_only",
        "1",
        None,
    )
}

/// 這個月的 K 線本機有就直接讀，沒有就下載（會連網路）。
async fn load_bars(
    base_dir: PathBuf,
    symbol: Symbol,
    interval: Interval,
    year: u32,
    month: u32,
) -> Result<(Vec<Bar>, PathBuf), String> {
    let path = local_path(&base_dir, &symbol, interval, year, month);
    if path.exists() {
        let bars = read_bars_file(&path).map_err(|e| format!("讀取本機 K 線檔失敗：{e}"))?;
        return Ok((bars, path));
    }
    // ureq 是同步 HTTP client，丟到 blocking thread pool 跑，不要卡住 async runtime。
    tauri::async_runtime::spawn_blocking(move || {
        download_and_store_monthly_klines(&symbol, interval, year, month, &base_dir)
    })
    .await
    .map_err(|e| format!("下載任務執行失敗：{e}"))?
    .map_err(|e| format!("下載歷史 K 線失敗：{e}"))
}

/// 回測頁面（3.6）用的 Tauri command。前端傳一個 `BacktestRequest`，
/// 回傳可以直接畫圖表的 [`BacktestSummary`]。
#[tauri::command]
pub async fn run_backtest_command(
    app: tauri::AppHandle,
    request: BacktestRequest,
) -> Result<BacktestSummary, String> {
    let symbol = Symbol::new(&request.symbol).map_err(|e| format!("交易對代號不合法：{e}"))?;
    let interval: Interval = request
        .interval
        .parse()
        .map_err(|e: at_core::ParseIntervalError| e.to_string())?;

    let base_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("找不到應用程式資料目錄：{e}"))?
        .join("klines");

    let (bars, data_source_path) =
        load_bars(base_dir, symbol, interval, request.year, request.month).await?;

    let summary = summarize(&bars, &request, &data_source_path)?;
    let store = app.state::<Arc<at_session_store::SessionStore>>();
    record_backtest_session(&store, &summary);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    const T0: i64 = 1_704_067_200_000;
    const HOUR: i64 = 3_600_000;

    /// 一串會讓均線交叉策略明確做多的 K 線：先平走暖機，再一路上漲。
    fn trending_bars() -> Vec<Bar> {
        let mut closes = vec![fx("100"); 12];
        for i in 0..20 {
            closes.push(
                fx("100")
                    .checked_add(fx("2").checked_mul(fx(&i.to_string())).unwrap())
                    .unwrap(),
            );
        }
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                open_time: T0 + i as i64 * HOUR,
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1.0,
            })
            .collect()
    }

    fn param_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// 預設現貨／只做多／1 倍槓桿，大多數測試不關心槓桿與方向時用這個。
    fn request(
        strategy_id: &str,
        params: &[(&str, &str)],
        starting_capital: &str,
    ) -> BacktestRequest {
        BacktestRequest {
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            year: 2024,
            month: 1,
            strategy_id: strategy_id.to_string(),
            params: param_map(params),
            starting_capital: starting_capital.to_string(),
            market: "spot".to_string(),
            direction: "long_only".to_string(),
            leverage: "1".to_string(),
            margin_mode: None,
        }
    }

    #[test]
    fn unsupported_strategy_id_is_a_clear_chinese_error() {
        let err = build_strategy("not_a_strategy", &HashMap::new())
            .err()
            .unwrap();
        assert_eq!(err, "不支援的策略代號：not_a_strategy");
    }

    #[test]
    fn missing_param_is_a_clear_chinese_error() {
        let err = build_strategy("sma_cross", &param_map(&[("fastPeriod", "10")]))
            .err()
            .unwrap();
        assert_eq!(err, "缺少參數：slowPeriod");
    }

    #[test]
    fn non_numeric_param_is_a_clear_chinese_error() {
        let err = build_strategy(
            "sma_cross",
            &param_map(&[("fastPeriod", "abc"), ("slowPeriod", "50")]),
        )
        .err()
        .unwrap();
        assert!(
            err.contains("fastPeriod"),
            "錯誤訊息應該點名是哪個參數：{err}"
        );
    }

    #[test]
    fn invalid_strategy_param_combination_surfaces_the_core_error_message() {
        // 快線週期沒有短於慢線：StrategyParamError::FastNotBelowSlow
        let err = build_strategy(
            "sma_cross",
            &param_map(&[("fastPeriod", "50"), ("slowPeriod", "10")]),
        )
        .err()
        .unwrap();
        assert!(err.contains("快線週期必須短於慢線週期"), "{err}");
    }

    #[test]
    fn builds_all_four_strategies_with_default_params() {
        for info in crate::strategies::builtin_strategies() {
            let params: HashMap<String, String> = info
                .params
                .iter()
                .map(|p| (p.key.clone(), p.default.clone()))
                .collect();
            assert!(
                build_strategy(&info.id, &params).is_ok(),
                "預設參數應該對每個內建策略都合法：{}",
                info.id
            );
        }
    }

    #[test]
    fn summarize_matches_a_direct_run_backtest_call_on_the_same_bars() {
        let bars = trending_bars();
        let req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );

        let summary = summarize(&bars, &req, Path::new("/tmp/fake.csv")).unwrap();

        // 直接照 summarize 內部用的同一組設定（現貨、只做多、1 倍槓桿），
        // 重新跑一次 run_backtest 對照。
        let config = BacktestConfig {
            initial_capital: fx("10000"),
            fees: Some(FeeModel::spot_vip0()),
            slippage: DEFAULT_SLIPPAGE,
            funding_rate: Fixed::ZERO,
            maintenance_margin_rate: None,
        };
        let strategy = at_core::SmaCross::new(3, 8).unwrap();
        let mut strategy =
            LeveragedStrategy::new(Box::new(strategy), fx("1"), DirectionMode::LongOnly);
        let direct = run_backtest(&bars, &mut strategy, &config).unwrap();
        let direct_metrics = Metrics::from_curve(&direct.curve);

        assert_eq!(summary.curve.len(), direct.curve.len());
        assert_eq!(
            summary.curve.last().unwrap().equity,
            direct.curve.last().unwrap().equity.to_string()
        );
        assert_eq!(summary.trades, direct.trades);
        assert_eq!(summary.liquidations, direct.liquidations);
        assert_eq!(
            summary.total_return,
            direct_metrics.total_return.map(|v| v.to_string())
        );
        assert_eq!(
            summary.max_drawdown,
            direct_metrics.max_drawdown.to_string()
        );
        assert_eq!(summary.strategy_name, "均線交叉");
        assert_eq!(summary.bar_count, bars.len());
        assert_eq!(summary.fee_model, "spot_vip0（現貨 VIP0，吃單 0.1%）");
    }

    #[test]
    fn spot_market_rejects_leverage_other_than_one() {
        let bars = trending_bars();
        let mut req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        req.leverage = "2".to_string();
        let err = summarize(&bars, &req, Path::new("/tmp/fake.csv"))
            .err()
            .unwrap();
        assert!(err.contains("現貨市場不支援槓桿"), "{err}");
    }

    #[test]
    fn spot_market_rejects_long_short_direction() {
        let bars = trending_bars();
        let mut req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        req.direction = "long_short".to_string();
        let err = summarize(&bars, &req, Path::new("/tmp/fake.csv"))
            .err()
            .unwrap();
        assert!(err.contains("現貨市場不支援做空"), "{err}");
    }

    #[test]
    fn futures_market_requires_margin_mode() {
        let bars = trending_bars();
        let mut req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        req.market = "usdm_perp".to_string();
        let err = summarize(&bars, &req, Path::new("/tmp/fake.csv"))
            .err()
            .unwrap();
        assert!(err.contains("必須選擇保證金模式"), "{err}");
    }

    #[test]
    fn leverage_scales_the_backtest_relative_to_one_x() {
        let bars = trending_bars();
        let mut one_x = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        one_x.market = "usdm_perp".to_string();
        one_x.margin_mode = Some("isolated".to_string());

        let mut two_x = one_x.clone();
        two_x.leverage = "2".to_string();

        let summary_1x = summarize(&bars, &one_x, Path::new("/tmp/fake.csv")).unwrap();
        let summary_2x = summarize(&bars, &two_x, Path::new("/tmp/fake.csv")).unwrap();

        // 2 倍槓桿在同一段上漲行情應該比 1 倍賺得更多（名目部位更大）。
        let final_1x: Fixed = summary_1x.curve.last().unwrap().equity.parse().unwrap();
        let final_2x: Fixed = summary_2x.curve.last().unwrap().equity.parse().unwrap();
        assert!(
            final_2x > final_1x,
            "2 倍槓桿應該比 1 倍賺更多：1x={final_1x} 2x={final_2x}"
        );
        assert_eq!(summary_2x.leverage, "2");
        assert_eq!(summary_2x.margin_mode.as_deref(), Some("逐倉"));
    }

    #[test]
    fn long_short_direction_mirrors_flat_bars_into_a_short() {
        // Donchian 在暖機期間 / 無突破時回傳空手；多空模式下這段應該變成做空，
        // 讓最終部位跟只做多模式不一樣。
        let bars = trending_bars();
        let mut long_only = request(
            "donchian",
            &[("entryPeriod", "20"), ("exitPeriod", "10")],
            "10000",
        );
        long_only.market = "usdm_perp".to_string();
        long_only.margin_mode = Some("cross".to_string());

        let mut long_short = long_only.clone();
        long_short.direction = "long_short".to_string();

        let a = summarize(&bars, &long_only, Path::new("/tmp/fake.csv")).unwrap();
        let b = summarize(&bars, &long_short, Path::new("/tmp/fake.csv")).unwrap();

        assert_ne!(a.curve, b.curve, "多空模式應該跟只做多模式算出不同的曲線");
    }

    #[test]
    fn summarize_rejects_bad_starting_capital_without_running_the_strategy() {
        let bars = trending_bars();
        let req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "not-a-number",
        );
        let err = summarize(&bars, &req, Path::new("/tmp/fake.csv"))
            .err()
            .unwrap();
        assert!(err.contains("起始資金"), "{err}");
    }

    #[test]
    fn empty_bars_produce_an_empty_curve_not_an_error() {
        let req = request(
            "sma_cross",
            &[("fastPeriod", "3"), ("slowPeriod", "8")],
            "10000",
        );
        let summary = summarize(&[], &req, Path::new("/tmp/fake.csv")).unwrap();
        assert_eq!(summary.curve, vec![]);
        assert_eq!(summary.trades, 0);
    }

    #[test]
    fn deserializes_from_camel_case_json_matching_the_frontend_shape() {
        let json = serde_json::json!({
            "symbol": "BTCUSDT",
            "interval": "1d",
            "year": 2024,
            "month": 1,
            "strategyId": "sma_cross",
            "params": {"fastPeriod": "10", "slowPeriod": "50"},
            "startingCapital": "10000",
            "market": "spot",
            "direction": "long_only",
            "leverage": "1",
            "marginMode": null,
        });
        let req: BacktestRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.strategy_id, "sma_cross");
        assert_eq!(req.params.get("fastPeriod").unwrap(), "10");
    }

    /// 真的打 data.binance.vision 下載一個月的資料、跑一次完整的
    /// `load_bars` + `summarize`，並且跟直接呼叫 `run_backtest` 的結果比對。
    /// 不在 `cargo test` 預設跑（需要網路）：`cargo test -p app -- --ignored`
    #[test]
    #[ignore]
    fn run_backtest_command_matches_a_direct_run_backtest_call_end_to_end() {
        let base_dir =
            std::env::temp_dir().join(format!("at_app_backtest_test_{}", std::process::id()));
        let symbol = Symbol::new("BTCUSDT").unwrap();
        let interval = Interval::D1;

        let (bars, path) =
            tauri::async_runtime::block_on(load_bars(base_dir.clone(), symbol, interval, 2024, 1))
                .unwrap();
        assert_eq!(bars.len(), 31, "2024-01 有 31 天");

        // 3/8 而不是內建預設 10/50：31 根日線的資料，慢線週期 50 永遠暖機不完、
        // 一筆都不會成交，驗證不到真正的成交路徑。3/8 在這組資料上真的會進出場。
        let req = BacktestRequest {
            symbol: "BTCUSDT".to_string(),
            interval: "1d".to_string(),
            year: 2024,
            month: 1,
            strategy_id: "sma_cross".to_string(),
            params: param_map(&[("fastPeriod", "3"), ("slowPeriod", "8")]),
            starting_capital: "10000".to_string(),
            market: "spot".to_string(),
            direction: "long_only".to_string(),
            leverage: "1".to_string(),
            margin_mode: None,
        };
        let summary = summarize(&bars, &req, &path).unwrap();

        let config = BacktestConfig {
            initial_capital: fx("10000"),
            fees: Some(FeeModel::spot_vip0()),
            slippage: DEFAULT_SLIPPAGE,
            funding_rate: Fixed::ZERO,
            maintenance_margin_rate: None,
        };
        let mut strategy = at_core::SmaCross::new(3, 8).unwrap();
        let direct = run_backtest(&bars, &mut strategy, &config).unwrap();

        assert_eq!(
            summary.curve.last().unwrap().equity,
            direct.curve.last().unwrap().equity.to_string()
        );
        assert_eq!(summary.trades, direct.trades);
        assert!(
            summary.trades > 0,
            "這組參數應該真的會成交，不只是暖機期空手"
        );
        println!(
            "2024-01 BTCUSDT 日線 均線交叉(3/8)：最終權益 {}，交易 {} 筆，總報酬 {:?}",
            summary.curve.last().unwrap().equity,
            summary.trades,
            summary.total_return
        );

        std::fs::remove_dir_all(&base_dir).ok();
    }

    #[test]
    fn buy_and_hold_always_targets_full_long() {
        let mut strategy = BuyAndHold;
        let bar = trending_bars()[0];
        assert_eq!(strategy.on_bar(&bar), TargetPosition::FULL_LONG);
        // 不管哪一根 K 線、策略有沒有「記憶」，永遠回滿倉做多——這是基準策略
        // 唯一需要保證的事。
        assert_eq!(strategy.on_bar(&bar), TargetPosition::FULL_LONG);
    }

    #[test]
    fn buy_hold_baseline_summary_matches_a_direct_full_long_run() {
        let bars = trending_bars();

        let mut baseline_strategy = BuyAndHold;
        let summary = summarize_with_strategy(
            "test-session-id",
            &bars,
            "BTCUSDT",
            "1h",
            2024,
            1,
            HashMap::new(),
            "10000",
            &mut baseline_strategy,
            BUY_HOLD_STRATEGY_ID,
            BUY_HOLD_STRATEGY_NAME,
            Path::new("/tmp/fake.csv"),
            FeeModel::spot_vip0(),
            "spot_vip0（現貨 VIP0，吃單 0.1%）",
            None,
            "spot",
            "long_only",
            "1",
            None,
        )
        .unwrap();

        let config = BacktestConfig {
            fees: Some(FeeModel::spot_vip0()),
            slippage: DEFAULT_SLIPPAGE,
            ..BacktestConfig::frictionless(fx("10000"))
        };
        let mut direct_strategy = BuyAndHold;
        let direct = run_backtest(&bars, &mut direct_strategy, &config).unwrap();

        assert_eq!(summary.strategy_id, "_buy_hold_baseline");
        assert_eq!(summary.strategy_name, "BTC 買入持有");
        assert!(summary.params.is_empty());
        assert_eq!(
            summary.curve.last().unwrap().equity,
            direct.curve.last().unwrap().equity.to_string()
        );
        assert_eq!(summary.trades, direct.trades);
        // 從頭到尾都持有、K 線一路上漲，應該只開一次倉，不會中途再交易。
        assert_eq!(summary.trades, 1);
    }
}
