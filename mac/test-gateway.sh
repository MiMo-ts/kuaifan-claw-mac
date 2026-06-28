#!/usr/bin/env bash
# =============================================================================
# test-gateway.sh — 完整诊断 + 启动测试 openclaw-cn gateway
# 用法：bash mac/test-gateway.sh
# 退出码：0=完全成功；1=某一步失败
# =============================================================================
set -e

DATA_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data"
OPENCLAW_DIR="$DATA_DIR/openclaw-cn"
CONFIG="$OPENCLAW_DIR/openclaw.json"
LOG_DIR="$DATA_DIR/logs"
GW_LOG="$LOG_DIR/openclaw-gateway.log"
APP_LOG="$LOG_DIR/app.log"
TEST_LOG=/tmp/test-gateway-$$.log

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; DIM='\033[2m'; RESET='\033[0m'
step() { echo -e "\n${YELLOW}==> $1${RESET}"; }
ok()   { echo -e "  ${GREEN}✓${RESET} $1"; }
fail() { echo -e "  ${RED}✗${RESET} $1"; }
info() { echo -e "    ${DIM}$1${RESET}"; }

PASS=0; FAIL=0
check() {
    if eval "$2" >/dev/null 2>&1; then
        ok "$1"
        PASS=$((PASS+1))
    else
        fail "$1"
        info "$3"
        FAIL=$((FAIL+1))
    fi
}

# ─── 0. 关闭所有残留进程 ────────────────────────────────────────────────────
step "0. 关闭残留进程（避免 zombie 干扰）"
pkill -9 -f "clawdbot-gateway\|openclaw-cn" 2>/dev/null || true
sleep 2
ok "残留进程已清理"

# ─── 1. reap zombie（关键：openclaw-cn lock 库误判 zombie 为活进程）────
step "1. reap zombie 子进程"
ZOMBIES_BEFORE=$(ps -A -o stat= | grep -c "^Z" || true)
info "reap 前 zombie 数: $ZOMBIES_BEFORE"
if [ "$ZOMBIES_BEFORE" -gt 0 ]; then
    ps -A -o pid=,stat= 2>/dev/null | awk '$2 ~ /^Z/ { print $1 }' | while read zpid; do
        ppid=$(ps -o ppid= -p "$zpid" 2>/dev/null | tr -d ' ')
        if [ -n "$ppid" ]; then
            kill -s SIGCHLD "$ppid" 2>/dev/null || true
            info "  给 PPID=$ppid 发 SIGCHLD（zombie=$zpid）"
        fi
    done
    sleep 1
fi
ZOMBIES_AFTER=$(ps -A -o stat= | grep -c "^Z" || true)
info "reap 后 zombie 数: $ZOMBIES_AFTER"
[ "$ZOMBIES_AFTER" -eq 0 ] && ok "无 zombie 残留" || ok "仍有 $ZOMBIES_AFTER 个 zombie（不影响本次测试）"

# ─── 2. 清理可能残留的 lock 文件 ───────────────────────────────────────────
step "2. 检查 /tmp/openclaw/ 锁目录"
LOCK_DIR="/tmp/openclaw"
if [ -d "$LOCK_DIR" ]; then
    for f in "$LOCK_DIR"/gateway.*.lock; do
        [ -f "$f" ] || continue
        PID_IN_LOCK=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('pid','?'))" "$f" 2>/dev/null || echo "?")
        if [ "$PID_IN_LOCK" = "?" ] || ! kill -0 "$PID_IN_LOCK" 2>/dev/null; then
            info "  旧锁 $f (pid=$PID_IN_LOCK 已死) → 删除"
            rm -f "$f"
        else
            info "  锁 $f 由活 pid=$PID_IN_LOCK 持有"
        fi
    done
fi
ok "锁目录检查完成"

# ─── 3. 环境检查 ─────────────────────────────────────────────────────────────
step "3. 环境检查（node, git, openclaw-cn 完整性）"
check "node ≥ 22" "[[ \$(/usr/local/bin/node -v | sed 's/v//' | cut -d. -f1) -ge 22 ]]" "请安装 Node.js 22+: brew install node@22"
check "git 可用" "command -v git" "请安装 git: brew install git"
check "openclaw-cn 解压" "[ -d '$OPENCLAW_DIR' ]" "请先在 app 向导第 2 步安装 openclaw-cn"
check "openclaw.json 存在" "[ -f '$CONFIG' ]" "请先启动 app 并完成配置同步"
check "entry.js 存在" "[ -f '$OPENCLAW_DIR/dist/entry.js' ]" "openclaw-cn 安装不完整"
check "node_modules 完整" "[ -d '$OPENCLAW_DIR/node_modules' ] && [ -d '$OPENCLAW_DIR/node_modules/.bin' ]" "请重新运行 npm install"
ok "node_modules 包数: $(ls $OPENCLAW_DIR/node_modules/ 2>/dev/null | wc -l)"

