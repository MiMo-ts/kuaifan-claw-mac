// GitHub 镜像与下载工具模块
// 提供多镜像自动切换、HTTP 流式下载进度、git clone 镜像重试

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{info, warn};

/// GitHub/git 外部资源拉取已全局禁止，仅使用本地内置包
const GITHUB_BLOCKED: bool = true;

/// GitHub 源码包加速前缀（国内优先；OPENCLAW_GITHUB_MIRROR_PREFIXES 可追加）。
/// ghproxy / ghproxy.net 排在最前，优先用直链代理加速；官方直连放最后兜底。
pub const GITHUB_MIRRORS: &[&str] = &[
    "https://ghproxy.net/",
    "https://mirror.ghproxy.com/",
    "https://ghproxy.com/",
    "https://ghps.cc/",
    "https://github.moeyy.xyz/",
    "https://ghfast.top/",
    // 官方直连放最后
    "",
];

/// Node.js MSI 国内镜像
const NODEJS_MIRRORS: &[&str] = &[
    "https://nodejs.org/dist/",
    "https://npmmirror.com/mirrors/node/",
];

/// Git for Windows 国内镜像（用于 installer exe）
const GIT_WINDOWS_MIRRORS: &[&str] = &[
    // npmmirror（npm 中国镜像站）为目前最稳定的 Git for Windows 镜像，维护良好
    "https://npmmirror.com/mirrors/git-for-windows/",
    // ghproxy 直链 GitHub releases（大文件友好）
    "https://ghproxy.com/https://github.com/git-for-windows/git/releases/download/",
    // 官方直链（放最后）
    "https://github.com/git-for-windows/git/releases/download/",
];

/// GitHub MinGit 便携版国内镜像（用于 portable zip）
#[allow(dead_code)]
const MINGIT_MIRRORS: &[&str] = &[
    "https://github.com/git-for-windows/git/releases/download/",
    "https://ghproxy.com/https://github.com/git-for-windows/git/releases/download/",
];

/// 自包含 Git 便携版版本号（MinGit 精简版，无 GUI / TTY 交互，体积小）
pub const MINGIT_VERSION: &str = "v2.53.0.windows.1";
/// MinGit 便携版 zip 文件名（win-x64，与 GitHub release 资源名一致）
pub const MINGIT_ZIP: &str = "MinGit-2.53.0-64-bit.zip";
/// MinGit 官方 releases 目录路径（URL path segment）
pub const MINGIT_FILENAME: &str = "v2.53.0.windows.1/MinGit-2.53.0-64-bit.zip";

/// 进度事件 Payload
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallProgressEvent {
    pub stage: String,
    pub status: String, // "started" | "progress" | "finished" | "failed" | "mirror-fallback" | "detail"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    pub message: String,
}

impl InstallProgressEvent {
    pub fn started(stage: &str, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "started".to_string(),
            percent: None,
            message: message.to_string(),
        }
    }

    pub fn progress(stage: &str, percent: f64, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "progress".to_string(),
            percent: Some(percent),
            message: message.to_string(),
        }
    }

    pub fn mirror_fallback(stage: &str, mirror: &str, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "mirror-fallback".to_string(),
            percent: None,
            message: format!("切换到镜像 {}：{}", mirror, message),
        }
    }

    pub fn finished(stage: &str, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "finished".to_string(),
            percent: None,
            message: message.to_string(),
        }
    }

    pub fn failed(stage: &str, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "failed".to_string(),
            percent: None,
            message: message.to_string(),
        }
    }

    /// 流式细节（如 git clone --progress 的 stderr 行），供前端日志展示
    pub fn detail(stage: &str, message: &str) -> Self {
        Self {
            stage: stage.to_string(),
            status: "detail".to_string(),
            percent: None,
            message: message.to_string(),
        }
    }
}

/// 发送进度事件（内部用）
fn emit_progress(app: &AppHandle, event: InstallProgressEvent) {
    let _ = app.emit("install-progress", event);
}

// ─── GitHub URL 镜像转换 ──────────────────────────────────────────────────

/// 判断 URL 是否为 GitHub 相关（github.com / raw.githubusercontent.com）
fn is_github_url(url: &str) -> bool {
    url.contains("github.com") || url.contains("raw.githubusercontent.com")
}

