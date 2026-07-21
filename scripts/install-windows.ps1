# tavily-rotator Windows 安装脚本(Task Scheduler + 注册表自启)
#
# 功能:
#   1. 检查前置条件(Windows / cargo)
#   2. cargo build --release 编译
#   3. 拷二进制到 ~/tavily-rotator/tavily-rotator.exe
#   4. 注册 Task Scheduler 任务(开机自启 + 崩溃重启)
#   5. 启动任务
#   6. 验证 + 首次运行引导
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/install-windows.ps1
#
# 幂等:重复运行安全。仅支持 Windows。

$ErrorActionPreference = "Stop"

if (-not $IsWindows -and -not ($PSVersionTable.Platform -eq "Win32NT")) {
    Write-Host "ERROR: 此脚本仅用于 Windows。macOS 用 install.sh,Linux 用 install-linux.ps1" -ForegroundColor Red
    exit 1
}

# === 路径 ===
$ProjectDir = Resolve-Path "$PSScriptRoot\.."
$InstallDir = "$env:USERPROFILE\tavily-rotator"
$InstallPath = "$InstallDir\tavily-rotator.exe"
$ConfigDir = "$env:USERPROFILE\.config\tavily-rotator"
$DataDir = "$env:USERPROFILE\.local\share\tavily-rotator"
$KeysToml = "$ConfigDir\keys.toml"
$LogPath = "$DataDir\daemon.log"
$TaskName = "TavilyRotator"

function Log($msg) { Write-Host "[$(Get-Date -Format 'HH:mm:ss')] $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "[$(Get-Date -Format 'HH:mm:ss')] WARN: $msg" -ForegroundColor Yellow }
function Err($msg) { Write-Host "[$(Get-Date -Format 'HH:mm:ss')] ERROR: $msg" -ForegroundColor Red }
function Step($msg) { Write-Host "`n━━━ $msg ━━━" -ForegroundColor Blue }

# === 前置检查 ===
if (-not (Test-Path "$ProjectDir\src")) { Err "不在 tavily-rotator 项目目录"; exit 1 }

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    Err "cargo 不在 PATH。先装 Rust: https://rustup.rs"
    exit 1
}

# === 1. 编译 ===
Step "1/6 编译 release"
Push-Location $ProjectDir
cargo build --release
$exePath = "target\release\tavily-rotator.exe"
$size = (Get-Item $exePath).Length / 1MB
Log ("✓ 编译完成: {0:N1} MB" -f $size)

# === 2. 拷二进制 ===
Step "2/6 安装二进制"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item $exePath $InstallPath -Force
Log "✓ $InstallPath"

Pop-Location

# === 3. 建目录 ===
Step "3/6 建配置/数据目录"
New-Item -ItemType Directory -Force -Path $ConfigDir, $DataDir | Out-Null
Log "✓ $ConfigDir"
Log "✓ $DataDir"

# === 4. Task Scheduler 任务 ===
Step "4/6 注册 Task Scheduler 任务"

# 删除旧任务(幂等)
$existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Log "✓ 旧任务已删除"
}

$action = New-ScheduledTaskAction -Execute $InstallPath
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1)
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive

Register-ScheduledTask -TaskName $TaskName `
    -Action $action `
    -Trigger $trigger `
    -Settings $settings `
    -Principal $principal `
    -Description "Tavily API Key Rotator daemon" | Out-Null

Log "✓ 任务 '$TaskName' 已注册(开机自启 + 崩溃重启)"

# === 5. 启动 ===
Step "5/6 启动任务"
Start-ScheduledTask -TaskName $TaskName
Start-Sleep -Seconds 3

$task = Get-ScheduledTask -TaskName $TaskName
$state = $task.State
if ($state -eq "Running") {
    Log "✓ 任务运行中"
} else {
    Warn "任务状态: $state(可能刚启动)"
}

# === 6. 验证 ===
Step "6/6 验证"
Start-Sleep -Seconds 2

try {
    $resp = Invoke-WebRequest -Uri "http://127.0.0.1:8731/health" -UseBasicParsing -TimeoutSec 5
    if ($resp.StatusCode -eq 200) {
        Log "✓ HTTP 服务正常(http://127.0.0.1:8731/)"
    }
} catch {
    Warn "HTTP 服务未就绪(可能刚启动,等几秒再试)"
}

Write-Host ""
if (Test-Path $KeysToml) {
    $keyCount = (Select-String -Path $KeysToml -Pattern '^\[\[keys\]\]').Count
    Log "✓ 已有配置: $KeysToml($keyCount 个 key)"
} else {
    Write-Host ""
    Write-Host "━━━ 首次运行引导 ━━━" -ForegroundColor Yellow
    Write-Host "还没有 key 配置。请打开 Web 面板添加你的 Tavily key:"
    Write-Host ""
    Write-Host "  1. 浏览器打开: http://127.0.0.1:8731/"
    Write-Host "  2. 点 '+ 添加',输入你的 Tavily API key(tvly-dev-...)"
    Write-Host ""
    Write-Host "⚠ Windows 环境变量推送机制:"
    Write-Host "  daemon 写注册表 HKCU\Environment\TAVILY_API_KEY"
    Write-Host "  新开的进程(CMD/PowerShell/IDE)能读到"
    Write-Host "  当前已开的窗口需要重开才能拿到新值"
}

Write-Host ""
Log "✓ 安装完成"
Write-Host ""
Write-Host "Web 面板: http://127.0.0.1:8731/"
Write-Host "日志:     Get-Content $LogPath -Wait"
Write-Host "状态:     Get-ScheduledTask -TaskName $TaskName"
Write-Host "停服务:   Stop-ScheduledTask -TaskName $TaskName; Unregister-ScheduledTask -TaskName $TaskName"
Write-Host "重启:     Restart tasks: Stop + Start"
Write-Host "升级:     git pull; powershell -File scripts/install-windows.ps1"
