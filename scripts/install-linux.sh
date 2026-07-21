#!/usr/bin/env bash
# tavily-rotator Linux 安装脚本(systemd --user 服务)
#
# 功能:
#   1. 检查前置条件(Linux / Rust / systemd)
#   2. cargo build --release 编译
#   3. 拷二进制到 ~/.local/bin/tavily-rotator
#   4. 生成 ~/.config/systemd/user/tavily-rotator.service
#   5. systemctl --user enable + start(开机自启 + 崩溃自愈)
#   6. 验证 + 首次运行引导
#
# 用法:
#   ./scripts/install-linux.sh           # 全新安装
#   ./scripts/install.sh --rebuild       # 只重编译(通用入口)
#
# 幂等:重复运行安全。仅支持 Linux。

set -Eeuo pipefail

if [ "$(uname)" != "Linux" ]; then
    echo "ERROR: 此脚本仅用于 Linux。macOS 用 install.sh,Windows 用 install-windows.ps1"
    exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
    echo "ERROR: 不要用 sudo/root 运行。tavily-rotator 是用户级 daemon(systemd --user)。"
    exit 1
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# === 路径 ===
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="tavily-rotator"
INSTALL_PATH="$HOME/.local/bin/$BIN_NAME"
CONFIG_DIR="$HOME/.config/tavily-rotator"
DATA_DIR="$HOME/.local/share/tavily-rotator"
KEYS_TOML="$CONFIG_DIR/keys.toml"
LOG_PATH="$DATA_DIR/daemon.log"
SERVICE_NAME="tavily-rotator"
SERVICE_PATH="$HOME/.config/systemd/user/${SERVICE_NAME}.service"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[$(date '+%H:%M:%S')]${NC} $*"; }
warn() { echo -e "${YELLOW}[$(date '+%H:%M:%S')] WARN:${NC} $*"; }
err()  { echo -e "${RED}[$(date '+%H:%M:%S')] ERROR:${NC} $*" >&2; }
step() { echo -e "\n${BLUE}━━━ $* ━━━${NC}"; }

# === 前置检查 ===
[ -d "$PROJECT_DIR/src" ] || { err "不在 tavily-rotator 项目目录"; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
    err "cargo 不在 PATH。先装 Rust:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    err "systemctl 不在 PATH。此脚本需要 systemd(绝大多数现代发行版自带)。"
    echo "无 systemd 的发行版(如某些容器)需要手动管理 daemon。"
    exit 1
fi

# === 1. 编译 ===
step "1/6 编译 release"
cd "$PROJECT_DIR"
cargo build --release
log "✓ 编译完成: $(ls -lh target/release/$BIN_NAME | awk '{print $5}')"

# === 2. 拷二进制 ===
step "2/6 安装二进制"
mkdir -p "$(dirname "$INSTALL_PATH")"
cp "target/release/$BIN_NAME" "$INSTALL_PATH"
chmod 0755 "$INSTALL_PATH"
log "✓ $INSTALL_PATH"

# === --rebuild 到此结束 ===
if [ "${1:-}" = "--rebuild" ]; then
    log "(--rebuild,重启 service)"
    systemctl --user restart "$SERVICE_NAME" 2>/dev/null || true
    log "✓ 完成(rebuild)"
    exit 0
fi

# === 3. 建目录 ===
step "3/6 建配置/数据目录"
mkdir -p "$CONFIG_DIR" "$DATA_DIR" "$(dirname "$LOG_PATH")"
mkdir -p "$(dirname "$SERVICE_PATH")"
log "✓ $CONFIG_DIR"
log "✓ $DATA_DIR"

# === 4. 生成 systemd unit ===
step "4/6 生成 systemd user service"
cat > "$SERVICE_PATH" <<EOF
[Unit]
Description=Tavily API Key Rotator
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$INSTALL_PATH
Restart=on-failure
RestartSec=3
StandardOutput=append:$LOG_PATH
StandardError=append:$LOG_PATH
Environment=RUST_LOG=info
Environment=HOME=$HOME

[Install]
WantedBy=default.target
EOF
log "✓ $SERVICE_PATH"

# === 5. enable + start ===
step "5/6 启动 service"
systemctl --user daemon-reload
systemctl --user enable "$SERVICE_NAME"
systemctl --user restart "$SERVICE_NAME"
sleep 2

if systemctl --user is-active --quiet "$SERVICE_NAME"; then
    log "✓ service 运行中(Restart=on-failure,崩溃自愈)"
else
    err "service 启动失败,查日志: journalctl --user -u $SERVICE_NAME -n 50"
    exit 1
fi

# 确保用户 logout 后 service 仍运行(lingering)
if command -v loginctl >/dev/null 2>&1; then
    loginctl enable-linger "$USER" 2>/dev/null || true
    log "✓ enable-linger(logout 后 service 仍运行)"
fi

# === 6. 验证 + 引导 ===
step "6/6 验证"
sleep 2

systemctl --user status "$SERVICE_NAME" --no-pager | head -5

echo
if curl -sf http://127.0.0.1:8731/health >/dev/null 2>&1; then
    log "✓ HTTP 服务正常(http://127.0.0.1:8731/)"
else
    warn "HTTP 服务未就绪(可能刚启动,等几秒再试)"
fi

echo
if [ -f "$KEYS_TOML" ]; then
    KEY_COUNT=$(grep -c '^\[\[keys\]\]' "$KEYS_TOML" 2>/dev/null || echo 0)
    log "✓ 已有配置: $KEYS_TOML($KEY_COUNT 个 key)"
else
    echo
    echo -e "${YELLOW}━━━ 首次运行引导 ━━━${NC}"
    echo "还没有 key 配置。请打开 Web 面板添加你的 Tavily key:"
    echo
    echo "  1. 浏览器打开: http://127.0.0.1:8731/"
    echo "  2. 点 '+ 添加',输入你的 Tavily API key(tvly-dev-...)"
    echo
    echo "⚠ Linux 环境变量推送机制:"
    echo "  daemon 会写 ~/.config/tavily-rotator/active-env.sh"
    echo "  并在 .bashrc/.zshrc 加一行 source 它"
    echo "  新开的终端会拿到最新 active key"
    echo "  如果当前 shell 没拿到,重开终端或手动 source"
fi

echo
log "✓ 安装完成"
echo
echo "Web 面板: http://127.0.0.1:8731/"
echo "日志:     tail -f $LOG_PATH"
echo "状态:     systemctl --user status $SERVICE_NAME"
echo "停服务:   systemctl --user disable --now $SERVICE_NAME"
echo "重启:     systemctl --user restart $SERVICE_NAME"
echo "升级:     git pull && ./scripts/install-linux.sh --rebuild"
