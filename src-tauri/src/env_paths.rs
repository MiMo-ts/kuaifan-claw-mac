// 自包含工具路径解析与解压工具
// 环境配置优先级（两层）：
//   第一优先：系统全局 PATH 中的工具（如用户已安装 node/git）
//   第二优先：内置工具（data/env/ 或 bundled-env/），无管理员权限也能使用
//
// 优先级设计：
//   - resolve_node / resolve_git：先查系统 PATH，再查内置
//   - build_deps_env_path：若用系统工具则不修改 PATH，若用内置则 prepend 内置目录

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::info;

#[cfg(windows)]
use crate::commands::hidden_cmd;

/// 自包含工具根目录（data/env/）
pub fn env_root(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("env")
}

/// 自包含 Node 安装根目录（其下直接有 `node.exe` / `bin/node`）。
/// 支持：`data/env/node/` 扁平布局，或官方 zip 解压后的 `data/env/node/node-v22.*-win-x64/` 单级子目录。
pub fn portable_node_root(env_dir: &Path) -> PathBuf {
    let base = env_dir.join("node");
    #[cfg(windows)]
    {
        if base.join("node.exe").is_file() {
            return base;
        }
        let mut nested: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("node.exe").is_file() {
                    nested.push(p);
                }
            }
        }
        nested.sort();
        if let Some(p) = nested.into_iter().next() {
            return p;
        }
    }
    #[cfg(not(windows))]
    {
        if base.join("bin").join("node").is_file() {
            return base;
        }
        let mut nested: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.join("bin").join("node").is_file() {
                    nested.push(p);
                }
            }
        }
        nested.sort();
        if let Some(p) = nested.into_iter().next() {
            return p;
        }
    }
    base
}

/// 自包含 Node.js 可执行文件路径
#[cfg(windows)]
pub fn node_exe(env_dir: &Path) -> PathBuf {
    portable_node_root(env_dir).join("node.exe")
}

#[cfg(not(windows))]
pub fn node_exe(env_dir: &Path) -> PathBuf {
    portable_node_root(env_dir).join("bin").join("node")
}

/// 自包含 Git 可执行文件路径（data/env/git/cmd/git.exe）
#[cfg(windows)]
pub fn git_exe(env_dir: &Path) -> PathBuf {
    env_dir.join("git").join("cmd").join("git.exe")
}

/// 自包含 Git 可执行文件路径（data/env/git/bin/git）
#[cfg(not(windows))]
pub fn git_exe(env_dir: &Path) -> PathBuf {
    env_dir.join("git").join("bin").join("git")
}

/// npm 可执行文件路径（与 `node.exe` 同目录，官方 zip 布局）
#[cfg(windows)]
pub fn npm_exe(env_dir: &Path) -> PathBuf {
    portable_node_root(env_dir).join("npm.cmd")
}

#[cfg(not(windows))]
pub fn npm_exe(env_dir: &Path) -> PathBuf {
    portable_node_root(env_dir).join("bin").join("npm")
}

/// 检查自包含工具是否存在
pub fn node_exists(env_dir: &Path) -> bool {
    node_exe(env_dir).exists()
}

pub fn git_exists(env_dir: &Path) -> bool {
    git_exe(env_dir).exists()
}

#[cfg(windows)]
fn node_executable_in_portable_root(root: &Path) -> PathBuf {
    root.join("node.exe")
}

#[cfg(not(windows))]
fn node_executable_in_portable_root(root: &Path) -> PathBuf {
    root.join("bin").join("node")
}

/// 在 data/env/node 或 bundled-env 中查找内置 node
fn find_bundled_node(_data_dir: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for cand in [
                exe_dir.join("data").join("env"),
                exe_dir.join("resources").join("bundled-env"),
                exe_dir.join("bundled-env"),
            ] {
                let found = portable_node_root(&cand);
                let node_path = node_executable_in_portable_root(&found);
                if node_path.is_file() {
                    tracing::debug!(
                        "find_bundled_node found: {} (root {})",
                        node_path.display(),
                        found.display()
                    );
                    return Some(node_path);
                }
            }
        }
    }
    None
}

