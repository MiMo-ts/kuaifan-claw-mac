#!/bin/bash
# 测试插件是否可正常加载到网关
# 用法: ./test-plugins.sh

set -e

OPENCLAW_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data/openclaw-cn"
LOG_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data/logs"
GATEWAY_LOG="$LOG_DIR/openclaw-gateway.log"
TEST_LOG="/tmp/plugin-test-$(date +%s).log"

echo "=========================================="
echo "  插件加载测试脚本"
echo "=========================================="
echo ""

# 1. 检查 openclaw-cn 目录
echo "[1/6] 检查 openclaw-cn 目录..."
if [ ! -d "$OPENCLAW_DIR" ]; then
    echo "❌ openclaw-cn 目录不存在: $OPENCLAW_DIR"
    exit 1
fi
echo "✅ openclaw-cn 目录存在"

# 2. 检查 plugin-sdk 子路径模块
echo ""
echo "[2/6] 检查 plugin-sdk 子路径模块..."
SDK_DIR="$OPENCLAW_DIR/dist/plugin-sdk"
MISSING=0

for module in "channel-config-schema.js" "runtime-store.js" "plugin-entry.js" "package.json"; do
    if [ -f "$SDK_DIR/$module" ]; then
        echo "  ✅ $module"
    else
        echo "  ❌ $module (缺失)"
        MISSING=1
    fi
done

if [ $MISSING -eq 1 ]; then
    echo ""
    echo "❌ 缺少必要的子路径模块，插件无法加载"
    exit 1
fi

# 3. 检查插件目录
echo ""
echo "[3/6] 检查插件目录..."
PLUGIN_DIR="$HOME/Library/Application Support/OpenClaw-CN Manager/data/plugins"

for plugin in "wechat_clawbot" "wecom" "qq"; do
    if [ -d "$PLUGIN_DIR/$plugin" ]; then
        echo "  ✅ $plugin (已安装)"
        # 检查 node_modules
        if [ -d "$PLUGIN_DIR/$plugin/node_modules" ]; then
            echo "    ✅ node_modules 已安装"
        else
            echo "    ⚠️  node_modules 未安装"
        fi
    else
        echo "  ⚠️  $plugin (未安装)"
    fi
done

# 4. 检查 openclaw.json 配置
echo ""
echo "[4/6] 检查 openclaw.json 插件配置..."
CONFIG="$OPENCLAW_DIR/openclaw.json"

if [ ! -f "$CONFIG" ]; then
    echo "❌ openclaw.json 不存在"
    exit 1
fi

# 检查 deny 列表
DENY_WEIXIN=$(python3 -c "import json; d=json.load(open('$CONFIG')); print('openclaw-weixin' in d.get('plugins',{}).get('deny',[]))" 2>/dev/null)
DENY_WECOM=$(python3 -c "import json; d=json.load(open('$CONFIG')); print('wecom-openclaw-plugin' in d.get('plugins',{}).get('deny',[]))" 2>/dev/null)

if [ "$DENY_WEIXIN" = "True" ]; then
    echo "  ❌ openclaw-weixin 在 deny 列表中"
else
    echo "  ✅ openclaw-weixin 未被禁用"
fi

if [ "$DENY_WECOM" = "True" ]; then
    echo "  ❌ wecom-openclaw-plugin 在 deny 列表中"
else
    echo "  ✅ wecom-openclaw-plugin 未被禁用"
fi

# 检查 entries
ENTRY_WEIXIN=$(python3 -c "import json; d=json.load(open('$CONFIG')); e=d.get('plugins',{}).get('entries',{}); print(e.get('openclaw-weixin',{}).get('enabled', False))" 2>/dev/null)
ENTRY_WECOM=$(python3 -c "import json; d=json.load(open('$CONFIG')); e=d.get('plugins',{}).get('entries',{}); print(e.get('wecom-openclaw-plugin',{}).get('enabled', False))" 2>/dev/null)

if [ "$ENTRY_WEIXIN" = "True" ]; then
    echo "  ✅ openclaw-weixin 已启用"
else
    echo "  ⚠️  openclaw-weixin 未启用"
fi

if [ "$ENTRY_WECOM" = "True" ]; then
    echo "  ✅ wecom-openclaw-plugin 已启用"