# ─── 4. openclaw.json 校验 ──────────────────────────────────────────────────
step "4. openclaw.json 配置校验"
python3 - "$CONFIG" <<'PY' && ok "openclaw.json 格式正确且包含必要字段" || fail "openclaw.json 缺少必要字段"
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
g = d.get("gateway", {})
assert g.get("mode") == "local", f"gateway.mode 应为 'local'，实际: {g.get('mode')}"
assert g.get("port") == 18789, f"gateway.port 应为 18789，实际: {g.get('port')}"
assert g.get("auth", {}).get("token"), "gateway.auth.token 缺失"
agents = d.get("agents", {})
default = agents.get("defaults", {}).get("model", {}).get("primary", "")
assert default, "agents.defaults.model.primary 缺失"
print(f"  mode: {g['mode']}, port: {g['port']}, token: {g['auth']['token'][:8]}...")
print(f"  default model: {default}")
PY

# ─── 5. plugins.deny 检查 ──────────────────────────────────────────────────
step "5. plugins.deny 检查（防止扩展加载错误刷屏）"
DENY_COUNT=$(python3 -c "import json; d=json.load(open('$CONFIG')); print(len(d.get('plugins',{}).get('deny',[])))" 2>/dev/null || echo 0)
info "当前 deny 列表条目数: $DENY_COUNT"
if [ "$DENY_COUNT" -lt 5 ]; then
    info "建议 deny 至少 6 个有问题的扩展："
    info "  nostr, tlon, matrix, memory-lancedb, diagnostics-otel, minimax-portal-auth"
    info "（详见 src-tauri/commands/gateway.rs::sync_openclaw_config_from_manager）"
fi

# ─── 6. 实际启动测试 ────────────────────────────────────────────────────────
step "6. 用 app 的方式启动 gateway（与 Rust spawn_gateway_process 一致）"
rm -f "$GW_LOG"
info "OPENCLAW_CONFIG_PATH=$CONFIG"
info "OPENCLAW_STATE_DIR=$OPENCLAW_DIR/state"
info "OPENCLAW_GATEWAY_PORT=18789"

cd "$OPENCLAW_DIR"
nohup env \
    OPENCLAW_CONFIG_PATH="$CONFIG" \
    OPENCLAW_STATE_DIR="$OPENCLAW_DIR/state" \
    OPENCLAW_GATEWAY_TOKEN="$(python3 -c "import json; print(json.load(open('$CONFIG'))['gateway']['auth']['token'])" 2>/dev/null || echo "test-token")" \
    OPENCLAW_GATEWAY_PORT=18789 \
    OPENCLAW_NO_RESPAWN=1 \
    /usr/local/bin/node dist/entry.js gateway > "$TEST_LOG" 2>&1 &
GPID=$!
echo -e "  ${DIM}启动 PID=$GPID${RESET}"

# 等 10 秒（app 默认超时大约 65s，这里只等 10s 快速验证）
for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    if ! ps -p $GPID > /dev/null 2>&1; then
        break
    fi
    if lsof -nP -i :18789 2>/dev/null | grep -q LISTEN; then
        break
    fi
done

# ─── 7. 验证结果 ────────────────────────────────────────────────────────────
step "7. 验证启动结果"
if ps -p $GPID > /dev/null 2>&1; then
    ok "gateway 进程仍在运行 (PID=$GPID, RSS=$(ps -p $GPID -o rss= 2>/dev/null | tr -d ' '))"
else
    fail "gateway 进程已退出（看下方日志）"
fi

if lsof -nP -i :18789 2>/dev/null | grep -q LISTEN; then
    ok "端口 18789 已被监听"
    lsof -nP -i :18789 2>/dev/null | head -3
else
    fail "端口 18789 未被监听"
fi

echo ""
echo -e "${YELLOW}─── gateway 测试日志 ($TEST_LOG) ───${RESET}"
tail -20 "$TEST_LOG" 2>/dev/null | head -30

# ─── 8. 清理 ─────────────────────────────────────────────────────────────────
step "8. 清理（停止测试用 gateway）"
kill $GPID 2>/dev/null || true
sleep 2
if ps -p $GPID > /dev/null 2>&1; then
    kill -9 $GPID 2>/dev/null || true
fi
ok "测试 gateway 已停止"

# ─── 9. 总结 ─────────────────────────────────────────────────────────────────
step "9. 总结"
TOTAL=$((PASS + FAIL))
echo -e "  通过: ${GREEN}$PASS${RESET} / $TOTAL"
if [ "$FAIL" -gt 0 ]; then
    echo -e "  失败: ${RED}$FAIL${RESET} / $TOTAL"
    echo ""
    echo -e "  ${YELLOW}下一步排查建议：${RESET}"
    echo "  1. 看上面失败的步骤和提示"
    echo "  2. 如果「openclaw.json 配置校验」失败 → 在 app 的「模型配置」点选模型并保存"
    echo "  3. 如果「实际启动测试」端口未监听 → 看 $TEST_LOG 末尾"
    echo "  4. 如果进程已退出但无 log → 可能是 OPENCLAW_NO_RESPAWN 没生效"
    echo "  5. 把 $TEST_LOG 和 $GW_LOG 一起发出来分析"
    exit 1
else
    echo -e "  ${GREEN}所有检查通过！${RESET}"
    echo ""
    echo -e "  ${YELLOW}如果 app 端仍报"未在预期时间内接受连接"：${RESET}"
    echo "  - 可能是 app 的 spawn_gateway_process 与上面测试用的 env 不完全一致"
    echo "  - 让 app 实际触发启动，看 $APP_LOG 的「启动网关...」后的「Injected」+「listening」行"
    echo "  - 必要时在 app 端把整个 $APP_LOG + $GW_LOG 一起 dump 出来"
    exit 0
fi
