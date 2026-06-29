// 安装服务命令

use crate::bundled_env::{
    resolve_bundled_openclaw_tarball, resolve_bundled_zip, resolve_bundled_zip_from_project,
};
use crate::commands::hidden_cmd;
use crate::env_paths::{
    build_deps_env_path, env_root, git_exe, git_exists, node_exe, resolve_node, tar_gz_extract,
    unzip,
};
#[cfg(target_os = "macos")]
use crate::mirror::github_mirror_urls;
use crate::mirror::{unpack_npm_tarball, InstallProgressEvent};
use crate::models::{InstallProgress, OpenClawCnStatus};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
#[cfg(target_os = "macos")]
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

// ─── 工具 ─────────────────────────────────────────────────────────────────

fn emit(app: &AppHandle, event: InstallProgressEvent) {
    let _ = app.emit("install-progress", event);
}

/// 依赖安装 stderr 若含权限类错误，追加面向用户的说明（杀毒/OneDrive 锁文件）。
fn npm_deps_permission_hint(stderr: &str) -> &'static str {
    let s = stderr.to_lowercase();
    if s.contains("eperm")
        || s.contains("eacces")
        || s.contains("operation not permitted")
        || s.contains("access is denied")
        || s.contains("拒绝访问")
    {
        return "\n\n【常见原因】EPERM/权限错误多为杀毒软件实时扫描或 OneDrive 同步占用 node_modules 内文件。\n建议：① 将数据目录加入 Defender 排除项；② 数据目录勿放在 OneDrive 同步文件夹内；③ 完全退出本程序后手动删除「数据目录\\openclaw-cn\\node_modules」，再重新进入向导第 2 步安装。";
    }
    ""
}

/// 安装全程心跳任务：`install_openclaw` 任意返回路径上 drop 时中止，避免泄漏。
struct InstallHeartbeatGuard(tokio::task::JoinHandle<()>);

impl Drop for InstallHeartbeatGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 网关启动与向导「完成」均以 `dist/entry.js` 为准；仅存在 `openclaw-cn` 目录或半成品 `node_modules` 仍视为未安装。
fn openclaw_core_ready(openclaw_dir: &str) -> bool {
    Path::new(openclaw_dir)
        .join("dist")
        .join("entry.js")
        .is_file()
}

/// `npm/pnpm install` 是否已实质完成（避免每次向导都全量重装 node_modules）。
fn openclaw_deps_ready(openclaw_dir: &str) -> bool {
    let nm = Path::new(openclaw_dir).join("node_modules");
    if !nm.is_dir() {
        return false;
    }
    // npm 平铺：核心依赖目录
    if nm.join("@mariozechner").join("pi-agent-core").is_dir() {
        return true;
    }
    // pnpm：虚拟存储 + 仍会有部分顶层链接
    if nm.join(".pnpm").is_dir() {
        return nm.join("chalk").exists() || nm.join("@mariozechner").exists();
    }
    nm.join("chalk").is_dir()
}

/// 获取 app 自包含数据目录（辅助函数，供本模块使用）
fn self_data_dir(data_dir: &tauri::State<'_, crate::AppState>) -> String {
    data_dir.inner().get_data_dir()
}

// 安装 Node.js（自包含模式：下载官方 .zip，解压到 data/env/node）
// openclaw-cn 要求 Node.js >= 22
const NODE_VERSION: &str = "v22.14.0";
#[cfg(windows)]
const NODE_WIN_ZIP: &str = "node-v22.14.0-win-x64.zip";
#[cfg(not(windows))]
const NODE_LINUX_TAR: &str = "node-v22.14.0-linux-x64.tar.gz";

#[tauri::command]
pub async fn install_node(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let base = self_data_dir(&data_dir);
    let env_dir = env_root(&base);
    let dest = env_dir.join("node");

    // ── 1. 检测系统 Node.js ─────────────────────────────────
    #[cfg(windows)]
    if let Ok(output) = hidden_cmd::cmd().arg("/C").arg("node").arg("--version").output() {
        if output.status.success() {
            let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
            emit(
                &app,
                InstallProgressEvent::finished("node", &format!("使用系统 Node.js（{}）", v)),
            );
            return Ok(format!("Node.js 已安装（系统）：{}", v));
        }
    }

    // ── 2. 检测内置 Node.js ─────────────────────────────────
    if node_exe(&env_dir).exists() {
        let node_v = node_exe(&env_dir);
        #[cfg(windows)]
        {
            if let Ok(output) = hidden_cmd::cmd().arg("/C").arg(&node_v).arg("--version").output() {
                if output.status.success() {
                    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    emit(
                        &app,
                        InstallProgressEvent::finished(
                            "node",
                            &format!("使用内置 Node.js（{}）", v),
                        ),
                    );
                    return Ok(format!("Node.js 已安装（内置）：{}", v));
                }
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(output) = Command::new(&node_v).arg("--version").output() {
                if output.status.success() {
                    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    emit(
                        &app,
                        InstallProgressEvent::finished(
                            "node",
                            &format!("使用内置 Node.js（{}）", v),
                        ),
                    );
                    return Ok(format!("Node.js 已安装（内置）：{}", v));
                }
            }
        }
        // 内置版本损坏，清理
        let _ = tokio::fs::remove_dir_all(&dest).await;
    }

    info!(
        "开始安装 Node.js {}（自包含）至 {}",
        NODE_VERSION,
        dest.display()
    );

    #[cfg(windows)]
    {
        let zip_path = resolve_bundled_zip_from_project(NODE_WIN_ZIP)
            .or_else(|| resolve_bundled_zip(&app, NODE_WIN_ZIP))
            .ok_or_else(|| {
                let hint = format!(
                    "未找到内置 Node.js zip（{}），请确认安装包中 bundled-env/ 目录存在并包含该文件",
                    NODE_WIN_ZIP
                );
                emit(&app, InstallProgressEvent::failed("node", &hint));
                hint
            })?;

        emit(
            &app,
            InstallProgressEvent::started(
                "node",
                &format!("正在从内置包解压 Node.js {}（离线）…", NODE_VERSION),
            ),
        );
        info!("Node 使用内置包: {}", zip_path.display());

        emit(
            &app,
            InstallProgressEvent::progress("node", 80.0, "正在解压到 data/env/node…"),
        );
        info!("Node 解压目录: {}", dest.display());
        unzip(&zip_path, &dest).await?;

        let node_v = node_exe(&env_dir);
        if !node_v.exists() {
            let msg = format!(
                "解压已完成但未找到 {}，请查看日志中的「Node 解压目录」。切勿用资源管理器把 zip 解到 D:\\ 根目录。",
                node_v.display()
            );
            emit(&app, InstallProgressEvent::failed("node", &msg));
            return Err(msg);
        }

        #[cfg(windows)]
        let v_out = hidden_cmd::cmd().arg("/C").arg(&node_v).arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        #[cfg(windows)]
        let v_out = hidden_cmd::cmd().arg("/C").arg(&node_v).arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        let v_out = Command::new(&node_v)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        if v_out == "unknown" {
            let msg = format!(
                "{} 存在但无法执行（{}），可能被安全软件拦截",
                node_v.display(),
                v_out
            );
            emit(&app, InstallProgressEvent::failed("node", &msg));
            return Err(msg);
        }

        emit(
            &app,
            InstallProgressEvent::finished("node", &format!("Node.js 安装完成（{}）", v_out)),
        );
        Ok(format!("Node.js 安装成功（自包含）：{}", v_out))
    }

    #[cfg(target_os = "macos")]
    {
        // macOS: 检测架构
        let is_arm64 = Command::new("uname")
            .arg("-m")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "arm64")
            .unwrap_or(false);

        let arch = if is_arm64 { "arm64" } else { "x64" };
        let tarball_name = format!("node-{}-darwin-{}.tar.gz", NODE_VERSION.trim_start_matches('v'), arch);

        // 优先使用内置 tarball 包
        let bundled_tarball = resolve_bundled_zip_from_project(&tarball_name)
            .or_else(|| resolve_bundled_zip(&app, &tarball_name));

        if let Some(tar_path) = bundled_tarball {
            // 使用内置包
            emit(
                &app,
                InstallProgressEvent::started("node", &format!("正在从内置包解压 Node.js {}（macOS {}，离线）…", NODE_VERSION, arch)),
            );
            info!("macOS Node 使用内置包: {}", tar_path.display());
            tar_gz_extract(&tar_path, &dest).await?;
        } else {
            // 没有内置包，使用 npmmirror 下载
            let url = format!("https://npmmirror.com/mirrors/node/{}/{}", NODE_VERSION, tarball_name);
            emit(
                &app,
                InstallProgressEvent::started("node", &format!("正在下载 Node.js {} macOS {} 版本（npmmirror 镜像）…", NODE_VERSION, arch)),
            );

            let temp_dir = std::env::temp_dir();
            let tar_path = temp_dir.join(&tarball_name);

            let client = reqwest::Client::new();
            let mut resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("下载失败: {}", e))?;

            let mut file = tokio::fs::File::create(&tar_path)
                .await
                .map_err(|e| format!("创建文件失败: {}", e))?;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| format!("读取流失败: {}", e))?
            {
                file.write_all(&chunk)
                    .await
                    .map_err(|e| format!("写入失败: {}", e))?;
            }
            file.flush()
                .await
                .map_err(|e| format!("刷新文件失败: {}", e))?;

            emit(&app, InstallProgressEvent::progress("node", 80.0, "正在解压…"));
            tar_gz_extract(&tar_path, &dest).await?;
            tokio::fs::remove_file(&tar_path).await.ok();
        }

        let node_v = node_exe(&env_dir);
        #[cfg(windows)]
        let v_out = hidden_cmd::cmd().arg("/C").arg(&node_v).arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        let v_out = Command::new(&node_v)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        emit(
            &app,
            InstallProgressEvent::finished("node", &format!("Node.js 安装完成（{}）", v_out)),
        );
        Ok(format!("Node.js 安装成功（macOS {}）：{}", arch, v_out))
    }

    #[cfg(target_os = "linux")]
    {
        let temp_dir = std::env::temp_dir();
        let tar_path = temp_dir.join(NODE_LINUX_TAR);

        emit(
            &app,
            InstallProgressEvent::started(
                "node",
                &format!("正在下载 Node.js {} Linux 版本（npmmirror 镜像）…", NODE_VERSION),
            ),
        );
        // 使用 npmmirror 国内镜像（参考 u-claw），速度快
        let url = format!(
            "https://npmmirror.com/mirrors/node/{}/{}",
            NODE_VERSION, NODE_LINUX_TAR
        );
        let client = reqwest::Client::new();
        let mut resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("下载失败: {}", e))?;
        let mut file = tokio::fs::File::create(&tar_path)
            .await
            .map_err(|e| format!("创建文件失败: {}", e))?;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("读取流失败: {}", e))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("写入失败: {}", e))?;
        }
        file.flush()
            .await
            .map_err(|e| format!("刷新文件失败: {}", e))?;

        emit(
            &app,
            InstallProgressEvent::progress("node", 80.0, "正在解压…"),
        );
        tar_gz_extract(&tar_path, &dest).await?;
        tokio::fs::remove_file(&tar_path).await.ok();

        let node_v = node_exe(&env_dir);
        #[cfg(windows)]
        let v_out = hidden_cmd::cmd().arg("/C").arg(&node_v).arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        let v_out = Command::new(&node_v)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        emit(
            &app,
            InstallProgressEvent::finished("node", &format!("Node.js 安装完成（{}）", v_out)),
        );
        Ok(format!("Node.js 安装成功（自包含）：{}", v_out))
    }
}

