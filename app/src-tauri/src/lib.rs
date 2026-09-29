// 證明 Tauri 的 Rust 殼能呼叫 at-core 的型別（3.2 會加上真正的
// list_builtin_strategies command，走同一條依賴路徑）。
#[tauri::command]
fn at_core_version() -> String {
    format!(
        "{} (Fixed::ZERO = {})",
        at_core::VERSION,
        at_core::Fixed::ZERO
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![at_core_version])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
