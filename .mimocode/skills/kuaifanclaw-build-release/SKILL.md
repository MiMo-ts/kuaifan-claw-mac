---
name: kuaifanclaw-build-release
description: Full clean build and release of 快泛Claw — kill app, rebuild frontend, build Rust release, generate DMG
---

# 快泛Claw 构建发布

完整的干净构建和发布流程。用于生成可分发的 .app 和 .dmg 安装包。

## 前置条件

- 需要安装 Rust: `source ~/.cargo/env`
- 项目路径: `/Users/apple/Desktop/kuaifanclaw`
- 构建时间: 约 6-7 分钟

## 步骤

### 1. 停止运行中的应用
```bash
pkill -f "快泛claw" 2>/dev/null; sleep 2
```

### 2. 清理并构建前端
```bash
cd /Users/apple/Desktop/kuaifanclaw/web && rm -rf dist node_modules/.vite && npm run build 2>&1 | tail -10
```

### 3. 构建 Rust 后端（release）
```bash
source ~/.cargo/env && cd /Users/apple/Desktop/kuaifanclaw/src-tauri && cargo build --release 2>&1 | tail -5
```

### 4. 打包 DMG
```bash
cd /Users/apple/Desktop/kuaifanclaw/src-tauri && source ~/.cargo/env && npx tauri build 2>&1 | tail -10
```

### 5. 验证输出
```bash
ls -la /Users/apple/Desktop/kuaifanclaw/src-tauri/target/release/bundle/dmg/
ls -la /Users/apple/Desktop/kuaifanclaw/src-tauri/target/release/bundle/macos/
```

### 6. 打开 DMG 安装包
```bash
open "/Users/apple/Desktop/kuaifanclaw/src-tauri/target/release/bundle/dmg/快泛claw_*.dmg"
```

## 注意事项

- `source ~/.cargo/env` 在每条命令中都需要（shell 不持久化）
- 如果只需检查编译错误，用 `cargo check --no-default-features` 代替完整 build
- 如果只需前端类型检查，用 `cd web && npx tsc --noEmit`
- 构建日志可输出到文件: `> /tmp/tauri_release.log 2>&1`
- Universal Binary 需要分别构建 aarch64 和 x86_64 再用 `lipo -create` 合并