/// 安装 Homebrew（仅 macOS；科大讯飞源为主，ghproxy 镜像为备）
#[tauri::command]
pub async fn install_homebrew(_app: AppHandle) -> Result<String, String> {
    #[cfg(not(target_os = "macos"))]
    {
        info!("Homebrew 安装跳过（仅 macOS 支持）");
        return Ok("Homebrew 在 Windows 上不需要，跳过安装".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        // 方案1: 科大讯飞源（官方脚本 + 环境变量）
        info!("尝试方案1: 科大讯飞源...");
        emit(
            &_app,
            InstallProgressEvent::started("homebrew", "正在安装 Homebrew（科大讯飞源）..."),
        );

        if let Ok(msg) = try_install_homebrew_ustc().await {
            emit(&_app, InstallProgressEvent::finished("homebrew", "Homebrew 安装成功"));
            return Ok(msg);
        }

        // 方案2: GitHub 官方源 + ghproxy 镜像
        info!("方案1失败，尝试方案2: GitHub + ghproxy 镜像...");
        emit(
            &_app,
            InstallProgressEvent::started("homebrew", "方案1失败，尝试 GitHub 镜像源..."),
        );

        if let Ok(msg) = try_install_homebrew_ghproxy().await {
            emit(&_app, InstallProgressEvent::finished("homebrew", "Homebrew 安装成功"));
            return Ok(msg);
        }

        emit(
            &_app,
            InstallProgressEvent::failed("homebrew", "所有安装方案均失败"),
        );
        Err("Homebrew 安装失败，请检查网络或手动安装（见 https://brew.sh）".to_string())
    }
}

/// 尝试使用科大讯飞源安装 Homebrew（官方脚本 + 环境变量）
async fn try_install_homebrew_ustc() -> Result<String, String> {
    // 方案A: 使用科大讯飞提供的安装脚本（已包含镜像配置）
    let script_url = "https://mirrors.ustc.edu.cn/misc/brew-install.sh";
    let output = Command::new("curl")
        .args(["-fsSL", script_url])
        .output()
        .map_err(|e| format!("下载安装脚本失败: {}", e))?;

    if !output.status.success() {
        return Err(format!("下载失败: {}", String::from_utf8_lossy(&output.stderr)));
    }

    // 保存脚本到临时文件
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("brew_install_ustc.sh");
    let script_content = String::from_utf8_lossy(&output.stdout);
    std::fs::write(&script_path, script_content.as_bytes())
        .map_err(|e| format!("保存脚本失败: {}", e))?;

    // 执行脚本（环境变量通过 Command::env 传递，更可靠）
    let status = Command::new("/bin/bash")
        .env("NONINTERACTIVE", "1")
        .env("HOMEBREW_BREW_GIT_REMOTE", "https://mirrors.ustc.edu.cn/brew.git")
        .env("HOMEBREW_CORE_GIT_REMOTE", "https://mirrors.ustc.edu.cn/homebrew-core.git")
        .env("HOMEBREW_BOTTLE_DOMAIN", "https://mirrors.ustc.edu.cn/homebrew-bottles")
        .env("HOMEBREW_API_DOMAIN", "https://mirrors.ustc.edu.cn/homebrew-bottles/api")
        .arg(&script_path)
        .spawn()
        .and_then(|mut child| child.wait())
        .map_err(|e| format!("执行安装脚本失败: {}", e))?;

    let _ = std::fs::remove_file(&script_path);

    if status.success() {
        Ok("Homebrew 安装成功（科大讯飞源），请重启终端后运行 `brew doctor` 验证".to_string())
    } else {
        Err("科大讯飞源安装失败".to_string())
    }
}

/// 尝试使用科大讯飞源安装 Homebrew（方案2备用）
async fn try_install_homebrew_ghproxy() -> Result<String, String> {
    // 方案B: 使用科大讯飞源安装脚本（备用方案）
    let script_url = "https://mirrors.ustc.edu.cn/misc/brew-install.sh";
    let output = Command::new("curl")
        .args(["-fsSL", script_url])
        .output()
        .map_err(|e| format!("下载安装脚本失败: {}", e))?;

    if !output.status.success() {
        return Err(format!("下载失败: {}", String::from_utf8_lossy(&output.stderr)));
    }

    // 保存脚本到临时文件
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("brew_install_ustc2.sh");
    let script_content = String::from_utf8_lossy(&output.stdout);
    std::fs::write(&script_path, script_content.as_bytes())
        .map_err(|e| format!("保存脚本失败: {}", e))?;

    // 执行脚本（科大讯飞源环境变量）
    let status = Command::new("/bin/bash")
        .env("NONINTERACTIVE", "1")
        .env("HOMEBREW_BREW_GIT_REMOTE", "https://mirrors.ustc.edu.cn/brew.git")
        .env("HOMEBREW_CORE_GIT_REMOTE", "https://mirrors.ustc.edu.cn/homebrew-core.git")
        .env("HOMEBREW_BOTTLE_DOMAIN", "https://mirrors.ustc.edu.cn/homebrew-bottles")
        .env("HOMEBREW_API_DOMAIN", "https://mirrors.ustc.edu.cn/homebrew-bottles/api")
        .arg(&script_path)
        .spawn()
        .and_then(|mut child| child.wait())
        .map_err(|e| format!("执行安装脚本失败: {}", e))?;

    let _ = std::fs::remove_file(&script_path);

    if status.success() {
        Ok("Homebrew 安装成功（科大讯飞备用源），请重启终端后运行 `brew doctor` 验证".to_string())
    } else {
        Err("科大讯飞备用源安装失败".to_string())
    }
}

// 安装 pnpm
#[tauri::command]
pub async fn install_pnpm(app: AppHandle) -> Result<String, String> {
    info!("开始安装 pnpm...");

    emit(
        &app,
        InstallProgressEvent::started("pnpm", "正在安装 pnpm..."),
    );

    #[cfg(windows)]
    let output = hidden_cmd::cmd()
        .args(["/C", "npm", "install", "-g", "pnpm"])
        .output()
        .map_err(|e| format!("执行失败: {}", e))?;

    #[cfg(not(windows))]
    let output = Command::new("npm")
        .args(["install", "-g", "pnpm"])
        .output()
        .map_err(|e| format!("执行失败: {}", e))?;

    if output.status.success() {
        emit(
            &app,
            InstallProgressEvent::finished("pnpm", "pnpm 安装完成"),
        );
        Ok("pnpm 安装成功".to_string())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        emit(&app, InstallProgressEvent::failed("pnpm", &error));
        Err(format!("pnpm 安装失败: {}", error))
    }
}

// 安装 Git（自包含模式：优先使用系统 Git，否则使用内置 MinGit）
#[tauri::command]
pub async fn install_git(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    info!("开始安装 Git...");

    let base = self_data_dir(&data_dir);
    let env_dir = env_root(&base);
    let dest = env_dir.join("git");

    // ── 1. 检测系统 Git ─────────────────────────────────
    #[cfg(windows)]
    {
        if let Ok(output) = hidden_cmd::cmd().arg("/C").arg("git").arg("--version").output() {
            if output.status.success() {
                let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                emit(&app, InstallProgressEvent::finished("git", &format!("使用系统 Git（{}）", v)));
                return Ok(format!("Git 已安装（系统）：{}", v));
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(output) = Command::new("git").arg("--version").output() {
            if output.status.success() {
                let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                emit(&app, InstallProgressEvent::finished("git", &format!("使用系统 Git（{}）", v)));
                return Ok(format!("Git 已安装（系统）：{}", v));
            }
        }
    }

    // ── 2. 检测内置 Git ─────────────────────────────────
    if git_exists(&env_dir) {
        let git_v = git_exe(&env_dir);
        #[cfg(windows)]
        {
            if let Ok(output) = hidden_cmd::cmd().arg("/C").arg(&git_v).arg("--version").output() {
                if output.status.success() {
                    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    emit(&app, InstallProgressEvent::finished("git", &format!("使用内置 Git（{}）", v)));
                    return Ok(format!("Git 已安装（内置）：{}", v));
                }
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(output) = Command::new(&git_v).arg("--version").output() {
                if output.status.success() {
                    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    emit(&app, InstallProgressEvent::finished("git", &format!("使用内置 Git（{}）", v)));
                    return Ok(format!("Git 已安装（内置）：{}", v));
                }
            }
        }
        // 内置版本损坏，清理
        let _ = tokio::fs::remove_dir_all(&dest).await;
    }

    info!(
        "开始安装 Git {}（MinGit 便携版）至 {}",
        crate::mirror::MINGIT_VERSION,
        dest.display()
    );

    #[cfg(windows)]
    {
        let zip_path = resolve_bundled_zip_from_project(crate::mirror::MINGIT_ZIP)
            .or_else(|| resolve_bundled_zip(&app, crate::mirror::MINGIT_ZIP))
            .ok_or_else(|| {
                let hint = format!(
                    "未找到内置 Git zip（{}），请确认安装包中 bundled-env/ 目录存在并包含该文件",
                    crate::mirror::MINGIT_ZIP
                );
                emit(&app, InstallProgressEvent::failed("git", &hint));
                hint
            })?;

        emit(
            &app,
            InstallProgressEvent::started(
                "git",
                &format!("正在从内置包解压 Git {}（离线）…", crate::mirror::MINGIT_VERSION),
            ),
        );
        info!("Git 使用内置包: {}", zip_path.display());

        emit(
            &app,
            InstallProgressEvent::progress("git", 80.0, "正在解压到 data/env/git…"),
        );
        unzip(&zip_path, &dest).await?;

        let git_v = git_exe(&env_dir);
        #[cfg(windows)]
        let v_out = hidden_cmd::cmd().arg("/C").arg(&git_v).arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        let v_out = Command::new(&git_v)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        if v_out != "unknown" {
            emit(&app, InstallProgressEvent::finished("git", &format!("Git 安装完成（{}）", v_out)));
            Ok(format!("Git 安装成功（内置）：{}", v_out))
        } else {
            emit(&app, InstallProgressEvent::failed("git", "Git 解压后执行失败"));
            Err("Git 安装后执行失败".to_string())
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS: 尝试使用内置 MinGit，如果没有则提示用户安装 Xcode Command Line Tools
        let zip_path = resolve_bundled_zip_from_project(crate::mirror::MINGIT_ZIP)
            .or_else(|| resolve_bundled_zip(&app, crate::mirror::MINGIT_ZIP));

        if let Some(zip) = zip_path {
            emit(
                &app,
                InstallProgressEvent::started(
                    "git",
                    &format!("正在从内置包解压 Git {}（离线）…", crate::mirror::MINGIT_VERSION),
                ),
            );
            unzip(&zip, &dest).await?;

            let git_v = git_exe(&env_dir);
            let v_out = Command::new(&git_v)
                .arg("--version")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|_| "unknown".to_string());

            if v_out != "unknown" {
                emit(&app, InstallProgressEvent::finished("git", &format!("Git 安装完成（{}）", v_out)));
                return Ok(format!("Git 安装成功（内置）：{}", v_out));
            }
        }

        // 没有内置 MinGit，提示用户安装 Xcode Command Line Tools
        emit(&app, InstallProgressEvent::failed("git", "未找到 Git，请运行: xcode-select --install"));
        Err("macOS Git 未安装，请运行 'xcode-select --install' 安装 Xcode Command Line Tools".to_string())
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: 尝试 apt 安装
        emit(&app, InstallProgressEvent::started("git", "通过 apt 安装 Git..."));
        let output = Command::new("sudo")
            .args(["apt", "install", "-y", "git"])
            .output()
            .map_err(|e| format!("安装失败: {}", e))?;

        if output.status.success() {
            emit(&app, InstallProgressEvent::finished("git", "Git 安装完成"));
            Ok("Git 安装成功（apt）".to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            emit(&app, InstallProgressEvent::failed("git", &stderr));
            Err(format!("apt 安装 Git 失败: {}", stderr))
        }
    }
}

// 获取 OpenClaw 版本
#[tauri::command]
pub async fn get_openclaw_version(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<Option<String>, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let pkg_path = format!("{}/openclaw-cn/package.json", data_dir);

    match tokio::fs::read_to_string(&pkg_path).await {
        Ok(content) => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                return Ok(json
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()));
            }
        }
        Err(_) => {}
    }

    Ok(None)
}

/// 向导第 2 步：检测 OpenClaw-CN 是否已完整安装，便于直接跳过。
#[tauri::command]
pub async fn get_openclaw_cn_status(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<OpenClawCnStatus, String> {
    let base = data_dir.inner().get_data_dir();
    let openclaw_dir = format!(
        "{}/openclaw-cn",
        base.trim_end_matches(|c| c == '/' || c == '\\')
    );
    let core = openclaw_core_ready(&openclaw_dir);
    let deps = openclaw_deps_ready(&openclaw_dir);
    let pkg_path = format!("{}/package.json", openclaw_dir);
    let version = std::fs::read_to_string(&pkg_path).ok().and_then(|c| {
        serde_json::from_str::<serde_json::Value>(&c)
            .ok()?
            .get("version")?
            .as_str()
            .map(|s| s.to_string())
    });
    Ok(OpenClawCnStatus {
        core_ready: core,
        deps_ready: deps,
        fully_ready: core && deps,
        version,
        openclaw_dir,
    })
}

// ─── 后台安装支持 ──────────────────────────────────────────────────────────────────

/// 后台安装状态（供 get_openclaw_install_status 使用）
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenClawInstallStatus {
    /// npm install 是否已在后台运行（process started, not yet completed）
    pub npm_install_running: bool,
    /// npm install 是否完成（无论成功/失败）
    pub npm_install_done: bool,
    /// npm install 是否失败
    pub npm_install_failed: bool,
    /// npm install 失败时的错误信息（如果有）
    pub npm_install_error: Option<String>,
    /// .installing marker 文件路径
    pub marker_path: Option<String>,
}

/// 返回当前后台安装状态
#[tauri::command]
pub async fn get_openclaw_install_status(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<OpenClawInstallStatus, String> {
    let base = data_dir.inner().get_data_dir();
    let openclaw_dir = format!(
        "{}/openclaw-cn",
        base.trim_end_matches(|c| c == '/' || c == '\\')
    );
    let marker_path = format!("{}/.installing", openclaw_dir);
    let marker_exists = Path::new(&marker_path).is_file();

    // 检查 npm install 是否完成（通过 deps_ready 判定）
    let deps_ready = openclaw_deps_ready(&openclaw_dir);

    // 读取错误信息（如果 marker 存在但 deps 已就绪，说明之前失败过但后来手动解决了）
    let npm_install_error = if marker_exists && !deps_ready {
        let marker_content = std::fs::read_to_string(&marker_path).ok();
        marker_content.and_then(|c| {
            let v: serde_json::Value = serde_json::from_str(&c).ok()?;
            v.get("error")?.as_str().map(|s| s.to_string())
        })
    } else {
        None
    };

    Ok(OpenClawInstallStatus {
        npm_install_running: marker_exists && !deps_ready,
        npm_install_done: deps_ready || !marker_exists,
        npm_install_failed: marker_exists && !deps_ready && npm_install_error.is_some(),
        npm_install_error,
        marker_path: Some(marker_path),
    })
}

/// 启动后台 npm install，解压 tgz 后立即返回，不等待安装完成。
/// 前端应轮询 get_openclaw_install_status 直至 npm_install_done。
/// 内部通过写入 .installing marker + spawn background npm install process 实现。
#[tauri::command]
pub async fn start_openclaw_background_install(
    app: tauri::AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let base = data_dir.inner().get_data_dir();
    let data_base = base.trim_end_matches(|c| c == '/' || c == '\\').to_string();
    let openclaw_dir = format!("{}/openclaw-cn", data_base);
    let marker_path = format!("{}/.installing", openclaw_dir);

    // 读取配置
    let app_yaml_path = format!("{}/config/app.yaml", data_base);
    let app_yaml_raw = std::fs::read_to_string(&app_yaml_path).unwrap_or_default();
    let app_yaml: serde_yaml::Value = serde_yaml::from_str(&app_yaml_raw).unwrap_or_default();

    let cfg_registry = app_yaml
        .get("openclaw")
        .and_then(|o| o.get("registry"))
        .and_then(|v| v.as_str())
        .unwrap_or("https://registry.npmmirror.com");
    let cfg_allow_scripts = app_yaml
        .get("openclaw")
        .and_then(|o| o.get("allow_scripts"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let cfg_prefer_system = app_yaml
        .get("openclaw")
        .and_then(|o| o.get("prefer_system_node"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let cfg_legacy_peer = app_yaml
        .get("openclaw")
        .and_then(|o| o.get("legacy_peer_deps"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Step 0: 检测内置 tgz，优先解压
    let bundled_tarball = resolve_bundled_openclaw_tarball(&app);
    if let Some(bundled_path) = bundled_tarball {
        emit(
            &app,
            InstallProgressEvent::started(
                "openclaw-pkg",
                &format!("检测到内置包（{}），正在解压…", bundled_path.display()),
            ),
        );
        let extract_failed = tokio::task::spawn_blocking({
            let bundled_path = bundled_path.clone();
            let openclaw_dir = openclaw_dir.clone();
            let data_base = data_base.clone();
            move || -> Result<(), String> {
                let bytes = std::fs::read(&bundled_path)
                    .map_err(|e| format!("读取内置 tarball 失败: {}", e))?;
                let extract_root = format!("{}/.openclaw-tarball-extract", data_base);
                let _ = std::fs::remove_dir_all(&extract_root);
                std::fs::create_dir_all(&extract_root)
                    .map_err(|e| format!("创建临时解压目录失败: {}", e))?;
                let cursor = std::io::Cursor::new(bytes);
                let dec = flate2::read::GzDecoder::new(cursor);
                let mut archive = tar::Archive::new(dec);
                archive
                    .unpack(&extract_root)
                    .map_err(|e| format!("解压内置 tarball 失败: {}", e))?;
                let pkg_folder = std::path::Path::new(&extract_root).join("package");
                if !pkg_folder.is_dir() {
                    return Err("内置 tarball 解压后未找到 package/ 目录".to_string());
                }
                if std::path::Path::new(&openclaw_dir).exists() {
                    std::fs::remove_dir_all(&openclaw_dir)
                        .map_err(|e| format!("删除旧目录失败: {}", e))?;
                }
                std::fs::rename(&pkg_folder, &openclaw_dir)
                    .map_err(|e| format!("移动到目标目录失败: {}", e))?;
                std::fs::remove_dir_all(&extract_root).ok();
                Ok(())
            }
        })
        .await
        .map_err(|e| format!("解压任务失败: {}", e))?;

        if extract_failed.is_err() {
            emit(
                &app,
                InstallProgressEvent::detail(
                    "openclaw-pkg",
                    "内置包解压失败，降级为 registry 下载",
                ),
            );
        }
    }

    // 如果 core 已就绪且 deps 也就绪，不需要后台安装
    if openclaw_core_ready(&openclaw_dir) && openclaw_deps_ready(&openclaw_dir) {
        return Ok(format!(
            "OpenClaw-CN 已完整安装（{}），无需后台安装",
            openclaw_dir
        ));
    }

    // 如果 core 不就绪，需要先下载包
    if !openclaw_core_ready(&openclaw_dir) {
        emit(
            &app,
            InstallProgressEvent::started("openclaw-pkg", "下载 openclaw-cn 程序包…"),
        );
        if let Err(e) = fetch_openclaw_via_npm_pkg(
            &app,
            &data_base,
            &openclaw_dir,
            "openclaw-cn",
            "latest",
            cfg_allow_scripts,
            cfg_prefer_system,
            cfg_registry,
            cfg_legacy_peer,
        )
        .await
        {
            return Err(format!("下载 openclaw-cn 失败: {}", e));
        }
        emit(&app, InstallProgressEvent::finished("openclaw-pkg", "程序包已获取"));
    }

    // 写入 installing marker（写入启动时的 deps_ready 状态作为基准）
        let marker_data = serde_json::json!({
            "started_at": chrono::Utc::now().to_rfc3339(),
            "npm_install_running": true,
            "npm_install_done": false,
            "error": serde_json::Value::Null
        });
    std::fs::write(&marker_path, serde_json::to_string_pretty(&marker_data).unwrap())
        .map_err(|e| format!("写入 .installing marker 失败: {}", e))?;

    emit(
        &app,
        InstallProgressEvent::started(
            "openclaw-deps",
            "已在后台启动 npm install（node_modules 正在安装，可继续其他配置）…",
        ),
    );

    // 在后台 spawn npm install，不 await，直接返回
    let openclaw_dir_clone = openclaw_dir.clone();
    let data_base_clone = data_base.clone();
    let cfg_registry_clone = cfg_registry.to_string();
    let cfg_allow_scripts_clone = cfg_allow_scripts;
    let cfg_prefer_system_clone = cfg_prefer_system;
    let app_clone = app.clone();
    let marker_path_clone = marker_path.clone();

    std::thread::spawn(move || {
        // 在同步线程中执行 npm install（因为 tokio::spawn 不能直接嵌套）
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let result = run_npm_install_for_background(
                &openclaw_dir_clone,
                &data_base_clone,
                &cfg_registry_clone,
                cfg_allow_scripts_clone,
                cfg_prefer_system_clone,
            )
            .await;

            // 安装完成后更新 marker
            let (done, error_msg) = match result {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e)),
            };
            let final_marker = serde_json::json!({
                "started_at": std::fs::read_to_string(&marker_path_clone)
                    .ok()
                    .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                    .and_then(|v| v.get("started_at")?.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
                "npm_install_running": false,
                "npm_install_done": done,
                "completed_at": chrono::Utc::now().to_rfc3339(),
                "error": error_msg
            });
            let _ = std::fs::write(
                &marker_path_clone,
                serde_json::to_string_pretty(&final_marker).unwrap(),
            );

            // 发送完成事件
            if done {
                let _ = app_clone.emit(
                    "install-progress",
                    InstallProgressEvent::finished(
                        "openclaw-deps",
                        "后台 npm install 已完成，node_modules 就绪",
                    ),
                );
            } else {
                let _ = app_clone.emit(
                    "install-progress",
                    InstallProgressEvent::failed(
                        "openclaw-deps",
                        &format!("后台 npm install 失败: {}", error_msg.as_ref().unwrap()),
                    ),
                );
            }
        });
    });

    Ok(format!(
        "后台安装已启动：{}",
        openclaw_dir
    ))
}

/// 在后台线程中执行 npm install（内部使用，不返回进度）
async fn run_npm_install_for_background(
    openclaw_dir: &str,
    data_base: &str,
    registry: &str,
    allow_scripts: bool,
    prefer_system: bool,
) -> Result<(), String> {
    let (node_exe, npm_cli, npm_cmd, _, _) =
        resolve_npm_exe_with_version(data_base, prefer_system);
    let pnpm_path = find_pnpm_executable();
    let use_pnpm = pnpm_path.is_some();

    let mut args = vec!["install".to_string()];
    if !use_pnpm {
        args.push("--legacy-peer-deps".to_string());
    }
    if !allow_scripts {
        args.push("--ignore-scripts".to_string());
    }

    let deps_env_path = build_deps_env_path(data_base);
    // Clone everything needed inside spawn_blocking since it requires 'static
    let openclaw_dir_owned = openclaw_dir.to_string();
    let registry_owned = registry.to_string();
    let node_exe_owned = node_exe.clone();
    let npm_cmd_owned = npm_cmd.clone();
    let npm_cli_owned = npm_cli.clone();
    let pnpm_path_owned = pnpm_path.clone();

    let output_result = tokio::task::spawn_blocking(move || {
        if let Some(ref pp) = pnpm_path_owned {
            // pnpm 通过 cmd /c 隐藏窗口
            let mut c = hidden_cmd::cmd();
            c.arg("/C")
                .arg(&*pp.to_string_lossy())
                .current_dir(&openclaw_dir_owned)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry_owned)
                .args(&args);
            c.output()
        } else if let Some(ref cli) = npm_cli_owned {
            // npm-cli.js 通过 cmd /c 隐藏窗口
            let mut c = hidden_cmd::cmd();
            c.arg("/C")
                .arg(&*node_exe_owned.to_string_lossy())
                .arg(cli.as_os_str())
                .current_dir(&openclaw_dir_owned)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry_owned)
                .args(&args);
            c.output()
        } else {
            let mut c = hidden_cmd::cmd();
            c.arg("/C")
                .arg(&*npm_cmd_owned.to_string_lossy())
                .current_dir(&openclaw_dir_owned)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry_owned)
                .args(&args);
            c.output()
        }
    })
    .await
    .map_err(|e| format!("后台安装任务失败: {}", e))?
    .map_err(|e| format!("无法启动包管理器: {}", e))?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        return Err(stderr.to_string());
    }

    Ok(())
}

// ─── 安装 OpenClaw-CN 的核心逻辑 ─────────────────────────────────────────────

/// 与 node.exe 同目录的 npm-cli.js（官方 zip/tar 布局：../node_modules/npm/bin/npm-cli.js）
fn npm_cli_js_next_to_node(node_exe: &Path) -> Option<PathBuf> {
    let dir = node_exe.parent()?;
    let p = dir
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// 返回 (node_exe, npm_cli_js_or_none, npm_cmd_fallback, node_ver, npm_ver)
/// prefer_system_node=true 时用 PATH 的 node/npm；npm-cli.js 若无法定位则回退 npm.cmd
fn resolve_npm_exe_with_version(
    data_base: &str,
    prefer_system_node: bool,
) -> (PathBuf, Option<PathBuf>, PathBuf, String, String) {
    let (node_path, npm_path) = if prefer_system_node {
        let node = find_node_executable().unwrap_or_else(|| PathBuf::from("node"));
        let npm = find_npm_cmd_full_path()
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));
        (node, npm)
    } else {
        let (n, _) = resolve_node(data_base);
        let npm = n
            .parent()
            .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));
        (n, npm)
    };

    let npm_cli = npm_cli_js_next_to_node(&node_path);

    #[cfg(windows)]
    let node_version = hidden_cmd::cmd().arg("/C").arg(&node_path).arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    #[cfg(not(windows))]
    let node_version = Command::new(&node_path)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    #[cfg(windows)]
    let npm_version = hidden_cmd::cmd().arg("/C").arg(&npm_path).arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    #[cfg(not(windows))]
    let npm_version = Command::new(&npm_path)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    (node_path, npm_cli, npm_path, node_version, npm_version)
}