/// 在 data/env/git 或 bundled-env 中查找内置 git
fn find_bundled_git(_data_dir: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for cand in [
                exe_dir.join("data").join("env"),
                exe_dir.join("resources").join("bundled-env"),
                exe_dir.join("bundled-env"),
            ] {
                let g = git_exe(&cand);
                if g.exists() {
                    tracing::debug!("find_bundled_git found: {}", g.display());
                    return Some(g);
                }
            }
        }
    }
    None
}

/// 解析 Node.js 路径。
/// 优先级：1. 系统 PATH 中的 node → 2. 内置 node
/// 返回 (path, is_system) 其中 is_system=true 表示来自系统 PATH
pub fn resolve_node(data_dir: &str) -> (PathBuf, bool) {
    // 第一优先：系统 PATH 中的 node
    #[cfg(windows)]
    if hidden_cmd::cmd().arg("/C").arg("node").arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return (PathBuf::from("node"), true);
    }
    #[cfg(not(windows))]
    if Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return (PathBuf::from("node"), true);
    }

    // macOS Apple Silicon: 额外检查 Homebrew 安装路径（GUI 应用可能 PATH 不完整）
    #[cfg(target_os = "macos")]
    {
        let homebrew_paths = [
            "/opt/homebrew/bin/node",
            "/opt/homebrew/opt/node/bin/node",
            "/usr/local/bin/node", // Intel Mac 兜底
        ];
        for p in &homebrew_paths {
            let node_path = Path::new(p);
            if node_path.is_file() {
                #[cfg(windows)]
                if hidden_cmd::cmd().arg("/C").arg(p).arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::info!("找到 Homebrew Node.js: {}", p);
                    return (PathBuf::from(p), true);
                }
                #[cfg(not(windows))]
                if Command::new(p)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::info!("找到 Homebrew Node.js: {}", p);
                    return (PathBuf::from(p), true);
                }
            }
        }
    }

    // 第二优先：内置 node（data/env/node）
    let env_dir = env_root(data_dir);
    let bundled_path = node_exe(&env_dir);
    if bundled_path.exists() {
        return (bundled_path, false);
    }

    // 第三优先：bundled-env（exe 同级目录下的内置包）
    if let Some(p) = find_bundled_node(data_dir) {
        return (p, false);
    }

    (bundled_path, false)
}

/// 实际用于运行 npm/子进程的 Node 安装根目录。
/// 若来自系统 PATH 则返回 None（不修改 PATH）
/// 若来自内置则返回其目录（前置到 PATH）
pub fn resolve_node_bin_dir_for_path(data_dir: &str) -> Option<PathBuf> {
    let (node_exe_path, is_system) = resolve_node(data_dir);
    if is_system {
        return None;
    }
    if !node_exe_path.is_file() {
        return None;
    }
    node_exe_path.parent().map(|p| p.to_path_buf())
}

/// 解析 Git 路径。
/// 优先级：1. 系统 PATH 中的 git → 2. 内置 git
/// 返回 (path, is_system) 其中 is_system=true 表示来自系统 PATH
pub fn resolve_git(data_dir: &str) -> (PathBuf, bool) {
    // 第一优先：系统 PATH 中的 git
    #[cfg(windows)]
    if hidden_cmd::cmd().arg("/C").arg("git").arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return (PathBuf::from("git"), true);
    }
    #[cfg(not(windows))]
    if Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return (PathBuf::from("git"), true);
    }

    // macOS Apple Silicon: 额外检查 Homebrew 安装路径（GUI 应用可能 PATH 不完整）
    #[cfg(target_os = "macos")]
    {
        let homebrew_git_paths = [
            "/opt/homebrew/bin/git",
            "/opt/homebrew/opt/git/bin/git",
            "/usr/local/bin/git", // Intel Mac 兜底
        ];
        for p in &homebrew_git_paths {
            let git_path = Path::new(p);
            if git_path.is_file() {
                #[cfg(windows)]
                if hidden_cmd::cmd().arg("/C").arg(p).arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::info!("找到 Homebrew Git: {}", p);
                    return (PathBuf::from(p), true);
                }
                #[cfg(not(windows))]
                if Command::new(p)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::info!("找到 Homebrew Git: {}", p);
                    return (PathBuf::from(p), true);
                }
            }
        }
    }

    // 第二优先：内置 git（data/env/git）
    let env_dir = env_root(data_dir);
    let bundled_path = git_exe(&env_dir);
    if bundled_path.exists() {
        return (bundled_path, false);
    }

    // 第三优先：bundled-env（exe 同级目录下的内置包）
    if let Some(p) = find_bundled_git(data_dir) {
        return (p, false);
    }

    (bundled_path, false)
}

