//! 交易引擎執行檔。目前只是骨架：確認整個專案能編譯、能執行。

use at_core::{Market, RunMode, VERSION};

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version") | Some("-V") => println!("engine {VERSION}"),
        None | Some("status") => print_status(),
        Some(other) => {
            eprintln!("不認得的指令：{other}");
            eprintln!("可用：status、--version");
            std::process::exit(2);
        }
    }
}

fn print_status() {
    println!("自動交易台 · 引擎 v{VERSION}");
    println!("狀態：骨架（第 0 步），尚未連線交易所，不會下任何單。");
    println!();
    println!(
        "支援市場：{}、{}",
        Market::Spot.label_zh(),
        Market::UsdmPerp.label_zh()
    );
    println!("執行模式：");
    for m in RunMode::ALL {
        let money = if m.uses_real_money() {
            "真實資金"
        } else {
            "不動用資金"
        };
        let orders = if m.sends_orders() {
            "會送單到交易所"
        } else {
            "不送單"
        };
        println!("  - {}：{}，{}", m.label_zh(), money, orders);
    }
}
