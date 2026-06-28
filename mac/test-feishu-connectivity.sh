#!/usr/bin/env bash
# =============================================================================
# test-feishu-connectivity.sh — 端到端验证飞书实例能否接入本 app 网关
#
# 流程：
#   1. 拉起本地 gateway（用当前飞书凭证启动）
#   2. 等飞书 WS 长连接 handshake 完成（client ready）
#   3. 调用飞书 Open API 验证凭证
#   4. 拉取 app 类型 + 事件订阅配置，判断能否收事件
#   5. 触发一个测试事件（如果可能），看 gateway 是否处理
#   6. 抓取 gateway 日志中飞书相关事件
#
# 退出码：0=连通；1=有问题
# =============================================================================
set -e

DATA_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data"
OPENCLAW_DIR="$DATA_DIR/openclaw-cn"
CONFIG="$OPENCLAW_DIR/openclaw.json"
CLAWDBOT_LOG=$(ls -t /tmp/clawdbot/clawdbot-*.log 2>/dev/null | head -1)
TEST_LOG=/tmp/feishu-connectivity-$$.log

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; DIM='\033[2m'; RESET='\033[0m'
step() { echo -e "\n${YELLOW}==> $1${RESET}"; }
ok()   { echo -e "  ${GREEN}✓${RESET} $1"; }
fail() { echo -e "  ${RED}✗${RESET} $1"; }
info() { echo -e "    ${DIM}$1${RESET}"; }

# ─── 0. 关闭残留进程 + reap zombie ────────────────────────────────────────
step "0. 准备（关闭残留 + reap zombie）"
pkill -9 -f "clawdbot-gateway\|openclaw-cn" 2>/dev/null || true
sleep 2
# reap zombie（如果有）
for zpid in $(ps -A -o pid=,stat= 2>/dev/null | awk '$2~/^Z/ {print $1}'); do
    ppid=$(ps -o ppid= -p "$zpid" 2>/dev/null | tr -d ' ')
    [ -n "$ppid" ] && kill -s SIGCHLD "$ppid" 2>/dev/null || true
done
# 清陈旧锁
rm -f /tmp/openclaw/gateway.*.lock 2>/dev/null || true
ok "环境已清理"

# ─── 1. 读取飞书凭证 ──────────────────────────────────────────────────────
step "1. 读取飞书凭证（从 openclaw.json）"
read -r APP_ID APP_SECRET <<< "$(python3 -c "
import json
d = json.load(open('$CONFIG'))
fs = d['channels']['feishu']
acc = list(fs['accounts'].values())[0]
print(acc['appId'], acc['appSecret'])
")"
if [ -z "$APP_ID" ] || [ -z "$APP_SECRET" ]; then
    fail "无法从 openclaw.json 读取 appId/appSecret"
    exit 1
fi
ok "App ID: ${APP_ID:0:12}..."
ok "App Secret: ${APP_SECRET:0:6}..."

# ─── 2. 调用飞书 Open API 验证凭证 + app 类型 ────────────────────────
step "2. 飞书 Open API：获取 tenant_access_token"
TOKEN_RESP=$(curl -s -m 10 -X POST "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal" \
    -H "Content-Type: application/json" \
    -d "{\"app_id\":\"$APP_ID\",\"app_secret\":\"$APP_SECRET\"}" 2>&1)
TOKEN_CODE=$(echo "$TOKEN_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('code',-1))" 2>/dev/null || echo -1)
if [ "$TOKEN_CODE" = "0" ]; then
    ok "tenant_access_token 获取成功 (code=0)"
    TENANT_TOKEN=$(echo "$TOKEN_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin)['tenant_access_token'])")
    EXPIRE=$(echo "$TOKEN_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin)['expire'])")
    info "expire: ${EXPIRE}s"
else
    fail "tenant_access_token 失败: $(echo "$TOKEN_RESP" | head -c 200)"
    echo "$TOKEN_RESP" | head -c 500
    exit 1
fi

# ─── 3. 拉取 app 基础信息（判断 app 类型）────────────────────────────────
step "3. 飞书 API：拉取 app 基础信息（判断应用类型）"
APP_INFO=$(curl -s -m 10 "https://open.feishu.cn/open-apis/application/v6/applications/$APP_ID" \
    -H "Authorization: Bearer $TENANT_TOKEN" 2>&1)
