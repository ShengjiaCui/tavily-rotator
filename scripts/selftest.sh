#!/usr/bin/env bash
# tavily-rotator selftest(跨平台)
#
# 装完后跑这个脚本,逐项核验。PASS/FAIL 计数,非零退出。
# 用法: ./scripts/selftest.sh

set -u

# 通用路径
KEYS_TOML="$HOME/.config/tavily-rotator/keys.toml"
STATE_DB="$HOME/.local/share/tavily-rotator/state.db"
LOG_PATH="$HOME/.local/share/tavily-rotator/daemon.log"
PORT="8731"
OS="$(uname)"

# 平台特定:二进制路径 + 服务检查方式
case "$OS" in
    Darwin)
        BIN_PATH="$HOME/.local/bin/tavily-rotator"
        SERVICE_LABEL="com.tavily-rotator"
        ;;
    Linux)
        BIN_PATH="$HOME/.local/bin/tavily-rotator"
        SERVICE_LABEL="tavily-rotator"  # systemd service 名
        ;;
    *)
        # Windows 用 selftest.ps1,这里退出
        echo "Windows 请用: powershell -File scripts/selftest.ps1"
        exit 1
        ;;
esac

failures=0
pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

echo "=== tavily-rotator selftest ($OS) ==="
echo

# 1. 二进制存在
if [ -x "$BIN_PATH" ]; then
    pass "二进制 $BIN_PATH 存在且可执行"
else
    fail "二进制 $BIN_PATH 缺失或不可执行"
fi

# 2+3. 服务状态(平台分发)
case "$OS" in
    Darwin)
        PLIST_PATH="$HOME/Library/LaunchAgents/${SERVICE_LABEL}.plist"
        if [ -f "$PLIST_PATH" ]; then
            pass "plist 存在"
        else
            fail "plist 缺失($PLIST_PATH)"
        fi
        if launchctl print "gui/$(id -u)/$SERVICE_LABEL" >/dev/null 2>&1; then
            pass "launchd daemon 运行中"
        else
            fail "launchd 未加载 $SERVICE_LABEL"
        fi
        ;;
    Linux)
        if systemctl --user is-active --quiet "$SERVICE_LABEL" 2>/dev/null; then
            pass "systemd service 运行中"
        else
            fail "systemd service 未运行(systemctl --user status $SERVICE_LABEL)"
        fi
        ;;
esac

# 4. keys.toml 权限 0600
if [ -f "$KEYS_TOML" ]; then
    PERM=$(stat -f '%Sp' "$KEYS_TOML" 2>/dev/null || stat -c '%A' "$KEYS_TOML" 2>/dev/null)
    if echo "$PERM" | grep -qE 'rw-------|600'; then
        pass "keys.toml 权限 0600"
    else
        fail "keys.toml 权限不是 0600(实际 $PERM)"
    fi
else
    fail "keys.toml 缺失"
fi

# 5. keys.toml 至少有 1 个 key
if [ -f "$KEYS_TOML" ]; then
    KEY_COUNT=$(grep -c '^\[\[keys\]\]' "$KEYS_TOML" 2>/dev/null || echo 0)
    if [ "$KEY_COUNT" -ge 1 ]; then
        pass "keys.toml 有 $KEY_COUNT 个 key"
    else
        fail "keys.toml 无 key"
    fi
fi

# 6. HTTP /health
if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    pass "HTTP /health 响应正常"
else
    fail "HTTP /health 无响应(端口 $PORT)"
fi

# 7. /api/active
ACTIVE_JSON=$(curl -sf "http://127.0.0.1:$PORT/api/active" 2>/dev/null || echo "")
if echo "$ACTIVE_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "label" in d' 2>/dev/null; then
    LABEL_VAL=$(echo "$ACTIVE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["label"])')
    pass "/api/active 返回: $LABEL_VAL"
else
    fail "/api/active 无响应或格式异常"
fi

# 8. 环境变量推送验证(平台分发)
case "$OS" in
    Darwin)
        TAVILY_VAL=$(launchctl getenv TAVILY_API_KEY 2>/dev/null || echo "")
        SOURCE_DESC="launchctl"
        ;;
    Linux)
        # Linux:读 active-env.sh
        ACTIVE_ENV="$HOME/.config/tavily-rotator/active-env.sh"
        if [ -f "$ACTIVE_ENV" ]; then
            TAVILY_VAL=$(grep -oE 'TAVILY_API_KEY="[^"]*"' "$ACTIVE_ENV" 2>/dev/null | sed 's/.*="//;s/"//' || echo "")
            SOURCE_DESC="active-env.sh"
        else
            TAVILY_VAL=""
            SOURCE_DESC="active-env.sh(缺失)"
        fi
        ;;
esac

if [ -n "$TAVILY_VAL" ]; then
    pass "$SOURCE_DESC TAVILY_API_KEY 有值(${TAVILY_VAL:0:16}...${TAVILY_VAL: -4})"
else
    fail "$SOURCE_DESC 无 TAVILY_API_KEY(daemon 未推送)"
fi

# 9. tvly auth source
if command -v tvly >/dev/null 2>&1; then
    AUTH_SRC=$(tvly auth --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("source"))' 2>/dev/null || echo "ERROR")
    if [ "$AUTH_SRC" = "None" ] || [ "$AUTH_SRC" = "null" ] || \
       echo "$AUTH_SRC" | grep -qi "environment variable"; then
        pass "tvly auth_source 正确($AUTH_SRC)"
    else
        fail "tvly auth_source=$AUTH_SRC(login 了会覆盖,请 tvly logout)"
    fi
else
    fail "tvly 不在 PATH(需安装:见 Web 面板一键安装)"
fi

# 10. SQLite 表存在
if [ -f "$STATE_DB" ]; then
    TABLES=$(sqlite3 "$STATE_DB" ".tables" 2>/dev/null || echo "")
    for t in usage_snapshots rotations active_pointer install_events; do
        if echo "$TABLES" | grep -qw "$t"; then
            pass "SQLite 表 $t 存在"
        else
            fail "SQLite 表 $t 缺失"
        fi
    done
else
    fail "SQLite 数据库缺失($STATE_DB)"
fi

# 11. 日志文件
if [ -f "$LOG_PATH" ]; then
    pass "日志文件存在"
else
    fail "日志文件缺失($LOG_PATH)"
fi

echo
if [ "$failures" -eq 0 ]; then
    printf '🎉 全部通过(0 failures)\n'
    exit 0
else
    printf '❌ %d 项失败\n' "$failures"
    exit 1
fi
