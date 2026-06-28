#!/usr/bin/env bash
# =============================================================================
# download-bundles.sh  —  OpenClaw-CN Manager  macOS / Linux 资源下载脚本
# 用法：
#   ./download-bundles.sh          # 下载全部
#   ./download-bundles.sh -f       # 强制重新下载
#   ./download-bundles.sh -p       # 仅下载通道插件
# =============================================================================

set -euo pipefail

FORCE=false
PLUGINS_ONLY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        -f|--force)        FORCE=true ;;
        -p|--plugins-only) PLUGINS_ONLY=true ;;
        *) echo "未知参数: $1"; exit 1 ;;
    esac
    shift
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_TAURI="$REPO_ROOT/src-tauri"

# ─── 颜色 ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'
YELLOW='\033[1;33m'; DIM='\033[2m'; RESET='\033[0m'

step()  { echo -e "\n${CYAN}[下载]${RESET} $1"; }
ok()    { echo -e "${GREEN}[  OK  ]${RESET} $1"; }
skip()  { echo -e "${DIM}[跳过]${RESET} $1"; }
fail()  { echo -e "${RED}[错误]${RESET} $1"; exit 1; }
info()  { echo -e "        ${DIM}$1${RESET}"; }

# ─── 检测架构 ────────────────────────────────────────────────────────────────
detect_arch() {
    case "$(uname -m)" in
        arm64|aarch64) echo "arm64" ;;
        *)             echo "x64"   ;;
    esac
}

ARCH="$(detect_arch)"
step "环境预检 — $(uname) $(uname -m), arch=$ARCH"

# ─── 检测 npm ────────────────────────────────────────────────────────────────
if command -v npm &>/dev/null; then
    info "Node.js $(node --version) / npm $(npm --version)"
else
    fail "npm 不可用。请安装 Node.js（建议 v18+）：https://nodejs.org"
fi

# ─── 下载辅助函数 ────────────────────────────────────────────────────────────
ensure_dir() { mkdir -p "$(dirname "$1")"; }

file_sufficient() {
    [[ -f "$1" ]] && [[ $(wc -c < "$1") -ge $2 ]]
}

fmt_size() {
    local bytes=$1
    if   (( bytes >= 1048576 )); then printf "%.1f MB"  "$(echo "scale=1; $bytes/1048576" | bc)"
    elif (( bytes >= 1024     )); then printf "%.0f KB"  "$(echo "scale=0; $bytes/1024"    | bc)"
    else                               printf "%d B"    "$bytes"
    fi
}

# Download-File <url> <dest> <label> <min_bytes>
download_file() {
    local url=$1 dest=$2 label=$3 min_bytes=$4

    if [[ "$FORCE" != true ]] && file_sufficient "$dest" "$min_bytes"; then
        skip "$label 已就绪 ($(fmt_size $(wc -c < "$dest")))"
        return 0
    fi

    ensure_dir "$dest"

    local fallback=""
    if [[ "$url" == *"npmmirror"* ]]; then
        fallback="${url//npmmirror.com\/mirrors\/node/npmjs.org\/dist}"
    fi

    for src in "$url" "$fallback"; do
        [[ -z "$src" ]] && continue
        info "尝试: $src"
        if curl -fSL --connect-timeout 30 --max-time 600 \
                -o "$dest" "$src" 2>/dev/null; then
            if file_sufficient "$dest" "$min_bytes"; then
                ok "$label 下载完成 ($(fmt_size $(wc -c < "$dest")))"
                return 0
            fi
            info "$label 文件过小，尝试下一个源"
            rm -f "$dest"
        else
            info "下载失败"
        fi
    done

    fail "$label 下载失败（所有源均不可达，请检查网络）"
}