/// 构建依赖安装子进程的 PATH 环境变量。
///
/// 规则：
/// - 若 Node 来自系统 PATH：不修改 PATH（保留用户系统环境）
/// - 若 Node 来自内置：将内置目录 prepend 到 PATH 前
/// - Windows 上同时追加内置 Git cmd 目录（部分依赖构建需调用 git）
pub fn build_deps_env_path(data_dir: &str) -> String {
    let system_path = std::env::var("PATH").unwrap_or_default();

    let Some(node_root) = resolve_node_bin_dir_for_path(data_dir) else {
        // 系统 PATH 的 node，但 macOS GUI 应用 PATH 可能不完整，需要确保常见路径存在
        #[cfg(target_os = "macos")]
        {
            let mut prepend = String::new();
            // 无论是否有 Homebrew，都要确保 /usr/local/bin 在 PATH 中（官网 pkg 安装的 node）
            if !system_path.contains("/usr/local/bin") {
                prepend = format!("{}:", "/usr/local/bin");
            }
            // Homebrew 路径也要添加
            if !system_path.contains("/opt/homebrew/bin") && Path::new("/opt/homebrew/bin").is_dir() {
                prepend.push_str("/opt/homebrew/bin:");
            }
            if prepend.is_empty() {
                return system_path;
            }
            return format!("{}{}", prepend, system_path);
        }
        return system_path;
    };

    let mut prepend = node_root.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        let env_dir = env_root(data_dir);
        let git_cmd_dir = env_dir.join("git").join("cmd");
        if git_cmd_dir.is_dir() {
            prepend = format!("{};{}", git_cmd_dir.to_string_lossy(), prepend);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS 上确保 /usr/local/bin 在 PATH 前（官网 pkg 安装）
        if !prepend.contains("/usr/local/bin") {
            prepend = format!("{}:{}", "/usr/local/bin", prepend);
        }
        // Homebrew 路径也要添加
        if !prepend.contains("/opt/homebrew/bin") && Path::new("/opt/homebrew/bin").is_dir() {
            prepend.push_str("/opt/homebrew/bin:");
        }
        return format!("{}:{}", prepend, system_path);
    }

    format!("{};{}", prepend, system_path)
}

/// 将 zip 内相对路径安全拼到 `dest` 下。
///
/// **Windows 关键**：`PathBuf::join("/node.exe")` 会把路径变成「带根组件」，整段前缀被替换，
/// 文件会落到当前盘符根目录（如 `D:\\node.exe`），而不是 `dest` 下。此处按 `/`、`\` 分段 `push`，
/// 且禁止 `..`、盘符与绝对路径片段。
pub fn join_under_dest(dest: &Path, zip_rel: &str) -> Result<PathBuf, String> {
    let normalized = zip_rel.replace('\\', "/");
    let trimmed = normalized
        .trim()
        .trim_start_matches(|c: char| c == '/' || c == '\\');
    if trimmed.is_empty() {
        return Ok(dest.to_path_buf());
    }
    let mut out = dest.to_path_buf();
    for part in trimmed.split('/').filter(|s| !s.is_empty()) {
        match part {
            "." => {}
            ".." => return Err("zip 路径包含非法的 ..".to_string()),
            p if p.contains(':') => return Err(format!("zip 路径非法（含盘符）: {}", zip_rel)),
            p => out.push(p),
        }
    }
    Ok(out)
}

