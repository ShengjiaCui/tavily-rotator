#!/usr/bin/env bash
# tavily-rotator 安装脚本(ADR-0018)
#
# 功能:
#   1. cargo build --release 编译
#   2. 拷二进制到 ~/.local/bin/tavily-rotator
#   3. 生成 ~/Library/LaunchAgents/com.opdev.tavily-rotator.plist
#   4. launchctl bootstrap(开机自启 + 崩溃自愈)
#   5. 验证
#
# 用法:
#   ./scripts/install.sh          # 全新安装
#   ./scripts/install.sh --rebuild # 只重编译 + 替换二进制(不重装 plist)
#
# 幂等:重复运行安全。

set -Eeuo pipefail

# 确保 cargo 在 PATH(rustup 装的)
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# === 路径 ===
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="tavily-rotator"
INSTALL_PATH="$HOME/.local/bin/$BIN_NAME"
PLIST_LABEL="com.opdev.tavily-rotator"
PLIST_PATH="$HOME/Library/LaunchAgents/${PLIST_LABEL}.plist"
LOG_PATH="$HOME/opdev/runlog/tavily-rotator.log"

# === 颜色 ===
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
log()  { echo -e "${GREEN}[$(date '+%H:%M:%S')]${NC} $*"; }
warn() { echo -e "${YELLOW}[$(date '+%H:%M:%S')] WARN:${NC} $*"; }
err()  { echo -e "${RED}[$(date '+%H:%M:%S')] ERROR:${NC} $*" >&2; }

# === 前置检查 ===
[ -d "$PROJECT_DIR/src" ] || { err "不在 tavily-rotator 项目目录"; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
    err "cargo 不在 PATH。先运行: . \"\$HOME/.cargo/env\" 或装 rustup"
    exit 1
fi

# === 1. 编译 release ===
log "1/5 编译 release(opt-level=z + lto + strip)..."
cd "$PROJECT_DIR"
cargo build --release
log "   ✓ 编译完成: $(ls -lh target/release/$BIN_NAME | awk '{print $5}')"

# === 2. 拷二进制 ===
log "2/5 安装二进制 → $INSTALL_PATH"
mkdir -p "$(dirname "$INSTALL_PATH")"
cp "target/release/$BIN_NAME" "$INSTALL_PATH"
chmod 0755 "$INSTALL_PATH"
log "   ✓ $INSTALL_PATH"

# 如果只要 --rebuild,到这里结束
if [ "${1:-}" = "--rebuild" ]; then
    log "   (--rebuild 模式,跳过 plist)"
    # 如果 daemon 在跑,重启加载新二进制
    if launchctl print "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1; then
        log "   重启 daemon 加载新二进制..."
        launchctl kickstart -k "gui/$(id -u)/$PLIST_LABEL"
        sleep 2
    fi
    log "✓ 完成(rebuild)"
    exit 0
fi

# === 3. 生成 plist ===
log "3/5 生成 plist → $PLIST_PATH"
mkdir -p "$(dirname "$PLIST_PATH")"
mkdir -p "$(dirname "$LOG_PATH")"

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
log "   ✓ $PLIST_PATH"

# === 4. bootstrap ===
log "4/5 launchctl bootstrap..."
# 先 bootout 旧的(如果有,幂等)
launchctl bootout "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1 || true
sleep 1
launchctl bootstrap "gui/$(id -u)" "$PLIST_PATH"
sleep 2

# 验证在跑
if launchctl print "gui/$(id -u)/$PLIST_LABEL" >/dev/null 2>&1; then
    log "   ✓ daemon 已启动(KeepAlive=SuccessfulExit:false,崩溃自愈)"
else
    err "   daemon 启动失败,查日志: tail -50 $LOG_PATH"
    exit 1
fi

# === 5. 验证 ===
log "5/5 验证..."
sleep 2

echo
echo "=== 运行状态 ==="
launchctl print "gui/$(id -u)/$PLIST_LABEL" | grep -E 'state|last exit code|pid' | head -3

echo
echo "=== /health ==="
curl -sf http://127.0.0.1:8731/health && echo " ✓" || err "HTTP 服务无响应"

echo
echo "=== /api/active ==="
curl -sf http://127.0.0.1:8731/api/active | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(f\"  active: key[{d['idx']}] {d['label']}, remaining={d.get('remaining')}, env_pushed={d['env_pushed']}\")
" 2>/dev/null || warn "  (active 查询失败,可能 /usage 还没跑完)"

echo
echo "=== launchctl getenv TAVILY_API_KEY ==="
VAL=$(launchctl getenv TAVILY_API_KEY)
if [ -n "$VAL" ]; then
    echo "  ${VAL:0:16}...${VAL: -4} ✓"
else
    err "  环境变量未推送"
fi

echo
log "✓ 安装完成"
echo
echo "Web 面板: http://127.0.0.1:8731/"
echo "日志:     tail -f $LOG_PATH"
echo "停服务:   launchctl bootout gui/$(id -u)/$PLIST_LABEL"
echo "重启:     launchctl kickstart -k gui/$(id -u)/$PLIST_LABEL"
