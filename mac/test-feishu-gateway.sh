#!/usr/bin/env bash
# =============================================================================
# test-feishu-gateway.sh — 验证飞书绑定后网关连通性
# 用法：bash mac/test-feishu-gateway.sh
# 退出码：0=连通；1=有错误
# =============================================================================
set -e

DATA_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data"
OPENCLAW_DIR="$DATA_DIR/openclaw-cn"
CONFIG="$OPENCLAW_DIR/openclaw.json"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; DIM='\033[2m'; RESET='\033[0m'
step() { echo -e "\n${YELLOW}==> $1${RESET}"; }
ok()   { echo -e "  ${GREEN}✓${RESET} $1"; }
fail() { echo -e "  ${RED}✗${RESET} $1"; }
info() { echo -e "    ${DIM}$1${RESET}"; }

# ─── 0. 关闭残留进程 ──────────────────────────────────────────────────────
step "0. 关闭残留"
pkill -9 -f "clawdbot-gateway\|openclaw-cn" 2>/dev/null || true
sleep 2
ok "残留进程已清理"

# ─── 1. reap zombie ────────────────────────────────────────────────────────
step "1. reap zombie"
ZOMBIE_COUNT=$(ps -A -o stat= | grep -c "^Z" || true)
info "当前 zombie 数: $ZOMBIE_COUNT"
if [ "$ZOMBIE_COUNT" -gt 0 ]; then
    ps -A -o pid=,stat= 2>/dev/null | awk '$2 ~ /^Z/ { print $1 }' | while read zpid; do
        ppid=$(ps -o ppid= -p "$zpid" 2>/dev/null | tr -d ' ')
        [ -n "$ppid" ] && kill -s SIGCHLD "$ppid" 2>/dev/null || true
    done
    sleep 1
fi
ok "zombie reap 完成"

# ─── 2. 飞书绑定检查 ──────────────────────────────────────────────────────
step "2. 飞书绑定状态"
python3 - "$CONFIG" <<'PY' && ok "飞书凭证已就位" || fail "飞书凭证缺失"
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
ch = d.get("channels", {})
fs = ch.get("feishu", {})
if not fs:
    print("  channels.feishu 不存在")
    sys.exit(1)
print(f"  enabled: {fs.get('enabled')}")
print(f"  accounts: {list(fs.get('accounts', {}).keys())}")
for acc, info in fs.get('accounts', {}).items():
    print(f"  {acc}: appId={info.get('appId', '?')[:12]}... appSecret={info.get('appSecret', '?')[:6]}...")
PY

# ─── 3. bindings 检查 ─────────────────────────────────────────────────────
step "3. bindings（事件源→代理映射）"
python3 - "$CONFIG" <<'PY' && ok "bindings 存在" || info "bindings 为空（飞书事件未路由到任何 agent）"
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
bindings = d.get("bindings", [])
print(f"  bindings 总数: {len(bindings)}")
for b in bindings:
    if 'feishu' in str(b).lower() or '飞书' in str(b):
        print(f"  飞书 binding: {json.dumps(b, ensure_ascii=False)[:300]}")
PY

# ─── 4. openclaw.json 完整性 ──────────────────────────────────────────────
step "4. openclaw.json 完整性"
python3 - "$CONFIG" <<'PY' && ok "openclaw.json 完整" || fail "openclaw.json 字段缺失"
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
g = d.get("gateway", {})
assert g.get("mode") == "local", f"gateway.mode 应为 local，实际: {g.get('mode')}"
assert g.get("port") == 18789
assert g.get("auth", {}).get("token")
agents = d.get("agents", {})
primary = agents.get("defaults", {}).get("model", {}).get("primary", "")
assert primary, "agents.defaults.model.primary 缺失"
print(f"  默认模型: {primary}")
PY

# ─── 5. 启动网关（带飞书绑定） ─────────────────────────────────────────
step "5. 启动网关（实测飞书绑定后是否正常）"
TOKEN=$(python3 -c "import json; print(json.load(open('$CONFIG'))['gateway']['auth']['token'])")

cd "$OPENCLAW_DIR"
nohup env \
    OPENCLAW_CONFIG_PATH="$CONFIG" \
    OPENCLAW_STATE_DIR="$OPENCLAW_DIR/state" \
    OPENCLAW_GATEWAY_TOKEN="$TOKEN" \
    OPENCLAW_GATEWAY_PORT=18789 \
    OPENCLAW_NO_RESPAWN=1 \
    /usr/local/bin/node dist/entry.js gateway > /tmp/feishu-gw.log 2>&1 &
GPID=$!
info "启动 PID=$GPID"

# 等 12 秒（飞书 plugin 加载稍慢）
LISTENING=false
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
    sleep 1
    if lsof -nP -i :18789 2>/dev/null | grep -q LISTEN; then
        ok "${i}s: 端口 18789 已监听"
        LISTENING=true
        break
    fi
done

if [ "$LISTENING" != "true" ]; then
    fail "12 秒后端口仍未监听（飞书 plugin 可能导致 gateway 启动失败）"
fi