/// GitHub 仓库源码包（tar.gz）直连 URL，供 `npm install <url>` 使用（走 HTTPS，无需 git）
/// `git_ref`：分支名（如 main）或标签名（如 v1.0.0，需以 v 开头且后跟数字以走 tags 路径）
pub fn github_repo_archive_tar_urls(owner: &str, repo: &str, git_ref: &str) -> Vec<String> {
    let base = if git_ref.starts_with('v')
        && git_ref
            .as_bytes()
            .get(1)
            .map_or(false, |b| b.is_ascii_digit())
    {
        format!(
            "https://github.com/{}/{}/archive/refs/tags/{}.tar.gz",
            owner, repo, git_ref
        )
    } else {
        format!(
            "https://github.com/{}/{}/archive/refs/heads/{}.tar.gz",
            owner, repo, git_ref
        )
    };
    github_mirror_urls(&base)
}

/// 将 GitHub 原始 URL 转为带镜像前缀的 URL 列表（主源 + 镜像）
pub fn github_mirror_urls(original: &str) -> Vec<String> {
    if !is_github_url(original) {
        return vec![original.to_string()];
    }

    let mut urls = Vec::with_capacity(GITHUB_MIRRORS.len());
    for mirror in GITHUB_MIRRORS {
        if mirror.is_empty() {
            urls.push(original.to_string());
        } else {
            urls.push(format!("{}{}", mirror, original));
        }
    }
    urls
}

/// 将 Node.js MSI URL 转为镜像列表
pub fn nodejs_mirror_urls(_original: &str, filename: &str) -> Vec<String> {
    let mut urls = Vec::with_capacity(NODEJS_MIRRORS.len());
    for mirror in NODEJS_MIRRORS {
        urls.push(format!("{}{}", mirror, filename));
    }
    urls
}

/// 将 Git for Windows URL 转为镜像列表
/// `full_url` 应为完整的官方 GitHub releases URL（如 `https://github.com/git-for-windows/git/releases/download/v2.43.0.windows.1/Git-2.43.0-64-bit.exe`）。
/// 镜像规则：
/// - npmmirror、npmmirror（保持结构）：直接拼接 `mirror_base + filename`
/// - ghproxy 直连代理：拼接 `mirror_base + full_url`（即把完整 URL 当作被代理资源）
/// - 官方直连（空字符串）：直接返回 full_url
pub fn git_windows_mirror_urls(full_url: &str, filename: &str) -> Vec<String> {
    let mut urls = Vec::with_capacity(GIT_WINDOWS_MIRRORS.len());
    for mirror in GIT_WINDOWS_MIRRORS {
        if mirror.is_empty() {
            urls.push(full_url.to_string());
        } else if mirror.ends_with("github.com/") || mirror.ends_with("github.com") {
            urls.push(format!("{}{}", mirror, filename));
        } else {
            urls.push(format!("{}{}", mirror, full_url));
        }
    }
    urls
}

/// 将 MinGit 便携版 URL 转为镜像列表（ghproxy 类前缀拼接，官方直连兜底）
pub fn mingit_mirror_urls(filename: &str) -> Vec<String> {
    let base = format!(
        "https://github.com/git-for-windows/git/releases/download/{}/{}",
        MINGIT_VERSION, filename
    );
    let mut urls = Vec::with_capacity(GITHUB_MIRRORS.len());
    for mirror in GITHUB_MIRRORS {
        if mirror.is_empty() {
            urls.push(base.clone());
        } else {
            urls.push(format!("{}{}", mirror, base));
        }
    }
    urls
}

// ─── HTTP 流式下载（支持镜像重试 + 进度回调）─────────────────────────────────

type ProgressFn = Box<dyn Fn(f64, u64, u64) + Send + Sync>;

/// 从 URL 列表依次尝试下载，返回成功时的内容字节；下载过程中通过 cb 回调进度
pub async fn download_with_mirrors<'a>(
    client: &reqwest::Client,
    urls: impl IntoIterator<Item = &'a str>,
    dest: &Path,
    stage: &str,
    app: &AppHandle,
    _cb: Option<ProgressFn>,
) -> Result<(), String> {
    let dest_str = dest.to_string_lossy().to_string();

    for (i, url) in urls.into_iter().enumerate() {
        if i > 0 {
            let mirror_name = url.split('/').nth(2).unwrap_or(url);
            emit_progress(
                app,
                InstallProgressEvent::mirror_fallback(
                    stage,
                    mirror_name,
                    "原链接不可用，尝试镜像源...",
                ),
            );
        } else {
            emit_progress(
                app,
                InstallProgressEvent::started(stage, &format!("开始下载: {}", url)),
            );
        }

        match download_single(client, url, &dest_str, stage, app).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!("下载失败 [{}]: {}", url, e);
                if i == 0 {
                    emit_progress(
                        app,
                        InstallProgressEvent::mirror_fallback(stage, "备用镜像", &e),
                    );
                }
            }
        }
    }

    Err("所有下载源均不可用，请检查网络或手动下载".to_string())
}