/// 解析系统 Node（GUI 进程 PATH 可能为空）
fn find_node_executable() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("where.exe");
        if let Ok(out) = cmd.arg("node.exe").output() {
            if out.status.success() {
                let line = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())?;
                let p = PathBuf::from(line);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            let p = PathBuf::from(&pf).join("nodejs").join("node.exe");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // macOS GUI 应用 PATH 经常不完整，直接遍历所有常见目录查找
        let common_node_paths = [
            "/usr/local/bin/node",            // 官网 pkg 安装
            "/opt/homebrew/bin/node",         // Homebrew 安装
            "/opt/homebrew/opt/node/bin/node",
            "/opt/local/bin/node",             // MacPorts
            "/usr/local/opt/node/bin/node",   // 常用备选
        ];
        for p in &common_node_paths {
            let path = Path::new(p);
            if path.is_file() {
                if Command::new(p)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::info!("find_node_executable 找到 Node: {}", p);
                    return Some(PathBuf::from(p));
                }
            }
        }
        // 搜索 ~/.nvm/versions/node/*/bin/node
        if let Ok(home) = std::env::var("HOME") {
            let nvm_dir = PathBuf::from(&home).join(".nvm").join("versions").join("node");
            if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let node_path = entry.path().join("bin").join("node");
                    if node_path.is_file() {
                        if Command::new(&node_path)
                            .arg("--version")
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false)
                        {
                            tracing::info!("find_node_executable 找到 nvm Node: {}", node_path.display());
                            return Some(node_path);
                        }
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("which")
            .arg("node")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(|l| PathBuf::from(l.trim()))
            })
    }
}

fn find_npm_cmd_full_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("where.exe");
        if let Ok(out) = cmd.arg("npm.cmd").output() {
            if out.status.success() {
                let line = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())?;
                let p = PathBuf::from(line);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            let p = PathBuf::from(&pf).join("nodejs").join("npm.cmd");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // macOS GUI 应用 PATH 经常不完整，直接遍历所有常见目录查找
        // npm 通常在 node 同级目录，或在 node 安装目录的 node_modules/npm/bin/
        let common_npm_paths = [
            "/usr/local/bin/npm",              // 官网 pkg 安装
            "/opt/homebrew/bin/npm",          // Homebrew 安装
            "/opt/homebrew/opt/node/bin/npm",
            "/opt/local/bin/npm",              // MacPorts
            "/usr/local/opt/node/bin/npm",    // 常用备选
        ];
        for p in &common_npm_paths {
            let path = Path::new(p);
            if path.is_file() {
                tracing::info!("find_npm_cmd_full_path 找到 npm: {}", p);
                return Some(PathBuf::from(p));
            }
        }
        // 搜索 ~/.nvm/versions/node/*/bin/npm
        if let Ok(home) = std::env::var("HOME") {
            let nvm_dir = PathBuf::from(&home).join(".nvm").join("versions").join("node");
            if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let npm_path = entry.path().join("bin").join("npm");
                    if npm_path.is_file() {
                        tracing::info!("find_npm_cmd_full_path 找到 nvm npm: {}", npm_path.display());
                        return Some(npm_path);
                    }
                    // 也检查 node_modules/npm/bin/npm-cli.js
                    let npm_cli_path = entry.path().join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js");
                    if npm_cli_path.is_file() {
                        tracing::info!("find_npm_cmd_full_path 找到 nvm npm-cli.js: {}", npm_cli_path.display());
                        return Some(npm_cli_path);
                    }
                }
            }
        }
        // 如果以上都找不到，尝试从已知的 node 路径推断 npm 位置
        let common_node_paths = [
            "/usr/local/bin/node",
            "/opt/homebrew/bin/node",
            "/opt/homebrew/opt/node/bin/node",
        ];
        for node_path in &common_node_paths {
            let node_bin = Path::new(node_path);
            if node_bin.is_file() {
                // npm 可能在同目录
                let npm_in_bin = node_bin.parent().unwrap().join("npm");
                if npm_in_bin.is_file() {
                    return Some(npm_in_bin);
                }
                // 或在 node_modules/npm/bin/
                let npm_in_nm = node_bin.parent().unwrap()
                    .join("node_modules").join("npm").join("bin").join("npm-cli.js");
                if npm_in_nm.is_file() {
                    return Some(npm_in_nm);
                }
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("which")
            .arg("npm")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(|l| PathBuf::from(l.trim()))
            })
    }
}

/// 在 GUI 进程中 PATH 常不含全局 npm/pnpm，需显式解析 pnpm 路径。
fn find_pnpm_executable() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("where.exe");
        if let Ok(out) = cmd.arg("pnpm.cmd").output() {
            if out.status.success() {
                let line = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())?;
                let p = PathBuf::from(line);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = PathBuf::from(appdata).join("npm").join("pnpm.cmd");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // macOS GUI 应用 PATH 经常不完整，直接遍历所有常见目录查找
        let common_pnpm_paths = [
            "/usr/local/bin/pnpm",              // 官网 pkg 安装
            "/opt/homebrew/bin/pnpm",          // Homebrew 安装
            "/opt/homebrew/opt/pnpm/bin/pnpm",
            "/opt/local/bin/pnpm",              // MacPorts
            "/usr/local/opt/pnpm/bin/pnpm",    // 常用备选
        ];
        for p in &common_pnpm_paths {
            let path = Path::new(p);
            if path.is_file() {
                tracing::info!("find_pnpm_executable 找到 pnpm: {}", p);
                return Some(PathBuf::from(p));
            }
        }
        // 搜索 ~/.nvm/versions/node/*/bin/pnpm
        if let Ok(home) = std::env::var("HOME") {
            let nvm_dir = PathBuf::from(&home).join(".nvm").join("versions").join("node");
            if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let pnpm_path = entry.path().join("bin").join("pnpm");
                    if pnpm_path.is_file() {
                        tracing::info!("find_pnpm_executable 找到 nvm pnpm: {}", pnpm_path.display());
                        return Some(pnpm_path);
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("which")
            .arg("pnpm")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(|l| PathBuf::from(l.trim()))
            })
    }
}

/// 解析 semver 主版本号（已废弃，保留以防将来 npm install 回退时重新启用）
#[allow(dead_code)]
fn parse_major(v: &str) -> u32 {
    v.trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// npm registry 包名路径段（scoped 包需把 `/` 编成 `%2F`）
fn registry_path_segment(pkg_name: &str) -> String {
    pkg_name.replace('/', "%2F")
}

fn registry_base_url(override_url: &str) -> String {
    let t = override_url.trim();
    if t.is_empty() {
        "https://registry.npmjs.org".to_string()
    } else {
        t.trim_end_matches('/').to_string()
    }
}

fn resolve_version_from_metadata(
    meta: &serde_json::Value,
    version_tag: &str,
) -> Result<String, String> {
    let tag = version_tag.trim();
    if tag.is_empty() || tag == "latest" {
        return meta
            .pointer("/dist-tags/latest")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "registry 元数据缺少 dist-tags.latest".to_string());
    }
    if let Some(versions) = meta.get("versions") {
        if versions.get(tag).is_some() {
            return Ok(tag.to_string());
        }
    }
    if let Some(tags) = meta.get("dist-tags") {
        if let Some(v) = tags.get(tag).and_then(|x| x.as_str()) {
            return Ok(v.to_string());
        }
    }
    Err(format!("registry 中找不到版本或标签: {}", tag))
}

fn tarball_url_from_metadata(meta: &serde_json::Value, ver: &str) -> Result<String, String> {
    let key = format!("/versions/{}/dist/tarball", ver);
    meta.pointer(&key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少 tarball 地址: {}", key))
}

/// 用 `npm pack` 拉取 registry 包并解压到 `openclaw_dir`，**不经过** `npm install --prefix`。
/// Windows 上部分环境在 `npm install` 时会触发 `@npmcli/arborist` 的 `realpathCached` 无限递归
///（`Maximum call stack size exceeded`）；`npm pack` 路径通常可绕过该问题。
fn fetch_openclaw_via_npm_pack_blocking(
    data_base: &str,
    pkg_spec: &str,
    registry_override: &str,
    openclaw_dir: &str,
    prefer_system_node: bool,
) -> Result<(), String> {
    use std::fs;

    let (node_exe, npm_cli, npm_cmd_fallback, _, _) =
        resolve_npm_exe_with_version(data_base, prefer_system_node);
    let npm_cmd = npm_cmd_fallback;

    let base = data_base.trim_end_matches(|c| c == '/' || c == '\\');
    let cache_root = PathBuf::from(base).join(".cache").join("npm-pack-openclaw");
    fs::create_dir_all(&cache_root).map_err(|e| e.to_string())?;
    let pack_dest = cache_root.join(format!("pack-{}", std::process::id()));
    if pack_dest.exists() {
        let _ = fs::remove_dir_all(&pack_dest);
    }
    fs::create_dir_all(&pack_dest).map_err(|e| e.to_string())?;
    let pack_dest_str = pack_dest.to_string_lossy().to_string();

    let pack_args = [
        "pack",
        pkg_spec,
        "--pack-destination",
        pack_dest_str.as_str(),
    ];

    #[cfg(windows)]
    let mut cmd: Command = if let Some(ref cli) = npm_cli {
        let mut c = hidden_cmd::cmd();
        c.arg("/C").arg(&node_exe).arg(cli).args(pack_args);
        c
    } else {
        let mut c = hidden_cmd::cmd();
        c.args(["/C", &npm_cmd.to_string_lossy()]).args(pack_args);
        c
    };
    #[cfg(not(windows))]
    let mut cmd: Command = if let Some(ref cli) = npm_cli {
        let mut c = Command::new(&node_exe);
        c.arg(cli).args(pack_args);
        c
    } else {
        let mut c = Command::new(&npm_cmd);
        c.args(pack_args);
        c
    };
    let reg = registry_override.trim();
    let deps_env_path = build_deps_env_path(data_base);
    if !reg.is_empty() {
        cmd.env("npm_config_registry", reg);
    }
    cmd.env("PATH", &deps_env_path);

    let npm_out = cmd
        .output()
        .map_err(|e| format!("启动 npm pack 失败: {}", e))?;

    if !npm_out.status.success() {
        let stderr = String::from_utf8_lossy(&npm_out.stderr);
        let stdout = String::from_utf8_lossy(&npm_out.stdout);
        let _ = fs::remove_dir_all(&pack_dest);
        return Err(format!("npm pack 失败: {}\n{}", stderr, stdout));
    }

    let tgz_files: Vec<PathBuf> = fs::read_dir(&pack_dest)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tgz"))
        .collect();

    if tgz_files.len() != 1 {
        let _ = fs::remove_dir_all(&pack_dest);
        return Err(format!(
            "npm pack 后应产生 1 个 .tgz，实际 {} 个",
            tgz_files.len()
        ));
    }

    let bytes = fs::read(&tgz_files[0]).map_err(|e| e.to_string())?;

    let dest = Path::new(openclaw_dir);
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| e.to_string())?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;

    unpack_npm_tarball(&bytes, dest).map_err(|e| {
        let _ = fs::remove_dir_all(&pack_dest);
        e
    })?;

    let _ = fs::remove_dir_all(&pack_dest);
    Ok(())
}

/// 直接从 registry 拉取包 tarball 并解压到 `openclaw_dir`，**不经过** `npm install <pkg>`，可彻底避开 npm Arborist 栈溢出。
async fn fetch_openclaw_via_registry_tarball(
    app: &AppHandle,
    data_base: &str,
    openclaw_dir: &str,
    pkg_name: &str,
    version_tag: &str,
    registry_override: &str,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent("openclaw-cn-manager/1.0")
        .connect_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(900))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败: {}", e))?;

    let base = registry_base_url(registry_override);
    let meta_url = format!("{}/{}", base, registry_path_segment(pkg_name));

    emit(
        app,
        InstallProgressEvent::detail(
            "openclaw-install",
            &format!(
                "优先策略: registry tarball（绕过 npm 依赖解析器） {}",
                meta_url
            ),
        ),
    );

    let meta_text = client
        .get(&meta_url)
        .send()
        .await
        .map_err(|e| format!("获取包元数据失败: {}", e))?
        .error_for_status()
        .map_err(|e| format!("registry 返回错误: {}", e))?
        .text()
        .await
        .map_err(|e| format!("读取元数据失败: {}", e))?;

    let meta: serde_json::Value =
        serde_json::from_str(&meta_text).map_err(|e| format!("解析 registry JSON 失败: {}", e))?;

    let ver = resolve_version_from_metadata(&meta, version_tag)?;
    let tb_url = tarball_url_from_metadata(&meta, &ver)?;

    emit(
        app,
        InstallProgressEvent::detail(
            "openclaw-install",
            &format!("正在下载 {}@{} …", pkg_name, ver),
        ),
    );

    let bytes = client
        .get(&tb_url)
        .send()
        .await
        .map_err(|e| format!("下载 tarball 失败: {}", e))?
        .error_for_status()
        .map_err(|e| format!("tarball HTTP 错误: {}", e))?
        .bytes()
        .await
        .map_err(|e| format!("读取 tarball 流失败: {}", e))?;

    let extract_root = format!(
        "{}/.openclaw-tarball-extract",
        data_base.trim_end_matches(|c| c == '/' || c == '\\')
    );
    let _ = tokio::fs::remove_dir_all(&extract_root).await;
    tokio::fs::create_dir_all(&extract_root)
        .await
        .map_err(|e| format!("创建临时解压目录失败: {}", e))?;

    let root_path = PathBuf::from(&extract_root);
    let bytes_vec = bytes.to_vec();

    tokio::task::spawn_blocking(move || {
        let cursor = Cursor::new(bytes_vec);
        let dec = flate2::read::GzDecoder::new(cursor);
        let mut archive = tar::Archive::new(dec);
        archive
            .unpack(&root_path)
            .map_err(|e| format!("解压 tarball 失败: {}", e))
    })
    .await
    .map_err(|e| format!("解压任务失败: {}", e))??;

    let pkg_folder = Path::new(&extract_root).join("package");
    if !pkg_folder.is_dir() {
        return Err("解压后未找到 package/ 目录".to_string());
    }

    if Path::new(openclaw_dir).exists() {
        emit(
            app,
            InstallProgressEvent::detail(
                "openclaw-install",
                "正在删除旧的 openclaw-cn 目录以替换为新包（文件多或杀毒实时扫描时可能持续数分钟，界面会静默）…",
            ),
        );
        tokio::fs::remove_dir_all(openclaw_dir)
            .await
            .map_err(|e| format!("删除旧目录失败: {}", e))?;
    }
    if let Some(parent) = Path::new(openclaw_dir).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建父目录失败: {}", e))?;
    }

    tokio::fs::rename(&pkg_folder, openclaw_dir)
        .await
        .map_err(|e| format!("移动到目标目录失败: {}", e))?;

    let _ = tokio::fs::remove_dir_all(&extract_root).await;

    emit(
        app,
        InstallProgressEvent::detail(
            "openclaw-install",
            &format!("registry tarball 安装完成 {}@{}", pkg_name, ver),
        ),
    );
    Ok(())
}