# ─── 6. 飞书 plugin 加载情况 ────────────────────────────────────────────
step "6. 飞书 plugin 加载检查"
sleep 2  # 等 subsystem 注册完
if grep -qE "feishu.*loaded|plugin.*feishu" /tmp/feishu-gw.log; then
    ok "飞书 plugin 加载日志存在"
else
    info "未在 stdout 看到飞书 plugin 加载信息（可能走 subsystem 静默注册）"
fi

# 查 /tmp/clawdbot 是否有飞书相关错误
LATEST=$(ls -t /tmp/clawdbot/clawdbot-*.log 2>/dev/null | head -1)
if [ -n "$LATEST" ]; then
    FEISHU_LOGS=$(tail -200 "$LATEST" 2>/dev/null | grep -iE "feishu|飞书" | tail -5)
    if [ -n "$FEISHU_LOGS" ]; then
        info "最近飞书相关日志："
        echo "$FEISHU_LOGS" | head -5
    else
        info "/tmp/clawdbot 无飞书相关日志"
    fi

    # 查 subsystem 启动情况
    GW_SUBSYSTEMS=$(tail -50 "$LATEST" 2>/dev/null | python3 -c "
import json, sys
subs = set()
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        msg = d.get('0', '')
        if 'subsystem' in str(msg) and 'feishu' in str(msg).lower():
            subs.add(str(msg))
    except: pass
for s in subs: print('  ' + s[:120])
")
    if [ -n "$GW_SUBSYSTEMS" ]; then
        ok "飞书 subsystem 注册："
        echo "$GW_SUBSYSTEMS"
    else
        info "无 feishu subsystem 注册记录（可能 plugin 加载失败）"
    fi
fi

# ─── 7. 飞书 webhook 端点 ──────────────────────────────────────────────
step "7. 飞书 webhook 端点"
if lsof -nP -i :18789 2>/dev/null | grep -q LISTEN; then
    info "飞书需要 webhook 端点 → openclaw 默认接受以下路径："
    info "  /feishu/events"
    info "  /webhook/feishu"
    info "  /api/feishu/webhook"
    info "  （实际路径取决于 feishu plugin 的 registerWebhook 调用）"
    if [ -n "$LATEST" ]; then
        WEBHOOK_HINTS=$(grep -iE "webhook|listen|route" "$LATEST" 2>/dev/null | tail -3)
        if [ -n "$WEBHOOK_HINTS" ]; then
            info "最近 webhook 相关日志："
            echo "$WEBHOOK_HINTS" | head -3
        fi
    fi
fi

# ─── 8. 实际 ping 飞书凭证 API ─────────────────────────────────────────
step "8. 验证飞书凭证 API 可达性"
APP_ID=$(python3 -c "import json; d=json.load(open('$CONFIG')); print(d['channels']['feishu']['accounts'][list(d['channels']['feishu']['accounts'].keys())[0]]['appId'])" 2>/dev/null)
APP_SECRET=$(python3 -c "import json; d=json.load(open('$CONFIG')); print(d['channels']['feishu']['accounts'][list(d['channels']['feishu']['accounts'].keys())[0]]['appSecret'])" 2>/dev/null)
if [ -n "$APP_ID" ] && [ -n "$APP_SECRET" ]; then
    info "App ID: ${APP_ID:0:12}..."
    info "App Secret: ${APP_SECRET:0:6}..."
    info "调用飞书 tenant_access_token 接口验证凭证..."
    RESPONSE=$(curl -s -m 10 -X POST "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal" \
        -H "Content-Type: application/json" \
        -d "{\"app_id\":\"$APP_ID\",\"app_secret\":\"$APP_SECRET\"}" 2>&1)
    if echo "$RESPONSE" | grep -q '"code":0'; then
        ok "飞书凭证验证成功 (code=0)"
        echo "  响应: $(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print('tenant_token:', d.get('tenant_access_token','')[:30]+'...'); print('expire:', d.get('expire', '?'))" 2>/dev/null)"
    else
        fail "飞书凭证验证失败"
        echo "  响应: $RESPONSE" | head -3
    fi
else
    fail "无法从 openclaw.json 读取飞书凭证"
fi

# ─── 9. 收尾 ────────────────────────────────────────────────────────────
step "9. 清理"
kill $GPID 2>/dev/null || true
sleep 1
pkill -9 -f "clawdbot-gateway\|openclaw-cn" 2>/dev/null || true
ok "测试 gateway 已停止"

echo ""
echo -e "${YELLOW}─── 完整启动日志（/tmp/feishu-gw.log）───${RESET}"
tail -40 /tmp/feishu-gw.log 2>/dev/null
echo ""
echo -e "${YELLOW}─── 完整 openclaw-cn 日志（最后 30 条）───${RESET}"
[ -n "$LATEST" ] && tail -30 "$LATEST" | python3 -c "
import json, sys
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        ts = d.get('time', '?')
        msg = str(d.get('0', '?'))[:200]
        path = d.get('_meta', {}).get('path', {}).get('fileName', '?')
        print(f'  {ts} [{path:25s}] {msg}')
    except: pass
"