/// 将 zip 条目路径规范化为正斜杠格式。
/// 统一去反斜杠、首尾空白、开头 `./`，保证后续 split('/') 正确工作。
fn normalize_zip_path(name: &str) -> String {
    name.replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .to_string()
}

/// 取规范路径的第一段（目录或文件名）。
fn first_segment(normalized: &str) -> &str {
    normalized.split('/').next().unwrap_or("")
}

/// 把 `src/` 下的所有文件和目录递归移动到 `dest/` 下，然后删除空 `src/`。
/// 用于解压后若顶层仍嵌套了一层版本子目录，将其"抬升"一级。
fn hoist_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let child = entry.path();
        let target = dest.join(entry.file_name());
        if child.is_dir() {
            hoist_recursive(&child, &target)?;
            std::fs::remove_dir(child)?;
        } else {
            std::fs::rename(&child, &target)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn dest_has_node_extract_layout(dest: &Path) -> bool {
    dest.join("node.exe").is_file()
}

#[cfg(not(windows))]
fn dest_has_node_extract_layout(dest: &Path) -> bool {
    dest.join("bin").join("node").is_file()
}

#[cfg(windows)]
fn dest_has_git_extract_layout(dest: &Path) -> bool {
    dest.join("cmd").join("git.exe").is_file()
}

#[cfg(not(windows))]
fn dest_has_git_extract_layout(dest: &Path) -> bool {
    dest.join("bin").join("git").is_file()
}

/// 验证 `dest`（即 `data/env/node` 或 `data/env/git`）下是否已有可执行文件；
/// 否则若仅有唯一子目录则反复抬升内容（支持双重嵌套，如 node/node-v22/...）。
fn flatten_dest_if_needed(dest: &Path) -> bool {
    const MAX_HOISTS: usize = 8;
    for attempt in 0..MAX_HOISTS {
        if dest_has_node_extract_layout(dest) || dest_has_git_extract_layout(dest) {
            return true;
        }

        let entries: Vec<_> = match std::fs::read_dir(dest) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => return false,
        };

        if entries.len() != 1 || !entries[0].path().is_dir() {
            return dest_has_node_extract_layout(dest) || dest_has_git_extract_layout(dest);
        }

        let child_dir = entries[0].path();
        let child_name = child_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");

        tracing::info!(
            "解压布局非扁平（第 {} 次抬升，子目录「{}」）→ {}",
            attempt + 1,
            child_name,
            dest.display()
        );

        match hoist_recursive(&child_dir, dest) {
            Ok(()) => {
                let _ = std::fs::remove_dir(&child_dir);
            }
            Err(e) => {
                tracing::warn!("子目录抬升失败: child={}, err={}", child_dir.display(), e);
                return dest_has_node_extract_layout(dest) || dest_has_git_extract_layout(dest);
            }
        }
    }

    dest_has_node_extract_layout(dest) || dest_has_git_extract_layout(dest)
}