/// 通过 `npm install <包名>` 从 npm 注册表拉取 openclaw-cn 包到目标目录。
/// - 优先使用 data/env/node 目录下的 npm，避免 PATH 版本不一致
/// - 失败时将 stderr 摘要写入返回错误，供 UI 红框展示根因
async fn fetch_openclaw_via_npm_pkg(
    app: &AppHandle,
    data_base: &str,
    openclaw_dir: &str,
    pkg_name: &str,
    version_tag: &str,
    _allow_scripts: bool,
    prefer_system_node: bool,
    registry_override: &str,
    _legacy_peer_deps: bool,
) -> Result<(), String> {
    let _staging = format!(
        "{}/.openclaw-install-staging",
        data_base.trim_end_matches(|c| c == '/' || c == '\\')
    );

    let pkg_full = if version_tag.is_empty() || version_tag == "latest" {
        pkg_name.to_string()
    } else {
        format!("{}@{}", pkg_name, version_tag)
    };

    emit(
        app,
        InstallProgressEvent::started(
            "openclaw-install",
            &format!(
                "安装 {}（优先 registry tarball，失败再回退 npm；prefer_system_node={}）…",
                pkg_full, prefer_system_node
            ),
        ),
    );

    match fetch_openclaw_via_registry_tarball(
        app,
        data_base,
        openclaw_dir,
        pkg_name,
        version_tag,
        registry_override,
    )
    .await
    {
        Ok(()) => {
            emit(
                app,
                InstallProgressEvent::finished("openclaw-install", "registry tarball 安装完成"),
            );
            return Ok(());
        }
        Err(e) => {
            warn!("registry tarball 路径失败: {}", e);
            emit(
                app,
                InstallProgressEvent::detail(
                    "openclaw-install",
                    &format!(
                        "registry tarball 不可用（{}）；将尝试 npm pack（避免部分环境下 npm install 栈溢出）…",
                        e
                    ),
                ),
            );
        }
    }

    // tarball 失败时优先 npm pack：仅下载包 tarball，不走 `npm install --prefix` 的完整依赖树解析
    let db = data_base.to_string();
    let pkg = pkg_full.clone();
    let reg = registry_override.to_string();
    let odir = openclaw_dir.to_string();
    let prefer = prefer_system_node;
    match tokio::task::spawn_blocking(move || {
        fetch_openclaw_via_npm_pack_blocking(&db, &pkg, &reg, &odir, prefer)
    })
    .await
    {
        Ok(Ok(())) => {
            emit(
                app,
                InstallProgressEvent::finished(
                    "openclaw-install",
                    "npm pack 获取包完成（已避开 npm install 拉包阶段）",
                ),
            );
            return Ok(());
        }
        Ok(Err(e_pack)) => {
            warn!("npm pack 获取 openclaw 失败，无其他回退方案: {}", e_pack);
            emit(
                app,
                InstallProgressEvent::failed(
                    "openclaw-install",
                    &format!(
                        "npm pack 也失败（{}）。\n\n\
                         registry tarball 下载已尝试，npm pack 也失败。\n\
                         可能原因：网络问题、registry 配置错误、npm 版本过低。\n\
                         建议：① 检查 app.yaml 中 registry 是否正确；② 确认网络可访问 npm registry；\
                         ③ 手动在 CMD 中运行 `npm pack openclaw-cn --pack-destination D:\\tmp` 测试是否正常。",
                        e_pack
                    ),
                ),
            );
            return Err(format!(
                "获取 openclaw-cn 包失败。registry tarball 失败，npm pack 也失败:\n{}",
                e_pack
            ));
        }
        Err(e_join) => {
            warn!("npm pack 任务 join 失败: {}", e_join);
            emit(
                app,
                InstallProgressEvent::failed(
                    "openclaw-install",
                    &format!(
                        "npm pack 任务异常: {}。registry tarball 失败后无可用回退方案。",
                        e_join
                    ),
                ),
            );
            return Err(format!(
                "npm pack 任务异常: {}。registry tarball 失败后无可用回退方案。",
                e_join
            ));
        }
    }
}

// 安装 OpenClaw-CN
#[tauri::command]
pub async fn install_openclaw(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
    version: Option<String>,
    force_reinstall: Option<bool>,
) -> Result<Vec<InstallProgress>, String> {
    info!("开始安装 OpenClaw-CN...");

    let mut progress = Vec::new();
    let data_base = data_dir.inner().get_data_dir();
    let openclaw_dir = format!("{}/openclaw-cn", data_base);

    emit(
        &app,
        InstallProgressEvent::detail("openclaw-install", "后端已收到安装请求，正在读取 app.yaml…"),
    );
    tokio::task::yield_now().await;

    // 从 app.yaml 读取配置，命令行参数 version 优先
    let (cfg_pkg, cfg_tag, cfg_registry, cfg_allow_scripts, cfg_prefer_system, cfg_legacy_peer) =
        crate::commands::config::parse_openclaw_config(&data_base);
    let pkg_name = cfg_pkg;
    let version_tag = version.unwrap_or_else(|| cfg_tag);

    let registry_hint = if !cfg_registry.trim().is_empty() {
        format!("（registry: {}）", cfg_registry.trim())
    } else {
        String::from("（registry: NPM_CONFIG_REGISTRY / .npmrc / 默认 npmjs.org）")
    };

    emit(
        &app,
        InstallProgressEvent::detail(
            "openclaw-install",
            &format!(
                "安装分两步：① 获取 {} 到目录（registry tarball 优先）；② 在目录内安装 package.json 依赖。{} 目标：{}",
                pkg_name,
                registry_hint,
                openclaw_dir.replace('\\', "/")
            ),
        ),
    );
    tokio::task::yield_now().await;

    let force = force_reinstall.unwrap_or(false);
    if !force && openclaw_core_ready(&openclaw_dir) && openclaw_deps_ready(&openclaw_dir) {
        emit(
            &app,
            InstallProgressEvent::finished(
                "openclaw-install",
                "OpenClaw-CN 已完整安装（dist/entry.js 与 node_modules 就绪），跳过下载与依赖安装。若需强制重装请使用「重新安装」。",
            ),
        );
        progress.push(InstallProgress {
            step: "openclaw-pkg".to_string(),
            progress: 100.0,
            message: "当前安装已完整，跳过获取程序包".to_string(),
            status: "skipped".to_string(),
        });
        progress.push(InstallProgress {
            step: "openclaw-deps".to_string(),
            progress: 100.0,
            message: "当前依赖已就绪，跳过 npm/pnpm install".to_string(),
            status: "skipped".to_string(),
        });
        progress.push(InstallProgress {
            step: "openclaw-init".to_string(),
            progress: 100.0,
            message: "向导内配置初始化完成（可进入下一步）".to_string(),
            status: "success".to_string(),
        });
        match patch_openclaw_broken_modules(&openclaw_dir).await {
            Ok(()) => {
                progress.push(InstallProgress {
                    step: "patch-broken-modules".to_string(),
                    progress: 100.0,
                    message: "已修复 openclaw-cn 损坏模块".to_string(),
                    status: "success".to_string(),
                });
            }
            Err(e) => {
                warn!("Patch openclaw broken modules failed (non-fatal): {}", e);
                progress.push(InstallProgress {
                    step: "patch-broken-modules".to_string(),
                    progress: 100.0,
                    message: format!("修复损坏模块失败（不影响运行）: {}", e),
                    status: "warning".to_string(),
                });
            }
        }
        info!("OpenClaw-CN 安装流程已跳过（本机已就绪）");
        return Ok(progress);
    }

    // ── Step 0（可选）：检测内置预构建包，优先直接解压，跳过网络拉包 ──
    let bundled_tarball = resolve_bundled_openclaw_tarball(&app);
    let mut step0_bundled_ok = false;
    if let Some(bundled_path) = bundled_tarball {
        emit(
            &app,
            InstallProgressEvent::started(
                "openclaw-pkg",
                &format!(
                    "检测到内置 openclaw-cn 包（{}），优先使用离线解压",
                    bundled_path.display()
                ),
            ),
        );

        match tokio::task::spawn_blocking({
            let bundled_path = bundled_path.clone();
            let openclaw_dir = openclaw_dir.clone();
            let data_base = data_base.clone();
            move || -> Result<(), String> {
                let bytes = std::fs::read(&bundled_path)
                    .map_err(|e| format!("读取内置 tarball 失败: {}", e))?;
                let extract_root = format!(
                    "{}/.openclaw-tarball-extract",
                    data_base.trim_end_matches(|c| c == '/' || c == '\\')
                );
                let _ = std::fs::remove_dir_all(&extract_root);
                std::fs::create_dir_all(&extract_root)
                    .map_err(|e| format!("创建临时解压目录失败: {}", e))?;

                let cursor = std::io::Cursor::new(bytes);
                let dec = flate2::read::GzDecoder::new(cursor);
                let mut archive = tar::Archive::new(dec);
                archive
                    .unpack(&extract_root)
                    .map_err(|e| format!("解压内置 tarball 失败: {}", e))?;

                let pkg_folder = std::path::Path::new(&extract_root).join("package");
                if !pkg_folder.is_dir() {
                    return Err("内置 tarball 解压后未找到 package/ 目录".to_string());
                }

                if std::path::Path::new(&openclaw_dir).exists() {
                    std::fs::remove_dir_all(&openclaw_dir)
                        .map_err(|e| format!("删除旧目录失败: {}", e))?;
                }
                std::fs::rename(&pkg_folder, &openclaw_dir)
                    .map_err(|e| format!("移动到目标目录失败: {}", e))?;
                std::fs::remove_dir_all(&extract_root).ok();
                Ok(())
            }
        })
        .await
        .map_err(|e| format!("内置包解压任务失败: {}", e))?
        {
            Ok(()) => {
                emit(
                    &app,
                    InstallProgressEvent::finished(
                        "openclaw-pkg",
                        &format!(
                            "内置 openclaw-cn 包解压完成（{}），跳过 registry 下载",
                            pkg_name
                        ),
                    ),
                );
                progress.push(InstallProgress {
                    step: "openclaw-pkg".to_string(),
                    progress: 100.0,
                    message: format!(
                        "内置包解压完成（{}），node_modules 将在下一步通过 npm/pnpm install 生成",
                        pkg_name
                    ),
                    status: "success".to_string(),
                });
                step0_bundled_ok = true;
            }
            Err(e) => {
                warn!("内置 tarball 解压失败，降级为 registry 下载: {}", e);
                emit(
                    &app,
                    InstallProgressEvent::detail(
                        "openclaw-pkg",
                        &format!("内置包解压失败（{}），将降级为 registry 下载…", e),
                    ),
                );
            }
        }
    }
    // ── Step 0 end ──

    // 全程心跳：原先仅在 npm install 阶段发送，导致「拉包 / 删旧目录」阶段界面长期无日志，易被误认为卡死。
    let app_hb = app.clone();
    let _install_heartbeat = InstallHeartbeatGuard(tokio::spawn(async move {
        let mut secs = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            secs += 15;
            let _ = app_hb.emit(
                "install-progress",
                InstallProgressEvent::detail(
                    "openclaw-install",
                    &format!(
                        "安装仍在进行…已约 {} 秒。下载大包、杀毒扫描或删除旧 openclaw-cn 时可能长时间无新行，可打开数据目录下 logs/app.log 查看后端日志。",
                        secs
                    ),
                ),
            );
        }
    }));

    // Step 0 已优先尝试内置包；若成功则跳过本 Step 1（registry 下载）
    if !step0_bundled_ok {
        // Step 1: 获取本体 —— 仅当已有完整 dist/entry.js 时才跳过拉包；否则目录里可能是中断安装留下的空壳（无 dist），必须重新拉取。
        let dir_exists = std::path::Path::new(&openclaw_dir).exists();
        let core_ok = openclaw_core_ready(&openclaw_dir);
        if dir_exists && core_ok {
            progress.push(InstallProgress {
            step: "openclaw-pkg".to_string(),
            progress: 100.0,
            message: format!(
                "已存在完整 {}（含 dist/entry.js），跳过拉取包；接下来仅为该目录安装/补齐 node_modules",
                pkg_name
            ),
            status: "skipped".to_string(),
        });
        } else {
            if dir_exists && !core_ok {
                emit(
                &app,
                InstallProgressEvent::detail(
                    "openclaw-install",
                    "检测到 openclaw-cn 目录存在但缺少 dist/entry.js（依赖安装曾失败或拷贝不完整），将重新拉取程序包…",
                ),
            );
            }
            match fetch_openclaw_via_npm_pkg(
                &app,
                &data_base,
                &openclaw_dir,
                &pkg_name,
                &version_tag,
                cfg_allow_scripts,
                cfg_prefer_system,
                cfg_registry.trim(),
                cfg_legacy_peer,
            )
            .await
            {
                Ok(()) => {
                    progress.push(InstallProgress {
                        step: "openclaw-pkg".to_string(),
                        progress: 100.0,
                        message: format!("{} 已获取到 openclaw-cn（含 package.json）", pkg_name),
                        status: "success".to_string(),
                    });
                }
                Err(e) => {
                    progress.push(InstallProgress {
                        step: "openclaw-pkg".to_string(),
                        progress: 0.0,
                        message: format!("获取包失败: {}", e),
                        status: "error".to_string(),
                    });
                    return Err(e);
                }
            }
        }
    } // end Step 1 (skipped when step0_bundled_ok)

    // Step 2: 在 openclaw-cn 内安装依赖（即 package.json 里的 dependencies 等，生成 node_modules）
    if openclaw_deps_ready(&openclaw_dir) {
        emit(
            &app,
            InstallProgressEvent::finished(
                "openclaw-deps",
                "检测到 node_modules 已就绪，跳过 pnpm/npm install",
            ),
        );
        progress.push(InstallProgress {
            step: "openclaw-deps".to_string(),
            progress: 100.0,
            message: "node_modules 已存在且含核心依赖，跳过重复安装以加快向导".to_string(),
            status: "skipped".to_string(),
        });
    } else {
        emit(
            &app,
            InstallProgressEvent::started(
                "openclaw-deps",
                "正在执行依赖安装（npm/pnpm install，仓库较大时可能需数分钟）…",
            ),
        );

        let (node_exe, npm_cli, npm_cmd, _, _) =
            resolve_npm_exe_with_version(&data_base, cfg_prefer_system);
        let pnpm_path = find_pnpm_executable();
        let pnpm_path_for_fallback = pnpm_path.clone();
        let use_pnpm = pnpm_path.is_some();

        let mut args = vec!["install".to_string()];
        // npm 默认加 --legacy-peer-deps：openclaw-cn 的 oxlint 生态有 peer-dep 冲突（oxlint-tsgolint），
        // 不加会 ERESOLVE。pnpm 自动忽略 peer 冲突，因此只对 npm 生效。
        // cfg_legacy_peer 为保留的用户可配置开关（true 时才加 force，覆盖默认行为）。
        if !use_pnpm {
            if cfg_legacy_peer {
                args.push("--force".to_string());
            } else {
                args.push("--legacy-peer-deps".to_string());
            }
        }
        if !cfg_allow_scripts {
            args.push("--ignore-scripts".to_string());
        }
        let openclaw_dir_for_task = openclaw_dir.clone();
        let registry_for_deps = cfg_registry.trim().to_string();
        // 构建子进程 PATH：prepend 自包含 Node/Git，避免生命周期脚本（postinstall 等）找不到 node/git
        let deps_env_path = build_deps_env_path(&data_base);
        let deps_env_path_clone = deps_env_path.clone();

        let deps_hint = if let Some(ref pp) = pnpm_path {
            format!("pnpm {}", pp.display())
        } else if npm_cli.is_some() {
            format!("{} + npm-cli.js", node_exe.display())
        } else {
            format!("npm {}", npm_cmd.display())
        };
        let allow_scripts_hint = if cfg_allow_scripts {
            "允许运行生命周期脚本（postinstall 等）".to_string()
        } else {
            "已禁用生命周期脚本（--ignore-scripts）".to_string()
        };
        emit(
        &app,
        InstallProgressEvent::detail(
            "openclaw-deps",
            &format!(
                "使用绝对路径执行依赖安装（避免 GUI 进程 PATH 缺少 npm）：{}\n子进程 PATH prepend: {}（使 node/git 可被 postinstall 找到）\n{}",
                deps_hint,
                build_deps_env_path(&data_base).split(';').next().unwrap_or(""),
                allow_scripts_hint
            ),
        ),
    );

        let output_result = tokio::task::spawn_blocking(move || {
            let apply_env = |c: &mut Command| {
                c.env("PATH", &deps_env_path);
                if !registry_for_deps.is_empty() {
                    c.env("npm_config_registry", &registry_for_deps);
                }
            };

            if let Some(ref pp) = pnpm_path {
                let mut c = Command::new(pp);
                c.current_dir(&openclaw_dir_for_task).args(&args);
                apply_env(&mut c);
                c.output()
            } else if let Some(ref cli) = npm_cli {
                let mut c = Command::new(&node_exe);
                c.arg(cli).current_dir(&openclaw_dir_for_task).args(&args);
                apply_env(&mut c);
                c.output()
            } else if cfg!(windows) {
                let mut c = hidden_cmd::cmd();
                c.args(["/C"])
                    .arg(&npm_cmd)
                    .current_dir(&openclaw_dir_for_task)
                    .args(&args);
                apply_env(&mut c);
                c.output()
            } else {
                let mut c = Command::new(&npm_cmd);
                c.current_dir(&openclaw_dir_for_task).args(&args);
                apply_env(&mut c);
                c.output()
            }
        })
        .await;

        let output = output_result
        .map_err(|e| format!("依赖安装任务失败: {}", e))?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "依赖安装失败: 找不到程序（NotFound）。已尝试: {}。请安装 Node.js 或将内置 Node 装到 data/env/node。",
                    deps_hint
                )
            } else {
                format!("依赖安装失败: 无法启动包管理器: {}", e)
            }
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            // npm 失败时若有 pnpm，尝试 pnpm 兜底（不使用 spawn_blocking，避免变量已被 move）
            if let Some(ref pp) = pnpm_path_for_fallback {
                warn!("npm install 失败，尝试 pnpm 兜底: {}", stderr);
                emit(
                    &app,
                    InstallProgressEvent::detail(
                        "openclaw-deps",
                        &format!(
                            "npm install 失败（{}），尝试 pnpm 兜底...",
                            stderr.lines().next().unwrap_or("")
                        ),
                    ),
                );

                let mut fb_args = vec!["install".to_string()];
                if !cfg_allow_scripts {
                    fb_args.push("--ignore-scripts".to_string());
                }
                let mut fb_cmd = Command::new(pp);
                fb_cmd.current_dir(&openclaw_dir).args(&fb_args);
                fb_cmd.env("PATH", &deps_env_path_clone);
                if !cfg_registry.trim().is_empty() {
                    fb_cmd.env("npm_config_registry", cfg_registry.trim());
                }

                let fb_out = fb_cmd
                    .output()
                    .map_err(|e| format!("pnpm 启动失败: {}", e))?;

                if fb_out.status.success() {
                    emit(
                        &app,
                        InstallProgressEvent::finished("openclaw-deps", "pnpm 兜底安装成功"),
                    );
                    progress.push(InstallProgress {
                        step: "openclaw-deps".to_string(),
                        progress: 100.0,
                        message: "pnpm 兜底安装成功，node_modules 就绪".to_string(),
                        status: "success".to_string(),
                    });
                } else {
                    let fb_err = String::from_utf8_lossy(&fb_out.stderr);
                    warn!("pnpm 兜底也失败: {}", fb_err);
                    emit(&app, InstallProgressEvent::failed("openclaw-deps", &fb_err));
                    progress.push(InstallProgress {
                        step: "openclaw-deps".to_string(),
                        progress: 0.0,
                        message: format!(
                            "npm 失败（{}），pnpm 兜底也失败: {}",
                            stderr.lines().next().unwrap_or(""),
                            fb_err.lines().next().unwrap_or("")
                        ),
                        status: "error".to_string(),
                    });
                    let combined = format!("{}\n{}", stderr, fb_err);
                    let extra = npm_deps_permission_hint(&combined);
                    return Err(format!(
                        "npm 失败（{}），pnpm 兜底也失败: {}{}",
                        stderr.lines().next().unwrap_or(""),
                        fb_err.lines().next().unwrap_or(""),
                        extra
                    ));
                }
            } else {
                warn!("依赖安装失败，无 pnpm 可兜底: {}", stderr);
                emit(&app, InstallProgressEvent::failed("openclaw-deps", &stderr));
                let hint = npm_deps_permission_hint(&stderr);
                progress.push(InstallProgress {
                    step: "openclaw-deps".to_string(),
                    progress: 0.0,
                    message: format!(
                        "依赖安装失败（npm；建议安装 pnpm 或检查 app.yaml prefer_system_node）: {}",
                        stderr
                    ),
                    status: "error".to_string(),
                });
                return Err(format!("依赖安装失败: {}{}", stderr, hint));
            }
        }

        emit(
            &app,
            InstallProgressEvent::finished("openclaw-deps", "依赖安装完成"),
        );
        progress.push(InstallProgress {
            step: "openclaw-deps".to_string(),
            progress: 100.0,
            message: "已在 openclaw-cn 目录执行 pnpm/npm install，node_modules 就绪".to_string(),
            status: "success".to_string(),
        });
    }

    // Step 3: 初始化配置（占位，后续可写默认配置）
    progress.push(InstallProgress {
        step: "openclaw-init".to_string(),
        progress: 100.0,
        message: "向导内配置初始化完成（可进入下一步）".to_string(),
        status: "success".to_string(),
    });

    // Step 4: Patch broken modules in openclaw-cn dist (non-fatal)
    match patch_openclaw_broken_modules(&openclaw_dir).await {
        Ok(()) => {
            progress.push(InstallProgress {
                step: "patch-broken-modules".to_string(),
                progress: 100.0,
                message: "已修复 openclaw-cn 损坏模块".to_string(),
                status: "success".to_string(),
            });
            info!("Patched openclaw-cn broken modules");
        }
        Err(e) => {
            warn!("Patch openclaw broken modules failed (non-fatal): {}", e);
            progress.push(InstallProgress {
                step: "patch-broken-modules".to_string(),
                progress: 100.0,
                message: format!("修复损坏模块失败（不影响运行）: {}", e),
                status: "warning".to_string(),
            });
        }
    }

    info!("OpenClaw-CN 安装完成");

    // 清理可能冲突的全局 launchd 服务：之前用户机器上如果 `npm install -g @dragonforce2010/openclaw-cn`
    // 注册过 com.clawdbot.gateway，会与本 app 启动的本地 gateway 抢同一端口 18789，并持续刷 "Missing config"
    // 错误。这里 bootout 掉它（找不到也无所谓，返回码非 0 仅打 warn）。
    cleanup_stale_clawdbot_launchd_service();

    Ok(progress)
}

