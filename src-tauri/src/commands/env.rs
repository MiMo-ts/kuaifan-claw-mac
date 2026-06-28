// 环境检测命令

use crate::commands::hidden_cmd;
use crate::env_paths::{env_root, npm_exe, resolve_git, resolve_node};
use crate::models::{EnvAutoFixResult, EnvCheckResult, EnvItem, EnvStatus};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
#[cfg(target_os = "macos")]
use crate::mirror::InstallProgressEvent;
use tracing::{info, warn};

// 检查 Node.js 版本（优先检测自包含 node.exe，兜底 PATH）
#[tauri::command]
pub async fn check_node_version(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvItem, String> {
    info!("检查 Node.js...");
    let data_base = data_dir.inner().get_data_dir();

    let (node_path, is_system) = resolve_node(&data_base);

    let output = Command::new(&node_path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let version_str = version.trim_start_matches('v');
            let major: u32 = version_str
                .split('.')
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);

            if major >= 18 {
                Ok(EnvItem {
                    name: "Node.js".to_string(),
                    version: Some(version.clone()),
                    status: EnvStatus::Success,
                    message: format!(
                        "已安装 {}（>=18）{}",
                        version,
                        if is_system { " [系统]" } else { " [内置]" }
                    ),
                    required: true,
                })
            } else {
                Ok(EnvItem {
                    name: "Node.js".to_string(),
                    version: Some(version),
                    status: EnvStatus::Error,
                    message: "版本过低，需要 >=18".to_string(),
                    required: true,
                })
            }
        }
        None => Ok(EnvItem {
            name: "Node.js".to_string(),
            version: None,
            status: EnvStatus::Error,
            message: if is_system {
                "系统 PATH 中 node 版本过低或不可用".to_string()
            } else {
                "未找到 Node.js（系统 PATH 与内置均缺失）。点击「自动修复」使用内置 Node".to_string()
            },
            required: true,
        }),
    }
}