APP_TYPE_CODE=$(echo "$APP_INFO" | python3 -c "import json,sys; print(json.load(sys.stdin).get('code',-1))" 2>/dev/null || echo -1)
if [ "$APP_TYPE_CODE" = "0" ]; then
    ok "app 信息获取成功"
    echo "$APP_INFO" | python3 -c "
import json, sys
d = json.load(sys.stdin)['data']['app']
print(f'  app_id:        {d.get(\"app_id\")}')
print(f'  app_name:      {d.get(\"app_name\")}')
print(f'  app_type:      {d.get(\"app_type\")}  (0=自建, 1=商店, 2=企业)')
print(f'  status:        {d.get(\"status\")}  (0=未发布, 1=已发布, 2=已下架)')
print(f'  scene_tag:     {d.get(\"scene_tag\")}')
print(f'  primary_locale:{d.get(\"primary_locale\")}')
"
else
    fail "app 信息获取失败: $(echo "$APP_INFO" | head -c 200)"
fi

# ─── 4. 拉取事件订阅配置（判断能否收长连接事件）──────────────────────
step "4. 飞书 API：拉取事件订阅配置"
EVENT_LIST=$(curl -s -m 10 "https://open.feishu.cn/open-apis/event/v1/subscriptions" \
    -H "Authorization: Bearer $TENANT_TOKEN" 2>&1)
EVENT_CODE=$(echo "$EVENT_LIST" | python3 -c "import json,sys; print(json.load(sys.stdin).get('code',-1))" 2>/dev/null || echo -1)
if [ "$EVENT_CODE" = "0" ]; then
    ok "事件订阅列表获取成功"
    echo "$EVENT_LIST" | python3 -c "
import json, sys
d = json.load(sys.stdin)
items = d.get('data', {}).get('items', [])
print(f'  订阅数: {len(items)}')
for sub in items:
    print(f'  - {sub.get(\"sub_event_type\")} → {sub.get(\"request_url\", sub.get(\"type\"))[:60]}')
" 2>&1
else
    fail "事件订阅列表获取失败 (code=$EVENT_CODE): $(echo $EVENT_LIST | head -c 200)"
fi

# ─── 5. 启动网关（含飞书长连接）────────────────────────────────────────
step "5. 启动网关"
GATEWAY_TOKEN=$(python3 -c "import json; print(json.load(open('$CONFIG'))['gateway']['auth']['token'])")
cd "$OPENCLAW_DIR"
nohup env \
    OPENCLAW_CONFIG_PATH="$CONFIG" \
    OPENCLAW_STATE_DIR="$OPENCLAW_DIR/state" \
    OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN" \
    OPENCLAW_GATEWAY_PORT=18789 \
    OPENCLAW_NO_RESPAWN=1 \
    /usr/local/bin/node dist/entry.js gateway > "$TEST_LOG" 2>&1 &
GPID=$!
info "PID=$GPID"

# 等 15 秒（飞书 WS handshake 可能稍慢）
LISTENING=false
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 1
    if lsof -nP -i :18789 2>/dev/null | grep -q LISTEN; then
        ok "${i}s: 端口 18789 已监听"
        LISTENING=true
        break
    fi
done
[ "$LISTENING" = "true" ] || fail "15 秒后端口仍未监听"