/// 卸载可能与本 app 冲突的全局 launchd 服务 `com.clawdbot.gateway`（来自
/// `npm install -g @dragonforce2010/openclaw-cn`）。该服务会反复重启并占用 18789 端口，
/// 造成本 app 启动的本地 gateway 启动后立即被 launchd 拉起的旧实例抢端口 + 持续刷 "Missing config" 日志。
pub fn cleanup_stale_clawdbot_launchd_service() {
    use std::process::Command;
    // 仅在 macOS 上有意义（launchd 是 macOS 专属）
    #[cfg(target_os = "macos")]
    {
        // 先查询是否存在；存在再 bootout，避免无谓的 stderr 输出
        let listed = Command::new("launchctl")
            .args(["list"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.contains("com.clawdbot.gateway"))
            .unwrap_or(false);

        if !listed {
            return;
        }

        match Command::new("launchctl")
            .args(["bootout", "gui/501/com.clawdbot.gateway"])
            .output()
        {
            Ok(o) if o.status.success() => {
                info!("已卸载冲突的全局 launchd 服务 com.clawdbot.gateway（来自旧版 npm install -g）");
            }
            Ok(o) => {
                warn!(
                    "bootout com.clawdbot.gateway 失败（exit={:?}）：{}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                warn!("无法调用 launchctl bootout：{}", e);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = Command::new("true");
    }
}

/// 通知 OS reap 任何 zombie 子进程，避免 openclaw-cn 的 lock 库误把僵尸当成"活进程"卡住新启动。
///
/// 背景：openclaw-cn 的 gateway-lock 用 `process.kill(pid, 0)` 判断 PID 是否存活。
/// 在 macOS / Linux 上，**僵尸进程**（stat = Z）会响应 `kill 0` 返回成功 → 旧 lock 永远不释放。
/// 父进程（本 app）只要有未 reap 的 zombie 残留，后续 start_gateway 都会
/// 报 "gateway already running (pid XXXX); lock timeout after 5000ms"。
///
/// 修复：发 SIGCHLD 给所有有 zombie 子进程的 PPID，触发其立即 reap。
/// 仅 Unix 有效；Windows 上无 zombie 概念。
pub fn reap_zombie_children() {
    #[cfg(unix)]
    {
        use std::process::Command;
        // ps 输出格式：PID STAT（默认带终端宽度时可能含多列，我们只关心前两列）
        // 找一个子进程 ID 仍在僵尸态的 PPID，对它发 SIGCHLD。
        // 用 sh 做 shell 解析最简单（避免依赖 regex crate）。
        let _ = Command::new("sh")
            .args([
                "-c",
                "ps -A -o pid=,stat= 2>/dev/null | awk '$2 ~ /^Z/ { print $1 }' | while read zpid; do \
                 # 找 zombie 的父进程 PPID 并发 SIGCHLD 触发 reap
                 ppid=$(ps -o ppid= -p $zpid 2>/dev/null | tr -d ' '); \
                 [ -n \"$ppid\" ] && kill -s SIGCHLD $ppid 2>/dev/null && true; \
                 done; true"
            ])
            .output();
        info!("已尝试 reap zombie 子进程（SIGCHLD → PPID）");
    }
}

// ─── Post-install patches for broken modules in openclaw-cn npm package ───────────

const GATEWAY_USAGE_PATCH_MARKER: &str = "openclaw-cn-manager: localhost gateway usage bypass";

/// 允许本机环回、仅网关 token 且无 device scopes 的连接调用只读用量 RPC（供管理端 WS 拉取用量）。
/// 与 `gateway_ws::call_gateway_method` 配套；安装与每次启动网关前各尝试一次（幂等）。
pub(crate) async fn patch_openclaw_gateway_localhost_usage(
    openclaw_dir: &str,
) -> Result<(), String> {
    let path = format!("{}/dist/gateway/server-methods.js", openclaw_dir);
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 server-methods.js 失败: {}", e))?;
    if content.contains(GATEWAY_USAGE_PATCH_MARKER) {
        return Ok(());
    }
    let needle = "    const scopes = client.connect.scopes ?? [];";
    if !content.contains(needle) {
        return Err("server-methods.js 中未找到 scopes 声明（上游结构可能已变）".to_string());
    }
    let inject = r#"    const scopes = client.connect.scopes ?? [];
    // openclaw-cn-manager: localhost gateway usage bypass — 本机 token 连接无 device scopes 时仍允许只读用量 RPC
    const _mgrIp = (client.clientIp ?? "").trim();
    const _mgrLoopback = _mgrIp === "127.0.0.1" || _mgrIp === "::1" || _mgrIp === "::ffff:127.0.0.1";
    const _mgrUsageMethods = new Set(["usage.status", "usage.cost", "sessions.usage", "sessions.usage.timeseries", "sessions.usage.logs"]);
    if (_mgrLoopback && scopes.length === 0 && _mgrUsageMethods.has(method)) {
        return null;
    }"#;
    let patched = content.replacen(needle, inject, 1);
    if patched == content {
        return Err("写入 server-methods.js 用量补丁时替换失败".to_string());
    }
    tokio::fs::write(&path, patched)
        .await
        .map_err(|e| format!("写入 server-methods.js 失败: {}", e))?;
    Ok(())
}

const SHELL_UTILS_MANAGER_HEUR_MARKER: &str =
    "OpenClaw-CN Manager patch: Windows exec shell (heuristic)";
const SHELL_UTILS_NODE_PS_LINES: &str =
    "    if (/\\bnode\\s+(-e|--eval)\\b/i.test(c))\n        return true;\n";

/// Windows：旧版启发式把 `node -e` 交给 PowerShell，嵌套引号下易 ParserError；移除该行后走 cmd.exe（幂等）。
pub(crate) async fn patch_shell_utils_drop_node_powershell(
    openclaw_dir: &str,
) -> Result<(), String> {
    let shell_utils_file = format!("{}/dist/agents/shell-utils.js", openclaw_dir);
    if !Path::new(&shell_utils_file).is_file() {
        return Ok(());
    }
    let su_on_disk = tokio::fs::read_to_string(&shell_utils_file)
        .await
        .map_err(|e| format!("读取 shell-utils.js 失败: {}", e))?;
    if su_on_disk.contains(SHELL_UTILS_MANAGER_HEUR_MARKER)
        && su_on_disk.contains(SHELL_UTILS_NODE_PS_LINES)
    {
        let upgraded = su_on_disk.replace(SHELL_UTILS_NODE_PS_LINES, "");
        if upgraded != su_on_disk {
            tokio::fs::write(&shell_utils_file, &upgraded)
                .await
                .map_err(|e| format!("升级 shell-utils.js 失败: {}", e))?;
            info!("Upgraded shell-utils.js (node -e no longer routed to PowerShell)");
        }
    }
    Ok(())
}

const WINDOWS_EXEC_CMD_QUOTE_PATCH_MARKER: &str = "export function normalizeWindowsExecCommand";

/// Windows：Node 用 `cmd /d /s /c` 执行时，`type \"D:\\path\\file\"` 常被 CMD 判语法错误；去掉无空格路径上的引号可修复。
/// 同时将 `type`/`dir`/`more`/`cmd` 起头的命令固定走 cmd，避免误用 PowerShell。
pub(crate) async fn patch_shell_utils_windows_exec_cmd_quoting(
    openclaw_dir: &str,
) -> Result<(), String> {
    let shell_utils_file = format!("{}/dist/agents/shell-utils.js", openclaw_dir);
    if !Path::new(&shell_utils_file).is_file() {
        return Ok(());
    }
    let mut su = tokio::fs::read_to_string(&shell_utils_file)
        .await
        .map_err(|e| format!("读取 shell-utils.js 失败: {}", e))?;
    if su.contains(WINDOWS_EXEC_CMD_QUOTE_PATCH_MARKER) {
        return Ok(());
    }
    const OLD_BATCH: &str = r#"function windowsExecLooksLikeCmdBatch(command) {
    const c = typeof command === "string" ? command.trim() : "";
    if (!c)
        return false;
    if (/2>nul\b|2>NUL\b/.test(c))
        return true;
    if (/\|\|/.test(c))
        return true;
    if (/%[A-Za-z0-9_]+%/.test(c))
        return true;
    return false;
}"#;
    const NEW_BATCH: &str = r#"function windowsExecLooksLikeCmdBatch(command) {
    const c = typeof command === "string" ? command.trim() : "";
    if (!c)
        return false;
    if (/2>nul\b|2>NUL\b/.test(c))
        return true;
    if (/\|\|/.test(c))
        return true;
    if (/%[A-Za-z0-9_]+%/.test(c))
        return true;
    if (/^\s*type\s/i.test(c))
        return true;
    if (/^\s*dir\s/i.test(c))
        return true;
    if (/^\s*more\s/i.test(c))
        return true;
    if (/^\s*cmd\s+/i.test(c))
        return true;
    return false;
}"#;
    const NORM_FN: &str = r##"
export function normalizeWindowsExecCommand(command) {
    if (process.platform !== "win32" || typeof command !== "string")
        return command;
    return command.replace(/\btype\s+"([^"\r\n]+)"/gi, (_m, p) => {
        const inner = String(p);
        if (/\s/.test(inner))
            return `type "${inner}"`;
        return `type ${inner}`;
    });
}
"##;
    if su.contains(OLD_BATCH) {
        su = su.replace(OLD_BATCH, &(NEW_BATCH.to_string() + NORM_FN));
    } else {
        warn!("shell-utils.js: 未找到可升级的 windowsExecLooksLikeCmdBatch，跳过 cmd 引号补丁");
        return Ok(());
    }
    tokio::fs::write(&shell_utils_file, &su)
        .await
        .map_err(|e| format!("写入 shell-utils.js (cmd 引号) 失败: {}", e))?;
    info!("Patched dist/agents/shell-utils.js (Windows type 引号 / CMD 启发式)");
    Ok(())
}

const BASH_EXEC_WIN_NORM_MARKER: &str = "openclaw-cn-manager: normalizeWindowsExecCommand";

/// 在 exec 进程启动前对 Windows 命令做规范化（依赖 shell-utils.normalizeWindowsExecCommand）。
pub(crate) async fn patch_bash_tools_exec_windows_command_normalize(
    openclaw_dir: &str,
) -> Result<(), String> {
    let path = format!("{}/dist/agents/bash-tools.exec.js", openclaw_dir);
    if !Path::new(&path).is_file() {
        return Ok(());
    }
    let mut s = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 bash-tools.exec.js 失败: {}", e))?;
    if s.contains(BASH_EXEC_WIN_NORM_MARKER) {
        return Ok(());
    }
    let shell_utils_check = format!("{}/dist/agents/shell-utils.js", openclaw_dir);
    let su_ck = tokio::fs::read_to_string(&shell_utils_check)
        .await
        .unwrap_or_default();
    if !su_ck.contains(WINDOWS_EXEC_CMD_QUOTE_PATCH_MARKER) {
        warn!("bash-tools.exec.js: shell-utils 缺少 normalizeWindowsExecCommand，跳过 exec 注入");
        return Ok(());
    }
    let import_old = "import { getShellConfig, sanitizeBinaryOutput } from \"./shell-utils.js\";";
    let import_new = "import { getShellConfig, sanitizeBinaryOutput, normalizeWindowsExecCommand } from \"./shell-utils.js\";";
    if s.contains(import_old) {
        s = s.replace(import_old, import_new);
    } else if !s.contains("normalizeWindowsExecCommand") {
        warn!("bash-tools.exec.js: 未识别的 shell-utils import，跳过 Windows 命令规范化");
        return Ok(());
    }
    let run_old = "async function runExecProcess(opts) {\n    const startedAt = Date.now();";
    let run_new = "async function runExecProcess(opts) {\n    // openclaw-cn-manager: normalizeWindowsExecCommand\n    if (process.platform === \"win32\") {\n        opts.command = normalizeWindowsExecCommand(opts.command);\n    }\n    const startedAt = Date.now();";
    if !s.contains(run_old) {
        warn!("bash-tools.exec.js: runExecProcess 开头未匹配，跳过 Windows 命令规范化");
        return Ok(());
    }
    s = s.replace(run_old, run_new);
    tokio::fs::write(&path, &s)
        .await
        .map_err(|e| format!("写入 bash-tools.exec.js (win normalize) 失败: {}", e))?;
    info!("Patched dist/agents/bash-tools.exec.js (Windows exec 命令规范化)");
    Ok(())
}

