#!/usr/bin/env bash
# 自動交易台：開發環境檢查（macOS）
# 用法：bash scripts/check-env.sh

ok=1
show() { # 名稱 是否找到 說明 安裝提示
  if [ "$2" = 1 ]; then printf "\033[32m[OK]\033[0m   %-20s %s\n" "$1" "$3"
  else printf "\033[33m[缺少]\033[0m %-20s %s\n" "$1" "$4"; ok=0; fi
}
ver() { command -v "$1" >/dev/null 2>&1 && "$1" "$2" 2>/dev/null | head -1; }

echo "檢查開發環境..."; echo

clt=$(xcode-select -p 2>/dev/null)
show "Xcode 命令列工具" $([ -n "$clt" ] && echo 1 || echo 0) "$clt" "執行：xcode-select --install"

v=$(ver rustc --version); show "Rust (rustc)" $([ -n "$v" ] && echo 1 || echo 0) "$v" \
  "執行：curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh ，裝完重開終端機"
v=$(ver cargo --version); show "Cargo" $([ -n "$v" ] && echo 1 || echo 0) "$v" "隨 Rust 一起安裝"
v=$(ver node --version); show "Node.js" $([ -n "$v" ] && echo 1 || echo 0) "$v" "安裝 LTS 版：https://nodejs.org（第 3 步開始需要）"
v=$(ver git --version); show "Git（建議）" $([ -n "$v" ] && echo 1 || echo 0) "$v" "隨 Xcode 命令列工具一起安裝"

echo
if [ $ok = 1 ]; then echo "全部就緒。接著執行：cargo build && cargo test && cargo run -p engine"
else echo "請先安裝上面標示「缺少」的項目，裝完重開終端機再執行一次。"; fi