# ─── 6. 验证飞书长连接 handshake ───────────────────────────────────────
step "6. 飞书长连接 handshake 验证"
sleep 3  # 让 feishu-monitor 跑完
if [ -n "$CLAWDBOT_LOG" ]; then
    # 用 Python 找 feishu 关键事件
    FEISHU_INIT=$(python3 -c "
import json
events = []
with open('$CLAWDBOT_LOG') as f:
    for line in f:
        try:
            d = json.loads(line.strip())
            p = d.get('_meta', {}).get('path', {}).get('fileName', '?')
            if 'monitor' not in p: continue
            if 'feishu-monitor' not in str(d.get('0', '')): continue
            ts = d.get('time', '?')
            msg1 = d.get('1', '')
            if msg1: events.append((ts, str(msg1)[:120]))
        except: pass
print(f'feishu-monitor 事件总数: {len(events)}')
for ts, m in events[-15:]:
    print(f'  {ts} | {m[:100]}')
" 2>&1)
    echo "$FEISHU_INIT"

    if echo "$FEISHU_INIT" | grep -q "Feishu WebSocket connection established"; then
        ok "Feishu WebSocket connection established ✓"
    else
        fail "未发现 'Feishu WebSocket connection established'"
    fi

    if echo "$FEISHU_INIT" | grep -q "ws client ready"; then
        ok "ws client ready ✓"
    fi

    if echo "$FEISHU_INIT" | grep -q "self-build & Feishu app"; then
        warn_msg=$(echo "$FEISHU_INIT" | grep "self-build" | head -1)
        info "⚠️ 长连接仅自建应用可用：$warn_msg"
    fi
else
    fail "无法读取 /tmp/clawdbot 日志"
fi

# ─── 7. 触发测试事件（用 Open API 发消息）──────────────────────────────
step "7. 触发测试事件（用 chat API 发消息）"
if [ -n "$TENANT_TOKEN" ] && [ "$LISTENING" = "true" ]; then
    # 拿当前 account 对应的 open_id（从 openclaw.json 读）
    OPEN_ID=$(python3 -c "
import json
d = json.load(open('$CONFIG'))
fs = d['channels']['feishu']
acc = list(fs['accounts'].values())[0]
print(acc.get('allowFrom', ''))
" 2>/dev/null)
    info "白名单 open_id: $OPEN_ID"

    if [ -n "$OPEN_ID" ]; then
        info "调用 im/v1/messages 发送测试消息（target open_id）..."
        MSG_RESP=$(curl -s -m 10 -X POST "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=open_id" \
            -H "Authorization: Bearer $TENANT_TOKEN" \
            -H "Content-Type: application/json" \
            -d "{\"receive_id\":\"$OPEN_ID\",\"msg_type\":\"text\",\"content\":\"{\\\"text\\\":\\\"[test] gateway connectivity check from app shell $(date +%s)\\\"}\"}" 2>&1)
        MSG_CODE=$(echo "$MSG_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('code',-1))" 2>/dev/null || echo -1)
        if [ "$MSG_CODE" = "0" ]; then
            ok "测试消息已发出"
            info "  msg_id: $(echo "$MSG_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('msg',{}).get('message_id', '?'))" 2>/dev/null)"
        else
            info "测试消息发送失败（code=$MSG_CODE），可能是 app 没有 im:message 权限"
            echo "$MSG_RESP" | head -c 200
        fi
    else
        info "openclaw.json 中没有 allowFrom，跳过主动发消息"
        info "（需要先在飞书后台给该 open_id 发送过消息，绑定时才会记录）"
    fi
fi

# ─── 8. 等待 5 秒看 gateway 是否收到事件 ───────────────────────────────
step "8. 等待 5 秒观察 gateway 是否收到事件"
sleep 5
if [ -n "$CLAWDBOT_LOG" ]; then
    echo "=== 最新 20 条飞书/agent/usage 事件 ==="
    tail -500 "$CLAWDBOT_LOG" | python3 -c "
import json, sys
events = []
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        p = d.get('_meta', {}).get('path', {}).get('fileName', '?')
        ts = d.get('time', '?')
        msg0 = d.get('0', '')
        msg1 = d.get('1', '')
        text = str(msg1)[:150] if msg1 else str(msg0)[:150]
        if any(k in str(p).lower() + str(msg0).lower() + str(msg1).lower() for k in ['feishu', 'lark', 'agent', 'usage', 'channel', 'inbound', 'event', 'ws', 'message']):
            events.append((ts, p, text))
    except: pass
for ts, p, t in events[-20:]:
    print(f'  {ts} [{p:25s}] {t[:120]}')
"
fi

# ─── 9. 收尾 + 总结 ─────────────────────────────────────────────────────
step "9. 总结"
if [ "$LISTENING" = "true" ]; then
    ok "网关 listening on 18789 ✓"
fi
if [ -n "$CLAWDBOT_LOG" ] && grep -q "Feishu WebSocket connection established" "$CLAWDBOT_LOG" 2>/dev/null; then
    ok "飞书长连接已建立 ✓"
    info "（能否收消息看飞书后台 app 类型 + 订阅方式）"
else
    fail "飞书长连接未建立"
fi

# 不停止 gateway，让用户自己决定
echo ""
echo -e "${CYAN}────────────────────────────────────────${RESET}"
echo -e "${CYAN}网关仍在运行（PID=$GPID）。如需停止：kill $GPID${RESET}"
echo -e "${CYAN}────────────────────────────────────────${RESET}"
