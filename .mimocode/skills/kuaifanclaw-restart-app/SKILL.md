---
name: kuaifanclaw-restart-app
description: Kill running 快泛claw app, wait, relaunch from debug binary, verify it's running
---

# 快泛Claw 重启应用

重启快泛Claw桌面应用的标准流程。用于开发调试时需要重启应用的场景。

## 步骤

1. **杀掉所有快泛claw进程**
   ```bash
   pkill -f "快泛claw" 2>/dev/null; sleep 1
   ```

2. **启动应用（debug二进制）**
   ```bash
   cd /Users/apple/Desktop/kuaifanclaw && open src-tauri/target/debug/快泛claw 2>&1 &
   sleep 8
   ```

3. **验证进程启动**
   ```bash
   ps aux | grep "快泛claw" | grep -v grep
   ```

## 注意事项

- 项目路径: `/Users/apple/Desktop/kuaifanclaw`
- 如果需要同时清理 clawdbot 网关进程，添加 `pkill -f "clawdbot" 2>/dev/null`
- 如果端口被占用（如 5173），先清理: `lsof -ti :5173 | xargs kill -9 2>/dev/null`
- 如果需要重启网关，还需清理锁文件: `rm -f /var/folders/*/T/openclaw-*/gateway.*.lock`
