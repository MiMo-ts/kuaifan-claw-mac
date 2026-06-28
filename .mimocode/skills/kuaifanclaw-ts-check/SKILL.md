---
name: kuaifanclaw-ts-check
description: Quick TypeScript type checking for the 快泛Claw frontend
---

# 快泛Claw TypeScript 类型检查

快速检查前端 TypeScript 编译错误，无需完整构建。

## 步骤

```bash
cd /Users/apple/Desktop/kuaifanclaw/web && npx tsc --noEmit 2>&1 | head -30
```

## 说明

- 不生成任何输出表示没有类型错误
- 输出显示文件名和行号的错误信息
- `--noEmit` 不生成文件，仅检查类型
- 超时建议 60 秒

## 相关命令

- **完整前端构建**: `cd web && rm -rf dist node_modules/.vite && npm run build`
- **Rust 编译检查**: `source ~/.cargo/env && cd src-tauri && cargo check --no-default-features`