async fn download_single(
    client: &reqwest::Client,
    url: &str,
    dest: &str,
    stage: &str,
    app: &AppHandle,
) -> Result<(), String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}: {}", response.status(), url));
    }

    let total_size = response.content_length();
    let dest_path = Path::new(dest);

    let mut file = tokio::fs::File::create(dest_path)
        .await
        .map_err(|e| format!("创建文件失败: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let mut last_report = std::time::Instant::now();

    use tokio::io::AsyncWriteExt;
    use tokio_stream::StreamExt;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("读取流失败: {}", e))?;
        let len = chunk.len() as u64;

        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("写入文件失败: {}", e))?;

        downloaded += len;

        // 每 0.5 秒向 UI 报告进度（有 Content-Length 时）
        if let Some(total) = total_size {
            if last_report.elapsed() > Duration::from_millis(500) || downloaded == total {
                let percent = (downloaded as f64 / total as f64) * 100.0;
                let downloaded_mb = downloaded as f64 / 1024.0 / 1024.0;
                let total_mb = total as f64 / 1024.0 / 1024.0;
                emit_progress(
                    app,
                    InstallProgressEvent::progress(
                        stage,
                        percent,
                        &format!("已下载 {:.1}/{:.1} MB", downloaded_mb, total_mb),
                    ),
                );
                last_report = std::time::Instant::now();
            }
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("刷新文件失败: {}", e))?;
    info!("下载完成: {} -> {}", url, dest);
    emit_progress(app, InstallProgressEvent::finished(stage, "下载完成"));

    Ok(())
}

// ─── GitHub 归档 .tar.gz（纯 HTTPS，不调用 git，避免弹出 Git Credential Manager）────────

/// 生成 `owner/repo` + 分支 的源码包 URL 列表（含 GITHUB_MIRRORS 前缀）
pub fn github_archive_tarball_urls(repo_path: &str, branch: &str) -> Vec<String> {
    let base = format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        repo_path, branch
    );
    let mut urls = Vec::new();
    for prefix in GITHUB_MIRRORS {
        let u = if prefix.is_empty() {
            base.clone()
        } else {
            format!("{}{}", prefix, base)
        };
        urls.push(u);
    }
    urls
}

/// 生成 `owner/repo` + 分支 的源码包 URL 列表，**官方直连优先，镜像兜底**（历史行为，少用）。
pub fn github_archive_tarball_urls_official_first(repo_path: &str, branch: &str) -> Vec<String> {
    let base = format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        repo_path, branch
    );
    let mut urls = Vec::with_capacity(GITHUB_MIRRORS.len() + 1);
    urls.push(base.clone());
    for prefix in GITHUB_MIRRORS {
        if !prefix.is_empty() {
            urls.push(format!("{}{}", prefix, base));
        }
    }
    urls
}

/// **国内网络优先**：自定义镜像前缀 → 内置镜像前缀 → 官方 GitHub（去重保序）
pub fn github_archive_tarball_urls_mirror_first(repo_path: &str, branch: &str) -> Vec<String> {
    let base = format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        repo_path, branch
    );
    let mut urls = Vec::new();
    let mut seen = HashSet::<String>::new();
    let mut push = |u: String| {
        if seen.insert(u.clone()) {
            urls.push(u);
        }
    };
    if let Ok(extra) = std::env::var("OPENCLAW_GITHUB_MIRROR_PREFIXES") {
        for prefix in extra.split(',') {
            let p = prefix.trim();
            if !p.is_empty() {
                let p = if p.ends_with('/') {
                    p.to_string()
                } else {
                    format!("{}/", p)
                };
                push(format!("{}{}", p, base));
            }
        }
    }
    for prefix in GITHUB_MIRRORS {
        if !prefix.is_empty() {
            push(format!("{}{}", prefix, base));
        }
    }
    push(base);
    urls
}

/// 技能/插件归档下载使用的 URL 顺序：**默认官方 github.com 优先**（与 Cursor、浏览器直连一致，且避免 ghproxy 返回 HTML 占位页）；
/// 若需国内镜像先试，设置环境变量 `OPENCLAW_GITHUB_ARCHIVE_MIRROR_FIRST=1`。
pub fn github_archive_tarball_urls_for_skills_download(
    repo_path: &str,
    branch: &str,
) -> Vec<String> {
    let mirror_first = matches!(
        std::env::var("OPENCLAW_GITHUB_ARCHIVE_MIRROR_FIRST")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("TRUE")
    );
    if mirror_first {
        github_archive_tarball_urls_mirror_first(repo_path, branch)
    } else {
        github_archive_tarball_urls_official_then_mirrors(repo_path, branch)
    }
}

