#!/usr/bin/env bash
# tavily-rotator selftest(ADR-0018)
#
# 装完 install.sh 后跑这个脚本,逐项核验。
# 参照 opdev .selftest.sh 模式:PASS/FAIL 计数,非零退出。
#
# 用法: ./scripts/selftest.sh

set -u

LABEL="com.opdev.tavily-rotator"
BIN_PATH="$HOME/.local/bin/tavily-rotator"
PLIST_PATH="$HOME/Library/LaunchAgents/${LABEL}.plist"
KEYS_TOML="$HOME/.config/tavily-rotator/keys.toml"
STATE_DB="$HOME/.local/share/tavily-rotator/state.db"
LOG_PATH="$HOME/opdev/runlog/tavily-rotator.log"
PORT="8731"

failures=0

pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

echo "=== tavily-rotator selftest ==="
echo

# 1. 二进制存在
if [ -x "$BIN_PATH" ]; then
    pass "二进制 $BIN_PATH 存在且可执行"
else
    fail "二进制 $BIN_PATH 缺失或不可执行"
fi

# 2. plist 存在
if [ -f "$PLIST_PATH" ]; then
    pass "plist $PLIST_PATH 存在"
else
    fail "plist $PLIST_PATH 缺失"
fi

# 3. launchd 在跑
if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
    STATE=$(launchctl print "gui/$(id -u)/$LABEL" | grep -E '^\s*state' | head -1 | sed 's/.*= //')
    if echo "$STATE" | grep -q running; then
        pass "launchd daemon 运行中(state=$STATE)"
    else
        fail "launchd daemon 状态异常(state=$STATE)"
    fi
else
    fail "launchd 未加载 $LABEL"
fi

# 4. keys.toml 权限 0600
if [ -f "$KEYS_TOML" ]; then
    PERM=$(stat -f '%Sp' "$KEYS_TOML" 2>/dev/null || stat -c '%A' "$KEYS_TOML" 2>/dev/null)
    if echo "$PERM" | grep -qE 'rw-------|600'; then
        pass "keys.toml 权限 0600 ($PERM)"
    else
        fail "keys.toml 权限不是 0600 (实际 $PERM)"
    fi
else
    fail "keys.toml 缺失($KEYS_TOML)"
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

# 7. HTTP /api/active
ACTIVE_JSON=$(curl -sf "http://127.0.0.1:$PORT/api/active" 2>/dev/null || echo "")
if echo "$ACTIVE_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "label" in d' 2>/dev/null; then
    LABEL_VAL=$(echo "$ACTIVE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["label"])')
    pass "/api/active 返回 active key: $LABEL_VAL"
else
    fail "/api/active 无响应或格式异常"
fi

# 8. launchctl getenv TAVILY_API_KEY 有值
TAVILY_VAL=$(launchctl getenv TAVILY_API_KEY 2>/dev/null || echo "")
if [ -n "$TAVILY_VAL" ]; then
    pass "launchctl getenv TAVILY_API_KEY 有值(${TAVILY_VAL:0:16}...${TAVILY_VAL: -4})"
else
    fail "launchctl getenv TAVILY_API_KEY 无值(daemon 未推送环境变量)"
fi

# 9. tvly auth 未 login(auth_source 必须为 null)
if command -v tvly >/dev/null 2>&1; then
    AUTH_SRC=$(tvly auth --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("source"))' 2>/dev/null || echo "ERROR")
    if [ "$AUTH_SRC" = "None" ] || [ "$AUTH_SRC" = "null" ]; then
        pass "tvly 未 login(auth_source=null,轮换生效前提)"
    else
        fail "tvly 已 login(auth_source=$AUTH_SRC),会覆盖环境变量注入,请运行 tvly logout"
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

# 11. 日志文件存在
if [ -f "$LOG_PATH" ]; then
    pass "日志文件存在($LOG_PATH)"
else
    fail "日志文件缺失($LOG_PATH)"
fi

echo
echo "=== 结果 ==="
if [ "$failures" -eq 0 ]; then
    printf '🎉 全部通过(0 failures)\n'
    exit 0
else
    printf '❌ %d 项失败\n' "$failures"
    exit 1
fi