else
    echo "  ⚠️  wecom-openclaw-plugin 未启用"
fi

# 5. 停止现有网关
echo ""
echo "[5/6] 停止现有网关..."
pkill -f "entry.js gateway" 2>/dev/null || true
pkill -f "clawdbot-gateway" 2>/dev/null || true
sleep 2
echo "✅ 网关已停止"

# 5.5 检查端口占用
echo ""
echo "[5.5/6] 检查端口 18789 占用情况..."
PORT_USER=$(lsof -ti:18789 2>/dev/null | head -1)
if [ -n "$PORT_USER" ]; then
    echo "⚠️  端口 18789 被占用 (PID: $PORT_USER)"
    echo "   尝试终止占用进程..."
    kill -9 $PORT_USER 2>/dev/null || true
    sleep 1
fi
echo "✅ 端口 18789 已释放"

# 6. 启动网关并测试
echo ""
echo "[6/6] 启动网关并监控插件加载..."
echo "启动网关..."

cd "$OPENCLAW_DIR"
node dist/entry.js gateway --port 18789 > "$TEST_LOG" 2>&1 &
GW_PID=$!

echo "网关 PID: $GW_PID"
echo "等待网关启动 (15秒)..."
sleep 15

# 检查插件加载结果
echo ""
echo "=========================================="
echo "  插件加载结果"
echo "=========================================="

# 检查 openclaw-weixin
if grep -q "openclaw-weixin.*failed\|openclaw-weixin.*error" "$TEST_LOG" 2>/dev/null; then
    echo "❌ openclaw-weixin 加载失败:"
    grep "openclaw-weixin" "$TEST_LOG" | head -3
elif grep -q "openclaw-weixin\|weixin.*register\|weixin.*channel" "$TEST_LOG" 2>/dev/null; then
    echo "✅ openclaw-weixin 加载成功"
else
    echo "⚠️  openclaw-weixin 未检测到加载记录"
fi

# 检查 wecom-openclaw-plugin
if grep -q "wecom-openclaw-plugin.*failed\|wecom-openclaw-plugin.*error" "$TEST_LOG" 2>/dev/null; then
    echo "❌ wecom-openclaw-plugin 加载失败:"
    grep "wecom-openclaw-plugin" "$TEST_LOG" | head -3
elif grep -q "wecom-openclaw-plugin\|wecom.*register\|wecom.*channel" "$TEST_LOG" 2>/dev/null; then
    echo "✅ wecom-openclaw-plugin 加载成功"
else
    echo "⚠️  wecom-openclaw-plugin 未检测到加载记录"
fi

# 检查 feishu
if grep -q "feishu.*failed\|feishu.*error" "$TEST_LOG" 2>/dev/null; then
    echo "❌ feishu 加载失败:"
    grep "feishu" "$TEST_LOG" | grep -i "error\|failed" | head -3
elif grep -q "feishu.*register\|feishu.*plugin\|starting Feishu provider" "$TEST_LOG" 2>/dev/null; then
    echo "✅ feishu 加载成功"
else
    echo "⚠️  feishu 未检测到加载记录"
fi

# 检查网关是否正常监听
if grep -q "listening on ws://127.0.0.1:18789" "$TEST_LOG" 2>/dev/null; then
    echo ""
    echo "✅ 网关已正常监听 127.0.0.1:18789"
else
    echo ""
    echo "❌ 网关未正常监听"
fi

# 检查是否有插件错误
ERROR_COUNT=$(grep -c "failed to load plugin\|plugin.*error" "$TEST_LOG" 2>/dev/null)
ERROR_COUNT=${ERROR_COUNT:-0}
if [ "$ERROR_COUNT" -gt 0 ]; then
    echo ""
    echo "⚠️  发现 $ERROR_COUNT 个插件错误:"
    grep "failed to load plugin\|plugin.*error" "$TEST_LOG" | head -5
fi

# 清理
echo ""
echo "=========================================="
echo "  测试日志: $TEST_LOG"
echo "=========================================="

# 停止测试网关
kill $GW_PID 2>/dev/null || true
echo "测试网关已停止"
echo ""
echo "测试完成!"