/// 官方 URL → `OPENCLAW_GITHUB_MIRROR_PREFIXES` → 内置镜像前缀（去重保序）
fn github_archive_tarball_urls_official_then_mirrors(repo_path: &str, branch: &str) -> Vec<String> {
    let base = format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        repo_path, branch
    );
    let mut urls = Vec::new();
    let mut seen = HashSet::<String>::new();
    let mut push = |u: String| {
        if seen.insert(u.clone()) {
            urls.push(u);
        }
    };
    push(base.clone());
    if let Ok(extra) = std::env::var("OPENCLAW_GITHUB_MIRROR_PREFIXES") {
        for prefix in extra.split(',') {
            let p = prefix.trim();
            if !p.is_empty() {
                let p = if p.ends_with('/') {
                    p.to_string()
                } else {
                    format!("{}/", p)
                };
                push(format!("{}{}", p, base));
            }
        }
    }
    for prefix in GITHUB_MIRRORS {
        if !prefix.is_empty() {
            push(format!("{}{}", prefix, base));
        }
    }
    urls
}

/// 检查字节流是否具有有效的 gzip 魔数（0x1f 0x8b）
fn is_gzip_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn github_download_http_client(user_agent: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("HTTP 客户端: {}", e))
}

/// 下载单个 URL，校验为合法 gzip tarball 字节流
async fn fetch_gzip_tarball_bytes_one(
    client: &reqwest::Client,
    url: &str,
    i: usize,
    stage: &str,
    app: &AppHandle,
    last_errs: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{}: 网络请求失败", e);
            warn!("{}", msg);
            last_errs.push(msg.clone());
            if i == 0 {
                emit_progress(
                    app,
                    InstallProgressEvent::mirror_fallback(stage, "备用源", &msg),
                );
            }
            return None;
        }
    };

    if !resp.status().is_success() {
        let msg = format!("HTTP {}", resp.status());
        last_errs.push(format!("{} -> {}", url, msg));
        if i == 0 {
            emit_progress(
                app,
                InstallProgressEvent::mirror_fallback(stage, "备用源", &msg),
            );
        }
        return None;
    }

    let total_size = resp.content_length();
    let bytes = match resp.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            let msg = format!("读取响应体失败: {}", e);
            last_errs.push(msg.clone());
            return None;
        }
    };

    if !is_gzip_magic(&bytes) {
        let preview: String = bytes
            .iter()
            .take(80)
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '?'
                }
            })
            .collect();
        let msg = format!(
            "返回内容不是 gzip 格式（可能为 HTML 或重定向页），前 80 字符: {}",
            preview.trim()
        );
        last_errs.push(msg.clone());
        if i == 0 {
            emit_progress(
                app,
                InstallProgressEvent::mirror_fallback(stage, "备用源", "内容校验失败（非 gzip）"),
            );
        }
        return None;
    }

    const MIN_TARBALL_SIZE: usize = 512;
    if bytes.len() < MIN_TARBALL_SIZE {
        let msg = format!("内容过小（{} 字节），可能不是完整 tarball", bytes.len());
        last_errs.push(msg.clone());
        if i == 0 {
            emit_progress(
                app,
                InstallProgressEvent::mirror_fallback(stage, "备用源", "内容过小"),
            );
        }
        return None;
    }

    if let Some(total) = total_size {
        if bytes.len() < (total as usize) / 2 {
            let msg = format!(
                "Content-Length 声明 {} 字节，实际仅收到 {} 字节（可能被截断）",
                total,
                bytes.len()
            );
            last_errs.push(msg.clone());
            if i == 0 {
                emit_progress(
                    app,
                    InstallProgressEvent::mirror_fallback(stage, "备用源", "响应被截断"),
                );
            }
            return None;
        }
    }

    Some(bytes)
}

fn summarize_tarball_errors(urls_len: usize, last_errs: &[String]) -> String {
    if last_errs.len() <= 3 {
        last_errs.join("；")
    } else {
        format!(
            "尝试了 {} 个地址均失败。典型错误：{}",
            urls_len,
            last_errs[..3.min(last_errs.len())].join("；")
        )
    }
}

