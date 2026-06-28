#!/usr/bin/env bash
# =============================================================================
# build-mac.sh — 快泛 Claw 原生 macOS 构建脚本
# -----------------------------------------------------------------------------
# 适用：macOS 主机（Apple Silicon 或 Intel）
# 产物：mac/bin/快泛 claw.app（universal arm64+x86_64） + mac/dmg/*.dmg
# 签名：通过 APPLE_SIGNING_IDENTITY 环境变量注入；未设置则产出未签名 .app
# =============================================================================

set -euo pipefail

# ─── 配置 ────────────────────────────────────────────────────────────────────
APP_NAME="快泛claw"                              # .app bundle 名称（与 productName 一致）
PRODUCT_BIN="快泛claw"                            # .app/Contents/MacOS/ 下的可执行文件名
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI="$REPO_ROOT/src-tauri"
WEB_DIR="$REPO_ROOT/web"
OUT_DIR="$REPO_ROOT/mac"
BIN_DIR="$OUT_DIR/bin"
DMG_DIR="$OUT_DIR/dmg"
ICON_PNG="$SRC_TAURI/icons/icon.png"
ICON_ICNS="$SRC_TAURI/icons/icon.icns"

# ─── 颜色 ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'
YELLOW='\033[1;33m'; DIM='\033[2m'; RESET='\033[0m'

step()  { echo -e "\n${CYAN}==>${RESET} $1"; }
ok()    { echo -e "${GREEN}OK${RESET}   $1"; }
warn()  { echo -e "${YELLOW}WARN${RESET} $1"; }
fail()  { echo -e "${RED}FAIL${RESET} $1"; exit 1; }

# ─── 0. 主机预检 ─────────────────────────────────────────────────────────────
step "0. 主机预检"

if [[ "$(uname -s)" != "Darwin" ]]; then
    fail "此脚本必须在 macOS 上运行。当前系统: $(uname -s)"
fi

if ! command -v xcode-select &>/dev/null; then
    fail "未检测到 xcode-select。请安装 Xcode Command Line Tools：xcode-select --install"
fi

CLT_PATH="$(xcode-select -p 2>/dev/null || true)"
if [[ -z "$CLT_PATH" || "$CLT_PATH" == *"error"* ]]; then
    fail "Xcode Command Line Tools 未配置。运行：xcode-select --install"
fi
ok "Xcode CLT: $CLT_PATH"

# rust / cargo
if ! command -v cargo &>/dev/null; then
    fail "未检测到 cargo。请安装 Rust：https://rustup.rs"
fi
ok "Rust: $(rustc --version) / Cargo: $(cargo --version)"

# node / npm
if ! command -v npm &>/dev/null; then
    fail "未检测到 npm。请安装 Node.js 18+：https://nodejs.org"
fi
ok "Node: $(node --version) / npm: $(npm --version)"

# lipo (用于 universal binary)
if ! command -v lipo &>/dev/null; then
    fail "未检测到 lipo。lipo 来自 Apple 工具链，请确认 Xcode CLT 已正确安装"
fi
ok "lipo: $(lipo -info 2>&1 | head -1 || echo available)"

# ─── 1. Rust 目标 ────────────────────────────────────────────────────────────
step "1. 安装 Rust 目标 (aarch64-apple-darwin, x86_64-apple-darwin)"
rustup target add aarch64-apple-darwin x86_64-apple-darwin
ok "目标已就绪"

# ─── 2. 内置包下载 ──────────────────────────────────────────────────────────
step "2. 检查/下载内置 Node.js / MinGit 包"

if [[ ! -d "$SRC_TAURI/bundled-env" ]]; then
    mkdir -p "$SRC_TAURI/bundled-env"
fi

NEED_DOWNLOAD=false
ARCH="$(uname -m)"
case "$ARCH" in
    arm64|aarch64) NODE_FILE="node-v22.14.0-darwin-arm64.tar.gz" ;;
    *)             NODE_FILE="node-v22.14.0-darwin-x64.tar.gz"   ;;
esac

if [[ ! -f "$SRC_TAURI/bundled-env/$NODE_FILE" ]]; then
    NEED_DOWNLOAD=true
fi

if [[ "$NEED_DOWNLOAD" == true ]]; then
    warn "检测到内置包缺失，运行 download-bundles.sh ..."
    chmod +x "$SRC_TAURI/scripts/download-bundles.sh"
    "$SRC_TAURI/scripts/download-bundles.sh"
else
    ok "内置包已就绪: $NODE_FILE"
fi

# ─── 3. icon.icns 生成 ──────────────────────────────────────────────────────
step "3. 检查/生成 icon.icns"

if [[ ! -f "$ICON_ICNS" ]]; then
    if [[ ! -f "$ICON_PNG" ]]; then
        fail "未找到 $ICON_PNG，无法生成 icon.icns"
    fi
    warn "icon.icns 缺失，使用 tauri icon 生成..."
    (cd "$WEB_DIR" && npm run tauri -- icon "$ICON_PNG")
    if [[ ! -f "$ICON_ICNS" ]]; then
        fail "icon.icns 生成失败，请手动执行：cd $WEB_DIR && npx tauri icon $ICON_PNG"
    fi
    ok "icon.icns 已生成"
else
    ok "icon.icns 已存在"
fi

