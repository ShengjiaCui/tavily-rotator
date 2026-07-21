#!/usr/bin/env bash
# tavily-rotator macOS 安装脚本(launchd plist)
#
# 功能:
#   1. 检查前置条件(macOS / Rust)
#   2. cargo build --release 编译
#   3. 拷二进制到 ~/.local/bin/tavily-rotator
#   4. 生成 ~/Library/LaunchAgents/com.tavily-rotator.plist
#   5. launchctl bootstrap(开机自启 + 崩溃自愈)
#   6. 验证 + 首次运行引导
#
# 用法:
#   ./scripts/install-macos.sh           # 全新安装
#   ./scripts/install-macos.sh --rebuild # 只重编译 + 替换二进制(不重装 plist)
#
# 幂等:重复运行安全。仅支持 macOS(launchd 专属)。

set -Eeuo pipefail

if [ "$(uname)" != "Darwin" ]; then
    echo "ERROR: 此脚本仅用于 macOS。用 ./install.sh 自动检测平台。"
    exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
    echo "ERROR: 不要用 sudo/root 运行。tavily-rotator 是用户级 daemon。"
    exit 1
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# === 路径(全部 $HOME 相对,无外部依赖)===
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="tavily-rotator"
INSTALL_PATH="$HOME/.local/bin/$BIN_NAME"
CONFIG_DIR="$HOME/.config/tavily-rotator"
DATA_DIR="$HOME/.local/share/tavily-rotator"
KEYS_TOML="$CONFIG_DIR/keys.toml"
PLIST_LABEL="com.tavily-rotator"
PLIST_PATH="$HOME/Library/LaunchAgents/${PLIST_LABEL}.plist"
LOG_PATH="$DATA_DIR/daemon.log"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[$(date '+%H:%M:%S')]${NC} $*"; }
warn() { echo -e "${YELLOW}[$(date '+%H:%M:%S')] WARN:${NC} $*"; }
err()  { echo -e "${RED}[$(date '+%H:%M:%S')] ERROR:${NC} $*" >&2; }
step() { echo -e "\n${BLUE}━━━ $* ━━━${NC}"; }

# === 前置检查 ===
[ -d "$PROJECT_DIR/src" ] || { err "不在 tavily-rotator 项目目录(找不到 src/)"; exit 1; }
[ -f "$PROJECT_DIR/Cargo.toml" ] || { err "找不到 Cargo.toml"; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
    err "cargo 不在 PATH。先装 Rust 工具链:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "  source \"\$HOME/.cargo/env\""
    exit 1
fi

# === 1. 编译 release ===
step "1/6 编译 release(opt-level=z + lto + strip)"
cd "$PROJECT_DIR"
cargo build --release
BIN_SIZE=$(ls -lh "target/release/$BIN_NAME" | awk '{print $5}')
log "✓ 编译完成: $BIN_SIZE"

# === 2. 拷二进制 ===
step "2/6 安装二进制"
mkdir -p "$(dirname "$INSTALL_PATH")"
cp "target/release/$BIN_NAME" "$INSTALL_PATH"
chmod 0755 "$INSTALL_PATH"
log "✓ $INSTALL_PATH"

# === --rebuild 到此结束 ===
if [ "${1:-}" = "--rebuild" ]; then
    log "(--rebuild 模式,跳过 plist)"
    if launchctl print "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1; then
        log "重启 daemon 加载新二进制..."
        launchctl kickstart -k "gui/$(id -u)/$PLIST_LABEL"
        sleep 2
    fi
    log "✓ 完成(rebuild)"
    exit 0
fi

# === 3. 建目录 ===
step "3/6 建配置/数据目录"
mkdir -p "$CONFIG_DIR" "$DATA_DIR" "$(dirname "$LOG_PATH")"
log "✓ $CONFIG_DIR"
log "✓ $DATA_DIR"

# === 4. 生成 plist ===
step "4/6 生成 launchd plist"
mkdir -p "$(dirname "$PLIST_PATH")"

# 停掉旧版(com.opdev.tavily-rotator,迁移用)
OLD_LABEL="com.opdev.tavily-rotator"
if launchctl print "gui/$(id -u)/$OLD_LABEL" >/dev/null 2>&1; then
    warn "检测到旧版 daemon($OLD_LABEL),停掉并迁移..."
    launchctl bootout "gui/$(id -u)/$OLD_LABEL" >/dev/null 2>&1 || true
    rm -f "$HOME/Library/LaunchAgents/${OLD_LABEL}.plist"
    log "✓ 旧版已停 + plist 已删"
fi

cat > "$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$PLIST_LABEL</string>

  <key>ProgramArguments</key>
  <array>
    <string>$INSTALL_PATH</string>
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>

  <key>StandardOutPath</key>
  <string>$LOG_PATH</string>
  <key>StandardErrorPath</key>
  <string>$LOG_PATH</string>

  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_LOG</key>
    <string>info</string>
    <key>HOME</key>
    <string>$HOME</string>
  </dict>
</dict>
</plist>
EOF
log "✓ $PLIST_PATH"

# === 5. bootstrap ===
step "5/6 启动 daemon"
launchctl bootout "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1 || true
sleep 1
launchctl bootstrap "gui/$(id -u)" "$PLIST_PATH"
sleep 2

if launchctl print "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1; then
    log "✓ daemon 已启动(KeepAlive,崩溃自愈)"
else
    err "daemon 启动失败,查日志: tail -50 $LOG_PATH"
    exit 1
fi

# === 6. 验证 + 首次运行引导 ===
step "6/6 验证"
sleep 2

echo
launchctl print "gui/$(id -u)/$PLIST_LABEL" | grep -E 'state|pid' | head -2

echo
if curl -sf http://127.0.0.1:8731/health >/dev/null 2>&1; then
    log "✓ HTTP 服务正常(http://127.0.0.1:8731/)"
else
    warn "HTTP 服务未就绪(可能 daemon 刚启动,等几秒再试)"
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
    echo "  3. key 会自动验证(查 /usage)并保存"
    echo
    echo "没有 Tavily key?去 https://tavily.com 免费注册(每月 1000 credits)。"
fi

echo
log "✓ 安装完成"
echo
echo "Web 面板: http://127.0.0.1:8731/"
echo "日志:     tail -f $LOG_PATH"
echo "自检:     $PROJECT_DIR/scripts/selftest.sh"
echo "停服务:   launchctl bootout gui/$(id -u)/$PLIST_LABEL"
echo "重启:     launchctl kickstart -k gui/$(id -u)/$PLIST_LABEL"
echo "升级:     git pull && ./scripts/install.sh --rebuild"