/// 依次尝试 URL，下载通过 gzip 校验的 tarball 字节（不解压）
pub async fn download_gzip_tarball_bytes_try_urls(
    urls: &[String],
    stage: &str,
    user_agent: &str,
    app: &AppHandle,
) -> Result<Vec<u8>, String> {
    let client = github_download_http_client(user_agent)?;
    let mut last_errs: Vec<String> = Vec::new();

    for (i, url) in urls.iter().enumerate() {
        if let Some(bytes) =
            fetch_gzip_tarball_bytes_one(&client, url, i, stage, app, &mut last_errs).await
        {
            info!("gzip tarball 下载成功: {}", url);
            return Ok(bytes);
        }
    }

    Err(summarize_tarball_errors(urls.len(), &last_errs))
}

/// 下载插件 tarball：逐个尝试 URL，成功条件为 HTTP 200 + gzip 魔数 + 最小体积 + 解压成功。
/// 任意中间步骤失败则自动换下一 URL，全部失败则返回汇总错误。
pub async fn download_plugin_tarball_try_urls<'a>(
    urls: &'a [String],
    dest_dir: PathBuf,
    stage: &str,
    user_agent: &str,
    app: &AppHandle,
) -> Result<(), String> {
    let client = github_download_http_client(user_agent)?;
    let dest_str = dest_dir.to_string_lossy().to_string();
    let mut last_errs: Vec<String> = Vec::new();

    for (i, url) in urls.iter().enumerate() {
        let Some(bytes) =
            fetch_gzip_tarball_bytes_one(&client, url, i, stage, app, &mut last_errs).await
        else {
            continue;
        };

        let dest_for_blocking = dest_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            unpack_tar_gz_github_archive(&bytes, &dest_for_blocking)
        })
        .await
        .map_err(|e| format!("解压任务崩溃: {}", e))?;

        match result {
            Ok(()) => {
                emit_progress(app, InstallProgressEvent::finished(stage, "下载并解压成功"));
                info!("插件 tarball 下载解压成功: {}", url);
                return Ok(());
            }
            Err(e) => {
                let msg = format!("解压失败: {}", e);
                warn!("{} [{}]", msg, url);
                last_errs.push(msg);
                if i == 0 {
                    emit_progress(
                        app,
                        InstallProgressEvent::mirror_fallback(stage, "备用源", "解压失败"),
                    );
                }
                let _ = std::fs::remove_dir_all(&dest_str);
            }
        }
    }

    Err(summarize_tarball_errors(urls.len(), &last_errs))
}

/// 按顺序尝试 URL，返回首个成功响应的字节
pub async fn download_bytes_first_ok(urls: &[String], user_agent: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .user_agent(user_agent)
        .build()
        .map_err(|e| format!("HTTP 客户端: {}", e))?;
    let mut last_err = "无可用地址".to_string();
    for url in urls {
        match client.get(url).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    last_err = format!("{} -> HTTP {}", url, resp.status());
                    continue;
                }
                match resp.bytes().await {
                    Ok(b) => return Ok(b.to_vec()),
                    Err(e) => last_err = format!("{}: {}", url, e),
                }
            }
            Err(e) => last_err = format!("{}: {}", url, e),
        }
    }
    Err(format!("下载失败: {}", last_err))
}

