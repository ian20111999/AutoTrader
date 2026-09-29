# 自動交易台：開發環境檢查
# 用法：powershell -ExecutionPolicy Bypass -File scripts\check-env.ps1

$ok = $true
function Show($name, $found, $detail, $hint) {
    if ($found) {
        Write-Host ("[OK]   {0,-22} {1}" -f $name, $detail) -ForegroundColor Green
    } else {
        Write-Host ("[缺少] {0,-22} {1}" -f $name, $hint) -ForegroundColor Yellow
        $script:ok = $false
    }
}
function Ver($cmd, $arg) {
    $c = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($c) { return (& $cmd $arg 2>$null | Select-Object -First 1) }
    return $null
}

Write-Host "檢查開發環境..." ; Write-Host ""

# 1. C++ 建置工具（Rust 在 Windows 上需要）
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
$vc = $null
if (Test-Path $vswhere) {
    $vc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property displayName
}
Show "C++ 建置工具" ([bool]$vc) "$vc" "安裝 Visual Studio Build Tools，勾選「使用 C++ 的桌面開發」：https://visualstudio.microsoft.com/visual-cpp-build-tools/"

# 2. Rust
$rustc = Ver "rustc" "--version"
Show "Rust (rustc)" ([bool]$rustc) "$rustc" "從 https://rustup.rs 下載 rustup-init.exe 安裝，裝完重開 PowerShell"
$cargo = Ver "cargo" "--version"
Show "Cargo" ([bool]$cargo) "$cargo" "隨 Rust 一起安裝"

# 3. Node.js（桌面 App 介面用，第 3 步開始需要）
$node = Ver "node" "--version"
Show "Node.js" ([bool]$node) "$node" "安裝 LTS 版：https://nodejs.org"

# 4. Git（建議，用來保存每一步的版本）
$git = Ver "git" "--version"
Show "Git（建議）" ([bool]$git) "$git" "https://git-scm.com/download/win"

# 5. WebView2（Tauri 桌面視窗用，Windows 11 內建）
$wv = $null
foreach ($k in @("HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
                 "HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}")) {
    $p = Get-ItemProperty -Path $k -Name pv -ErrorAction SilentlyContinue
    if ($p -and $p.pv -and $p.pv -ne "0.0.0.0") { $wv = $p.pv; break }
}
Show "WebView2" ([bool]$wv) "$wv" "https://developer.microsoft.com/microsoft-edge/webview2/"

Write-Host ""
if ($ok) {
    Write-Host "全部就緒。接著執行：cargo build ; cargo test ; cargo run -p engine" -ForegroundColor Green
} else {
    Write-Host "請先安裝上面標示「缺少」的項目，裝完重開 PowerShell 再執行一次。" -ForegroundColor Yellow
}
