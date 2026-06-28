#!/usr/bin/env bash
# =============================================================================
# mac/start.sh — 快泛 Claw macOS 启动脚本
# 用法：
#   ./start.sh                  # 启动 .app（前台）
#   ./start.sh --background     # 启动并立即返回
# =============================================================================

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="快泛claw"
APP="$DIR/${APP_NAME}.app"

if [[ ! -d "$APP" ]]; then
    echo "[错误] 未找到 $APP"
    echo "请先在仓库根目录执行：./build-mac.sh"
    exit 1
fi

case "${1:-}" in
    --background|-b)
        open "$APP"
        echo "已在后台启动 ${APP_NAME}"
        ;;
    --help|-h)
        echo "用法: $0 [--background|--help]"
        echo "  --background, -b   后台启动（不阻塞当前 shell）"
        echo "  --help, -h         显示帮助"
        ;;
    "")
        # 默认前台启动（open -W 会等待应用退出）
        exec open -W "$APP"
        ;;
    *)
        echo "[错误] 未知参数: $1"
        exit 1
        ;;
esac
