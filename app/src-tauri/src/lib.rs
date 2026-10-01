mod account_permissions;
mod backtest;
mod paper_trading;
mod session_registry;
mod sessions;
mod settings;
mod strategies;
mod testnet_settings;
mod testnet_trading;
mod warmup;

use account_permissions::check_account_permissions;
use backtest::{run_backtest_command, run_buy_hold_baseline_command, validate_strategy_ast};
use paper_trading::{paper_trading_status, start_paper_trading, stop_paper_trading};
use session_registry::SessionRegistry;
use sessions::{
    delete_session, list_live_sessions, list_sessions, mark_session_saved, read_session_curve,
    session_store_health,
};
use settings::{binance_credentials_status, clear_binance_credentials, save_binance_credentials};
use std::sync::Arc;
use strategies::StrategyInfo;
use tauri::Manager;
use testnet_settings::{
    clear_testnet_credentials, save_testnet_credentials, testnet_credentials_status,
};
use testnet_trading::{
    set_testnet_kill_switch, start_testnet_trading, stop_testnet_trading, testnet_trading_status,
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
        .setup(|app| {
            // session store 的 base_dir 要等 App 真的跑起來才拿得到
            // （app.path().app_data_dir()，跟 testnet_trading.rs 既有的用法
            // 一致），所以 registry／store 的 `.manage()` 放在 `setup` 裡，
            // 不是 `Builder::default()` 鏈的頂層。
            let base_dir = app.path().app_data_dir().expect("找不到應用程式資料目錄");
            let store = Arc::new(at_session_store::SessionStore::new(base_dir));

            // 啟動對帳（ADR §9.3）：把上次留下的孤兒 running 紀錄轉成
            // interrupted。對帳失敗不擋啟動——頂多是這次看到舊的孤兒紀錄，
            // 不影響這次執行的正確性。
            match store.reconcile_on_startup() {
                Ok(report) if !report.interrupted_ids.is_empty() => {
                    eprintln!(
                        "啟動對帳：{} 場孤兒 session 已標記為 interrupted：{:?}",
                        report.interrupted_ids.len(),
                        report.interrupted_ids
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("啟動對帳失敗：{e}"),
            }

            app.manage(store.clone());
            app.manage(SessionRegistry::new(store));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            at_core_version,
            list_builtin_strategies,
            run_backtest_command,
            run_buy_hold_baseline_command,
            validate_strategy_ast,
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
            testnet_trading_status,
            list_live_sessions,
            list_sessions,
            read_session_curve,
            mark_session_saved,
            delete_session,
            session_store_health
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