/// 解压 GitHub `archive`  tarball：顶层为单一目录 `repo-branch/`，移动到 `dest_dir`
pub fn unpack_tar_gz_github_archive(bytes: &[u8], dest_dir: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::fs;
    use std::io::Cursor;
    use tar::Archive;

    let tmp = std::env::temp_dir().join(format!(
        "oc-plugin-extract-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("临时目录: {}", e))?;

    let cursor = Cursor::new(bytes);
    let dec = GzDecoder::new(cursor);
    let mut archive = Archive::new(dec);
    archive
        .unpack(&tmp)
        .map_err(|e| format!("解压 tarball 失败: {}", e))?;

    let entries: Vec<_> = fs::read_dir(&tmp)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if entries.len() != 1 {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!(
            "归档应仅含一个根目录，实际 {} 项（可能不是 GitHub archive 格式）",
            entries.len()
        ));
    }
    let inner = entries[0].path();
    if !inner.is_dir() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("归档根项不是目录".to_string());
    }
    if let Some(parent) = dest_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dest_dir.exists() {
        fs::remove_dir_all(dest_dir).map_err(|e| e.to_string())?;
    }
    fs::rename(&inner, dest_dir).map_err(|e| format!("移动到目标目录失败: {}", e))?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

/// 解压 npm tarball（.tgz）：顶层为 `package/` 子目录，直接将其内容展开到 `dest_dir`。
/// npm pack 产生的 tarball 结构为 `package/<files>`，需要提取 `package/` 前缀。
pub fn unpack_npm_tarball(bytes: &[u8], dest_dir: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::fs;
    use std::io::Cursor;
    use tar::Archive;

    let tmp = std::env::temp_dir().join(format!(
        "oc-npm-extract-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录: {}", e))?;

    let cursor = Cursor::new(bytes);
    let dec = GzDecoder::new(cursor);
    let mut archive = Archive::new(dec);

    // npm tarball 结构：`package/<files>`，去除 `package/` 前缀后直接解压到目标目录
    archive
        .entries()
        .map_err(|e| format!("读取 tarball 条目失败: {}", e))?
        .filter_map(|entry| entry.ok())
        .filter_map(|mut entry| {
            let path = entry.path().ok()?.into_owned();
            // npm tarball 顶层为 `package/`，跳过它
            let stripped: std::path::PathBuf =
                if let Some(stripped) = path.strip_prefix("package/").ok() {
                    stripped.to_path_buf()
                } else {
                    // 跳过顶层条目本身（如 `package` 目录条目）
                    return None;
                };
            if stripped.as_os_str().is_empty() {
                return None;
            }
            let dst_path = dest_dir.join(&stripped);
            if let Some(parent) = dst_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            entry.unpack(&dst_path).ok()?;
            None::<()>
        })
        .count();

    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

/// 管理器拉取 GitHub 归档时的 User-Agent（避免部分镜像返回 403）
pub const OPENCLAW_MANAGER_UA: &str = "openclaw-cn-manager/1.0";

/// 并发探测多个镜像的响应速度，返回最快可达的 URL（5 秒超时）
pub async fn probe_best_mirror(urls: &[String], stage: &str, app: &AppHandle) -> Option<String> {
    if urls.is_empty() {
        return None;
    }
    let client = match reqwest::Client::builder()
        .user_agent(OPENCLAW_MANAGER_UA)
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return urls.first().cloned(),
    };

    let futures: Vec<_> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let c = client.clone();
            let url_c = url.clone();
            async move {
                let start = std::time::Instant::now();
                let ok = c
                    .head(&url_c)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                (i, start.elapsed().as_millis() as u64, ok, url_c)
            }
        })
        .collect();

    let results: Vec<(usize, u64, bool, String)> = futures_util::future::join_all(futures).await;
    let best = results
        .into_iter()
        .filter(|(_, _, ok, _)| *ok)
        .min_by_key(|(_, ms, _, _)| *ms);

    if let Some((_, ms, _, ref url)) = best {
        info!("镜像测速完成，最快: {} ({}ms)", url, ms);
        emit_progress(
            app,
            InstallProgressEvent::detail(stage, &format!("测速完成，使用 {}ms 的源", ms)),
        );
    }
    best.map(|(_, _, _, url)| url)
}

fn copy_dir_all_mirror(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::fs;
    if !src.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "源不是目录",
        ));
    }
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }

    fn walk(src: &Path, dst: &Path) -> std::io::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if ty.is_dir() {
                walk(&from, &to)?;
            } else {
                fs::copy(&from, &to)?;
            }
        }
        Ok(())
    }

    walk(src, dst)
}

/// 从 GitHub archive 字节流中仅解压 `skills/<skill_id>/` 到 `dest_dir`（与总仓库目录布局一致）
pub fn unpack_tar_gz_github_archive_skills_child(
    bytes: &[u8],
    dest_dir: &Path,
    skill_id: &str,
) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::fs;
    use std::io::Cursor;
    use tar::Archive;

    let tmp = std::env::temp_dir().join(format!(
        "oc-skill-extract-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("临时目录: {}", e))?;

    let cursor = Cursor::new(bytes);
    let dec = GzDecoder::new(cursor);
    let mut archive = Archive::new(dec);
    archive
        .unpack(&tmp)
        .map_err(|e| format!("解压 tarball 失败: {}", e))?;

    let entries: Vec<_> = fs::read_dir(&tmp)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if entries.len() != 1 {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!(
            "归档应仅含一个根目录，实际 {} 项（可能不是 GitHub archive 格式）",
            entries.len()
        ));
    }
    let root = entries[0].path();
    let src = root.join("skills").join(skill_id);
    if !src.is_dir() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!(
            "技能总仓库中不存在目录 skills/{}（请确认 monorepo 已包含该技能）",
            skill_id
        ));
    }
    if let Some(parent) = dest_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dest_dir.exists() {
        fs::remove_dir_all(dest_dir).map_err(|e| e.to_string())?;
    }
    copy_dir_all_mirror(&src, dest_dir).map_err(|e| format!("复制技能目录失败: {}", e))?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

