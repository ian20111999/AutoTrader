mod backtest;
mod settings;
mod strategies;

use backtest::run_backtest_command;
use settings::{binance_credentials_status, clear_binance_credentials, save_binance_credentials};
use strategies::StrategyInfo;

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
        .invoke_handler(tauri::generate_handler![
            at_core_version,
            list_builtin_strategies,
            run_backtest_command,
            save_binance_credentials,
            binance_credentials_status,
            clear_binance_credentials
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
