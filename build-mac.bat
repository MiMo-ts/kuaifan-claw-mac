@echo off
REM ============================================================================
REM !!! 警告 !!!
REM Tauri 2 的 macOS 产物（.app / .dmg）必须在 macOS 主机上构建。
REM Windows 上的 osxcross 交叉编译已弃用，且无法对 .app 进行代码签名/公证。
REM 请在 Mac 上改用 build-mac.sh。
REM
REM 此脚本仅作为占位，避免既有 CI / 旧引用断链；真实构建请使用：
REM     cd kuaifanclaw
REM     chmod +x build-mac.sh
REM     ./build-mac.sh
REM ============================================================================

echo.
echo ============================================================
echo   此脚本已弃用。请在 macOS 主机上执行 build-mac.sh。
echo   Windows 交叉编译不再受支持。
echo ============================================================
echo.

REM 若误在 macOS / WSL 中执行，仍尝试走 bash 路径
where bash >nul 2>&1
if %errorlevel% neq 0 (
    echo 未检测到 bash。请在 Mac 上运行 build-mac.sh。
    pause
    exit /b 1
)

cd /d "%~dp0"
bash ./build-mac.sh
if %errorlevel% neq 0 (
    echo 构建失败。
    pause
    exit /b %errorlevel%
)

echo macOS 构建完成，产物已保存到 mac/ 目录
pause