/// 解压 zip 文件到目标目录（自动创建父目录，支持嵌套顶层文件夹）。
/// 解压后自动检测并修正「版本子目录嵌套」问题（Node / Git 常见）。
pub async fn unzip(zip_path: &Path, dest_dir: &Path) -> Result<(), String> {
    use std::fs::File;

    let file = File::open(zip_path).map_err(|e| format!("打开 zip 失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析 zip 失败: {}", e))?;

    // 收集所有规范化后的条目路径（非目录）
    // zip 库会解码路径（UTF-8），直接用 file.name() + normalize 即可覆盖所有格式
    let all_names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            let raw = entry.name().to_string();
            let norm = normalize_zip_path(&raw);
            if norm.ends_with('/') || norm.is_empty() {
                None
            } else {
                Some(norm)
            }
        })
        .collect();

    if all_names.is_empty() {
        return Err("zip 文件内没有可解压的文件条目".to_string());
    }

    // 判断是否为单一顶层目录结构（统一规范化后分析）
    let first_seg = first_segment(&all_names[0]).to_string();
    let has_single_root =
        !first_seg.is_empty() && all_names.iter().all(|n| first_segment(n) == first_seg);

    // 单一顶层目录名
    let strip_prefix = if has_single_root {
        Some(first_seg)
    } else {
        None
    };

    // zip 内相对路径 → 目标路径。必须用 join_under_dest，禁止 PathBuf::join 整段含前导 / 的字符串。
    fn zip_entry_dest(
        dest_dir: &Path,
        entry_name: &str,
        root_folder: Option<&str>,
    ) -> Result<PathBuf, String> {
        let name = normalize_zip_path(entry_name);
        match root_folder {
            Some(root) if !root.is_empty() => {
                let root_slash = format!("{}/", root);
                let dir_only = name.trim_end_matches('/');
                if dir_only == root {
                    return Ok(dest_dir.to_path_buf());
                }
                if let Some(rest) = name.strip_prefix(&root_slash) {
                    join_under_dest(dest_dir, rest)
                } else {
                    join_under_dest(dest_dir, &name)
                }
            }
            _ => join_under_dest(dest_dir, &name),
        }
    }

    std::fs::create_dir_all(dest_dir).map_err(|e| format!("创建解压目录失败: {}", e))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 条目 {} 失败: {}", i, e))?;

        let out_path = zip_entry_dest(dest_dir, entry.name(), strip_prefix.as_deref())?;

        // 防止 zip slip
        if out_path.as_path() != dest_dir {
            match out_path.strip_prefix(dest_dir) {
                Ok(rest) => {
                    if rest.components().any(|c| {
                        matches!(
                            c,
                            std::path::Component::ParentDir | std::path::Component::RootDir
                        )
                    }) {
                        return Err(format!("非法 zip 路径（路径穿越）: {}", entry.name()));
                    }
                }
                Err(_) => {
                    return Err(format!(
                        "解压路径不在目标目录内: {} → {}",
                        entry.name(),
                        out_path.display()
                    ));
                }
            }
        }

        if entry.name().ends_with('/') || entry.name().ends_with('\\') {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("创建目录 {} 失败: {}", out_path.display(), e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建目录 {} 失败: {}", parent.display(), e))?;
            }
            let mut out_file = std::fs::File::create(&out_path)
                .map_err(|e| format!("创建文件 {} 失败: {}", out_path.display(), e))?;
            std::io::copy(&mut entry, &mut out_file)
                .map_err(|e| format!("写入文件 {} 失败: {}", out_path.display(), e))?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode)).ok();
            }
        }
    }

    // 解压后兜底：如果没有预期的 exe，进行子目录抬升
    if !flatten_dest_if_needed(dest_dir) {
        tracing::warn!(
            "解压完成但预期可执行文件仍未找到: dest={}",
            dest_dir.display()
        );
    }

    info!("解压完成: {} → {}", zip_path.display(), dest_dir.display());
    Ok(())
}

/// 解压 .tar.gz 文件到目标目录（自动创建父目录，支持嵌套顶层文件夹）。
/// 解压后自动检测并修正「版本子目录嵌套」问题（Node / Git 常见）。
pub async fn tar_gz_extract(tar_path: &Path, dest_dir: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::fs::File;
    

    let file = File::open(tar_path).map_err(|e| format!("打开 tar.gz 失败: {}", e))?;
    let dec = GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    // 先解压到临时目录，再检测是否需要抬升顶层目录
    let temp_dir = tar_path
        .parent()
        .map(|p| p.join(format!(".tarball-extract-{}", std::process::id())))
        .unwrap_or_else(|| std::env::temp_dir().join(format!(".tarball-extract-{}", std::process::id())));

    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("创建临时解压目录失败: {}", e))?;

    archive
        .unpack(&temp_dir)
        .map_err(|e| format!("解压 tar.gz 失败: {}", e))?;

    // 检测顶层单一目录（node-v22.14.0-darwin-arm64/ 等）
    let entries = std::fs::read_dir(&temp_dir)
        .map_err(|e| format!("读取临时目录失败: {}", e))?
        .filter_map(|e| e.ok())
        .collect::<Vec<_>>();

    if entries.len() == 1 {
        if let Some(first) = entries.first() {
            if first.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let top_dir = first.path();
                // 将顶层目录的内容移动到目标目录
                std::fs::create_dir_all(dest_dir)
                    .map_err(|e| format!("创建目标目录失败: {}", e))?;
                move_dir_contents(&top_dir, dest_dir)?;
                std::fs::remove_dir_all(&temp_dir).ok();
                info!(
                    "解压完成（已抬升顶层目录）: {} → {}",
                    tar_path.display(),
                    dest_dir.display()
                );
                return Ok(());
            }
        }
    }

    // 无顶层目录或多个条目，直接移动到目标目录
    if temp_dir.exists() {
        move_dir_contents(&temp_dir, dest_dir)?;
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    info!("解压完成: {} → {}", tar_path.display(), dest_dir.display());
    Ok(())
}