# ─── 4. 前端构建 ─────────────────────────────────────────────────────────────
step "4. 构建前端 (web/dist)"
if [[ ! -d "$WEB_DIR/node_modules" ]]; then
    (cd "$WEB_DIR" && npm install)
fi
(cd "$WEB_DIR" && npm run build)
ok "前端已构建"

# ─── 5. Tauri 通用二进制构建 ────────────────────────────────────────────────
step "5. cargo tauri build --target universal-apple-darwin"

cd "$SRC_TAURI"

# 5a. 如果设置了 APPLE_SIGNING_IDENTITY，临时注入 tauri.conf.json
TAURI_CONF="$SRC_TAURI/tauri.conf.json"
TAURI_CONF_BAK=""
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] && command -v jq &>/dev/null; then
    warn "检测到 APPLE_SIGNING_IDENTITY=$APPLE_SIGNING_IDENTITY"
    TAURI_CONF_BAK="$(mktemp -t tauri.conf.json.XXXXXX)"
    cp "$TAURI_CONF" "$TAURI_CONF_BAK"
    jq --arg id "$APPLE_SIGNING_IDENTITY" \
       '.bundle.macOS.signingIdentity = $id' \
       "$TAURI_CONF" > "$TAURI_CONF.tmp" && mv "$TAURI_CONF.tmp" "$TAURI_CONF"
    ok "已注入 signingIdentity 到 tauri.conf.json（构建后会自动还原）"
fi

# 5b. 通用二进制（arm64 + x86_64）由 Tauri CLI 内部 lipo 合成
cargo tauri build --target universal-apple-darwin
ok "universal-apple-darwin 构建完成"

# 5c. 还原 tauri.conf.json
if [[ -n "$TAURI_CONF_BAK" && -f "$TAURI_CONF_BAK" ]]; then
    mv "$TAURI_CONF_BAK" "$TAURI_CONF"
    rm -f "$TAURI_CONF.tmp"
    ok "已还原 tauri.conf.json"
fi

# ─── 6. 拷贝产物 ─────────────────────────────────────────────────────────────
step "6. 拷贝产物到 $BIN_DIR / $DMG_DIR"

mkdir -p "$BIN_DIR" "$DMG_DIR"

# .app bundle: src-tauri/target/universal-apple-darwin/release/bundle/macos/<APP_NAME>.app
APP_SRC="$SRC_TAURI/target/universal-apple-darwin/release/bundle/macos/${APP_NAME}.app"
if [[ ! -d "$APP_SRC" ]]; then
    fail "未找到 .app: $APP_SRC"
fi

# 清理旧产物
rm -rf "$BIN_DIR/${APP_NAME}.app"
cp -R "$APP_SRC" "$BIN_DIR/${APP_NAME}.app"
ok "已拷贝 .app → $BIN_DIR/${APP_NAME}.app"

# 同步启动脚本
cp "$REPO_ROOT/mac/start.sh" "$BIN_DIR/start.sh"
chmod +x "$BIN_DIR/start.sh"
chmod +x "$BIN_DIR/${APP_NAME}.app/Contents/MacOS/${PRODUCT_BIN}"
ok "已拷贝 start.sh + 设置可执行权限"

# .dmg: src-tauri/target/universal-apple-darwin/release/bundle/dmg/*.dmg
shopt -s nullglob
DMG_FILES=("$SRC_TAURI/target/universal-apple-darwin/release/bundle/dmg"/*.dmg)
shopt -u nullglob
if (( ${#DMG_FILES[@]} > 0 )); then
    cp "${DMG_FILES[@]}" "$DMG_DIR/"
    ok "已拷贝 dmg: $(ls "$DMG_DIR")"
else
    warn "未在 bundle/dmg 下找到 .dmg（可能 icon.icns 缺失或 codesign 失败）"
fi

# ─── 7. 校验 ────────────────────────────────────────────────────────────────
step "7. 产物校验"

EXEC="$BIN_DIR/${APP_NAME}.app/Contents/MacOS/${PRODUCT_BIN}"
if [[ ! -x "$EXEC" ]]; then
    fail "可执行文件不可执行: $EXEC"
fi

ARCH_INFO="$(file "$EXEC")"
echo -e "        ${DIM}$ARCH_INFO${RESET}"
if echo "$ARCH_INFO" | grep -q "universal"; then
    ok "Universal Binary 检测通过"
else
    warn "未检测到 universal 字样，请人工确认 lipo 合成"
fi

# ─── 完成 ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}============================================================${RESET}"
echo -e "${GREEN}  macOS 构建完成${RESET}"
echo -e "${GREEN}============================================================${RESET}"
echo ""
echo "  产物位置："
echo "    .app  → $BIN_DIR/${APP_NAME}.app"
echo "    .dmg  → $DMG_DIR/"
echo ""
echo "  启动应用："
echo -e "    ${DIM}open '$BIN_DIR/${APP_NAME}.app'${RESET}"
echo -e "    ${DIM}bash $BIN_DIR/start.sh${RESET}"
echo ""
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
    echo -e "  ${YELLOW}提示：未设置 APPLE_SIGNING_IDENTITY，产物未签名。${RESET}"
    echo -e "  ${YELLOW}分发前请配置：export APPLE_SIGNING_IDENTITY=\"Developer ID Application: Your Name (TEAMID)\"${RESET}"
fi
echo ""