/// 从技能总仓库归档中仅拉取 `skills/<skill_id>/`（HTTPS + 国内镜像优先，不调用 git）
pub async fn fetch_github_monorepo_skill_folder(
    repo_path: &str,
    branch: &str,
    skill_id: &str,
    dest_dir: &Path,
    stage: &str,
    app: &AppHandle,
) -> Result<(), String> {
    if GITHUB_BLOCKED {
        return Err("GitHub 外部资源已被禁止，请使用本地内置包".into());
    }
    let urls = github_archive_tarball_urls_for_skills_download(repo_path, branch);
    let bytes =
        download_gzip_tarball_bytes_try_urls(&urls, stage, OPENCLAW_MANAGER_UA, app).await?;
    let skill_id = skill_id.to_string();
    let dest_path = dest_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        unpack_tar_gz_github_archive_skills_child(&bytes, &dest_path, &skill_id)
    })
    .await
    .map_err(|e| format!("解压任务崩溃: {}", e))??;
    emit_progress(app, InstallProgressEvent::finished(stage, "下载并解压成功"));
    Ok(())
}

/// 从 `https://github.com/owner/repo(.git)`、`owner/repo` 或单独 `owner` 解析出路径段（用于 archive API）
pub fn github_owner_repo_from_url_or_path(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches('/').trim_start();
    if t.is_empty() {
        return None;
    }
    if !t.contains("://") {
        let t = t.trim_start_matches('/');
        if t.contains(' ') {
            return None;
        }
        return Some(t.to_string());
    }
    let needle = "github.com/";
    let i = t.find(needle)?;
    let mut rest = t[i + needle.len()..].to_string();
    if let Some(pos) = rest.find(['?', '#']) {
        rest.truncate(pos);
    }
    let rest = rest
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// 从 `OPENCLAW_SKILLS_STANDALONE_BASE`（如 `https://github.com/openclaw-cn`）解析 GitHub 用户名/组织名
pub fn github_org_from_skills_standalone_base(base: &str) -> String {
    let t = base.trim();
    if t.is_empty() {
        return "openclaw-cn".to_string();
    }
    match github_owner_repo_from_url_or_path(t) {
        Some(s) => s
            .split_once('/')
            .map(|(org, _)| org.to_string())
            .unwrap_or(s),
        None => "openclaw-cn".to_string(),
    }
}

/// 下载 GitHub 仓库 `owner/repo` 某分支的 archive.tar.gz 并解压到 `dest_dir`（纯 HTTPS，不调用 git）
pub async fn fetch_github_repo_tarball_to_dir(
    repo_path: &str,
    branch: &str,
    dest_dir: &Path,
    stage: &str,
    app: &AppHandle,
) -> Result<(), String> {
    if GITHUB_BLOCKED {
        return Err("GitHub 外部资源已被禁止，请使用本地内置包".into());
    }
    let urls = github_archive_tarball_urls_for_skills_download(repo_path, branch);
    download_plugin_tarball_try_urls(
        &urls,
        dest_dir.to_path_buf(),
        stage,
        OPENCLAW_MANAGER_UA,
        app,
    )
    .await
}

// ─── Git Clone 镜像重试 ─────────────────────────────────────────────────────

/// 克隆完成后返回 Ok(())
pub async fn git_clone_with_mirrors(
    repo_url: &str,
    dest: &Path,
    branch: Option<&str>,
    stage: &str,
    app: &AppHandle,
    mirror_prefixes: &[&str],
) -> Result<(), String> {
    if GITHUB_BLOCKED {
        return Err("Git/git clone 外部资源已被禁止，请使用本地内置包".into());
    }
    // 若 URL 非 GitHub 相关，直接克隆不走镜像
    if !is_github_url(repo_url) {
        emit_progress(
            app,
            InstallProgressEvent::started(stage, &format!("克隆: {}", repo_url)),
        );
        return git_clone_single(repo_url, dest, branch, stage, app).await;
    }

    // 主源 + 镜像列表
    let mut all_urls: Vec<String> = vec![repo_url.to_string()];
    for prefix in mirror_prefixes {
        if !prefix.is_empty() {
            all_urls.push(format!("{}{}", prefix, repo_url));
        }
    }

    for (i, url) in all_urls.iter().enumerate() {
        let url_ref: &str = url.as_str();
        if i > 0 {
            let mirror_name = url.split('/').nth(2).unwrap_or(url.as_str());
            emit_progress(
                app,
                InstallProgressEvent::mirror_fallback(
                    stage,
                    mirror_name,
                    "原链接不可用，尝试镜像...",
                ),
            );
        } else {
            // 第一个 URL 就是当前使用的（已优先镜像）
            emit_progress(
                app,
                InstallProgressEvent::started(stage, &format!("正在克隆: {}", url)),
            );
        }

        match git_clone_single(url_ref, dest, branch, stage, app).await {
            Ok(()) => {
                emit_progress(app, InstallProgressEvent::finished(stage, "克隆完成"));
                return Ok(());
            }
            Err(e) => {
                warn!("克隆失败 [{}]: {}", url, e);
            }
        }
    }

    Err("所有克隆源均不可用，请检查网络或手动克隆".to_string())
}

async fn git_clone_single(
    url: &str,
    dest: &Path,
    branch: Option<&str>,
    stage: &str,
    app: &AppHandle,
) -> Result<(), String> {
    let dest_str = dest.to_string_lossy().to_string();

    // Windows: 使用 hidden_cmd 隐藏窗口
    #[cfg(windows)]
    return {
        use crate::commands::hidden_cmd;

        let branch_arg = branch.map(|b| format!(" -b {}", b)).unwrap_or_default();
        let git_clone_cmd = format!(
            "git clone --progress --depth 1{} \"{}\" \"{}\"",
            branch_arg, url, dest_str
        );

        let mut cmd = hidden_cmd::cmd();
        cmd.arg("/C")
            .arg(&git_clone_cmd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_HTTP_TIMEOUT", "60")
            .env(
                "HTTPS_PROXY",
                std::env::var("HTTPS_PROXY").unwrap_or_default(),
            )
            .env(
                "HTTP_PROXY",
                std::env::var("HTTP_PROXY").unwrap_or_default(),
            );

        // Windows 上 git clone 可能比较慢，用 tokio::task::spawn_blocking 执行
        let output = tokio::task::spawn_blocking(move || {
            cmd.output()
        })
        .await
        .map_err(|e| format!("git clone 执行失败: {}", e))?
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
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("clone").arg("--progress").arg("--depth").arg("1");
        if let Some(b) = branch {
            cmd.args(["-b", b]);
        }
        cmd.arg(url).arg(&dest_str);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        // git 内部网络超时，避免长时间挂在单个连接上
        cmd.env("GIT_HTTP_TIMEOUT", "60");
        // Windows 代理环境变量（curl 使用）
        cmd.env(
            "HTTPS_PROXY",
            std::env::var("HTTPS_PROXY").unwrap_or_default(),
        );
        cmd.env(
            "HTTP_PROXY",
            std::env::var("HTTP_PROXY").unwrap_or_default(),
        );

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
                tokio::time::sleep(Duration::from_secs(15)).await;
                if !r.load(Ordering::Relaxed) {
                    break;
                }
                secs += 15;
                emit_progress(
                    &app_hb,
                    InstallProgressEvent::detail(
                        &stage_hb,
                        &format!("克隆仍在进行（已约 {} 秒，Git 可能暂无新输出）…", secs),
                    ),
                );
            }
        });

        let app_clone = app.clone();
        let stage_owned = stage.to_string();
        // 用 read_buf 读原始字节，手动维护行缓冲，避免 read_line 碰到无 \n 的行永久阻塞
        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = vec![0u8; 4096];
            let mut line_buf = Vec::new();
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        for &byte in &buf[..n] {
                            line_buf.push(byte);
                            if byte == b'\n' {
                                if let Ok(line) = String::from_utf8(line_buf.clone()) {
                                    let t = line.trim();
                                    if !t.is_empty() {
                                        emit_progress(
                                            &app_clone,
                                            InstallProgressEvent::detail(&stage_owned, t),
                                        );
                                    }
                                }
                                line_buf.clear();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // 全局 120s 超时，防止所有镜像都重试后仍然永久挂死
        let timeout_result = tokio::time::timeout(Duration::from_secs(120), child.wait()).await;

        let _ = read_task.await;
        running.store(false, Ordering::Relaxed);
        hb_task.abort();
        let _ = hb_task.await;

        let status = match timeout_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("git clone 进程等待失败: {}", e)),
            Err(_) => {
                // 超时，杀掉子进程
                let _ = child.kill().await;
                return Err(format!(
                    "克隆超时（120 秒），请检查网络或手动克隆。镜像 URL: {}",
                    url
                ));
            }
        };

        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "git clone 失败（退出码 {:?}），请查看上方实时输出或检查网络",
                status.code()
            ))
        }
    }
}

// ─── 网络连通性探测 ─────────────────────────────────────────────────────────

/// 用 HEAD 请求快速探测 URL 是否可达（用于镜像预检）
pub async fn probe_url(client: &reqwest::Client, url: &str) -> bool {
    match client
        .head(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            resp.status().is_success()
                || resp.status().as_u16() == 302
                || resp.status().as_u16() == 301
        }
        Err(_) => false,
    }
}
