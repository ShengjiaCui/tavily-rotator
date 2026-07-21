#!/usr/bin/env bash
# tavily-rotator 安装脚本(平台分发器)
#
# 检测平台,调用对应的安装脚本:
#   macOS  → install-macos.sh(launchd plist)
#   Linux  → install-linux.sh(systemd --user)
#   Windows → install-windows.ps1(Task Scheduler)
#
# 用法:
#   ./scripts/install.sh           # 全新安装(自动检测平台)
#   ./scripts/install.sh --rebuild # 只重编译 + 重启 daemon
#
# 幂等:重复运行安全。

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

OS="$(uname)"

case "$OS" in
    Darwin)
        # macOS
        exec bash "$SCRIPT_DIR/install-macos.sh" "$@"
        ;;
    Linux)
        # Linux
        exec bash "$SCRIPT_DIR/install-linux.sh" "$@"
        ;;
    *)
        # Windows(Git Bash / MSYS2 / Cygwin 下 uname 可能是 MINGW* 或 CYGWIN*)
        if [[ "$OS" == MINGW* ]] || [[ "$OS" == CYGWIN* ]] || [[ "$OS" == MSYS* ]]; then
            echo "检测到 Windows($OS)。请用 PowerShell 运行:"
            echo "  powershell -ExecutionPolicy Bypass -File scripts/install-windows.ps1"
            exit 1
        fi
        echo "ERROR: 不支持的平台: $OS"
        echo "支持: macOS / Linux / Windows"
        exit 1
        ;;
esac
