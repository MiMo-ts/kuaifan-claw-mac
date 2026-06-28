//! 打包前断言所有内置资源已就位（**不下载任何文件**）。
//!
//! ## 设计原则
//!
//! - **零网络**：本文件不发起任何 HTTP / npm 请求，所有下载由 `download-bundles.ps1` 负责。
//! - **纯断言**：只验证文件是否存在、大小是否合理，不做任何 I/O 写操作（除了 `.resource_version`）。
//! - **release 严格**：release 模式缺失任何必需文件时 panic，打印明确的手动修复指引。
//! - **debug 宽松**：debug 模式只打警告，不断言，避免阻塞本地开发调试。
//!
//! ## 必需文件清单（release 模式必须全部存在；按构建 target 自动选平台）
//!
//! | 相对路径                                           | 最小大小   | 平台                | 描述          |
//! |--------------------------------------------------|----------|-------------------|-------------|
//! | `bundled-env/node-v22.14.0-win-x64.zip`          | 5 MB     | Windows x64       | Node.js 离线包 |
//! | `bundled-env/node-v22.14.0-darwin-arm64.tar.gz`  | 25 MB    | macOS Apple Silicon | Node.js 离线包 |
//! | `bundled-env/node-v22.14.0-darwin-x64.tar.gz`    | 25 MB    | macOS Intel        | Node.js 离线包 |
//! | `bundled-env/node-v22.14.0-linux-x64.tar.gz`     | 25 MB    | Linux x64         | Node.js 离线包 |
//! | `bundled-env/MinGit-2.53.0-64-bit.zip`           | 400 KB   | Windows x64       | MinGit 离线包  |
//! | `bundled-openclaw/openclaw-cn.zip`               | 1 MB     | 全平台              | openclaw-cn npm 包 |
//! | `resources/data/config/app.yaml`                | >0 B     | 全平台              | 应用配置模板    |
//! | `resources/data/config/instances.yaml`          | >0 B     | 全平台              | 实例配置模板    |
//! | `resources/data/config/models.yaml`             | >0 B     | 全平台              | 模型配置模板    |
//! | `resources/data/config/plugins.yaml`            | >0 B     | 全平台              | 插件配置模板    |
//! | `resources/data/config/robots.yaml`             | >0 B     | 全平台              | 机器人配置模板  |
//!
//! 注：macOS / Linux 不内置 MinGit，系统通常已自带（Xcode CLT / 系统包）。
//!
//! ## 安全检查
//!
//! release 模式额外检查 `resources/data/` 目录，禁止用户运行时数据目录混入打包资源。
//! 允许的子目录只有：`config/` 和以 `.` 开头的隐藏目录。

use std::fs;
use std::path::PathBuf;

/// Returns the platform-specific bundled Node.js file name and minimum size.
/// Windows: node-v22.14.0-win-x64.zip
/// macOS Apple Silicon: node-v22.14.0-darwin-arm64.tar.gz
/// macOS Intel: node-v22.14.0-darwin-x64.tar.gz
/// Linux: node-v22.14.0-linux-x64.tar.gz
fn bundled_node_filename() -> Option<(&'static str, u64)> {
    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        return Some(("node-v22.14.0-darwin-arm64.tar.gz", 25 * 1024 * 1024));
        #[cfg(target_arch = "x86_64")]
        return Some(("node-v22.14.0-darwin-x64.tar.gz", 25 * 1024 * 1024));
    }
    #[cfg(target_os = "windows")]
    {
        Some(("node-v22.14.0-win-x64.zip", 5 * 1024 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        Some(("node-v22.14.0-linux-x64.tar.gz", 25 * 1024 * 1024))
    }
}

/// Returns the platform-specific MinGit bundled file name and minimum size.
///
/// macOS 不内置 MinGit：系统已通过 Xcode Command Line Tools 自带 git，
/// 且 env_paths.rs 优先探测 `/usr/bin/git` → `/opt/homebrew/bin/git` → `/usr/local/bin/git`。
/// 强制打包 MinGit 既无意义也会因为 MinGit 官方只发 .zip 而破坏 release 断言。
fn bundled_mingit_filename() -> Option<(&'static str, u64)> {
    #[cfg(target_os = "macos")]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        Some(("MinGit-2.53.0-64-bit.zip", 400 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        None
    }
}

const REQUIRED_CONFIG_FILES: &[&str] = &[
    "app.yaml",
    "instances.yaml",
    "models.yaml",
    "plugins.yaml",
    "robots.yaml",
];

/// 内置环境包目录（与 tauri.conf.json `bundle.resources` 中的 `bundled-env/*` 一致）
const BUNDLED_ENV_DIR: &str = "bundled-env";