/// Windows：修正 `cmd /c "x.bat 参数"`（整段被一对引号包住 → CMD 把「路径+参数」当成单个程序名）；
/// 并让含盘符/UNC 的 `.bat`/`.cmd` 路径固定走 cmd.exe，避免误用 PowerShell。
pub(crate) async fn patch_shell_utils_windows_bat_exec_normalize(
    openclaw_dir: &str,
) -> Result<(), String> {
    let shell_utils_file = format!("{}/dist/agents/shell-utils.js", openclaw_dir);
    if !Path::new(&shell_utils_file).is_file() {
        return Ok(());
    }
    let mut su = tokio::fs::read_to_string(&shell_utils_file)
        .await
        .map_err(|e| format!("读取 shell-utils.js 失败: {}", e))?;
    const HEUR_MARKER: &str = "openclaw-cn-manager: 含盘符/UNC 的 .bat/.cmd";
    const BATCH_NORM_MARKER: &str = "openclaw-cn-manager: batch-call-normalize";
    if su.contains(BATCH_NORM_MARKER) && su.contains(HEUR_MARKER) {
        return Ok(());
    }
    let mut changed = false;
    const OLD_NORM: &str = r##"export function normalizeWindowsExecCommand(command) {
    if (process.platform !== "win32" || typeof command !== "string")
        return command;
    return command.replace(/\btype\s+"([^"\r\n]+)"/gi, (_m, p) => {
        const inner = String(p);
        if (/\s/.test(inner))
            return `type "${inner}"`;
        return `type ${inner}`;
    });
}"##;
    const NEW_NORM: &str = r##"export function normalizeWindowsExecCommand(command) {
    if (process.platform !== "win32" || typeof command !== "string")
        return command;
    // openclaw-cn-manager: batch-call-normalize — cmd /c "x.bat --a" 会被当成单个可执行文件名，改为 call "x.bat" --a
    let c = command.replace(/\btype\s+"([^"\r\n]+)"/gi, (_m, p) => {
        const inner = String(p);
        if (/\s/.test(inner))
            return `type "${inner}"`;
        return `type ${inner}`;
    });
    c = c.replace(/\bcmd(?:\.exe)?\s+\/c\s+"(?!call\s)((?:[A-Za-z]:|\\\\)[^"]+?\.(?:bat|cmd))(\s+[^"]*)"\s*/gi, (_m, bat, args) => {
        return `call "${String(bat).trim()}"${args != null ? args : ""}`;
    });
    return c;
}"##;
    if !su.contains(BATCH_NORM_MARKER) && su.contains(OLD_NORM) {
        su = su.replace(OLD_NORM, NEW_NORM);
        changed = true;
    } else if !su.contains(BATCH_NORM_MARKER) && su.contains("normalizeWindowsExecCommand") {
        warn!("shell-utils.js: normalizeWindowsExecCommand 形态未知，跳过 .bat 函数体升级（仍尝试启发式补丁）");
    }
    const LOOK_OLD: &str = r#"    if (/^\s*cmd\s+/i.test(c))
        return true;
    return false;
}"#;
    const LOOK_NEW: &str = r#"    if (/^\s*cmd\s+/i.test(c))
        return true;
    // openclaw-cn-manager: 含盘符/UNC 的 .bat/.cmd 固定走 cmd，裸路径交给 PowerShell 易解析失败
    if (/(?:[A-Za-z]:\\|\\\\)[^\n\r"|&]*\.(?:bat|cmd)\b/i.test(c))
        return true;
    return false;
}"#;
    if !su.contains(HEUR_MARKER) && su.contains(LOOK_OLD) {
        su = su.replace(LOOK_OLD, LOOK_NEW);
        changed = true;
    }
    if changed {
        tokio::fs::write(&shell_utils_file, &su)
            .await
            .map_err(|e| format!("写入 shell-utils.js (.bat exec) 失败: {}", e))?;
        info!("Patched dist/agents/shell-utils.js (Windows .bat/cmd exec 规范化)");
    }
    Ok(())
}

/// 修复 openclaw-dist 中 sessions.usage 的两处 bug：
/// 1. 命名会话的 updatedAt 仅取 store.updatedAt，压制文件 mtime。
/// 2. 聚合循环遍历 limitedEntries，导致合计/按模型/日趋势只统计前 limit 个会话。
///
/// 采用小步替换（保留原循环体），避免大块替换留下重复代码导致网关 JS 语法错误。
/// 幂等：检测 limited-keys、mergedEntries 循环、list 包裹与闭合结构。
const SESSIONS_USAGE_AGG_MARKER: &str = "_mgrSessionsUsageLimitedKeys";
const SESSIONS_USAGE_PATCH_TAIL_OK: &str =
    "                });\n            }\n        }\n        // Format dates back";

pub(crate) async fn patch_sessions_usage_aggregate_fix(openclaw_dir: &str) -> Result<(), String> {
    let path = format!("{}/dist/gateway/server-methods/usage.js", openclaw_dir);
    if !Path::new(&path).is_file() {
        return Ok(());
    }
    let mut content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 usage.js 失败: {}", e))?;

    if content.contains(SESSIONS_USAGE_AGG_MARKER)
        && content.contains("for (const merged of mergedEntries)")
        && content.contains("if (_mgrSessionsUsageLimitedKeys.has(merged.key))")
        && content.contains(SESSIONS_USAGE_PATCH_TAIL_OK)
    {
        return Ok(());
    }

    const UPDATED_AT_OLD: &str = "updatedAt: storeMatch.entry.updatedAt ?? discovered.mtime,";
    const UPDATED_AT_NEW: &str =
        "updatedAt: Math.max(storeMatch.entry.updatedAt ?? 0, discovered.mtime),";
    if content.contains(UPDATED_AT_OLD) {
        content = content.replace(UPDATED_AT_OLD, UPDATED_AT_NEW);
    }

    const SLICE_LINE: &str = "        const limitedEntries = mergedEntries.slice(0, limit);";
    const SLICE_INJECT: &str = r#"        const limitedEntries = mergedEntries.slice(0, limit);
        // openclaw-cn-manager: sessions.usage — 汇总遍历全量会话，列表仍受 limit 约束
        const _mgrSessionsUsageLimitedKeys = new Set(limitedEntries.map((e) => e.key));"#;
    if content.contains(SLICE_LINE) && !content.contains(SESSIONS_USAGE_AGG_MARKER) {
        content = content.replace(SLICE_LINE, SLICE_INJECT);
    }

    const FOR_OLD: &str = "for (const merged of limitedEntries)";
    const FOR_NEW: &str = "for (const merged of mergedEntries)";
    if content.contains(FOR_OLD) {
        content = content.replacen(FOR_OLD, FOR_NEW, 1);
    } else if !content.contains(FOR_NEW) {
        return Err("usage.js: 未找到 sessions.usage 聚合 for 循环".to_string());
    }

    const PUSH_HEAD_OLD: &str = "            sessions.push({\n                key: merged.key,";
    const PUSH_HEAD_NEW: &str = "            if (_mgrSessionsUsageLimitedKeys.has(merged.key)) {\n                sessions.push({\n                key: merged.key,";
    if content.contains(PUSH_HEAD_OLD) && !content.contains(PUSH_HEAD_NEW) {
        content = content.replacen(PUSH_HEAD_OLD, PUSH_HEAD_NEW, 1);
    } else if !content.contains("_mgrSessionsUsageLimitedKeys.has(merged.key)") {
        return Err("usage.js: 未找到 sessions.push 注入点（上游结构可能已变）".to_string());
    }

    const PUSH_TAIL_OLD: &str = r#"                    : undefined,
            });
        }
        // Format dates back to YYYY-MM-DD strings"#;
    const PUSH_TAIL_NEW: &str = r#"                    : undefined,
                });
            }
        }
        // Format dates back to YYYY-MM-DD strings"#;
    if content.contains(PUSH_TAIL_OLD) {
        content = content.replacen(PUSH_TAIL_OLD, PUSH_TAIL_NEW, 1);
    }

    // 打补丁后完整性校验（避免半拉子替换仍落盘）
    if !content.contains(SESSIONS_USAGE_AGG_MARKER)
        || !content.contains("for (const merged of mergedEntries)")
        || !content.contains("if (_mgrSessionsUsageLimitedKeys.has(merged.key))")
        || !content.contains(SESSIONS_USAGE_PATCH_TAIL_OK)
    {
        return Err(
            "usage.js: sessions.usage 补丁不完整（闭合块或注入点与上游不一致）".to_string(),
        );
    }

    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| format!("写入 usage.js 失败: {}", e))?;
    info!("Patched dist/gateway/server-methods/usage.js (sessions.usage: mergedEntries aggregate + list limit)");
    Ok(())
}

/// 当 session store 未写入 channel / model 时，从 session key 与 transcript 汇总的 modelUsage 回退填充，
/// 避免用量页「最近会话」表中渠道、模型列为空。
const SESSIONS_USAGE_DISPLAY_MARKER: &str = "_mgrSessionUsageInferDisplay";

pub(crate) async fn patch_sessions_usage_session_display_fallbacks(
    openclaw_dir: &str,
) -> Result<(), String> {
    let path = format!("{}/dist/gateway/server-methods/usage.js", openclaw_dir);
    if !Path::new(&path).is_file() {
        return Ok(());
    }
    let mut content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 usage.js 失败: {}", e))?;

    if content.contains(SESSIONS_USAGE_DISPLAY_MARKER) {
        return Ok(());
    }

    const CHANNEL_OLD: &str = r#"            const agentId = parseAgentSessionKey(merged.key)?.agentId;
            const channel = merged.storeEntry?.channel ?? merged.storeEntry?.origin?.provider;
            const chatType = merged.storeEntry?.chatType ?? merged.storeEntry?.origin?.chatType;"#;

    const CHANNEL_NEW: &str = r#"            const agentId = parseAgentSessionKey(merged.key)?.agentId;
            // openclaw-cn-manager: _mgrSessionUsageInferDisplay — store 未写 channel 时从 session key 推断（如 agent:x:feishu:group:id）
            let channel = merged.storeEntry?.channel ?? merged.storeEntry?.origin?.provider;
            if (!channel) {
                const parsedKey = parseAgentSessionKey(merged.key);
                const rest = parsedKey?.rest;
                if (rest && rest !== "main") {
                    const restLow = rest.toLowerCase();
                    if (!restLow.startsWith("subagent:") && !restLow.startsWith("acp:") && !restLow.startsWith("cron:")) {
                        const segs = rest.split(":");
                        const peerKinds = new Set(["dm", "group", "channel", "thread", "topic", "space"]);
                        if (segs.length >= 3 && peerKinds.has(String(segs[1] ?? "").toLowerCase())) {
                            channel = segs[0];
                        }
                    }
                }
            }
            const chatType = merged.storeEntry?.chatType ?? merged.storeEntry?.origin?.chatType;
            const _mgrDominantModelUsage = usage?.modelUsage?.length
                ? usage.modelUsage.reduce((best, cur) => ((cur?.totals?.totalTokens ?? 0) > (best?.totals?.totalTokens ?? 0) ? cur : best))
                : null;"#;

    if !content.contains(CHANNEL_OLD) {
        return Err("usage.js: 未找到 channel 赋值块，无法注入会话展示回退".to_string());
    }
    content = content.replacen(CHANNEL_OLD, CHANNEL_NEW, 1);

    const MODEL_FIELDS_OLD: &str = r#"                modelProvider: merged.storeEntry?.modelProvider,
                model: merged.storeEntry?.model,"#;
    const MODEL_FIELDS_NEW: &str = r#"                modelProvider: merged.storeEntry?.modelProvider ?? _mgrDominantModelUsage?.provider,
                model: merged.storeEntry?.model ?? _mgrDominantModelUsage?.model,"#;

    if !content.contains(MODEL_FIELDS_OLD) {
        return Err(
            "usage.js: 未找到 model/modelProvider 字段，无法注入 dominant modelUsage 回退"
                .to_string(),
        );
    }
    content = content.replacen(MODEL_FIELDS_OLD, MODEL_FIELDS_NEW, 1);

    if !content.contains(SESSIONS_USAGE_DISPLAY_MARKER)
        || !content.contains("_mgrDominantModelUsage")
    {
        return Err("usage.js: 会话展示回退补丁不完整".to_string());
    }

    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| format!("写入 usage.js 失败: {}", e))?;
    info!("Patched dist/gateway/server-methods/usage.js (sessions.usage: channel/model display fallbacks)");
    Ok(())
}

/// ── session-cost-usage.js 三项修复 ─────────────────────────────────────────

/// 1. formatDayKey 改用 UTC 避免服务器本地时区偏移（否则日趋势日期与 startDate/endDate 不一致）。
/// 2. discoverAllSessions 移除 mtime 过滤，让 loadSessionCostSummary 在 entry 级别过滤。
/// 3. loadCostUsageSummary days 回退改用 UTC（与 parseDateRange 保持一致）。
const SCU_TIMEZONE_MARKER: &str = "_mgrScuUtcDayKey";
const SCU_DISCOVER_SKIP_MARKER: &str = "_mgrScuDiscoverNoMtimeFilter";
const SCU_COST_SINCE_MARKER: &str = "_mgrScuCostSinceUtc";

pub(crate) async fn patch_session_cost_usage_utc_and_discover(
    openclaw_dir: &str,
) -> Result<(), String> {
    let path = format!("{}/dist/infra/session-cost-usage.js", openclaw_dir);
    if !Path::new(&path).is_file() {
        return Ok(());
    }
    let mut content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 session-cost-usage.js 失败: {}", e))?;

    if content.contains(SCU_TIMEZONE_MARKER) {
        return Ok(());
    }

    // ── 1. formatDayKey: 改用 UTC ──────────────────────────────────────────
    const FDK_OLD: &str = r#"const formatDayKey = (date) => date.toLocaleDateString("en-CA", { timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone });"#;
    const FDK_NEW: &str = r#"// openclaw-cn-manager: _mgrScuUtcDayKey — 用 UTC 避免服务器时区偏移导致日趋势错位
const formatDayKey = (date) => date.toISOString().slice(0, 10);"#;
    if !content.contains(FDK_OLD) {
        return Err("session-cost-usage.js: 未找到 formatDayKey（上游可能已变）".to_string());
    }
    content = content.replace(FDK_OLD, FDK_NEW);

    // ── 2. discoverAllSessions: 移除 mtime 过滤 ─────────────────────────────
    const DISC_OLD: &str = r#"        // Filter by date range if provided
        if (params?.startMs && stats.mtimeMs < params.startMs) {
            continue;
        }
        // Do not exclude by endMs"#;
    const DISC_NEW: &str = r#"        // openclaw-cn-manager: _mgrScuDiscoverNoMtimeFilter — 不按 mtime 预过滤；
        // 存在"文件 mtime 在 30 天前，但文件内消息在范围内"的会话，
        // 其 mtime 会后延到最近一次写入，但仍应被发现。
        // 后续 loadSessionCostSummary 在 entry 级别做日期范围过滤。
        // Do not exclude by endMs"#;
    if content.contains(DISC_OLD) {
        content = content.replace(DISC_OLD, DISC_NEW);
    } else if !content.contains(SCU_DISCOVER_SKIP_MARKER) {
        return Err("session-cost-usage.js: 未找到 discoverAllSessions mtime 过滤块".to_string());
    }

    // ── 3. loadCostUsageSummary days 回退改 UTC ─────────────────────────────
    const COST_OLD: &str = r#"        // Fallback to days-based calculation for backwards compatibility
        const days = Math.max(1, Math.floor(params?.days ?? 30));
        const since = new Date(now);
        since.setDate(since.getDate() - (days - 1));
        sinceTime = since.getTime();
        untilTime = now.getTime();"#;
    const COST_NEW: &str = r#"        // openclaw-cn-manager: _mgrScuCostSinceUtc — 与 parseDateRange 保持一致，统一用 UTC
        // 修复：untilTime 必须覆盖完整当天 UTC（到 23:59:59.999），否则最后一天的数据全被漏掉。
        // 旧代码 untilTime = now.getTime() 导致在 parseDateRange 的 <= 过滤中，
        // 当天 00:00 UTC < untilTime == 当天 00:00 UTC 时毫秒级差值导致全漏。
        // 现改为 todayStartMs + 1天，确保 <= untilTime 覆盖当天全部毫秒。
        const days = Math.max(1, Math.floor(params?.days ?? 30));
        const clampedDays = Math.max(1, days);
        const todayStartMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
        sinceTime = todayStartMs - (clampedDays - 1) * 24 * 60 * 60 * 1000;
        untilTime = todayStartMs + 24 * 60 * 60 * 1000;"#;
    if !content.contains(COST_OLD) {
        return Err("session-cost-usage.js: 未找到 loadCostUsageSummary days 回退块".to_string());
    }
    content = content.replace(COST_OLD, COST_NEW);

    if !content.contains(SCU_TIMEZONE_MARKER)
        || !content.contains(SCU_DISCOVER_SKIP_MARKER)
        || !content.contains(SCU_COST_SINCE_MARKER)
    {
        return Err("session-cost-usage.js: 三项补丁未完整注入".to_string());
    }

    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| format!("写入 session-cost-usage.js 失败: {}", e))?;
    info!("Patched dist/infra/session-cost-usage.js (UTC dayKey + discoverAllSessions mtime + cost days fallback)");
    Ok(())
}

/// 修复 sessions.usage 的 discoverAllSessions 只扫描 "main" agent 的根本问题。
/// upstream bug: discoverAllSessions(undefined) → agentId = "main" → 只扫 main/agents/main/sessions/（2个旧会话）。
/// 实际活跃会话在 inst_1774534098306 等多 agent 目录，完全未被扫描。
/// 补丁用 loadConfig().agents.list 遍历所有 agent，收集全部 discoveredSessions 并去重（sessionId 为主键）。
const USAGE_ALL_AGENTS_MARKER: &str = "_mgrUsageAllAgents";

pub(crate) async fn patch_sessions_usage_all_agents(openclaw_dir: &str) -> Result<(), String> {
    let path = format!("{}/dist/gateway/server-methods/usage.js", openclaw_dir);
    if !Path::new(&path).is_file() {
        return Ok(());
    }
    let mut content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("读取 usage.js 失败: {}", e))?;

    if content.contains(USAGE_ALL_AGENTS_MARKER) {
        return Ok(());
    }

    // 在 sessions.usage 的 else 块中，将 discoverAllSessions({ startMs, endMs })
    // 替换为遍历所有 agent 的版本。
    // 关键锚点：下游代码 "for (const discovered of discoveredSessions)" 必须保留，
    // 这样后续的 mergedEntries 填充逻辑不变。
    const OLD_CALL: &str = r#"            const discoveredSessions = await discoverAllSessions({
                startMs,
                endMs,
            });"#;
    const NEW_CALL: &str = r#"            // openclaw-cn-manager: _mgrUsageAllAgents — 遍历所有 agent 而非仅 "main"
            // upstream bug: discoverAllSessions(undefined) → agentId="main" → 漏扫 inst_xxx 等活跃 agent。
            // 用 loadConfig().agents.list 收集全量 agent_id，discoverAllSessions 并行扫描，去重合并。
            const _mgrAllAgents = loadConfig().agents.list ?? [];
            const _mgrAgentIds = ["main", ..._mgrAllAgents.map((a) => a.id).filter(Boolean)];
            const _mgrAllDiscovered = await Promise.all(
                _mgrAgentIds.map((_aid) => discoverAllSessions({ startMs, endMs, agentId: _aid })),
            );
            const _mgrSeen = new Set();
            const discoveredSessions = [];
            for (const _batch of _mgrAllDiscovered) {
                for (const _s of _batch) {
                    if (!_mgrSeen.has(_s.sessionId)) {
                        _mgrSeen.add(_s.sessionId);
                        discoveredSessions.push(_s);
                    }
                }
            }"#;

    if !content.contains(OLD_CALL) {
        return Err("usage.js: 未找到 discoverAllSessions({ startMs, endMs }) 调用".to_string());
    }
    content = content.replacen(OLD_CALL, NEW_CALL, 1);

    if !content.contains(USAGE_ALL_AGENTS_MARKER) {
        return Err("usage.js: _mgrUsageAllAgents 标记未注入".to_string());
    }

    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| format!("写入 usage.js (all agents) 失败: {}", e))?;
    info!("Patched dist/gateway/server-methods/usage.js (sessions.usage: discoverAllSessions 扫描全量 agent)");
    Ok(())
}