// 检查 Git 版本（优先检测自包含 git.exe，兜底 PATH）
#[tauri::command]
pub async fn check_git_version(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvItem, String> {
    info!("检查 Git...");
    let data_base = data_dir.inner().get_data_dir();

    let (git_path, is_system) = resolve_git(&data_base);

    let output = Command::new(&git_path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => {
            let output_str = String::from_utf8_lossy(&out.stdout).to_string();
            let version = output_str.replace("git version ", "").trim().to_string();

            Ok(EnvItem {
                name: "Git".to_string(),
                version: Some(version.clone()),
                status: EnvStatus::Success,
                message: format!(
                    "已安装 Git {}{}",
                    version,
                    if is_system { " [系统]" } else { " [内置]" }
                ),
                required: true,
            })
        }
        None => Ok(EnvItem {
            name: "Git".to_string(),
            version: None,
            status: EnvStatus::Error,
            message: if is_system {
                "系统 PATH 中 git 不可用".to_string()
            } else {
                "未找到 Git（系统 PATH 与内置均缺失）。点击「自动修复」使用内置 Git".to_string()
            },
            required: true,
        }),
    }
}

/// 尝试运行 npm 并获取版本。exe_path 若提供，则优先使用该路径。
fn try_npm_version_output(exe_path: Option<&std::path::Path>) -> Option<std::process::Output> {
    let try_with = |cmd: &str, args: &[&str]| -> Option<std::process::Output> {
        let mut c = Command::new(cmd);
        #[cfg(windows)]
        if cmd == "cmd" || cmd == "pnpm" {
            c.creation_flags(0x08000000);
        }
        c.args(args).arg("--version");
        if let Some(p) = exe_path {
            c.env_clear();
            #[cfg(windows)]
            {
                c.env("PATH", p.parent()?.to_str()?)
                    .env("PATHEXT", ".COM;.EXE");
            }
            #[cfg(not(windows))]
            {
                c.env("PATH", p.parent()?.to_str()?);
            }
        }
        c.output().ok().filter(|o| o.status.success())
    };

    try_with("npm", &[])
        .or_else(|| try_with("npm", &["--version"]))
        .or_else(|| try_with("pnpm", &[]))
        .or_else(|| try_with("pnpm", &["--version"]))
        .or_else(|| {
            #[cfg(windows)]
            {
                try_with("cmd", &["/C", "npm"]).or_else(|| try_with("npm.cmd", &[]))
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
}

// 检查 npm 版本（优先检测系统 PATH 中的 npm，再回退内置 npm）
#[tauri::command]
pub async fn check_npm_version(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvItem, String> {
    info!("检查 npm...");

    let data_base = data_dir.inner().get_data_dir();
    let env_dir = env_root(&data_base);

    // macOS GUI 进程的 PATH 可能不完整，额外检查常见路径
    #[cfg(target_os = "macos")]
    {
        let common_npm_paths = [
            "/usr/local/bin/npm",
            "/opt/homebrew/bin/npm",
            "/opt/homebrew/opt/node/bin/npm",
            "/opt/local/bin/npm",
            "/usr/local/opt/node/bin/npm",
        ];
        for npm_path in &common_npm_paths {
            let path = Path::new(npm_path);
            if path.is_file() {
                if let Ok(output) = Command::new(&path).arg("--version").output() {
                    if output.status.success() {
                        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        tracing::info!("check_npm_version 找到 npm: {} v{}", npm_path, version);
                        return Ok(EnvItem {
                            name: "npm".to_string(),
                            version: Some(version.clone()),
                            status: EnvStatus::Success,
                            message: format!("已安装 npm {} [系统]", version),
                            required: false,
                        });
                    }
                }
            }
        }
        // 搜索 ~/.nvm/versions/node/*/bin/npm
        if let Ok(home) = std::env::var("HOME") {
            let nvm_dir = PathBuf::from(&home).join(".nvm").join("versions").join("node");
            if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let npm_path = entry.path().join("bin").join("npm");
                    if npm_path.is_file() {
                        if let Ok(output) = Command::new(&npm_path).arg("--version").output() {
                            if output.status.success() {
                                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                tracing::info!("check_npm_version 找到 nvm npm: {} v{}", npm_path.display(), version);
                                return Ok(EnvItem {
                                    name: "npm".to_string(),
                                    version: Some(version.clone()),
                                    status: EnvStatus::Success,
                                    message: format!("已安装 npm {} [nvm]", version),
                                    required: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // 第一优先：系统 PATH 中的 npm
    match try_npm_version_output(None) {
        Some(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Ok(EnvItem {
                name: "npm".to_string(),
                version: Some(version.clone()),
                status: EnvStatus::Success,
                message: format!("已安装 npm {} [系统]", version),
                required: false,
            });
        }
        None => {}
    }

    // 第二优先：内置 npm（data/env/node）
    let bundled_npm = npm_exe(&env_dir);
    if bundled_npm.exists() {
        let output = Command::new(&bundled_npm)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success());
        if let Some(out) = output {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Ok(EnvItem {
                name: "npm".to_string(),
                version: Some(version.clone()),
                status: EnvStatus::Success,
                message: format!("已安装 npm {} [内置]", version),
                required: false,
            });
        }
    }

    Ok(EnvItem {
        name: "npm".to_string(),
        version: None,
        status: EnvStatus::Warning,
        message: "未检测到 npm（系统 PATH 与内置均缺失）".to_string(),
        required: false,
    })
}

// 检查 pnpm 是否已安装
#[tauri::command]
pub async fn check_pnpm_installation() -> Result<EnvItem, String> {
    info!("检查 pnpm...");

    // macOS GUI 进程的 PATH 可能不完整，额外检查常见路径
    #[cfg(target_os = "macos")]
    {
        let common_pnpm_paths = [
            "/usr/local/bin/pnpm",
            "/opt/homebrew/bin/pnpm",
            "/opt/homebrew/opt/pnpm/bin/pnpm",
            "/opt/local/bin/pnpm",
            "/usr/local/opt/pnpm/bin/pnpm",
        ];
        for pnpm_path in &common_pnpm_paths {
            let path = Path::new(pnpm_path);
            if path.is_file() {
                if let Ok(output) = Command::new(&path).arg("--version").output() {
                    if output.status.success() {
                        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        tracing::info!("check_pnpm_installation 找到 pnpm: {} v{}", pnpm_path, version);
                        return Ok(EnvItem {
                            name: "pnpm".to_string(),
                            version: Some(version.clone()),
                            status: EnvStatus::Success,
                            message: format!("已安装 pnpm {}", version),
                            required: false,
                        });
                    }
                }
            }
        }
        // 搜索 ~/.nvm/versions/node/*/bin/pnpm
        if let Ok(home) = std::env::var("HOME") {
            let nvm_dir = PathBuf::from(&home).join(".nvm").join("versions").join("node");
            if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let pnpm_path = entry.path().join("bin").join("pnpm");
                    if pnpm_path.is_file() {
                        if let Ok(output) = Command::new(&pnpm_path).arg("--version").output() {
                            if output.status.success() {
                                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                tracing::info!("check_pnpm_installation 找到 nvm pnpm: {} v{}", pnpm_path.display(), version);
                                return Ok(EnvItem {
                                    name: "pnpm".to_string(),
                                    version: Some(version.clone()),
                                    status: EnvStatus::Success,
                                    message: format!("已安装 pnpm {}", version),
                                    required: false,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let output = Command::new("pnpm")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .or_else(|| {
            #[cfg(windows)]
            {
                hidden_cmd::cmd()
                    .args(["/C", "pnpm", "--version"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
            }
            #[cfg(not(windows))]
            {
                None
            }
        });

    match output {
        Some(out) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();

            Ok(EnvItem {
                name: "pnpm".to_string(),
                version: Some(version.clone()),
                status: EnvStatus::Success,
                message: format!("已安装 pnpm {}", version),
                required: false,
            })
        }
        None => Ok(EnvItem {
            name: "pnpm".to_string(),
            version: None,
            status: EnvStatus::Warning,
            message: "未安装 pnpm（推荐使用）".to_string(),
            required: false,
        }),
    }
}

/// 检查 Homebrew（仅 macOS 有意义；本应用在 mac 上可用 brew 安装 Node/Git）
#[tauri::command]
pub async fn check_homebrew() -> Result<EnvItem, String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("Homebrew 检测仅在 macOS 上提供".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        info!("检查 Homebrew...");

        // 尝试多个可能的 brew 路径（GUI 应用可能没有完整 PATH）
        let brew_paths = [
            "brew",
            "/opt/homebrew/bin/brew",
            "/usr/local/bin/brew",
            "/home/linuxbrew/.linuxbrew/bin/brew",
        ];

        for brew_path in &brew_paths {
            match Command::new(brew_path).arg("--version").output() {
                Ok(output) if output.status.success() => {
                    let first = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    return Ok(EnvItem {
                        name: "Homebrew".to_string(),
                        version: None,
                        status: EnvStatus::Success,
                        message: if first.is_empty() {
                            format!("已安装 ({})", brew_path)
                        } else {
                            format!("已安装（{}）", first)
                        },
                        required: false,
                    });
                }
                _ => continue,
            }
        }

        // 所有路径都失败
        Ok(EnvItem {
            name: "Homebrew".to_string(),
            version: None,
            status: EnvStatus::Warning,
            message: "未检测到 Homebrew（可选，用于一键安装 Node/Git，见 https://brew.sh）".to_string(),
            required: false,
        })
    }
}

/// 获取应用版本号
#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// 网络连通性检测（探测实际下载资源，比首页检测更准确）
#[tauri::command]
pub async fn check_network_connectivity() -> Result<EnvItem, String> {
    info!("检查网络连通性（探测下载资源）...");

    let client = reqwest::Client::builder()
        .user_agent("OpenClaw-CN-Manager/1.0")
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    // 探测目标：GitHub releases（Git 下载源） + Node.js 官方 dist（Node 下载源）
    // 用 HEAD 请求快速检测，避免下载完整资源
    let targets = [
        // GitHub releases 页面（实际大文件 CDN）
        (
            "GitHub releases",
            "https://github.com/git-for-windows/git/releases/expanded_assets/v2.43.0.windows.1",
        ),
        // Node.js 官方 dist 目录
        ("Node.js dist", "https://nodejs.org/dist/v22.14.0/"),
    ];

    let mut results: Vec<(&str, bool, String)> = Vec::new();

    for (name, url) in &targets {
        match client.head(*url).send().await {
            Ok(resp) => {
                let ok = resp.status().is_success()
                    || resp.status().as_u16() == 302
                    || resp.status().as_u16() == 301;
                results.push((name, ok, format!("{}", resp.status())));
                if !ok {
                    warn!("网络探测失败 [{}]: {} → {}", name, url, resp.status());
                }
            }
            Err(e) => {
                results.push((name, false, e.to_string()));
                warn!("网络探测请求失败 [{}]: {}", name, e);
            }
        }
    }

    // 同时保留 GitHub 首页检测作为兜底参考
    let homepage_ok = match client.get("https://github.com").send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    };
    results.push((
        "GitHub 首页",
        homepage_ok,
        if homepage_ok { "OK" } else { "失败" }.to_string(),
    ));

    // 判断：只要 GitHub releases 或 Node.js dist 之一可达，即认为下载网络可用
    let download_ok = results
        .iter()
        .any(|(n, ok, _)| *ok && (*n == "GitHub releases" || *n == "Node.js dist"));

    let detail_lines: Vec<String> = results
        .iter()
        .map(|(n, ok, detail)| format!("{}: {}{}", n, if *ok { "正常" } else { "失败" }, detail))
        .collect();

    if download_ok {
        Ok(EnvItem {
            name: "网络连通性".to_string(),
            version: None,
            status: EnvStatus::Success,
            message: format!("下载资源可达。详情: {}", detail_lines.join(" | ")),
            required: false,
        })
    } else {
        Ok(EnvItem {
            name: "网络连通性".to_string(),
            version: None,
            status: EnvStatus::Warning,
            message: format!(
                "下载资源不可达（GitHub releases: {}，Node.js: {}），可能需要配置代理。详情: {}",
                if results
                    .iter()
                    .find(|(n, _, _)| *n == "GitHub releases")
                    .map(|(_, ok, _)| *ok)
                    .unwrap_or(false)
                {
                    "OK"
                } else {
                    "失败"
                },
                if results
                    .iter()
                    .find(|(n, _, _)| *n == "Node.js dist")
                    .map(|(_, ok, _)| *ok)
                    .unwrap_or(false)
                {
                    "OK"
                } else {
                    "失败"
                },
                detail_lines.join(" | ")
            ),
            required: false,
        })
    }
}

/// Strip Windows `\\?\` extended-length prefix so `Path` components yield `Disk` instead of a broken drive letter from `VerbatimDisk` string parsing.
fn strip_extended_path_prefix(path: &str) -> &str {
    if path.starts_with("\\\\?\\") {
        path.get(4..).unwrap_or(path)
    } else {
        path
    }
}

#[cfg(windows)]
fn windows_drive_letter_for_path(root: &Path) -> Option<String> {
    use std::path::Prefix;
    let c = root.components().next()?;
    let std::path::Component::Prefix(pref) = c else {
        return None;
    };
    match pref.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
            Some((d as char).to_ascii_uppercase().to_string())
        }
        _ => None,
    }
}

fn disk_free_gb_for_path(data_dir: &str) -> Option<f64> {
    let trimmed = data_dir.trim();
    let normalized = strip_extended_path_prefix(trimmed);
    let root = Path::new(if normalized.is_empty() {
        "."
    } else {
        normalized
    });

    #[cfg(windows)]
    {
        let drive = windows_drive_letter_for_path(root).unwrap_or_else(|| "C".to_string());

        let ps = format!("(Get-PSDrive -Name '{}').Free / 1GB", drive);
        let output = hidden_cmd::powershell()
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
            .output()
            .ok()?;
        let free_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        free_str.parse::<f64>().ok()
    }

    #[cfg(not(windows))]
    {
        let output = Command::new("df")
            .args(["-Pk", root.to_str()?])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().nth(1)?;
        let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
        Some(avail_kb as f64 / (1024.0 * 1024.0))
    }
}

fn env_item_disk_space(data_dir: &str) -> EnvItem {
    let drive_hint = {
        #[cfg(windows)]
        {
            let normalized = strip_extended_path_prefix(data_dir.trim());
            let root = Path::new(if normalized.is_empty() {
                "."
            } else {
                normalized
            });
            windows_drive_letter_for_path(root)
                .map(|d| format!("（检测盘: {}:）", d))
                .unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            String::new()
        }
    };

    if let Some(free_gb) = disk_free_gb_for_path(data_dir) {
        let status = if free_gb >= 10.0 {
            EnvStatus::Success
        } else if free_gb >= 5.0 {
            EnvStatus::Warning
        } else {
            EnvStatus::Error
        };
        let message = if free_gb >= 10.0 {
            format!(
                "数据目录所在盘可用 {:.1} GB（推荐 >=10GB）{}",
                free_gb, drive_hint
            )
        } else if free_gb >= 5.0 {
            format!(
                "数据目录所在盘可用 {:.1} GB，建议 >=10GB{}",
                free_gb, drive_hint
            )
        } else {
            format!(
                "数据目录所在盘空间不足 {:.1} GB，需要 >=10GB{}",
                free_gb, drive_hint
            )
        };

        EnvItem {
            name: "磁盘空间".to_string(),
            version: None,
            status,
            message,
            required: false,
        }
    } else {
        EnvItem {
            name: "磁盘空间".to_string(),
            version: None,
            status: EnvStatus::Warning,
            message: "无法检测磁盘空间".to_string(),
            required: false,
        }
    }
}

// 检查磁盘空间（按应用 data 目录所在分区；兼容旧调用）
#[tauri::command]
pub async fn check_disk_space(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvItem, String> {
    info!("检查磁盘空间...");
    let dir = data_dir.inner().get_data_dir();
    Ok(env_item_disk_space(&dir))
}

// 运行环境检测（返回所有检测项的汇总结果）
#[tauri::command]
pub async fn run_env_check(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvCheckResult, String> {
    info!("开始环境检测...");

    let mut items = Vec::new();
    let mut recommendations = Vec::new();
    let mut has_error = false;

    let dir = data_dir.inner().get_data_dir();
    let disk_item = env_item_disk_space(&dir);

    // 并行检测（磁盘按 data 目录分区）
    let (node, git, npm, pnpm, network) = tokio::join!(
        check_node_version(data_dir.clone()),
        check_git_version(data_dir.clone()),
        check_npm_version(data_dir.clone()),
        check_pnpm_installation(),
        check_network_connectivity(),
    );

    if let Ok(item) = node {
        items.push(item.clone());
        if item.status == EnvStatus::Error {
            has_error = true;
        }
    }
    if let Ok(item) = git {
        items.push(item.clone());
        if item.status == EnvStatus::Error {
            has_error = true;
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(item) = check_homebrew().await {
            items.push(item);
        }
    }

    if let Ok(item) = npm {
        items.push(item);
    }
    if let Ok(item) = pnpm {
        items.push(item);
    }
    if let Ok(item) = network {
        items.push(item);
    }
    items.push(disk_item);

    // 生成建议
    for item in &items {
        if item.status == EnvStatus::Warning {
            recommendations.push(format!("建议安装/更新: {}", item.name));
        } else if item.status == EnvStatus::Error {
            recommendations.push(format!("必须安装/更新: {}", item.name));
        }
    }

    Ok(EnvCheckResult {
        success: !has_error,
        items,
        recommendations,
    })
}

/// 一键尝试安装：缺失的 Node / Git / pnpm（需管理员权限与网络；安装后可能需要重启本程序以刷新 PATH）
/// 在 macOS 上，若 Homebrew 缺失则优先安装（因为后续 Node/Git 均依赖它）
#[tauri::command]
pub async fn run_env_auto_fix(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<EnvAutoFixResult, String> {
    info!("开始一键修复环境...");

    let mut messages = Vec::new();
    let mut any_err = false;

    // ── macOS: Homebrew 安装（可选，不阻塞其他组件）───────────────
    #[cfg(target_os = "macos")]
    {
        let brew = check_homebrew().await?;
        if brew.status != EnvStatus::Success {
            messages.push("> Homebrew 未安装（可选，用于安装 Node/Git）".to_string());
            let _ = app.emit(
                "install-progress",
                InstallProgressEvent::started("homebrew", "Homebrew 未安装（跳过，安装 Node/Git 备用方案）"),
            );
            match crate::commands::installer::install_homebrew(app.clone()).await {
                Ok(m) => {
                    messages.push(format!("Homebrew: {}", m));
                    let _ = app.emit(
                        "install-progress",
                        InstallProgressEvent::finished("homebrew", &m),
                    );
                }
                Err(e) => {
                    // Homebrew 可选，不设置 any_err，只提示信息
                    messages.push(format!(
                        "> Homebrew 跳过（可选，可手动安装 https://brew.sh）：{}",
                        e
                    ));
                    let _ = app.emit(
                        "install-progress",
                        InstallProgressEvent::detail("homebrew", &format!("Homebrew 可选，跳过：{}", e)),
                    );
                }
            }
        }
    }

    let node = check_node_version(data_dir.clone()).await?;
    if node.status == EnvStatus::Error {
        match crate::commands::installer::install_node(app.clone(), data_dir.clone()).await {
            Ok(m) => messages.push(format!("Node.js: {}", m)),
            Err(e) => {
                any_err = true;
                messages.push(format!("Node.js 安装失败: {}", e));
            }
        }
    }

    let git = check_git_version(data_dir.clone()).await?;
    if git.status == EnvStatus::Error {
        match crate::commands::installer::install_git(app.clone(), data_dir.clone()).await {
            Ok(m) => messages.push(format!("Git: {}", m)),
            Err(e) => {
                any_err = true;
                messages.push(format!("Git 安装失败: {}", e));
            }
        }
    }

    let pnpm = check_pnpm_installation().await?;
    if pnpm.status != EnvStatus::Success && try_npm_version_output(None).is_some() {
        match crate::commands::installer::install_pnpm(app.clone()).await {
            Ok(m) => messages.push(format!("pnpm: {}", m)),
            Err(e) => messages.push(format!("pnpm 未安装或安装跳过: {}", e)),
        }
    } else if pnpm.status != EnvStatus::Success {
        messages.push("pnpm: 跳过（需先可用 npm）".to_string());
    }

    if messages.is_empty() {
        messages.push("未发现需要本工具代为安装的必需项（Node/Git 已就绪）".to_string());
    } else {
        messages.push(
            "提示：若刚完成安装，建议关闭本程序并重新打开（刷新 PATH），再点「重新检测」验证。"
                .to_string(),
        );
    }

    Ok(EnvAutoFixResult {
        ok: !any_err,
        messages,
    })
}