const FORBIDDEN_DATA_SUBDIRS: &[&str] = &[
    "backups",
    "logs",
    "metrics",
    "openclaw-cn",
    "plugins",
    "robots",
    "instances",
    "openclaw-state",
    ".cache",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn file_size(path: &PathBuf) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

fn file_sufficient(path: &PathBuf, min_bytes: u64) -> bool {
    file_size(path).is_some_and(|s| s >= min_bytes)
}

fn print_file_info(path: &PathBuf) -> String {
    match file_size(path) {
        Some(sz) => format!("{:.1} KB", sz as f64 / 1024.0),
        None => "不存在".to_string(),
    }
}

/// 检查 resources/data/ 目录下是否有禁止的用户数据子目录混入。
/// 允许的目录：config/、以 . 开头的隐藏目录。
fn check_user_data_forbidden(manifest_dir: &PathBuf) -> Vec<String> {
    let data_dir = manifest_dir.join("resources/data");
    if !data_dir.is_dir() {
        return Vec::new();
    }

    let mut forbidden = Vec::new();
    if let Ok(entries) = fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();

            // config/ 和隐藏目录（.xxx）允许存在
            if name == "config" || name.starts_with('.') {
                continue;
            }

            if FORBIDDEN_DATA_SUBDIRS.iter().any(|&f| f == name) {
                forbidden.push(name);
            }
        }
    }
    forbidden
}

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let is_release = profile == "release";

    let md = manifest_dir();

    // ── 1. 安全检查：禁止用户运行时数据混入 resources/data ──────────────
    let forbidden = check_user_data_forbidden(&md);
    if !forbidden.is_empty() {
        let msg = format!(
            "release 打包拒绝：resources/data/ 下存在用户运行时数据目录「{}」。\n\n\
             这通常是因为开发调试时把 data/ 目录错误地复制进了 src-tauri/resources/data/。\n\n\
             解决方法：\n\
               从 src-tauri/resources/data/ 删除上述目录后重新构建。\n\n\
             排除规则：\n\
               - config/ 是配置模板目录（可保留）\n\
               - .xxx 隐藏目录（可保留）\n\
               - 其余如 backups/ logs/ openclaw-cn/ plugins/ 等均为运行时数据（勿打包）。",
            forbidden.join("、")
        );
        if is_release {
            panic!("{}", msg);
        } else {
            println!("cargo:warning={}", msg);
        }
    }

    // ── 2. 必需打包文件断言（仅 release 模式）─────────────────────────────
    // Skip if SKIP_BUNDLED_CHECK=true (set by CI for cross-platform builds)
    let skip_check = std::env::var("SKIP_BUNDLED_CHECK").ok().map(|v| v == "true").unwrap_or(false);

    if is_release && !skip_check {
        let mut missing_files: Vec<String> = Vec::new();

        // 2a. Build the actual bundle list with platform-specific filenames
        let node_bundle = bundled_node_filename();
        let mingit = bundled_mingit_filename();

        let mut actual_bundles: Vec<(&str, &str, u64, &str)> = Vec::new();
        if let Some((file, min)) = node_bundle {
            actual_bundles.push((BUNDLED_ENV_DIR, file, min, "Node.js 离线包"));
        }
        if let Some((file, min)) = mingit {
            actual_bundles.push((BUNDLED_ENV_DIR, file, min, "MinGit 离线包"));
        }
        // openclaw-cn.zip 不在 bundled-env/ 下，独立目录
        actual_bundles.push((
            "bundled-openclaw",
            "openclaw-cn.zip",
            1024 * 1024,
            "openclaw-cn npm 包 (openclaw-cn.zip)",
        ));

        for (subdir, rel_path, min_bytes, desc) in actual_bundles {
            let full = md.join(subdir).join(rel_path);
            if !file_sufficient(&full, min_bytes) {
                let info = print_file_info(&full);
                missing_files.push(format!(
                    "  [缺失或过小] {}\n    路径: {}\n    当前: {}  最低: {} KB\n    状态: 需运行 download-bundles 脚本下载",
                    desc,
                    full.display(),
                    info,
                    min_bytes / 1024
                ));
            }
        }

        // 2b. 配置模板文件（config/*.yaml）
        let config_dir = md.join("resources/data/config");
        for filename in REQUIRED_CONFIG_FILES {
            let path = config_dir.join(filename);
            if !path.is_file() {
                missing_files.push(format!(
                    "  [缺失] config/{}\n    路径: {}\n    状态: 配置模板文件必须存在",
                    filename,
                    path.display()
                ));
            }
        }

        if !missing_files.is_empty() {
            panic!(
                "release 打包前置条件未满足。请先运行下载脚本：\n\n\
                 # Windows (PowerShell):\n\
                 cd src-tauri\n\
                 pwsh -File ./download-bundles.ps1\n\n\
                 # macOS / Linux:\n\
                 cd src-tauri\n\
                 chmod +x scripts/download-bundles.sh\n\
                 ./scripts/download-bundles.sh\n\n\
                 或参考以下缺失项手动处理：\n\n\
                 {}\n\n\
                 完整构建命令（下载完成后）：\n\
                   cargo tauri build\n\n\
                 若下载脚本失败，请检查网络后重试。",
                missing_files.join("\n")
            );
        }

        // ── 3. 写入 .resource_version ───────────────────────────────────
        let ver = env!("CARGO_PKG_VERSION");
        let ver_file = md.join("resources/data/.resource_version");
        let new_content = format!("{}\n", ver);
        let needs_write = fs::read_to_string(&ver_file)
            .map(|c| c.trim() != ver)
            .unwrap_or(true);
        if needs_write {
            if let Err(e) = fs::write(&ver_file, new_content) {
                eprintln!(
                    "warning: 写入 .resource_version 失败（非致命）: {}",
                    e
                );
            } else {
                println!("cargo:warning=resources/data/.resource_version 已更新为 v{}", ver);
            }
        }
    }

    // Tauri 2：仅当 STATIC_VCRUNTIME=true 时才会执行 static_vcruntime（真正去掉对 VCRUNTIME140.dll 的依赖）。
    // 否则即便各 crate 带 +crt-static，最终 exe 仍可能动态链接 VC 运行库，干净 Windows 会报缺 DLL。
    #[cfg(windows)]
    std::env::set_var("STATIC_VCRUNTIME", "true");

    tauri_build::build()
}