/// Patches two known-broken JS files in the openclaw-cn dist that cause gateway startup to crash:
/// 1. dist/commands/onboarding/registry.js — imports qqbot.js which doesn't exist in the npm package
/// 2. dist/plugin-sdk/index.js — re-exports feishuOutbound and normalizeFeishuTarget from non-existent paths
///
/// This is a band-aid for an upstream bug in the published openclaw-cn npm package.
/// Runs after install_openclaw completes; failures are logged but do not block installation.
pub async fn patch_openclaw_broken_modules(openclaw_dir: &str) -> Result<(), String> {
    // 1. Patch registry.js — replace broken static imports with inline stubs
    let registry_path = format!("{}/dist/commands/onboarding/registry.js", openclaw_dir);
    let stub_registry = r#"// Broken imports — patched by openclaw-cn-manager installer
import { listChannelPlugins } from "../../channels/plugins/index.js";
// qqbot: dist file missing in openclaw-cn npm package
const _stub_qqbot = {
    channel: "qqbot",
    getStatus: () => ({ configured: false, statusLines: [], selectionHint: "", quickstartScore: 10 }),
    configure: () => { throw new Error("qqbot onboarding not available (missing dist file in npm package)"); },
    disable: (cfg) => cfg
};
// openclaw-weixin.js / wecom-connector.js: dist files missing in npm package — inline stubs
const _stub_openclaw_weixin = {
    channel: "openclaw-weixin",
    getStatus: () => ({ configured: false, statusLines: [], selectionHint: "", quickstartScore: 10 }),
    configure: () => { throw new Error("openclaw-weixin onboarding not available (missing dist file in npm package)"); },
    disable: (cfg) => cfg
};
const _stub_wecom_connector = {
    channel: "wecom",
    getStatus: () => ({ configured: false, statusLines: [], selectionHint: "", quickstartScore: 10 }),
    configure: () => { throw new Error("wecom-connector onboarding not available (missing dist file in npm package)"); },
    disable: (cfg) => cfg
};
// Core onboarding adapters for channels whose official plugins do not provide
// their own onboarding adapter (or may not be loaded at configure time).
const CORE_ONBOARDING_ADAPTERS = new Map([
    ["wecom", _stub_wecom_connector],
    ["openclaw-weixin", _stub_openclaw_weixin],
    ["qqbot", _stub_qqbot],
]);
const CHANNEL_ONBOARDING_ADAPTERS = () => {
    const pluginAdapters = new Map(listChannelPlugins()
        .map((plugin) => plugin.onboarding ? [plugin.id, plugin.onboarding] : null)
        .filter((entry) => Boolean(entry)));
    // Merge: plugin adapters take precedence over core fallbacks
    const merged = new Map(CORE_ONBOARDING_ADAPTERS);
    for (const [id, adapter] of pluginAdapters) {
        merged.set(id, adapter);
    }
    return merged;
};
export function getChannelOnboardingAdapter(channel) {
    return CHANNEL_ONBOARDING_ADAPTERS().get(channel);
}
export function listChannelOnboardingAdapters() {
    return Array.from(CHANNEL_ONBOARDING_ADAPTERS().values());
}
// Legacy aliases (pre-rename).
export const getProviderOnboardingAdapter = getChannelOnboardingAdapter;
export const listProviderOnboardingAdapters = listChannelOnboardingAdapters;
"#;
    tokio::fs::write(&registry_path, stub_registry)
        .await
        .map_err(|e| format!("写入 registry.js patch 失败: {}", e))?;
    info!("Patched dist/commands/onboarding/registry.js (stubbed missing modules)");

    // 2. Patch plugin-sdk/index.js — fix broken feishu re-exports.
    //    normalizeFeishuTarget / feishuOutbound are defined in dist/channels/plugins/,
    //    but plugin-sdk references non-existent paths (../../../plugins/feishu/...).
    let sdk_path = format!("{}/dist/plugin-sdk/index.js", openclaw_dir);
    let sdk_content = tokio::fs::read_to_string(&sdk_path)
        .await
        .map_err(|e| format!("读取 plugin-sdk/index.js 失败: {}", e))?;

    let sdk_patched = sdk_content
        .lines()
        .map(|line| {
            let t = line.trim();
            if t.starts_with("export { feishuOutbound } from") {
                "// feishuOutbound re-export fixed by openclaw-cn-manager\n\
                 export { feishuOutbound } from \"../channels/plugins/outbound/feishu.js\";"
                    .to_string()
            } else if t.starts_with("export { normalizeFeishuTarget } from") {
                "// normalizeFeishuTarget re-export fixed by openclaw-cn-manager\n\
                 export { normalizeFeishuTarget } from \"../channels/plugins/normalize/feishu.js\";"
                    .to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    tokio::fs::write(&sdk_path, &sdk_patched)
        .await
        .map_err(|e| format!("写入 plugin-sdk/index.js patch 失败: {}", e))?;
    info!("Patched dist/plugin-sdk/index.js (fixed feishu re-export paths)");

    // 2.5. 创建 plugin-sdk 子路径模块（微信/企微插件需要）
    let sdk_dir = format!("{}/dist/plugin-sdk", openclaw_dir);
    let subpath_modules = [
        ("account-id.js", "export { DEFAULT_ACCOUNT_ID, normalizeAccountId } from \"../routing/session-key.js\";\n"),
        ("channel-config-schema.js", "export { buildChannelConfigSchema } from \"../channels/plugins/config-schema.js\";\n"),
        ("channel-contract.js", "export {};\n"),
        ("channel-policy.js", "export function buildAccountScopedDmSecurityPolicy(params) {\n  const DEFAULT_ACCOUNT = \"default\";\n  const resolvedAccountId = params.accountId ?? params.fallbackAccountId ?? DEFAULT_ACCOUNT;\n  const channelConfig = params.cfg?.channels?.[params.channelKey];\n  const useAccountPath = Boolean(channelConfig?.accounts?.[resolvedAccountId]);\n  const basePath = useAccountPath ? `channels.${params.channelKey}.accounts.${resolvedAccountId}.` : `channels.${params.channelKey}.`;\n  const allowFromPath = `${basePath}${params.allowFromPathSuffix ?? \"\"}`;\n  const policyPath = params.policyPathSuffix != null ? `${basePath}${params.policyPathSuffix}` : undefined;\n  return { policy: params.policy ?? params.defaultPolicy ?? \"pairing\", allowFrom: params.allowFrom ?? [], policyPath, allowFromPath, approveHint: params.approveHint ?? `Approve via: openclaw pairing approve ${params.channelKey} <code>`, normalizeEntry: params.normalizeEntry };\n}\n"),
        ("channel-runtime.js", "export { createTypingCallbacks } from \"../channels/typing.js\";\n"),
        ("command-auth.js", "export function resolveSenderCommandAuthorizationWithRuntime(ctx) { return { authorized: true }; }\nexport function resolveDirectDmAuthorizationOutcome(ctx) { return { authorized: true }; }\n"),
        ("config-runtime.js", "export {};\n"),
        ("core.js", "export { DEFAULT_ACCOUNT_ID, normalizeAccountId } from \"../routing/session-key.js\";\nexport { SILENT_REPLY_TOKEN, isSilentReplyText } from \"../auto-reply/tokens.js\";\nexport { createTypingCallbacks } from \"../channels/typing.js\";\nexport { logAckFailure, logInboundDrop, logTypingFailure } from \"../channels/logging.js\";\nexport { emitDiagnosticEvent, isDiagnosticsEnabled, onDiagnosticEvent } from \"../infra/diagnostic-events.js\";\nexport function emptyPluginConfigSchema() { return { type: 'object', additionalProperties: false, properties: {} }; }\n"),
        ("infra-runtime.js", "import os from \"node:os\";\nimport path from \"node:path\";\nexport function resolvePreferredOpenClawTmpDir() { return path.join(os.tmpdir(), \"openclaw\"); }\nexport async function withFileLock(filePath, fn) { return fn(); }\n"),
        ("plugin-entry.js", "export {};\n"),
        ("reply-runtime.js", "export {};\n"),
        ("runtime-env.js", "export {};\n"),
        ("runtime-store.js", "export function createPluginRuntimeStore(errorMessage) {\n  let _runtime = null;\n  return {\n    setRuntime(r) { _runtime = r; },\n    getRuntime() {\n      if (!_runtime) throw new Error(errorMessage || 'Runtime not initialized');\n      return _runtime;\n    },\n  };\n}\n"),
        ("setup.js", "export {};\n"),
        ("package.json", "{\n  \"name\": \"openclaw-plugin-sdk\",\n  \"type\": \"module\",\n  \"exports\": {\n    \".\": \"./index.js\",\n    \"./account-id\": \"./account-id.js\",\n    \"./channel-config-schema\": \"./channel-config-schema.js\",\n    \"./channel-contract\": \"./channel-contract.js\",\n    \"./channel-policy\": \"./channel-policy.js\",\n    \"./channel-runtime\": \"./channel-runtime.js\",\n    \"./command-auth\": \"./command-auth.js\",\n    \"./config-runtime\": \"./config-runtime.js\",\n    \"./core\": \"./core.js\",\n    \"./infra-runtime\": \"./infra-runtime.js\",\n    \"./plugin-entry\": \"./plugin-entry.js\",\n    \"./reply-runtime\": \"./reply-runtime.js\",\n    \"./runtime-env\": \"./runtime-env.js\",\n    \"./runtime-store\": \"./runtime-store.js\",\n    \"./setup\": \"./setup.js\"\n  }\n}\n"),
    ];
    for (filename, content) in subpath_modules {
        let path = format!("{}/{}", sdk_dir, filename);
        // 总是覆盖，确保内容正确
        tokio::fs::write(&path, content)
            .await
            .map_err(|e| format!("创建 {} 失败: {}", filename, e))?;
    }
    info!("Ensured plugin-sdk subpath modules exist");

    // 2.6. 修复根 package.json：name 改为 "openclaw"，exports 子路径在通配符之前
    let root_pkg_path = format!("{}/package.json", openclaw_dir);
    if let Ok(content) = tokio::fs::read_to_string(&root_pkg_path).await {
        if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            // 修复 package name：插件 import 路径是 "openclaw/..."，必须匹配
            if pkg.get("name").and_then(|n| n.as_str()) != Some("openclaw") {
                pkg["name"] = serde_json::json!("openclaw");
            }
            // 重建 exports，确保子路径在通配符之前（Node.js 按顺序匹配）
            let new_exports = serde_json::json!({
                ".": "./dist/index.js",
                "./plugin-sdk/channel-config-schema": "./dist/plugin-sdk/channel-config-schema.js",
                "./plugin-sdk/runtime-store": "./dist/plugin-sdk/runtime-store.js",
                "./plugin-sdk/plugin-entry": "./dist/plugin-sdk/plugin-entry.js",
                "./plugin-sdk": "./dist/plugin-sdk/index.js",
                "./plugin-sdk/*": "./dist/plugin-sdk/*"
            });
            pkg["exports"] = new_exports;
            let pretty = serde_json::to_string_pretty(&pkg).unwrap_or_default();
            tokio::fs::write(&root_pkg_path, &pretty)
                .await
                .map_err(|e| format!("修复 package.json 失败: {}", e))?;
            info!("Patched package.json exports (subpath before wildcard)");
        }
    }

    // 2.7. Patch plugins/loader.js — fix jiti alias for plugin-sdk subpath imports.
    //    resolvePluginSdkAlias() returns ".../dist/plugin-sdk/index.js" (a file), but jiti's
    //    alias does prefix replacement: "openclaw/plugin-sdk/channel-config-schema" becomes
    //    "dist/plugin-sdk/index.js/channel-config-schema" (nonexistent). Fix: use the
    //    directory path so subpath imports resolve correctly within the plugin-sdk dir.
    let loader_path = format!("{}/dist/plugins/loader.js", openclaw_dir);
    if let Ok(loader_content) = tokio::fs::read_to_string(&loader_path).await {
        // Only patch if not already patched (avoid duplicate declarations)
        let needs_patch = !loader_content.contains("pluginSdkDir")
            && loader_content.contains("pluginSdkAlias");
        if needs_patch {
            let patched_loader = loader_content
                .replace(
                    "\"openclaw/plugin-sdk\": pluginSdkAlias,",
                    "\"openclaw/plugin-sdk\": pluginSdkDir,",
                )
                .replace(
                    "\"clawdbot/plugin-sdk\": pluginSdkAlias,",
                    "\"clawdbot/plugin-sdk\": pluginSdkDir,",
                )
                .replace(
                    "\"openclaw-cn/plugin-sdk\": pluginSdkAlias,",
                    "\"openclaw-cn/plugin-sdk\": pluginSdkDir,",
                )
                .replace(
                    "const pluginSdkAlias = resolvePluginSdkAlias();",
                    "const pluginSdkAlias = resolvePluginSdkAlias();\n    const pluginSdkDir = pluginSdkAlias ? path.dirname(pluginSdkAlias) : null;",
                );
            if patched_loader != loader_content {
                tokio::fs::write(&loader_path, &patched_loader)
                    .await
                    .map_err(|e| format!("写入 loader.js patch 失败: {}", e))?;
                info!("Patched dist/plugins/loader.js (fixed jiti alias for plugin-sdk subpath imports)");
            }
        } else {
            // Already patched — just ensure alias values use pluginSdkDir (not pluginSdkAlias)
            let patched_loader = loader_content
                .replace(
                    "\"openclaw/plugin-sdk\": pluginSdkAlias,",
                    "\"openclaw/plugin-sdk\": pluginSdkDir,",
                )
                .replace(
                    "\"clawdbot/plugin-sdk\": pluginSdkAlias,",
                    "\"clawdbot/plugin-sdk\": pluginSdkDir,",
                )
                .replace(
                    "\"openclaw-cn/plugin-sdk\": pluginSdkAlias,",
                    "\"openclaw-cn/plugin-sdk\": pluginSdkDir,",
                );
            if patched_loader != loader_content {
                tokio::fs::write(&loader_path, &patched_loader)
                    .await
                    .map_err(|e| format!("写入 loader.js patch 失败: {}", e))?;
                info!("Patched dist/plugins/loader.js (updated alias values)");
            }
        }
    }

    // 3. Patch fs-paths.js — fix parseSandboxBindMount to handle Windows drive letters.
    //    Replace the whole function (marker: next export) — replacing only the opening
    //    line would duplicate the old body and corrupt the file.
    //    - "D:/path:/container": first ":/" at index 1 is a false positive; skip and re-search.
    //    - "D:\path:C:\container": no ":/" substring; use /^([A-Za-z]:[^:]+):/ fallback.
    //    - cfg.binds with backslashes: normalised in docker.js (see step 4).
    let fsp_path = format!("{}/dist/agents/sandbox/fs-paths.js", openclaw_dir);
    if !Path::new(&fsp_path).is_file() {
        warn!("fs-paths.js not found at {}, skipping patch (different OpenClaw version)", fsp_path);
        return Ok(());
    }
    let fsp_content = tokio::fs::read_to_string(&fsp_path)
        .await
        .map_err(|e| format!("读取 fs-paths.js 失败: {}", e))?;

    let fixed_parse = r#"export function parseSandboxBindMount(spec) {
    const trimmed = spec.trim();
    if (!trimmed) {
        return null;
    }
    let hostToken;
    let containerAndOptions;
    const WIN_DRIVE_RE = /^[A-Za-z]:\//;
    let sepIdx = trimmed.search(/:\//);
    if (sepIdx !== -1 && sepIdx === 1 && WIN_DRIVE_RE.test(trimmed)) {
        sepIdx = trimmed.slice(2).search(/:\//);
        if (sepIdx === -1) return null;
        sepIdx += 2;
    }
    if (sepIdx !== -1) {
        hostToken = trimmed.slice(0, sepIdx).trim();
        containerAndOptions = trimmed.slice(sepIdx + 1);
    } else {
        const winSplit = trimmed.match(/^([A-Za-z]:[^:]+):([\s\S]*)$/);
        if (winSplit) {
            hostToken = winSplit[1].trim();
            containerAndOptions = winSplit[2];
        } else {
            const firstColon = trimmed.indexOf(":");
            if (firstColon === -1) return null;
            hostToken = trimmed.slice(0, firstColon).trim();
            containerAndOptions = trimmed.slice(firstColon + 1);
        }
    }
    // Reject degenerate host tokens (e.g. "D" only) from bad splits — avoids path.resolve → "D:" / lstat issues.
    if (/^[A-Za-z]$/.test(hostToken)) {
        return null;
    }
    const containerParts = containerAndOptions.split(":");
    const containerToken = (containerParts[0] ?? "").trim();
    const optionsToken = containerParts.slice(1).join(":").trim().toLowerCase();
    if (!hostToken || !containerToken || !path.posix.isAbsolute(containerToken)) {
        return null;
    }
    const optionParts = optionsToken
        ? optionsToken
            .split(",")
            .map((entry) => entry.trim())
            .filter(Boolean)
        : [];
    const writable = !optionParts.includes("ro");
    return {
        hostRoot: path.resolve(hostToken),
        containerRoot: normalizeContainerPath(containerToken),
        writable,
    };
}"#;

    const FSP_START: &str = "export function parseSandboxBindMount(spec) {";
    const FSP_END: &str = "export function buildSandboxFsMounts";
    match (fsp_content.find(FSP_START), fsp_content.find(FSP_END)) {
        (Some(start_idx), Some(end_idx)) if start_idx < end_idx => {
            let fsp_patched = format!(
                "{}{}{}",
                &fsp_content[..start_idx],
                fixed_parse,
                &fsp_content[end_idx..]
            );
            tokio::fs::write(&fsp_path, &fsp_patched)
                .await
                .map_err(|e| format!("写入 fs-paths.js patch 失败: {}", e))?;
            info!("Patched dist/agents/sandbox/fs-paths.js (Windows bind mount parse: full function replace)");
        }
        _ => {
            warn!("fs-paths.js patch: parseSandboxBindMount / buildSandboxFsMounts markers not found, skipping");
        }
    }

    // 4. Patch docker.js — normalise Windows host paths in -v mount arguments.
    //    Docker CLI on Windows accepts forward-slash paths for bind mounts
    //    (e.g.  -v /d/data:/workspace ), so we convert backslashes to slashes
    //    before passing the host portion to the -v flag.
    let docker_path = format!("{}/dist/agents/sandbox/docker.js", openclaw_dir);
    if !Path::new(&docker_path).is_file() {
        warn!("docker.js not found at {}, skipping patch (different OpenClaw version)", docker_path);
        return Ok(());
    }
    let docker_content = tokio::fs::read_to_string(&docker_path)
        .await
        .map_err(|e| format!("读取 docker.js 失败: {}", e))?;

    let docker_patched = docker_content
        // Fix workspace mount: workspaceDir -> winSlash(workspaceDir)
        .replace(
            r#"args.push("-v", `${workspaceDir}:${cfg.workdir}${mainMountSuffix}`);"#,
            r#"const _hostWs = process.platform === "win32" ? workspaceDir.replace(/\\/g, "/") : workspaceDir;
        args.push("-v", `${_hostWs}:${cfg.workdir}${mainMountSuffix}`);"#,
        )
        // Fix agent mount: params.agentWorkspaceDir -> winSlash(params.agentWorkspaceDir)
        .replace(
            r#"args.push("-v", `${params.agentWorkspaceDir}:${SANDBOX_AGENT_WORKSPACE_MOUNT}${agentMountSuffix}`);"#,
            r#"const _hostAgentWs = process.platform === "win32" ? params.agentWorkspaceDir.replace(/\\/g, "/") : params.agentWorkspaceDir;
        args.push("-v", `${_hostAgentWs}:${SANDBOX_AGENT_WORKSPACE_MOUNT}${agentMountSuffix}`);"#,
        )
        // Normalise cfg.binds on win32 so parseSandboxBindMount sees "D:/..." consistently.
        .replace(
            r#"if (params.cfg.binds?.length) {
        for (const bind of params.cfg.binds) {
            args.push("-v", bind);
        }
    }"#,
            r#"if (params.cfg.binds?.length) {
        for (const bind of params.cfg.binds) {
            const _normBind = process.platform === "win32" ? bind.replace(/\\/g, "/") : bind;
            args.push("-v", _normBind);
        }
    }"#,
        );

    if docker_patched == docker_content {
        warn!("docker.js patch: target lines not found, skipping");
    } else {
        tokio::fs::write(&docker_path, &docker_patched)
            .await
            .map_err(|e| format!("写入 docker.js patch 失败: {}", e))?;
        info!("Patched dist/agents/sandbox/docker.js (normalised Windows host paths in -v args)");
    }

    // 5. Patch browser.js — same Windows -v normalisation issue exists in the browser
    //    sandbox path (ensureSandboxBrowser).  Unlike docker.js (buildSandboxCreateArgs),
    //    browser.js builds its own -v args inline.  Normalise params.workspaceDir and
    //    params.agentWorkspaceDir to forward slashes on win32 so parseSandboxBindMount
    //    receives clean "D:/path:/container" strings it can correctly handle.
    let browser_path = format!("{}/dist/agents/sandbox/browser.js", openclaw_dir);
    let browser_content = tokio::fs::read_to_string(&browser_path)
        .await
        .map_err(|e| format!("读取 browser.js 失败: {}", e))?;

    let browser_patched = browser_content
        // Fix workspace mount in browser sandbox
        .replace(
            r#"args.push("-v", `${params.workspaceDir}:${params.cfg.docker.workdir}${mainMountSuffix}`);"#,
            r#"const _hostBrowserWs = process.platform === "win32" ? params.workspaceDir.replace(/\\/g, "/") : params.workspaceDir;
        args.push("-v", `${_hostBrowserWs}:${params.cfg.docker.workdir}${mainMountSuffix}`);"#,
        )
        // Fix agent mount in browser sandbox
        .replace(
            r#"args.push("-v", `${params.agentWorkspaceDir}:${SANDBOX_AGENT_WORKSPACE_MOUNT}${agentMountSuffix}`);"#,
            r#"const _hostBrowserAgentWs = process.platform === "win32" ? params.agentWorkspaceDir.replace(/\\/g, "/") : params.agentWorkspaceDir;
        args.push("-v", `${_hostBrowserAgentWs}:${SANDBOX_AGENT_WORKSPACE_MOUNT}${agentMountSuffix}`);"#,
        );

    if browser_patched == browser_content {
        warn!("browser.js patch: target lines not found, skipping");
    } else {
        tokio::fs::write(&browser_path, &browser_patched)
            .await
            .map_err(|e| format!("写入 browser.js patch 失败: {}", e))?;
        info!("Patched dist/agents/sandbox/browser.js (normalised Windows host paths in -v args)");
    }

    // 6. Patch sandbox-paths.js — Windows bare drive "D:" from fileURLToPath must resolve to "D:\".
    let sandbox_paths_file = format!("{}/dist/agents/sandbox-paths.js", openclaw_dir);
    let sp_content = tokio::fs::read_to_string(&sandbox_paths_file)
        .await
        .map_err(|e| format!("读取 sandbox-paths.js 失败: {}", e))?;
    let sp_patched = sp_content.replace(
        r#"function resolveToCwd(filePath, cwd) {
    const expanded = expandPath(filePath);
    if (path.isAbsolute(expanded)) {
        return expanded;
    }
    return path.resolve(cwd, expanded);
}"#,
        r#"function resolveToCwd(filePath, cwd) {
    let expanded = expandPath(filePath);
    if (process.platform === "win32" && /^[A-Za-z]:$/.test(expanded)) {
        expanded = expanded + path.sep;
    }
    if (path.isAbsolute(expanded)) {
        return expanded;
    }
    return path.resolve(cwd, expanded);
}"#,
    );
    if sp_patched == sp_content {
        warn!("sandbox-paths.js patch: resolveToCwd block not found, skipping");
    } else {
        tokio::fs::write(&sandbox_paths_file, &sp_patched)
            .await
            .map_err(|e| format!("写入 sandbox-paths.js patch 失败: {}", e))?;
        info!("Patched dist/agents/sandbox-paths.js (Windows bare drive letter in resolveToCwd)");
    }

    // 7. Patch shell-utils.js — Windows：含 python/node -c 时用 PowerShell（引号正确）；含 2>nul、||、%VAR% 时用 cmd。
    let shell_utils_file = format!("{}/dist/agents/shell-utils.js", openclaw_dir);
    let su_content = tokio::fs::read_to_string(&shell_utils_file)
        .await
        .map_err(|e| format!("读取 shell-utils.js 失败: {}", e))?;
    const SU_MARKER_HEUR: &str = "OpenClaw-CN Manager patch: Windows exec shell (heuristic)";
    let block_cmd = r#"export function getShellConfig() {
    if (process.platform === "win32") {
        // OpenClaw-CN Manager patch: exec shell (cmd) for LLM-generated CMD/bash one-liners.
        // Upstream uses PowerShell (-Command); strings like `2>nul || echo` cause ParserError InvalidEndOfLine.
        const comspec = process.env.ComSpec?.trim();
        const shell = comspec && comspec.length > 0 ? comspec : "cmd.exe";
        return { shell, args: ["/d", "/s", "/c"] };
    }
    const envShell = process.env.SHELL?.trim();"#;
    let block_upstream = r#"export function getShellConfig() {
    if (process.platform === "win32") {
        // Use PowerShell instead of cmd.exe on Windows.
        // Problem: Many Windows system utilities (ipconfig, systeminfo, etc.) write
        // directly to the console via WriteConsole API, bypassing stdout pipes.
        // When Node.js spawns cmd.exe with piped stdio, these utilities produce no output.
        // PowerShell properly captures and redirects their output to stdout.
        return {
            shell: resolvePowerShellPath(),
            args: ["-NoProfile", "-NonInteractive", "-Command"],
        };
    }
    const envShell = process.env.SHELL?.trim();"#;
    let block_new = r##"function windowsExecNeedsPowershellQuoting(command) {
    const c = typeof command === "string" ? command.trim() : "";
    if (!c)
        return false;
    if (/\b(python\d*|py)\s+(-c|--command)\b/i.test(c))
        return true;
    // node -e / --eval：经 PowerShell -Command 再包一层时引号常断裂，表现为把 const 当 cmdlet → 走 cmd.exe
    return false;
}
function windowsExecLooksLikeCmdBatch(command) {
    const c = typeof command === "string" ? command.trim() : "";
    if (!c)
        return false;
    if (/2>nul\b|2>NUL\b/.test(c))
        return true;
    if (/\|\|/.test(c))
        return true;
    if (/%[A-Za-z0-9_]+%/.test(c))
        return true;
    if (/^\s*type\s/i.test(c))
        return true;
    if (/^\s*dir\s/i.test(c))
        return true;
    if (/^\s*more\s/i.test(c))
        return true;
    if (/^\s*cmd\s+/i.test(c))
        return true;
    return false;
}
export function normalizeWindowsExecCommand(command) {
    if (process.platform !== "win32" || typeof command !== "string")
        return command;
    return command.replace(/\btype\s+"([^"\r\n]+)"/gi, (_m, p) => {
        const inner = String(p);
        if (/\s/.test(inner))
            return `type "${inner}"`;
        return `type ${inner}`;
    });
}
export function getShellConfig(command) {
    if (process.platform === "win32") {
        const comspec = process.env.ComSpec?.trim();
        const cmdShell = comspec && comspec.length > 0 ? comspec : "cmd.exe";
        const cmdArgs = ["/d", "/s", "/c"];
        const psShell = resolvePowerShellPath();
        const psArgs = ["-NoProfile", "-NonInteractive", "-Command"];
        const cmdText = typeof command === "string" ? command : "";
        const chainedAmp = cmdText.includes("&&");
        // OpenClaw-CN Manager patch: Windows exec shell (heuristic).
        // cmd.exe breaks python/node -c quoting; PowerShell chokes on `2>nul`, `||`, %VAR%.
        // Windows PowerShell 5.1 常不支持 `cd x && python y` → ParserError InvalidEndOfLine；含 && 走 cmd。
        if (windowsExecNeedsPowershellQuoting(cmdText) && !chainedAmp)
            return { shell: psShell, args: psArgs };
        if (windowsExecLooksLikeCmdBatch(cmdText) || chainedAmp)
            return { shell: cmdShell, args: cmdArgs };
        return { shell: psShell, args: psArgs };
    }
    const envShell = process.env.SHELL?.trim();"##;
    let su_patched = if su_content.contains(SU_MARKER_HEUR) {
        su_content.clone()
    } else if su_content.contains(block_cmd) {
        su_content.replace(block_cmd, block_new)
    } else if su_content.contains(block_upstream) {
        su_content.replace(block_upstream, block_new)
    } else {
        warn!("shell-utils.js patch: 未识别的 getShellConfig 形态，跳过（可能已手动修改）");
        su_content.clone()
    };
    if su_patched != su_content {
        tokio::fs::write(&shell_utils_file, &su_patched)
            .await
            .map_err(|e| format!("写入 shell-utils.js patch 失败: {}", e))?;
        info!("Patched dist/agents/shell-utils.js (Windows exec shell heuristic)");
    }

    if let Err(e) = patch_shell_utils_drop_node_powershell(openclaw_dir).await {
        warn!("shell-utils node-e 升级失败（非致命）: {}", e);
    }
    if let Err(e) = patch_shell_utils_windows_exec_cmd_quoting(openclaw_dir).await {
        warn!("Windows exec cmd 引号补丁失败（非致命）: {}", e);
    }
    if let Err(e) = patch_shell_utils_windows_bat_exec_normalize(openclaw_dir).await {
        warn!("Windows .bat/cmd exec 规范化补丁失败（非致命）: {}", e);
    }

    // 8. Patch bash-tools.exec.js — 将待执行命令传入 getShellConfig，供 Windows 启发式选 shell。
    let bash_exec_file = format!("{}/dist/agents/bash-tools.exec.js", openclaw_dir);
    let be_content = tokio::fs::read_to_string(&bash_exec_file)
        .await
        .map_err(|e| format!("读取 bash-tools.exec.js 失败: {}", e))?;
    if be_content.contains("getShellConfig(opts.command)") {
        // already patched
    } else {
        let be_patched = be_content.replace(
            "const { shell, args: shellArgs } = getShellConfig();",
            "const { shell, args: shellArgs } = getShellConfig(opts.command);",
        );
        if be_patched == be_content {
            warn!("bash-tools.exec.js patch: getShellConfig() call site not found, skipping");
        } else {
            tokio::fs::write(&bash_exec_file, &be_patched)
                .await
                .map_err(|e| format!("写入 bash-tools.exec.js patch 失败: {}", e))?;
            info!("Patched dist/agents/bash-tools.exec.js (getShellConfig per-command on Windows)");
        }
    }

    if let Err(e) = patch_bash_tools_exec_windows_command_normalize(openclaw_dir).await {
        warn!("Windows bash-tools.exec 规范化补丁失败（非致命）: {}", e);
    }

    match patch_openclaw_gateway_localhost_usage(openclaw_dir).await {
        Ok(()) => info!("Patched dist/gateway/server-methods.js (localhost gateway usage RPC)"),
        Err(e) => warn!("server-methods.js 用量补丁未应用: {}", e),
    }

    match patch_sessions_usage_aggregate_fix(openclaw_dir).await {
        Ok(()) => info!(
            "Patched dist/gateway/server-methods/usage.js (sessions.usage aggregate all sessions)"
        ),
        Err(e) => warn!("sessions.usage aggregate fix 未应用: {}", e),
    }

    match patch_sessions_usage_session_display_fallbacks(openclaw_dir).await {
        Ok(()) => info!("Patched dist/gateway/server-methods/usage.js (sessions.usage session display fallbacks)"),
        Err(e) => warn!("sessions.usage 会话展示回退未应用: {}", e),
    }

    match patch_session_cost_usage_utc_and_discover(openclaw_dir).await {
        Ok(()) => info!("Patched dist/infra/session-cost-usage.js (UTC dayKey + discoverAllSessions + cost days fallback)"),
        Err(e) => warn!("session-cost-usage.js UTC + discover 补丁未应用: {}", e),
    }

    match patch_sessions_usage_all_agents(openclaw_dir).await {
        Ok(()) => info!("Patched dist/gateway/server-methods/usage.js (sessions.usage: discoverAllSessions 扫描全量 agent)"),
        Err(e) => warn!("sessions.usage 全 agent 发现补丁未应用: {}", e),
    }

    // Patch wechat plugin: withReplyDispatcher fallback for OpenClaw < 2026.3.22
    // jiti loads .ts source directly, so patch the .ts file (not .js)
    let wechat_ts_path = format!("{}/../plugins/wechat_clawbot/src/messaging/process-message.ts", openclaw_dir);
    if let Ok(ts_content) = tokio::fs::read_to_string(&wechat_ts_path).await {
        if ts_content.contains("withReplyDispatcher") && !ts_content.contains("typeof deps.channelRuntime.reply.withReplyDispatcher") {
            let patched = ts_content.replace(
                "await deps.channelRuntime.reply.withReplyDispatcher({\n      dispatcher,\n      run: () =>\n        deps.channelRuntime.reply.dispatchReplyFromConfig({\n          ctx: finalized,\n          cfg: deps.config,\n          dispatcher,\n          replyOptions: { ...replyOptions, disableBlockStreaming: true },\n        }),\n    });",
                "const replyArgs = {\n      ctx: finalized,\n      cfg: deps.config,\n      dispatcher,\n      replyOptions: { ...replyOptions, disableBlockStreaming: true },\n    };\n    if (typeof deps.channelRuntime.reply.withReplyDispatcher === \"function\") {\n      await deps.channelRuntime.reply.withReplyDispatcher({\n        dispatcher,\n        run: () => deps.channelRuntime.reply.dispatchReplyFromConfig(replyArgs),\n      });\n    } else {\n      await deps.channelRuntime.reply.dispatchReplyFromConfig(replyArgs);\n    }",
            );
            if patched != ts_content {
                tokio::fs::write(&wechat_ts_path, &patched).await.ok();
                info!("Patched wechat process-message.ts (withReplyDispatcher fallback)");
            }
        }
    }

    // Ensure wechat plugin account index exists (required by listIndexedWeixinAccountIds)
    let wechat_state_dir = format!("{}/../plugins/../openclaw-cn/state/openclaw-weixin", openclaw_dir);
    let wechat_accounts_dir = format!("{}/accounts", wechat_state_dir);
    let wechat_index_path = format!("{}/accounts.json", wechat_state_dir);
    if !Path::new(&wechat_index_path).exists() {
        let _ = tokio::fs::create_dir_all(&wechat_accounts_dir).await;
        let _ = tokio::fs::write(&wechat_index_path, "[\"default\"]").await;
        info!("Created wechat accounts.json index (default account)");
    }

    Ok(())
}
