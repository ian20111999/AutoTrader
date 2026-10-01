mod account_permissions;
mod backtest;
mod paper_trading;
mod settings;
mod strategies;
mod testnet_settings;
mod testnet_trading;
mod warmup;

use account_permissions::check_account_permissions;
use backtest::{run_backtest_command, run_buy_hold_baseline_command};
use paper_trading::{
    paper_trading_status, start_paper_trading, stop_paper_trading, PaperTradingState,
};
use settings::{binance_credentials_status, clear_binance_credentials, save_binance_credentials};
use strategies::StrategyInfo;
use testnet_settings::{
    clear_testnet_credentials, save_testnet_credentials, testnet_credentials_status,
};
use testnet_trading::{
    set_testnet_kill_switch, start_testnet_trading, stop_testnet_trading, testnet_trading_status,
    TestnetTradingState,
};

// 證明 Tauri 的 Rust 殼能呼叫 at-core 的型別（3.1 鋪的路，3.2 的
// list_builtin_strategies 走同一條依賴路徑）。
#[tauri::command]
fn at_core_version() -> String {
    format!(
        "{} (Fixed::ZERO = {})",
        at_core::VERSION,
        at_core::Fixed::ZERO
    )
}

/// 四個內建策略的名稱與參數 schema，給前端動態產生調參表單用（3.5）。
/// 這裡不會失敗，但 Tauri command 慣例回傳 `Result`，讓之後其他 command
/// （例如會讀檔案的）可以照同一個模式處理錯誤。
#[tauri::command]
fn list_builtin_strategies() -> Result<Vec<StrategyInfo>, String> {
    Ok(strategies::builtin_strategies())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(PaperTradingState::default())
        .manage(TestnetTradingState::default())
        .invoke_handler(tauri::generate_handler![
            at_core_version,
            list_builtin_strategies,
            run_backtest_command,
            run_buy_hold_baseline_command,
            save_binance_credentials,
            binance_credentials_status,
            clear_binance_credentials,
            check_account_permissions,
            start_paper_trading,
            stop_paper_trading,
            paper_trading_status,
            save_testnet_credentials,
            testnet_credentials_status,
            clear_testnet_credentials,
            start_testnet_trading,
            stop_testnet_trading,
            set_testnet_kill_switch,
            testnet_trading_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