/// 移动 src 目录内容到 dest 目录（递归）
fn move_dir_contents(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("创建目标目录失败: {}", e))?;
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let src_path = entry.path();
            let dest_path = dest.join(entry.file_name());
            if src_path.is_dir() {
                std::fs::create_dir_all(&dest_path)
                    .map_err(|e| format!("创建子目录失败: {}", e))?;
                move_dir_contents(&src_path, &dest_path)?;
                std::fs::remove_dir_all(&src_path).ok();
            } else {
                std::fs::rename(&src_path, &dest_path)
                    .map_err(|e| format!("移动文件失败: {}", e))?;
            }
        }
    }
    Ok(())
}

/// 在指定目录中运行 git clone（使用绝对路径的 git）
pub async fn git_clone_with_exe(
    git_path: &Path,
    url: &str,
    dest: &Path,
    branch: Option<&str>,
    stage: &str,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    
    
    
    
    

    let dest_str = dest.to_string_lossy().to_string();

    // Windows: 使用 hidden_cmd 隐藏窗口
    #[cfg(windows)]
    return {
        // 构建完整的 git 命令（用于后续环境变量设置）
        let git_clone_cmd = format!(
            "git clone --progress --depth 1{} \"{}\" \"{}\"",
            branch.map(|b| format!(" -b {}", b)).unwrap_or_default(),
            url,
            dest_str
        );

        let mut cmd = hidden_cmd::cmd();
        cmd.arg("/C")
            .arg(&git_clone_cmd)
            .current_dir(&dest_str[..dest_str.rfind('\\').unwrap_or(0)])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "ssh -o StrictHostKeyChecking=no");

        let output = cmd
            .output()
            .map_err(|e| format!("启动 git clone 失败: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "git clone 失败，请检查网络: {}",
                stderr.trim()
            ))
        }
    };

    // 非 Windows: 使用 tokio::process::Command
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new(git_path);
        cmd.arg("clone").arg("--progress").arg("--depth").arg("1");
        if let Some(b) = branch {
            cmd.args(["-b", b]);
        }
        cmd.arg(url).arg(&dest_str);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.env("GIT_TERMINAL_PROMPT", "0");

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 git clone 失败: {}", e))?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "无法读取 git stderr".to_string())?;

        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let app_hb = app.clone();
        let stage_hb = stage.to_string();
        let hb_task = tokio::spawn(async move {
            let mut secs = 0u32;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                if !r.load(Ordering::Relaxed) {
                    break;
                }
                secs += 20;
                let _ = app_hb.emit(
                    "install-progress",
                    crate::mirror::InstallProgressEvent::detail(
                        &stage_hb,
                        &format!("克隆仍在进行（已约 {} 秒）…", secs),
                    ),
                );
            }
        });

        let app_clone = app.clone();
        let stage_owned = stage.to_string();
        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let t = line.trim();
                        if !t.is_empty() {
                            let _ = app_clone.emit(
                                "install-progress",
                                crate::mirror::InstallProgressEvent::detail(&stage_owned, t),
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let status = child
            .wait()
            .await
            .map_err(|e| format!("git clone 等待失败: {}", e))?;

        let _ = read_task.await;
        running.store(false, Ordering::Relaxed);
        hb_task.abort();
        let _ = hb_task.await;

        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "git clone 失败（退出码 {:?}），请检查网络",
                status.code()
            ))
        }
    }
}