# Npm-Pack <pkg> <dest> <label> <min_bytes>
npm_pack() {
    local pkg=$1 dest=$2 label=$3 min_bytes=$4

    if [[ "$FORCE" != true ]] && file_sufficient "$dest" "$min_bytes"; then
        skip "$label 已就绪 ($(fmt_size $(wc -c < "$dest")))"
        return 0
    fi

    ensure_dir "$dest"
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    trap "rm -rf '$tmp_dir'" EXIT

    local registries=("https://registry.npmmirror.com" "https://registry.npmjs.org")
    local done=false

    for reg in "${registries[@]}"; do
        info "npm pack $pkg @ $reg"
        if npm pack "$pkg" --registry "$reg" --pack-destination "$tmp_dir" &>/dev/null; then
            local tgz
            tgz=$(ls "$tmp_dir"/*.tgz 2>/dev/null | head -1)
            if [[ -n "$tgz" ]]; then
                local size
                size=$(wc -c < "$tgz")
                if (( size >= min_bytes )); then
                    mv "$tgz" "$dest"
                    ok "$label 下载完成 ($(fmt_size $size)) ← $reg"
                    done=true
                    break
                else
                    info "tgz 过小 ($(fmt_size $size))，尝试下一个 registry"
                    rm -f "$tgz"
                fi
            fi
        fi
    done

    if [[ "$done" != true ]]; then
        fail "$label 下载失败（所有 npm registry 均不可达）"
    fi
}

# ─── 阶段 A：内置环境包 ──────────────────────────────────────────────────────
if [[ "$PLUGINS_ONLY" != true ]]; then

    step "下载内置环境包 — arch=$ARCH"

    # A1. Node.js (tar.gz，按架构选择)
    case "$ARCH" in
        arm64)
            NODE_FILE="node-v22.14.0-darwin-arm64.tar.gz"
            NODE_URL="https://npmmirror.com/mirrors/node/v22.14.0/$NODE_FILE" ;;
        x64)
            NODE_FILE="node-v22.14.0-darwin-x64.tar.gz"
            NODE_URL="https://npmmirror.com/mirrors/node/v22.14.0/$NODE_FILE" ;;
    esac
    NODE_DEST="$SRC_TAURI/bundled-env/$NODE_FILE"
    download_file "$NODE_URL" "$NODE_DEST" "Node.js v22.14.0 (darwin-$ARCH)" $((25 * 1024 * 1024))

    # A2. MinGit — macOS 通常自带 git，仅当缺失时才拉
    if command -v git &>/dev/null; then
        skip "Git 已安装 ($(git --version))，跳过 MinGit 下载"
    else
        case "$ARCH" in
            arm64)
                GIT_FILE="mingit-2.53.0-arm64.tar.gz"
                GIT_URL="https://github.com/git-for-windows/git/releases/download/v2.53.0.windows.1/$GIT_FILE" ;;
            x64)
                GIT_FILE="mingit-2.53.0-intel.tar.gz"
                GIT_URL="https://github.com/git-for-windows/git/releases/download/v2.53.0.windows.1/$GIT_FILE" ;;
        esac
        GIT_DEST="$SRC_TAURI/bundled-env/$GIT_FILE"
        if curl -fSL --connect-timeout 10 -o /dev/null -s "$GIT_URL" 2>/dev/null; then
            download_file "$GIT_URL" "$GIT_DEST" "MinGit 2.53.0 (darwin-$ARCH)" $((10 * 1024 * 1024))
        else
            skip "MinGit tar.gz 不可达，跳过（可自行安装 git 或配置 PATH）"
        fi
    fi

    # A3. openclaw-cn
    step "下载 openclaw-cn npm 包（npm pack，可能需要 1~5 分钟）"
    OC_DEST="$SRC_TAURI/bundled-openclaw/openclaw-cn.tgz"
    npm_pack "openclaw-cn" "$OC_DEST" "openclaw-cn" $((1 * 1024 * 1024))

fi

# ─── 阶段 B：通道插件 ────────────────────────────────────────────────────────
step "下载通道插件 tgz"

# 固定遍历顺序，避免 set -u + 关联数组遍历顺序导致的 unbound 报错
CHANNEL_PLUGIN_IDS=(wxwork qq wechat_clawbot telegram)
declare -A CHANNEL_PLUGINS=(
    ["wxwork"]="@wecom/wecom-openclaw-plugin"
    ["qq"]="@sliverp/qqbot"
    ["wechat_clawbot"]="@tencent-weixin/openclaw-weixin"
    ["telegram"]="@clawdbot/telegram"
)

for plugin_id in "${CHANNEL_PLUGIN_IDS[@]}"; do
    pkg="${CHANNEL_PLUGINS[$plugin_id]:-}"
    if [[ -z "$pkg" ]]; then
        skip "插件 ${plugin_id} 缺少包名映射，跳过"
        continue
    fi
    dest="$SRC_TAURI/resources/plugins/${plugin_id}.tgz"
    npm_pack "$pkg" "$dest" "插件 ${plugin_id}" $((10 * 1024)) || \
        warn "插件 ${plugin_id} 下载失败，继续下一个"
done

# ─── 阶段 C：写入 .resource_version ──────────────────────────────────────────
step "更新 .resource_version"
CARGO_TOML="$SRC_TAURI/Cargo.toml"
VER_FILE="$SRC_TAURI/resources/data/.resource_version"

if [[ -f "$CARGO_TOML" ]]; then
    version=$(grep '^\s*version\s*=' "$CARGO_TOML" | head -1 | sed 's/.*"\([^"]*\)".*/\1/')
    if [[ -n "$version" ]]; then
        if [[ -f "$VER_FILE" ]]; then
            current=$(cat "$VER_FILE" | tr -d '[:space:]')
        else
            current=""
        fi
        if [[ "$current" != "$version" ]]; then
            echo "$version" > "$VER_FILE"
            ok ".resource_version 已更新为 v$version"
        else
            skip ".resource_version 已是最新 (v$version)"
        fi
    fi
fi

# ─── 完成 ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}============================================================${RESET}"
echo -e "${GREEN}  下载完成${RESET}"
echo -e "${GREEN}============================================================${RESET}"
echo ""
echo -e "  下一步 — 运行构建："
echo ""
echo -e "    正式打包（release）：${DIM}cd src-tauri && cargo tauri build${RESET}"
echo -e "    开发调试（debug）：  ${DIM}cd src-tauri && cargo build${RESET}"
echo ""
echo -e "    重新下载（覆盖已有文件）：${DIM}./download-bundles.sh -f${RESET}"
echo ""
