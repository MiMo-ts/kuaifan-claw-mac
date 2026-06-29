// 插件管理命令

use crate::bundled_env::resolve_bundled_plugin_tgz;
use crate::commands::hidden_cmd;
use crate::env_paths::resolve_node;
use crate::mirror::{unpack_npm_tarball, InstallProgressEvent};
use crate::models::PluginInfo;
use futures_util::future::join_all;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter};
use tracing::info;

/// 检查 registry 值是否为合法 http(s) URL，无效时返回 None。
fn validate_registry_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.starts_with("https://") {
        return None;
    }
    // 去掉末尾斜杠后返回（保持与 push() 去斜杠逻辑一致）
    Some(trimmed.trim_end_matches('/').to_string())
}

/// 解析 npm registry 路径，优先级：环境变量 > config/app.yaml > npmmirror 国内镜像。
/// 对非法值（空串、非 http(s) URL）打 warn 并回退 npmmirror，保证插件安装不会因畸形 registry 报 ERR_INVALID_URL。
fn resolve_npm_registry(data_base: &str) -> String {
    let primary = std::env::var("NPM_CONFIG_REGISTRY")
        .ok()
        .and_then(|s| validate_registry_url(&s));

    let yaml_reg = if primary.is_some() {
        None
    } else {
        let cfg = format!("{}/config/app.yaml", data_base);
        std::fs::read_to_string(&cfg)
            .ok()
            .and_then(|content| {
                content.lines().find_map(|l: &str| {
                    let t = l.trim();
                    if !t.starts_with("registry:") {
                        return None;
                    }
                    let rest = t.split_once(':')?.1.trim().trim_matches('"').to_string();
                    validate_registry_url(&rest)
                })
            })
    };

    primary
        .or(yaml_reg)
        .unwrap_or_else(|| {
            tracing::warn!(
                "npm registry 配置无效（环境变量或 app.yaml），回退使用 https://registry.npmmirror.com"
            );
            "https://registry.npmmirror.com".to_string()
        })
}

/// 插件目录内锁文件会固定旧版 registry 解析结果（如镜像未同步的 `@openclaw-cn/*@0.1.0` → ETARGET），移植或换镜像后应删除并重解。
fn remove_plugin_lockfiles(plugin_dir: &Path) {
    for name in [
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
    ] {
        let p = plugin_dir.join(name);
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
            tracing::info!("已移除插件锁文件以便重新解析依赖: {}", p.display());
        }
    }
}

/// 是否为「精确」semver（无 ^ ~ >= 等），镜像上常缺旧精确版导致 ETARGET。
fn looks_like_exact_npm_version(ver: &str) -> bool {
    let v = ver.trim();
    if v.is_empty()
        || v == "*"
        || v == "latest"
        || v.starts_with("workspace:")
        || v.starts_with("file:")
        || v.starts_with("link:")
        || v.starts_with("http://")
        || v.starts_with("https://")
    {
        return false;
    }
    if v.starts_with('^')
        || v.starts_with('~')
        || v.starts_with(">=")
        || v.starts_with("<=")
        || v.starts_with('>')
        || v.starts_with('<')
        || v.contains('*')
        || v.contains("||")
        || v.contains(" - ")
    {
        return false;
    }
    // x.y.z 或 v1.2.3
    let t = v.trim_start_matches('v');
    let mut parts = t.split('.');
    let a = parts.next();
    let b = parts.next();
    let c = parts.next();
    match (a, b, c) {
        (Some(x), Some(y), Some(z)) if x.chars().all(|c| c.is_ascii_digit()) => {
            y.chars().all(|c| c.is_ascii_digit()) && z.chars().all(|c| c.is_ascii_digit() || c == '-')
        }
        _ => false,
    }
}

/// Validate npm spec string for shell safety (prevent command injection).
/// Only allows: alphanumeric, @, /, -, _, ., ~, ^, >=, <=, >, <, whitespace, *, |
fn validate_npm_spec(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("npm spec 不能为空".to_string());
    }
    // Reject shell metacharacters
    let forbidden = [';', '&', '`', '$', '(', ')', '{', '}', '<', '>', '!', '\n', '\r', '"', '\''];
    for ch in trimmed.chars() {
        if forbidden.contains(&ch) {
            return Err(format!("npm spec 包含非法字符: '{}'", ch));
        }
    }
    Ok(())
}

/// 写入前净化：去掉自依赖、把 `@openclaw-cn/*` 的过时精确版放宽为 `*`，避免国内镜像缺包导致整次 install 失败。
fn sanitize_plugin_package_json_for_install(plugin_dir: &Path) -> Result<(), String> {
    let pkg_path = plugin_dir.join("package.json");
    let Ok(content) = fs::read_to_string(&pkg_path) else {
        return Ok(());
    };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Ok(());
    };
    let pkg_name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let mut changed = false;
    for section in ["dependencies", "devDependencies", "optionalDependencies"] {
        let Some(obj) = v.get_mut(section).and_then(|x| x.as_object_mut()) else {
            continue;
        };
        if !pkg_name.is_empty() && obj.remove(&pkg_name).is_some() {
            tracing::info!(
                "已移除 package.json {} 中的自引用依赖「{}」",
                section,
                pkg_name
            );
            changed = true;
        }
        let keys: Vec<String> = obj.keys().cloned().collect();
        for k in keys {
            if !k.starts_with("@openclaw-cn/") {
                continue;
            }
            let Some(ver_val) = obj.get(&k) else {
                continue;
            };
            let ver = ver_val.as_str().unwrap_or("").to_string();
            if looks_like_exact_npm_version(&ver) {
                obj.insert(k.clone(), json!("*"));
                tracing::info!(
                    "已将 {} 中「{}」从精确版「{}」放宽为 *（避免镜像缺该版本）",
                    section,
                    k,
                    &ver
                );
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(());
    }
    let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&pkg_path, pretty).map_err(|e| e.to_string())
}

/// npm pack / install 使用的 registry 候选：主配置优先，失败则回退官方源（与 openclaw 安装策略一致）。
fn npm_registry_candidates(data_base: &str) -> Vec<String> {
    let primary = resolve_npm_registry(data_base);
    let mut out: Vec<String> = Vec::new();
    let push = |v: &mut Vec<String>, s: &str| {
        let t = s.trim().trim_end_matches('/').to_string();
        if !t.is_empty() && !v.iter().any(|x| x == &t) {
            v.push(t);
        }
    };
    push(&mut out, &primary);
    push(&mut out, "https://registry.npmjs.org");
    push(&mut out, "https://registry.npmmirror.com");
    out
}

/// 从 `npm install` 用的 spec 中取出包名（去掉末尾的 `@semver` 范围），用于判断是否指向当前目录自身。
fn npm_install_spec_package_base(spec: &str) -> String {
    let s = spec.trim();
    if let Some(at) = s.rfind('@') {
        let after = &s[at + 1..];
        let looks_like_version = !after.is_empty()
            && (after.starts_with('^')
                || after.starts_with('~')
                || after.starts_with(">=")
                || after.starts_with("<=")
                || after.starts_with('>')
                || after.starts_with('<')
                || after.chars().next().is_some_and(|c| c.is_ascii_digit()));
        if looks_like_version {
            return s[..at].trim().to_string();
        }
    }
    s.to_string()
}

/// 将 data/plugins 目录追加到 openclaw.json 的 plugins.load.paths，
/// 使网关启动时能发现已安装的插件（bundled/global 之外的用户安装插件）。
/// 将向导/实例里用到的 `channel_type` 映射为 `data/plugins` 下的插件目录名。
fn channel_type_to_plugin_id(channel_type: &str) -> Option<&'static str> {
    match channel_type.trim() {
        "feishu" => Some("feishu"),
        "wxwork" | "wecom" => Some("wxwork"),
        "qq" => Some("qq"),
        "wechat_clawbot" => Some("wechat_clawbot"),
        _ => None,
    }
}

/// `extensions/openclaw-weixin` 随 OpenClaw-CN 打包的是**占位**实现：`gateway.startAccount` 不建立真实微信连接，
/// 网关日志里也不会出现可工作的微信通道。必须安装 npm 上的 `@tencent-weixin/openclaw-weixin` 才能收消息。
fn wechat_plugin_dir_is_official_tencent_package(plugin_dir: &Path) -> bool {
    let pkg = plugin_dir.join("package.json");
    let Ok(s) = fs::read_to_string(&pkg) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return false;
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
    name.starts_with("@tencent-weixin/openclaw-weixin")
}

fn wechat_plugin_is_stub_or_unknown(plugin_dir: &Path) -> bool {
    !wechat_plugin_dir_is_official_tencent_package(plugin_dir)
}

/// `package.json` 的 `dependencies` 是否已在 `node_modules` 下落地（跳过 workspace:/file:/link: 协议）。
fn plugin_package_json_deps_installed(plugin_dir: &Path) -> bool {
    let pkg_path = plugin_dir.join("package.json");
    let Ok(content) = fs::read_to_string(&pkg_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) else {
        return true;
    };
    for (name, ver_val) in deps {
        let ver = ver_val.as_str().unwrap_or("");
        if ver.starts_with("workspace:") || ver.starts_with("file:") || ver.starts_with("link:") {
            continue;
        }
        let mut p = plugin_dir.join("node_modules");
        for part in name.split('/') {
            p = p.join(part);
        }
        if !p.is_dir() {
            return false;
        }
    }
    true
}

/// QQ 插件就绪判定：
/// 1) OpenClaw 包装包 `@openclaw-cn/qqbot`：`npm install` 会把 `@sliverp/qqbot` 装进 `node_modules/@sliverp/qqbot`。
/// 2) 内置/离线 tgz 若直接是官方包根目录（`package.json` 的 `name` 为 `@sliverp/qqbot`），npm 不会把包自身再装入
///    `node_modules/@sliverp/qqbot`；此时 `install_plugin_deps_blocking` 会因 spec 与包名相同而跳过定向安装，
///    仅执行普通 `npm install` 拉取顶层依赖。若仍只用 (1) 的路径检测会误判为「依赖缺失」。
fn qq_channel_runtime_ready(plugin_dir: &Path) -> bool {
    let nm = plugin_dir.join("node_modules");
    if nm.is_dir() && nm.join("@sliverp").join("qqbot").is_dir() {
        return true;
    }
    let pkg_path = plugin_dir.join("package.json");
    let Ok(content) = fs::read_to_string(&pkg_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
    if name != "@sliverp/qqbot" {
        return false;
    }
    if !plugin_dir.join("dist").join("index.js").is_file() {
        return false;
    }
    plugin_package_json_deps_installed(plugin_dir)
}

/// 通道插件运行时依赖是否就绪（与 `extensions/*/package.json` 及定向 npm 安装约定一致）。
/// 注意：检测的是插件的**实际外部依赖**，而非插件自身（npm 不会把自己安装为 node_modules 子目录）。
fn channel_plugin_runtime_ready(plugin_dir: &Path, plugin_id: &str) -> bool {
    let nm = plugin_dir.join("node_modules");
    if !nm.is_dir() {
        if plugin_id == "qq" {
            return qq_channel_runtime_ready(plugin_dir);
        }
        return false;
    }
    match plugin_id {
        // wxwork / wecom: @wecom/wecom-openclaw-plugin 依赖 @wecom/aibot-node-sdk
        "wxwork" | "wecom" => nm.join("@wecom").join("aibot-node-sdk").is_dir(),
        // qq: 见 qq_channel_runtime_ready
        "qq" => qq_channel_runtime_ready(plugin_dir),
        // feishu: 核心依赖 @larksuiteoapi/node-sdk
        "feishu" => nm.join("@larksuiteoapi").join("node-sdk").is_dir(),
        // wechat_clawbot: @tencent-weixin/openclaw-weixin
        "wechat_clawbot" => nm.join("qrcode-terminal").is_dir(),
        _ => plugin_package_json_deps_installed(plugin_dir),
    }
}

/// 启动网关前：根据已启用实例自动从 `openclaw-cn/extensions` 复制、装依赖、编译 TS，补全缺失的通道插件。
/// 不弹 UI；失败仅打日志，避免阻塞已配置好的网关启动。
pub(crate) async fn ensure_plugins_for_enabled_instances(data_dir: &str) {
    let inst_path = PathBuf::from(data_dir)
        .join("config")
        .join("instances.yaml");
    let raw = match tokio::fs::read_to_string(&inst_path).await {
        Ok(s) => s,
        Err(_) => return,
    };
    let doc: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(list) = doc.get("instances").and_then(|v| v.as_sequence()) else {
        return;
    };
    let mut need: std::collections::HashSet<String> = std::collections::HashSet::new();
    for inst in list {
        let enabled = inst
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let ct = inst
            .get("channel_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // feishu 是内置扩展 (extensions/feishu/)，不需要复制到 plugins/
        if ct == "feishu" {
            continue;
        }
        if let Some(pid) = channel_type_to_plugin_id(ct) {
            need.insert(pid.to_string());
        }
    }
    if need.is_empty() {
        return;
    }
    let data_dir_owned = data_dir.to_string();

    // dist/index.js 存在不代表 npm 依赖已装全。飞书等插件曾在「仅有空 node_modules 或缺依赖」时被误判为已完整，
    // 导致网关启动前跳过 npm install，运行时 Cannot find module '@larksuiteoapi/node-sdk'。

    let mut work: Vec<String> = Vec::new();
    for plugin_id in need {
        let root = PathBuf::from(&data_dir_owned)
            .join("plugins")
            .join(&plugin_id);
        let dist_index = root.join("dist").join("index.js");
        let need_repair_wechat_stub = plugin_id == "wechat_clawbot"
            && root.is_dir()
            && wechat_plugin_is_stub_or_unknown(&root);
        // dist 存在但 npm 包缺失 → 需要重新安装 npm 依赖
        let npm_missing = dist_index.is_file() && !channel_plugin_runtime_ready(&root, &plugin_id);
        if dist_index.is_file() && !need_repair_wechat_stub && !npm_missing {
            continue;
        }
        work.push(plugin_id);
    }

    if work.is_empty() {
        if let Err(e) =
            crate::commands::gateway::sync_openclaw_config_from_manager(data_dir).await
        {
            tracing::warn!("同步网关配置失败（非致命）: {}", e);
        }
        return;
    }

    info!(
        "网关启动前自检：将并行准备 {} 个通道插件（可能含 npm 安装与 TS 编译，首启或缺 dist 时可达数分钟）: {:?}",
        work.len(),
        work
    );

    let handles: Vec<tokio::task::JoinHandle<Result<(), String>>> = work
        .iter()
        .cloned()
        .map(|pid| {
            let dd = data_dir_owned.clone();
            tokio::task::spawn_blocking(move || ensure_one_channel_plugin_blocking(&dd, &pid))
        })
        .collect();

    let outcomes = join_all(handles).await;
    for (plugin_id, outcome) in work.into_iter().zip(outcomes) {
        match outcome {
            Ok(Ok(())) => info!("通道插件「{}」已自动准备完成", plugin_id),
            Ok(Err(e)) => tracing::warn!(
                "通道插件「{}」自动准备失败（可在插件页手动安装）: {}",
                plugin_id,
                e
            ),
            Err(e) => tracing::warn!("通道插件「{}」自动准备任务异常: {}", plugin_id, e),
        }
    }
    if let Err(e) =
        crate::commands::gateway::sync_openclaw_config_from_manager(data_dir).await
    {
        tracing::warn!("同步网关配置失败（非致命）: {}", e);
    }
}

fn ensure_one_channel_plugin_blocking(data_dir: &str, plugin_id: &str) -> Result<(), String> {
    let Some((ext_folder, npm_name)) = plugin_extension_and_npm_name(plugin_id) else {
        return Ok(());
    };
    let bundled_src = PathBuf::from(data_dir)
        .join("openclaw-cn")
        .join("extensions")
        .join(ext_folder);
    let dest = PathBuf::from(data_dir).join("plugins").join(plugin_id);

    // 微信和钉钉：优先从 npm 下载官方包（含完整 dist/ 和已编译的 TS 产物）。
    // extensions/ 目录只有 TS 源码，需 peer dependency openclaw 才能编译，单独编译会失败。
    let dest_has_dist = dest.is_dir() && dest.join("dist").join("index.js").is_file();
    if plugin_id == "wechat_clawbot" {
        // dist/index.js 不存在时，强制重新获取（含 npm install），否则网关启动会报 "plugin not found"
        let need_fetch = !dest_has_dist
            || (plugin_id == "wechat_clawbot" && wechat_plugin_is_stub_or_unknown(&dest));
        if need_fetch {
            if dest.is_dir() {
                let _ = fs::remove_dir_all(&dest);
            }
            match npm_pack_unpack_blocking(data_dir, npm_name, &dest) {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(
                        "通道 {}：从 npm 拉取官方包 {} 失败（{}），回退复制内置占位包",
                        plugin_id,
                        npm_name,
                        e
                    );
                    if !bundled_src.is_dir() {
                        return Err(format!(
                            "无法安装 {} 官方插件（{}），且本地无 extensions/{} 可回退",
                            plugin_id, e, ext_folder
                        ));
                    }
                    if let Err(e2) =
                        build_ts_extensions_blocking(data_dir, bundled_src.to_str().unwrap_or(""))
                    {
                        tracing::warn!("extensions/{} 预编译失败（继续复制）: {}", ext_folder, e2);
                    }
                    copy_dir_all(&bundled_src, &dest)?;
                }
            }
        }
    } else if !dest.is_dir() {
        if !bundled_src.is_dir() {
            return Err(format!(
                "本地无 openclaw-cn/extensions/{}，请先在插件页安装「{}」",
                ext_folder, plugin_id
            ));
        }
        if let Err(e) = build_ts_extensions_blocking(data_dir, bundled_src.to_str().unwrap_or("")) {
            tracing::warn!("extensions/{} 预编译失败（继续复制）: {}", ext_folder, e);
        }
        copy_dir_all(&bundled_src, &dest)?;
    }

    install_plugin_deps_blocking(data_dir, dest.to_str().unwrap_or(""), plugin_id, false)?;
    if let Err(e) = build_ts_extensions_blocking(data_dir, dest.to_str().unwrap_or("")) {
        tracing::warn!("插件 {} dist 编译失败: {}", plugin_id, e);
    }
    if !dest.join("dist").join("index.js").is_file() {
        return Err(format!(
            "插件 {} 仍缺少 dist/index.js，请检查 TypeScript 编译或手动在插件页重装",
            plugin_id
        ));
    }
    Ok(())
}

pub async fn sync_plugins_load_paths(data_dir: &str) -> Result<(), String> {
    let openclaw_json_path = PathBuf::from(data_dir)
        .join("openclaw-cn")
        .join("openclaw.json");

    let base: serde_json::Value = if openclaw_json_path.exists() {
        let s = tokio::fs::read_to_string(&openclaw_json_path)
            .await
            .map_err(|e| format!("读取 openclaw.json 失败: {}", e))?;
        serde_json::from_str(&s).unwrap_or(json!({}))
    } else {
        json!({})
    };

    let plugins_dir_str = PathBuf::from(data_dir)
        .join("plugins")
        .to_string_lossy()
        .replace('\\', "/");

    // 读取现有 load.paths，避免重复写入
    let existing_paths: Vec<String> = base
        .get("plugins")
        .and_then(|p| p.as_object())
        .and_then(|o| o.get("load"))
        .and_then(|l| l.as_object())
        .and_then(|o| o.get("paths"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if existing_paths.contains(&plugins_dir_str) {
        return Ok(()); // 已存在，跳过
    }

    let mut new_paths = existing_paths;
    new_paths.push(plugins_dir_str);

    let mut load_obj = serde_json::Map::new();
    load_obj.insert("paths".to_string(), serde_json::json!(new_paths));
    let mut plugins_obj = serde_json::Map::new();
    plugins_obj.insert("load".to_string(), serde_json::json!(load_obj));

    let mut merged = base.clone();
    crate::commands::gateway::merge_json_deep(&mut merged, json!({ "plugins": plugins_obj }));

    let pretty = serde_json::to_string_pretty(&merged)
        .map_err(|e| format!("序列化 openclaw.json 失败: {}", e))?;
    tokio::fs::write(&openclaw_json_path, pretty)
        .await
        .map_err(|e| format!("写入 openclaw.json 失败: {}", e))?;

    info!("已将 plugins 目录同步到 openclaw.json 的 plugins.load.paths");
    Ok(())
}

#[cfg_attr(windows, allow(unused))]
fn get_plugins_dir(data_dir: &str) -> String {
    format!("{}/plugins", data_dir)
}

fn find_pnpm_cmd() -> Option<PathBuf> {
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
    #[cfg(not(windows))]
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

/// 检查插件目录是否已完整安装 npm 依赖（可跳过 install）。
/// 需同时满足：openclaw.plugin.json 含 configSchema（stub 完整），且运行时依赖已落地（见 channel_plugin_runtime_ready）。
fn plugin_npm_installed(plugin_dir: &Path, plugin_id: &str) -> bool {
    let manifest_path = plugin_dir.join("openclaw.plugin.json");
    let has_schema = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .is_some_and(|v| v.get("configSchema").is_some());
    if !has_schema {
        return false;
    }
    channel_plugin_runtime_ready(plugin_dir, plugin_id)
}

/// 定向安装 @sliverp/qqbot / @wecom/wecom-openclaw-plugin 后，将 `openclaw.extensions` 指向**插件根目录**下真实入口。
/// 注意：`data/plugins/<id>` 即为 npm 包根目录，npm 不会把自身装进 `node_modules/@scope/pkg`，
/// 故不得使用 `./node_modules/@sliverp/qqbot/...` 这类路径（会导致网关报 entry not found）。
fn patch_official_npm_channel_plugin_after_install(
    plugin_dir: &Path,
    npm_spec: &str,
) -> Result<(), String> {
    let spec_lower = npm_spec.to_lowercase();
    let (ext_path, manifest_value): (&str, serde_json::Value) = if spec_lower
        .contains("@sliverp/qqbot")
    {
        (
            // package.json main 为 dist/index.js；相对插件根目录
            "./dist/index.js",
            json!({
                "id": "qqbot",
                "name": "QQ Bot Channel",
                "description": "QQ Bot channel plugin with message support, cron jobs, and proactive messaging",
                "channels": ["qqbot"],
                "skills": [
                    "./skills/qqbot-cron",
                    "./skills/qqbot-media"
                ],
                "capabilities": {
                    "proactiveMessaging": true,
                    "cronJobs": true
                },
                "configSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {}
                }
            }),
        )
    } else if spec_lower.contains("@wecom/wecom-openclaw-plugin") {
        (
            "./dist/esm/index.js",
            json!({
                // 与官方包 openclaw.plugin.json 一致，避免 id 与 entry 提示不一致
                "id": "wecom-openclaw-plugin",
                "channels": ["wecom"],
                "skills": ["./skills"],
                "configSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {}
                }
            }),
        )
    } else {
        return Ok(());
    };

    let pkg_path = plugin_dir.join("package.json");
    let pkg_content =
        fs::read_to_string(&pkg_path).map_err(|e| format!("读取 package.json 失败: {}", e))?;
    let mut pkg: serde_json::Value =
        serde_json::from_str(&pkg_content).map_err(|e| format!("解析 package.json 失败: {}", e))?;

    if let Some(openclaw) = pkg.get_mut("openclaw").and_then(|o| o.as_object_mut()) {
        openclaw.insert("extensions".to_string(), json!([ext_path]));
    }

    let pretty = serde_json::to_string_pretty(&pkg)
        .map_err(|e| format!("序列化 package.json 失败: {}", e))?;
    fs::write(&pkg_path, pretty).map_err(|e| format!("写入 package.json 失败: {}", e))?;

    let manifest_path = plugin_dir.join("openclaw.plugin.json");
    let pretty_m = serde_json::to_string_pretty(&manifest_value)
        .map_err(|e| format!("序列化 openclaw.plugin.json 失败: {}", e))?;
    fs::write(&manifest_path, pretty_m)
        .map_err(|e| format!("写入 openclaw.plugin.json 失败: {}", e))?;

    tracing::info!(
        "插件定向安装后已切换为官方 npm 入口: {}",
        plugin_dir.display()
    );
    Ok(())
}

/// GUI 进程内为插件目录安装 node_modules（与向导依赖安装策略一致）。
/// 若插件目录含 openclaw.install.npmSpec（如 QQ 的 @sliverp/qqbot@latest），则安装该指定包；
/// 安装后恢复 stub 的 openclaw.plugin.json（含完整 configSchema）。
/// `force`：为 true 时忽略「已完整安装」早退（用于「重装依赖」与移植修复）。
fn install_plugin_deps_blocking(
    data_base: &str,
    plugin_dir: &str,
    plugin_id: &str,
    force: bool,
) -> Result<(), String> {
    let plugin_dir_path = PathBuf::from(plugin_dir);
    tracing::info!(
        "install_plugin_deps_blocking 开始: plugin={}, dir={}, force={}",
        plugin_id,
        plugin_dir_path.display(),
        force
    );

    // 检查是否已完整安装（node_modules 存在 + stub manifest 含 configSchema）
    if !force && plugin_npm_installed(&plugin_dir_path, plugin_id) {
        tracing::info!(
            "插件 {} npm 依赖已完整安装（跳过）: {}",
            plugin_id,
            plugin_dir
        );
        return Ok(());
    }

    remove_plugin_lockfiles(&plugin_dir_path);
    sanitize_plugin_package_json_for_install(&plugin_dir_path)?;

    // 备份 stub 的 openclaw.plugin.json（含正确 configSchema）。
    // npm 包（@sliverp/qqbot / @wecom/wecom-openclaw-plugin）安装后会覆盖此文件，
    // 导致 configSchema 丢失 → 插件加载失败。
    let stub_manifest_backup = if plugin_dir_path.join("openclaw.plugin.json").is_file() {
        std::fs::read(&plugin_dir_path.join("openclaw.plugin.json")).ok()
    } else {
        None
    };

    // 读取 npmSpec。openclaw 插件把 install.npmSpec 写在 package.json 而非 openclaw.plugin.json，
    // 所以两个文件都要查。也支持 officialSpec（wecom 插件用此字段）。
    let npm_spec_opt: Option<String> = (|| {
        // 先查 openclaw.plugin.json（少数插件把 npmSpec 写在这里）
        let manifest_path = plugin_dir_path.join("openclaw.plugin.json");
        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(spec) = v
                    .get("openclaw")
                    .and_then(|o| o.get("install"))
                    .and_then(|o| o.get("npmSpec"))
                    .and_then(|x| x.as_str())
                {
                    return Some(spec.to_string());
                }
            }
        }
        // 再查 package.json（QQ / 企业微信 stub 的 npmSpec、officialSpec 在这里；不能用 ? 短路，否则上面无 openclaw 时永远不读 package.json）
        let pkg_path = plugin_dir_path.join("package.json");
        let content = std::fs::read_to_string(&pkg_path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&content).ok()?;
        let install = v.get("openclaw").and_then(|o| o.get("install"))?;
        install
            .get("npmSpec")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                install
                    .get("officialSpec")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            })
    })();

    // 若 npmSpec/officialSpec 指向本目录 package.json 的 name（已从 extensions 展开或 npm pack），
    // 定向 `npm install @scope/pkg@^x` 会再从 registry 拉同一包；公网/镜像常无旧版本 → ETARGET。
    let npm_spec_opt = npm_spec_opt.and_then(|spec| {
        let pkg_path = plugin_dir_path.join("package.json");
        let Ok(content) = std::fs::read_to_string(&pkg_path) else {
            return Some(spec);
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Some(spec);
        };
        let Some(pkg_name) = v.get("name").and_then(|x| x.as_str()) else {
            return Some(spec);
        };
        let base = npm_install_spec_package_base(&spec);
        if base.trim() == pkg_name.trim() {
            tracing::info!(
                "插件 {} 定向安装 spec「{}」与当前包 name「{}」相同，跳过定向安装",
                plugin_id,
                spec,
                pkg_name
            );
            return None;
        }
        Some(spec)
    });

    let restore_stub_manifest = |backup: &Option<Vec<u8>>, id: &str| {
        if let Some(ref bytes) = backup {
            let manifest_path = plugin_dir_path.join("openclaw.plugin.json");
            if let Err(e) = std::fs::write(&manifest_path, bytes) {
                tracing::warn!("恢复 stub openclaw.plugin.json 失败（非致命）: {}", e);
            } else {
                tracing::info!(
                    "插件 {} npm 安装成功，已恢复 stub openclaw.plugin.json（含正确 configSchema）",
                    id
                );
            }
        }
    };

    let deps_env_path = crate::env_paths::build_deps_env_path(data_base);
    let registry_primary = resolve_npm_registry(data_base);
    let apply_registry = |cmd: &mut Command, reg: &str| {
        let t = reg.trim();
        if !t.is_empty() {
            cmd.env("npm_config_registry", t);
            cmd.env("NPM_CONFIG_REGISTRY", t);
        }
    };

    // 优先：若 manifest 声明了特定 npm 包（如 QQ 的 @sliverp/qqbot），直接安装它
    if let Some(ref npm_spec) = npm_spec_opt {
        // Validate npm spec for shell safety
        if let Err(e) = validate_npm_spec(npm_spec) {
            return Err(format!("插件 {} npm spec 校验失败: {}", plugin_id, e));
        }
        tracing::info!(
            "插件 {} manifest 声明 npmSpec={}，执行定向安装",
            plugin_id,
            npm_spec
        );
        // 使用 manifest 中声明的版本（支持精确版本 / ^ / @latest 等）。
        // 与旧代码区别：不再强制追加 @latest，避免破坏已有精确版本约束。
        // 镜像未同步旧版导致的 ETARGET 由 registry 回退机制处理（见 npm_registry_candidates）。
        let add_spec = npm_spec.clone();
        let had_pnpm = find_pnpm_cmd().is_some();

        if let Some(pnpm) = find_pnpm_cmd() {
            let mut c = Command::new(&pnpm);
            c.current_dir(plugin_dir)
                .args(&["add", &add_spec])
                .env("PATH", &deps_env_path);
            apply_registry(&mut c, &registry_primary);
            let o = c
                .output()
                .map_err(|e| format!("启动 pnpm 失败: {}", e))?;
            if !o.status.success() {
                let stderr = String::from_utf8_lossy(&o.stderr);
                let stderr_lower = stderr.to_lowercase();
                if stderr_lower.contains("notarget") || stderr.contains("ETARGET") {
                    let mut c2 = Command::new(&pnpm);
                    c2.current_dir(plugin_dir)
                        .args(&["add", &add_spec])
                        .env("PATH", &deps_env_path);
                    apply_registry(&mut c2, "https://registry.npmjs.org");
                    let o2 = c2
                        .output()
                        .map_err(|e| format!("启动 pnpm 失败: {}", e))?;
                    if !o2.status.success() {
                        return Err(format!(
                            "{}\n（已在 npmjs.org 重试仍失败）\n{}",
                            stderr,
                            String::from_utf8_lossy(&o2.stderr)
                        ));
                    }
                } else if stderr_lower.contains("err_invalid_url") {
                    return Err(format!(
                        "pnpm ERR_INVALID_URL（registry 地址无效）\n当前 registry: {}\n请检查 config/app.yaml 中的 registry: 是否为合法 https:// URL\npnpm 原始错误:\n{}",
                        registry_primary, stderr
                    ));
                } else {
                    return Err(format!(
                        "pnpm add {} 失败（registry: {}）\n{}\n如遇网络问题，可尝试更换 config/app.yaml 中的 registry: 为 https://registry.npmmirror.com",
                        add_spec, registry_primary, stderr
                    ));
                }
            }
            restore_stub_manifest(&stub_manifest_backup, plugin_id);
            if let Err(e) =
                patch_official_npm_channel_plugin_after_install(&plugin_dir_path, npm_spec)
            {
                tracing::warn!("切换官方 npm 插件入口失败（非致命）: {}", e);
            }
            if channel_plugin_runtime_ready(&plugin_dir_path, plugin_id) {
                return Ok(());
            }
            tracing::warn!(
                "插件 {} 定向 pnpm add 后仍未满足运行时依赖检测（{}），将执行完整 pnpm/npm install 补全",
                plugin_id,
                plugin_dir_path.display()
            );
        }

        if !had_pnpm {
        let install_args: Vec<String> = vec![
            "install".to_string(),
            add_spec.clone(),
            "--legacy-peer-deps".to_string(),
        ];

        let (node_exe, _) = resolve_node(data_base);
        let npm_cli = node_exe.parent().and_then(|p| {
            let c = p
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js");
            if c.is_file() {
                Some(c)
            } else {
                None
            }
        });
        let npm_cmd = node_exe
            .parent()
            .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));

        let args: Vec<&str> = install_args.iter().map(|s| s.as_str()).collect();

        let o = if let Some(ref cli) = npm_cli {
            let mut c = Command::new(&node_exe);
            c.arg(cli)
                .current_dir(plugin_dir)
                .args(&args)
                .env("PATH", &deps_env_path);
            apply_registry(&mut c, &registry_primary);
            c.output()
        } else if cfg!(windows) {
            let mut c = hidden_cmd::cmd();
            c.args(["/C"])
                .arg(&npm_cmd)
                .current_dir(plugin_dir)
                .args(&args)
                .env("PATH", &deps_env_path);
            apply_registry(&mut c, &registry_primary);
            c.output()
        } else {
            let mut c = Command::new(&npm_cmd);
            c.current_dir(plugin_dir)
                .args(&args)
                .env("PATH", &deps_env_path);
            apply_registry(&mut c, &registry_primary);
            c.output()
        }
        .map_err(|e| format!("启动 npm 失败: {}", e))?;

        if !o.status.success() {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stderr_lower = stderr.to_lowercase();
            if stderr_lower.contains("notarget") || stderr.contains("ETARGET") {
                let o2 = if let Some(ref cli) = npm_cli {
                    let mut c = Command::new(&node_exe);
                    c.arg(cli)
                        .current_dir(plugin_dir)
                        .args(&args)
                        .env("PATH", &deps_env_path);
                    apply_registry(&mut c, "https://registry.npmjs.org");
                    c.output()
                } else if cfg!(windows) {
                    let mut c = hidden_cmd::cmd();
                    c.args(["/C"])
                        .arg(&npm_cmd)
                        .current_dir(plugin_dir)
                        .args(&args)
                        .env("PATH", &deps_env_path);
                    apply_registry(&mut c, "https://registry.npmjs.org");
                    c.output()
                } else {
                    let mut c = Command::new(&npm_cmd);
                    c.current_dir(plugin_dir)
                        .args(&args)
                        .env("PATH", &deps_env_path);
                    apply_registry(&mut c, "https://registry.npmjs.org");
                    c.output()
                }
                .map_err(|e| format!("启动 npm 失败: {}", e))?;
                if !o2.status.success() {
                    return Err(format!(
                        "{}\n（已在 registry.npmjs.org 重试仍失败）\n{}",
                        stderr,
                        String::from_utf8_lossy(&o2.stderr)
                    ));
                }
            } else if stderr_lower.contains("err_invalid_url") {
                return Err(format!(
                    "npm ERR_INVALID_URL（registry 地址无效）\n当前 registry: {}\n请检查 config/app.yaml 中的 registry: 是否为合法 https:// URL\nnpm 原始错误:\n{}",
                    registry_primary, stderr
                ));
            } else {
                return Err(format!(
                    "npm install {} 失败（registry: {}）\n{}\n如遇网络问题，可尝试更换 config/app.yaml 中的 registry: 为 https://registry.npmmirror.com",
                    add_spec, registry_primary, stderr
                ));
            }
        }
        restore_stub_manifest(&stub_manifest_backup, plugin_id);
        if let Err(e) = patch_official_npm_channel_plugin_after_install(&plugin_dir_path, npm_spec) {
            tracing::warn!("切换官方 npm 插件入口失败（非致命）: {}", e);
        }
        if channel_plugin_runtime_ready(&plugin_dir_path, plugin_id) {
            return Ok(());
        }
        tracing::warn!(
            "插件 {} 定向 npm install {} 后仍未满足运行时依赖检测（{}），将执行完整 pnpm/npm install 补全",
            plugin_id,
            add_spec,
            plugin_dir_path.display()
        );
        }
    }

    // 默认：通用 npm install（安装 package.json 中的 dependencies + optionalDependencies）
    tracing::info!(
        "插件 {} 执行完整 npm/pnpm install（npm_spec_opt={:?}）",
        plugin_id,
        npm_spec_opt.as_ref().map(|s| s.as_str())
    );
    if let Some(pnpm) = find_pnpm_cmd() {
        let mut c = Command::new(&pnpm);
        c.current_dir(plugin_dir)
            .arg("install")
            .env("PATH", &deps_env_path);
        apply_registry(&mut c, &registry_primary);
        let o = c
            .output()
            .map_err(|e| format!("启动 pnpm 失败: {}", e))?;
        if !o.status.success() {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stderr_lower = stderr.to_lowercase();
            if stderr_lower.contains("notarget") || stderr.contains("ETARGET") {
                let o2 = Command::new(&pnpm)
                    .current_dir(plugin_dir)
                    .arg("install")
                    .env("PATH", &deps_env_path)
                    .env("npm_config_registry", "https://registry.npmjs.org")
                    .env("NPM_CONFIG_REGISTRY", "https://registry.npmjs.org")
                    .output()
                    .map_err(|e| format!("启动 pnpm 失败: {}", e))?;
                if !o2.status.success() {
                    return Err(format!(
                        "{}\n（已在 npmjs.org 重试仍失败）\n{}",
                        stderr,
                        String::from_utf8_lossy(&o2.stderr)
                    ));
                }
            } else if stderr_lower.contains("err_invalid_url") {
                return Err(format!(
                    "pnpm ERR_INVALID_URL（registry 地址无效）\n当前 registry: {}\n请检查 config/app.yaml 中的 registry: 是否为合法 https:// URL\n\
                    pnpm 原始错误:\n{}",
                    registry_primary, stderr
                ));
            } else {
                return Err(format!(
                    "pnpm install 失败（registry: {}）\n{}\n\
                    如遇网络问题，可尝试更换 config/app.yaml 中的 registry: 为 https://registry.npmmirror.com",
                    registry_primary, stderr
                ));
            }
        }
        if channel_plugin_runtime_ready(&plugin_dir_path, plugin_id) {
            return Ok(());
        }
        tracing::warn!(
            "插件 {} 完整 pnpm install 后仍未满足运行时依赖检测（{}），将回退执行 npm install",
            plugin_id,
            plugin_dir_path.display()
        );
    }

    let (node_exe, _) = resolve_node(data_base);
    let npm_cli = node_exe.parent().and_then(|p| {
        let c = p
            .join("node_modules")
            .join("npm")
            .join("bin")
            .join("npm-cli.js");
        if c.is_file() {
            Some(c)
        } else {
            None
        }
    });
    let npm_cmd = node_exe
        .parent()
        .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));

    tracing::info!(
        "npm 安装诊断 - node_exe={}, npm_cli={:?}, npm_cmd={}, workdir={}, registry={}",
        node_exe.display(),
        npm_cli.as_ref().map(|p| p.display().to_string()),
        npm_cmd.display(),
        plugin_dir,
        registry_primary
    );

    let args = ["install", "--legacy-peer-deps"];
    let run_npm_install = |reg: &str| -> Result<std::process::Output, std::io::Error> {
        let t = reg.trim();
        let fill_env = |cmd: &mut Command| {
            cmd.env("PATH", &deps_env_path);
            if !t.is_empty() {
                cmd.env("npm_config_registry", t);
                cmd.env("NPM_CONFIG_REGISTRY", t);
            }
        };
        if let Some(ref cli) = npm_cli {
            let mut c = Command::new(&node_exe);
            c.arg(cli).current_dir(plugin_dir).args(args);
            fill_env(&mut c);
            c.output()
        } else if cfg!(windows) {
            let mut c = hidden_cmd::cmd();
            c.args(["/C"])
                .arg(&npm_cmd)
                .current_dir(plugin_dir)
                .args(args);
            fill_env(&mut c);
            c.output()
        } else {
            let mut c = Command::new(&npm_cmd);
            c.current_dir(plugin_dir).args(args);
            fill_env(&mut c);
            c.output()
        }
    };

    let o = run_npm_install(&registry_primary).map_err(|e| format!("启动 npm 失败: {}", e))?;

    // 诊断：无论成功失败都记录 npm 输出
    let npm_stdout = String::from_utf8_lossy(&o.stdout);
    let npm_stderr = String::from_utf8_lossy(&o.stderr);
    tracing::info!(
        "npm install 结果: exit={:?}, stdout={}, stderr={}",
        o.status.code(),
        npm_stdout.lines().take(10).collect::<Vec<_>>().join(" | "),
        npm_stderr.lines().take(10).collect::<Vec<_>>().join(" | ")
    );

    if !o.status.success() {
        let stderr = String::from_utf8_lossy(&o.stderr);
        let stderr_lower = stderr.to_lowercase();
        if stderr_lower.contains("notarget") || stderr.contains("ETARGET") {
            let o2 = run_npm_install("https://registry.npmjs.org")
                .map_err(|e| format!("启动 npm 失败: {}", e))?;
            if !o2.status.success() {
                return Err(format!(
                    "{}\n（已在 registry.npmjs.org 重试仍失败）\n{}",
                    stderr,
                    String::from_utf8_lossy(&o2.stderr)
                ));
            }
        } else if stderr_lower.contains("err_invalid_url") {
            return Err(format!(
                "npm ERR_INVALID_URL（registry 地址无效）\n当前 registry: {}\n请检查 config/app.yaml 中的 registry: 是否为合法 https:// URL\n\
                npm 原始错误:\n{}",
                registry_primary, stderr
            ));
        } else {
            return Err(format!(
                "npm install 失败（registry: {}）\n{}\n\
                如遇网络问题，可尝试更换 config/app.yaml 中的 registry: 为 https://registry.npmmirror.com",
                registry_primary, stderr
            ));
        }
    }

    restore_stub_manifest(&stub_manifest_backup, plugin_id);
    if channel_plugin_runtime_ready(&plugin_dir_path, plugin_id) {
        Ok(())
    } else {
        Err(format!(
            "依赖安装流程已执行，但仍未检测到插件「{}」所需的 node_modules（{}）。请检查网络与 npm 镜像配置后点击「重装依赖」。",
            plugin_id,
            plugin_dir_path.display()
        ))
    }
}

/// 与 OpenClaw-CN 仓库 `extensions/<folder>` 及 npm 包名的对应（与 plugins.yaml 中的 id 一致）。
///
/// **npm pack 必须使用 registry 上真实存在的包名**：
/// - `wxwork`：extensions 里 stub 的 `name` 为 `@openclaw-cn/wecom-openclaw-plugin`，但该包往往未发布到 npm；
///   实际安装应拉取企业微信官方包 `@wecom/wecom-openclaw-plugin`（与 stub 的 `openclaw.install.officialSpec` 一致）。
/// - `qq`：定向依赖为社区包 `@sliverp/qqbot`（与 stub 的 `openclaw.install.npmSpec` 一致），`@openclaw-cn/qqbot` 可能未同步到镜像。
fn plugin_extension_and_npm_name(plugin_id: &str) -> Option<(&'static str, &'static str)> {
    match plugin_id {
        "feishu" => Some(("feishu", "@openclaw-cn/feishu")),
        "wxwork" | "wecom" => Some((
            "wecom-connector",
            "@wecom/wecom-openclaw-plugin",
        )),
        "wechat_clawbot" => Some(("openclaw-weixin", "@tencent-weixin/openclaw-weixin")),
        "qq" => Some(("qqbot", "@sliverp/qqbot")),
        _ => None,
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!("源不是目录: {}", src.display()));
    }
    if dst.exists() {
        fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    fn walk(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
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

    walk(src, dst).map_err(|e| format!("复制插件目录失败: {}", e))
}

/// npm pack 使用显式 `@latest`，避免镜像只同步了部分版本导致拉到不可安装的元数据。
fn npm_pack_spec_with_latest_tag(npm_package: &str) -> String {
    let s = npm_package.trim();
    if s.ends_with("@latest") {
        return s.to_string();
    }
    if let Some(at) = s.rfind('@') {
        let after = s[at + 1..].trim();
        if !after.is_empty()
            && (after == "latest"
                || after.starts_with('^')
                || after.starts_with('~')
                || after.starts_with(">=")
                || after.starts_with("<=")
                || after.starts_with('>')
                || after.starts_with('<')
                || after.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return s.to_string();
        }
    }
    format!("{}@latest", s)
}

/// 使用 npm pack 下载 registry 包并解压到目标目录（与 GitHub tarball 一样为单根目录结构）。
/// 按 `npm_registry_candidates` 依次尝试，主镜像缺包时自动回退 npmjs.org。
fn npm_pack_unpack_blocking(
    data_base: &str,
    npm_package: &str,
    dest_dir: &Path,
) -> Result<(), String> {
    let pack_spec = npm_pack_spec_with_latest_tag(npm_package);
    let cache_root = PathBuf::from(data_base).join(".cache").join("npm-pack");
    fs::create_dir_all(&cache_root).map_err(|e| e.to_string())?;
    let pack_dest = cache_root.join(format!(
        "{}-{}",
        npm_package.replace(['/', '@'], "-"),
        std::process::id()
    ));
    if pack_dest.exists() {
        let _ = fs::remove_dir_all(&pack_dest);
    }
    fs::create_dir_all(&pack_dest).map_err(|e| e.to_string())?;
    let pack_dest_str = pack_dest.to_string_lossy().to_string();

    let (node_exe, _) = resolve_node(data_base);
    let npm_cli = node_exe.parent().and_then(|p| {
        let c = p
            .join("node_modules")
            .join("npm")
            .join("bin")
            .join("npm-cli.js");
        if c.is_file() {
            Some(c)
        } else {
            None
        }
    });
    let npm_cmd = node_exe
        .parent()
        .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));

    let pack_args = [
        "pack",
        pack_spec.as_str(),
        "--pack-destination",
        pack_dest_str.as_str(),
    ];

    let deps_env_path = crate::env_paths::build_deps_env_path(data_base);
    let mut last_combined = String::new();

    for registry in npm_registry_candidates(data_base) {
        let npm_out = if let Some(ref cli) = npm_cli {
            Command::new(&node_exe)
                .arg(cli)
                .args(pack_args)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry)
                .env("NPM_CONFIG_REGISTRY", &registry)
                .output()
        } else if cfg!(windows) {
            hidden_cmd::cmd()
                .args(["/C"])
                .arg(&npm_cmd)
                .args(pack_args)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry)
                .env("NPM_CONFIG_REGISTRY", &registry)
                .output()
        } else {
            Command::new(&npm_cmd)
                .args(pack_args)
                .env("PATH", &deps_env_path)
                .env("npm_config_registry", &registry)
                .env("NPM_CONFIG_REGISTRY", &registry)
                .output()
        }
        .map_err(|e| format!("启动 npm pack 失败: {}", e))?;

        if !npm_out.status.success() {
            let stderr = String::from_utf8_lossy(&npm_out.stderr);
            let stdout = String::from_utf8_lossy(&npm_out.stdout);
            last_combined = format!(
                "[registry {}] npm pack 失败:\n{}\n{}",
                registry, stderr, stdout
            );
            tracing::warn!("{}", last_combined);
            if let Ok(rd) = fs::read_dir(&pack_dest) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|x| x == "tgz") {
                        let _ = fs::remove_file(&p);
                    }
                }
            }
            continue;
        }

        let tgz_files: Vec<PathBuf> = fs::read_dir(&pack_dest)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "tgz"))
            .collect();

        if tgz_files.len() != 1 {
            let _ = fs::remove_dir_all(&pack_dest);
            last_combined = format!(
                "[registry {}] npm pack 后应产生 1 个 .tgz，实际 {} 个",
                registry,
                tgz_files.len()
            );
            tracing::warn!("{}", last_combined);
            fs::create_dir_all(&pack_dest).map_err(|e| e.to_string())?;
            continue;
        }

        let bytes = fs::read(&tgz_files[0]).map_err(|e| e.to_string())?;
        match unpack_npm_tarball(&bytes, dest_dir) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&pack_dest);
                tracing::info!("npm pack 成功: {} @ {}", pack_spec, registry);
                return Ok(());
            }
            Err(e) => {
                last_combined = format!("[registry {}] 解压 tgz 失败: {}", registry, e);
                tracing::warn!("{}", last_combined);
                let _ = fs::remove_dir_all(&pack_dest);
                fs::create_dir_all(&pack_dest).map_err(|e2| e2.to_string())?;
            }
        }
    }

    let _ = fs::remove_dir_all(&pack_dest);
    Err(format!(
        "npm pack {} 在所有 registry 均失败。最后信息:\n{}",
        pack_spec, last_combined
    ))
}

/// 找到 tsc 可执行文件。
/// 优先级：1. openclaw-cn 主程序的 tsc（openclaw 安装完后才有）；2. bundled node 的 npm 全局 tsc；3. 系统 PATH 中的 tsc。
/// 插件安装必须在 openclaw 安装完成后才能执行（向导顺序），所以 tsc 在 openclaw-cn/node_modules/.bin/tsc 必定存在。
/// Fallback 到系统 PATH 时走 bundled node 的 npm（GUI 进程 PATH 可能为空）。
fn find_tsc_exe(data_base: &str) -> (PathBuf, String) {
    // 1. 优先：openclaw-cn 主程序的 tsc
    let openclaw_tsc = PathBuf::from(data_base)
        .join("openclaw-cn")
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) { "tsc.cmd" } else { "tsc" });
    if openclaw_tsc.is_file() {
        return (
            openclaw_tsc,
            "openclaw-cn/node_modules/.bin/tsc".to_string(),
        );
    }

    // 2. 兜底：通过 bundled node 的 npm config prefix 找全局 TypeScript
    let bundled_node_exe = PathBuf::from(data_base)
        .join("env")
        .join(if cfg!(windows) { "node" } else { "bin" })
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    if bundled_node_exe.exists() {
        let npm_bin = bundled_node_exe
            .parent()
            .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
            .filter(|p| p.is_file());
        if let Some(npm) = npm_bin {
            // npm config get prefix 找全局包根目录（即使 GUI 进程 PATH 为空也能用绝对路径执行 npm）
            let prefix_out = if cfg!(windows) {
                hidden_cmd::cmd()
                    .args(["/C"])
                    .arg(&npm)
                    .args(["config", "get", "prefix"])
                    .output()
            } else {
                Command::new(&npm)
                    .args(["config", "get", "prefix"])
                    .output()
            };
            if let Ok(out) = prefix_out {
                if out.status.success() {
                    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let tsc_path = PathBuf::from(&prefix).join(if cfg!(windows) {
                        "tsc.exe"
                    } else {
                        "bin/tsc"
                    });
                    if tsc_path.is_file() {
                        return (
                            tsc_path.clone(),
                            format!("bundled npm global tsc ({})", tsc_path.display()),
                        );
                    }
                }
            }
        }
    }

    // 3. 降级：系统 PATH 中的 tsc（GUI 进程 PATH 可能为空，风险较高）
    let system_tsc = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
    (
        PathBuf::from(system_tsc),
        "system PATH (PATH 可能为空)".to_string(),
    )
}

/// 为含有 tsconfig.json 的插件目录运行 TypeScript 编译（生成 dist/）。
/// 这对于 openclaw.extensions 指向 ./dist/index.js 的插件（如 feishu）是必需的。
/// 若目录中无 tsconfig.json 但有 index.ts（npm 包未附带的 tsconfig），自动生成一份。
fn build_ts_extensions_blocking(data_base: &str, plugin_dir: &str) -> Result<(), String> {
    let base = PathBuf::from(plugin_dir);
    if base.join("dist").is_dir() {
        return Ok(()); // 已有 dist/，跳过编译
    }

    // 若无 tsconfig.json 但有 index.ts（如 npm 官方包 @tencent-weixin/openclaw-weixin），自动生成
    if !base.join("tsconfig.json").is_file() {
        if !base.join("index.ts").is_file() {
            return Ok(()); // 无 tsconfig 且无 index.ts，跳过编译
        }
        let generated = serde_json::json!({
            "compilerOptions": {
                "target": "ES2022",
                "module": "ES2022",
                "moduleResolution": "bundler",
                "outDir": "./dist",
                "rootDir": ".",
                "strict": true,
                "esModuleInterop": true,
                "skipLibCheck": true,
                "resolveJsonModule": true,
                "allowSyntheticDefaultImports": true,
                "declaration": false
            },
            "include": ["**/*.ts"]
        });
        let tsconfig_path = base.join("tsconfig.json");
        fs::write(
            &tsconfig_path,
            serde_json::to_string_pretty(&generated).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("写入自动生成的 tsconfig.json 失败: {}", e))?;
        tracing::info!(
            "为插件 {} 自动生成了 tsconfig.json（npm 包未附带）",
            plugin_dir
        );
    }

    let (tsc_exe, tsc_source) = find_tsc_exe(data_base);
    let args = vec!["--project", plugin_dir];

    let o = if cfg!(windows) {
        hidden_cmd::cmd()
            .args(["/C"])
            .arg(&tsc_exe)
            .current_dir(plugin_dir)
            .args(&args)
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .output()
    } else {
        Command::new(&tsc_exe)
            .current_dir(plugin_dir)
            .args(&args)
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .output()
    }
    .map_err(|e| format!("启动 tsc 失败: {}", e))?;

    if !o.status.success() {
        let stderr = String::from_utf8_lossy(&o.stderr);
        let stdout = String::from_utf8_lossy(&o.stdout);
        return Err(format!(
            "tsc 编译失败（tsc 来源: {}）:\n{}\n{}",
            tsc_source, stderr, stdout
        ));
    }
    Ok(())
}

// 列出所有可用插件
#[tauri::command]
pub async fn list_plugins(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<Vec<PluginInfo>, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let plugins_dir = get_plugins_dir(&data_dir);

    // 定义可用插件列表（与 plugins.yaml 一致）
    let plugin_defs = vec![
        ("feishu", "飞书", "📱", "飞书企业自建应用", true),
        ("wxwork", "企业微信", "📱", "企业微信自建应用", true),
        (
            "wechat_clawbot",
            "微信 ClawBot",
            "💬",
            "微信官方插件（协议接入，非公众号）",
            true,
        ),
        ("qq", "QQ", "📱", "QQ 机器人协议", true),
    ];

    let mut plugins = Vec::new();

    for (id, name, icon, description, _webhook) in plugin_defs {
        let plugin_path = PathBuf::from(&plugins_dir).join(id);
        let pp = plugin_path.clone();
        let pid = id.to_string();
        // 「已安装」不得仅用 is_dir() 判断：空目录、半套 stub、网关自检留下的无 node_modules 目录
        // 会被误判为已安装并显示「依赖缺失」。与「未下载」一致：仅当 package.json 存在且运行时依赖就绪才算已安装。
        // 微信另需官方 @tencent-weixin 包，占位 stub 不算安装。
        let (installed, deps_ready, version) = tokio::task::spawn_blocking(move || {
            if !pp.is_dir() {
                return (false, false, None);
            }
            let pkg_path = pp.join("package.json");
            let has_pkg = pkg_path.is_file();
            let version = if has_pkg {
                fs::read_to_string(&pkg_path).ok().and_then(|content| {
                    serde_json::from_str::<serde_json::Value>(&content).ok().and_then(|v| {
                        v.get("version")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                    })
                })
            } else {
                None
            };
            let ready = if pid == "wechat_clawbot" {
                wechat_plugin_dir_is_official_tencent_package(&pp)
                    && channel_plugin_runtime_ready(&pp, &pid)
            } else {
                has_pkg && channel_plugin_runtime_ready(&pp, &pid)
            };
            (ready, ready, version)
        })
        .await
        .unwrap_or((false, false, None));

        plugins.push(PluginInfo {
            id: id.to_string(),
            name: name.to_string(),
            icon: icon.to_string(),
            description: description.to_string(),
            installed,
            version,
            enabled: installed,
            deps_ready,
        });
    }

    Ok(plugins)
}

// 检查插件是否已安装
#[tauri::command]
pub async fn check_plugin_installed(
    data_dir: tauri::State<'_, crate::AppState>,
    plugin_id: String,
) -> Result<bool, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let plugin_path = PathBuf::from(&data_dir).join("plugins").join(&plugin_id);

    // 先检查 plugins/ 目录
    if plugin_path.is_dir() {
        if plugin_id == "wechat_clawbot" {
            return Ok(
                wechat_plugin_dir_is_official_tencent_package(&plugin_path)
                    && channel_plugin_runtime_ready(&plugin_path, &plugin_id),
            );
        }
        if plugin_path.join("package.json").is_file()
            && channel_plugin_runtime_ready(&plugin_path, &plugin_id)
        {
            return Ok(true);
        }
    }

    // 回退检查：built-in 扩展（extensions/ 目录下有编译好的 dist/）
    if let Some((ext_folder, _)) = plugin_extension_and_npm_name(&plugin_id) {
        let ext_path = PathBuf::from(&data_dir)
            .join("openclaw-cn")
            .join("extensions")
            .join(ext_folder);
        if ext_path.join("dist").join("index.js").is_file()
            || ext_path.join("dist").is_dir()
        {
            // 飞书等内置扩展：有 dist 即视为可用
            if plugin_id == "feishu" {
                return Ok(true);
            }
            // 钉钉/企微：需检查 node_modules 依赖
            if ext_path.join("node_modules").is_dir() {
                return Ok(true);
            }
            // 有 dist 但无 node_modules：视为已安装（网关会从 openclaw-cn node_modules 解析）
            return Ok(true);
        }
    }

    Ok(false)
}

/// 强制重装插件 npm 依赖（忽略已安装判断）。
/// 用于：移植后依赖缺失（node_modules 不完整）、网关报 Cannot find module 时一键修复。
/// 会重新执行 npm install、tsc 编译、更新 openclaw.json，重启网关后生效。
#[tauri::command]
pub async fn reinstall_plugin_deps(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
    plugin_id: String,
) -> Result<String, String> {
    info!("强制重装插件依赖: {}", plugin_id);
    let data_dir = data_dir.inner().get_data_dir();
    let plugin_path = PathBuf::from(&data_dir).join("plugins").join(&plugin_id);

    if !plugin_path.is_dir() {
        return Err(format!("插件「{}」目录不存在，请先安装插件", plugin_id));
    }

    let stage = format!("fix-deps-{}", plugin_id);
    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::started(
            &stage,
            &format!("正在重新安装插件「{}」依赖（npm install）…", plugin_id),
        ),
    );

    let base = data_dir.clone();
    let cwd = plugin_path.to_string_lossy().to_string();
    let pid = plugin_id.clone();
    tokio::task::spawn_blocking(move || install_plugin_deps_blocking(&base, &cwd, &pid, true))
        .await
        .map_err(|e| format!("依赖安装任务失败: {}", e))?
        .map_err(|e| {
            let _ = app.emit("install-progress", InstallProgressEvent::failed(&stage, &e));
            e
        })?;

    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::started(&stage, "正在重新编译 TypeScript 扩展…"),
    );
    let base2 = data_dir.clone();
    let cwd2 = plugin_path.to_string_lossy().to_string();
    tokio::task::spawn_blocking(move || build_ts_extensions_blocking(&base2, &cwd2))
        .await
        .map_err(|e| format!("编译任务失败: {}", e))?
        .map_err(|e| {
            let _ = app.emit("install-progress", InstallProgressEvent::failed(&stage, &e));
            e
        })?;

    crate::commands::gateway::sync_openclaw_config_from_manager(&data_dir)
        .await
        .map_err(|e| format!("同步网关配置失败: {}", e))?;

    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::finished(
            &stage,
            &format!("插件「{}」依赖重装完成，请重启网关使新插件生效", plugin_id),
        ),
    );
    info!("插件 {} 依赖重装完成", plugin_id);
    Ok(format!(
        "插件「{}」依赖重装完成，请重启网关使新插件生效",
        plugin_id
    ))
}

/// 飞书专用兜底：通用 npm install 失败后，尝试显式安装核心依赖 @larksuiteoapi/node-sdk。
/// 镜像元数据异常时（如 ETARGET / 同步延迟），完整 install 失败但单包可能成功，
/// 飞书插件加载只需此一个外部依赖，其余为可选或已有内置。
fn feishu_node_sdk_fallback_install(
    data_base: &str,
    plugin_dir: &Path,
) -> Result<(), String> {
    let sdk_package = "@larksuiteoapi/node-sdk";
    let pkg_json_path = plugin_dir.join("package.json");
    let sdk_version = if let Ok(content) = std::fs::read_to_string(&pkg_json_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            v.get("dependencies")
                .and_then(|d| d.get(sdk_package))
                .and_then(|x| x.as_str())
                .unwrap_or("^1.56.1")
                .to_string()
        } else {
            "^1.56.1".to_string()
        }
    } else {
        "^1.56.1".to_string()
    };

    let spec = format!("{}@{}", sdk_package, sdk_version);
    tracing::warn!(
        "飞书插件 npm install 失败，尝试单独安装核心依赖 {}（镜像元数据异常时此兜底可能生效）",
        spec
    );

    let deps_env_path = crate::env_paths::build_deps_env_path(data_base);
    let (node_exe, _) = resolve_node(data_base);
    let npm_cli = node_exe.parent().and_then(|p| {
        let c = p
            .join("node_modules")
            .join("npm")
            .join("bin")
            .join("npm-cli.js");
        if c.is_file() {
            Some(c)
        } else {
            None
        }
    });
    let npm_cmd = node_exe
        .parent()
        .map(|p| p.join(if cfg!(windows) { "npm.cmd" } else { "npm" }))
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }));

    let args = ["install", "--legacy-peer-deps", &spec];
    let registries = npm_registry_candidates(data_base);

    for reg in &registries {
        let fill_env = |cmd: &mut Command| {
            cmd.env("PATH", &deps_env_path);
            cmd.env("npm_config_registry", reg);
            cmd.env("NPM_CONFIG_REGISTRY", reg);
        };

        let output = if let Some(ref cli) = npm_cli {
            let mut c = Command::new(&node_exe);
            c.arg(cli).current_dir(plugin_dir).args(args);
            fill_env(&mut c);
            c.output()
        } else if cfg!(windows) {
            let mut c = hidden_cmd::cmd();
            c.args(["/C"]).arg(&npm_cmd).current_dir(plugin_dir).args(args);
            fill_env(&mut c);
            c.output()
        } else {
            let mut c = Command::new(&npm_cmd);
            c.current_dir(plugin_dir).args(args);
            fill_env(&mut c);
            c.output()
        };

        let o = match output {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("飞书兜底：在 {} 启动 npm 失败: {}", reg, e);
                continue;
            }
        };

        if o.status.success() {
            tracing::info!("飞书兜底安装 {} 成功（registry: {}）", spec, reg);
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&o.stderr);
        tracing::warn!(
            "飞书兜底安装 {} 在 {} 失败:\n{}",
            spec, reg, stderr
        );
    }

    Err(format!(
        "飞书插件核心依赖 {} 安装失败。\n\
        请检查网络后重新安装飞书插件，或手动执行：\n\
        cd \"{}\" && npm install\n\
        如网络异常，请确认 config/app.yaml 中 registry: 为 https://registry.npmmirror.com",
        spec,
        plugin_dir.display()
    ))
}

// 安装插件
#[tauri::command]
pub async fn install_plugin(
    app: AppHandle,
    data_dir: tauri::State<'_, crate::AppState>,
    plugin_id: String,
) -> Result<String, String> {
    info!("开始安装插件: {}", plugin_id);

    let data_dir = data_dir.inner().get_data_dir();
    let plugins_dir = get_plugins_dir(&data_dir);
    let plugin_path = format!("{}/{}", plugins_dir, plugin_id);
    let dest_path = PathBuf::from(&plugin_path);

    // 已存在目录：微信检查是否为官方包；其他插件检查运行时依赖是否就绪。
    // 目录存在但依赖不全时仍触发安装（npm install），避免网关无法加载。
    if dest_path.exists() {
        if plugin_id == "wechat_clawbot" && wechat_plugin_is_stub_or_unknown(&dest_path) {
            let _ = tokio::fs::remove_dir_all(&plugin_path).await;
        } else if plugin_id == "wechat_clawbot"
            || channel_plugin_runtime_ready(&dest_path, &plugin_id)
        {
            return Ok(format!("插件「{}」已就绪（依赖完整）", plugin_id));
        } else {
            // 目录存在但运行时依赖缺失（如 node_modules 不全）：继续执行 npm install
            info!("插件「{}」目录存在但依赖缺失，将重新安装依赖", plugin_id);
        }
    }

    let stage = format!("plugin-{}", plugin_id);

    if plugin_id == "email" {
        let msg = "邮件插件暂未提供向导内一键安装，请查阅文档或手动部署。";
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::failed(&stage, msg),
        );
        return Err(msg.to_string());
    }

    let Some((ext_folder, npm_name)) = plugin_extension_and_npm_name(plugin_id.as_str()) else {
        let msg = format!("未知插件 id: {}", plugin_id);
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::failed(&stage, &msg),
        );
        return Err(msg);
    };

    let bundled_src = PathBuf::from(&data_dir)
        .join("openclaw-cn")
        .join("extensions")
        .join(ext_folder);
    let dest = PathBuf::from(&plugin_path);

    let source_label: String;
    // 国内维护的通道插件：优先 npm pack（含完整 dist/），避免精简版 openclaw-cn 无 extensions 时只剩源码。
    // extensions/ 与本地编译仅作离线回退。
    let npm_first = matches!(
        plugin_id.as_str(),
        "wechat_clawbot" | "feishu" | "wxwork" | "qq" | "wecom"
    );
    tracing::info!(
        "install_plugin: id={}, dest={}, bundled_src_exists={}, npm_first={}",
        plugin_id,
        dest.display(),
        bundled_src.is_dir(),
        npm_first
    );

    if npm_first {
        let plugin_display_name = match plugin_id.as_str() {
            "wechat_clawbot" => "微信",
            "feishu" => "飞书",
            "wxwork" | "wecom" => "企业微信",
            "qq" => "QQ",
            _ => "通道",
        };

        // ── 优先：尝试使用内置插件包（离线，企业内网环境的关键路径）─────────
        let bundled_tgz = resolve_bundled_plugin_tgz(&app, plugin_id.as_str());
        tracing::info!(
            "插件 {} 内置 tgz 解析结果: {:?}",
            plugin_id,
            bundled_tgz.as_ref().map(|p| p.display().to_string())
        );
        if let Some(tgz_path) = bundled_tgz {
            let tgz_path_str = tgz_path.display().to_string();
            let _ = app.emit(
                "install-progress",
                InstallProgressEvent::detail(
                    &stage,
                    &format!("正在从安装包加载 {} 插件…", plugin_display_name),
                ),
            );
            let bytes = match std::fs::read(&tgz_path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("读取内置插件包失败 {}: {}", tgz_path_str, e);
                    vec![]
                }
            };
            if !bytes.is_empty() {
                match crate::mirror::unpack_npm_tarball(&bytes, &dest) {
                    Ok(()) => {
                        tracing::info!(
                            "内置插件包 {} 解压成功",
                            tgz_path_str
                        );
                        // 安装后打 patch：修复 openclaw.plugin.json 和 package.json
                        if let Err(e) =
                            patch_official_npm_channel_plugin_after_install(&dest, npm_name)
                        {
                            tracing::warn!("插件 {} 安装后 patch 失败（不影响运行）: {}", plugin_id, e);
                        }
                        // 安装 npm 依赖（plugin 的 node_modules）
                        let _ = app.emit(
                            "install-progress",
                            InstallProgressEvent::detail(
                                &stage,
                                &format!("正在安装 {} 插件依赖…", plugin_display_name),
                            ),
                        );
                        match install_plugin_deps_blocking(
                            &data_dir,
                            dest.to_str().unwrap_or(""),
                            plugin_id.as_str(),
                            false,
                        ) {
                            Ok(()) => {
                                if !channel_plugin_runtime_ready(&dest, plugin_id.as_str()) {
                                    let msg = format!(
                                        "内置包已解压，但依赖仍未就绪（{}）。请检查网络与镜像后重试「重装依赖」。",
                                        dest.display()
                                    );
                                    let _ = app.emit(
                                        "install-progress",
                                        InstallProgressEvent::failed(&stage, &msg),
                                    );
                                    return Err(msg);
                                }
                                return Ok(format!("插件「{}」已就绪（内置包离线安装）", plugin_id));
                            }
                            Err(e) => {
                                let msg = format!("内置包离线安装后依赖安装失败: {}", e);
                                tracing::warn!("插件 {} {}", plugin_id, msg);
                                let _ = app.emit(
                                    "install-progress",
                                    InstallProgressEvent::failed(&stage, &msg),
                                );
                                return Err(msg);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("内置插件包 {} 解压失败: {}", tgz_path_str, e);
                    }
                }
            }
        } else {
            tracing::info!("插件 {} 无内置 tgz，尝试 npm 安装", plugin_id);
        }

        // ── 其次：npm pack（需要网络）─────────────────────────────────────────
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::started(
                &stage,
                &format!(
                    "正在从 npm 获取 {} 官方插件 {}（内置包仅作离线回退）…",
                    plugin_display_name, npm_name
                ),
            ),
        );
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::detail(
                &stage,
                "正在连接 npm 镜像并下载包（npm pack，可能需要 1～5 分钟），请勿关闭窗口…",
            ),
        );
        let base = data_dir.clone();
        let pkg = npm_name.to_string();
        let dst = dest.clone();
        let stage_clone = stage.clone();
        let npm_res =
            tokio::task::spawn_blocking(move || npm_pack_unpack_blocking(&base, &pkg, &dst))
                .await
                .map_err(|e| format!("npm 安装任务失败: {}", e))?;
        match npm_res {
            Ok(()) => {
                source_label = format!("npm 官方包（{}）", npm_name);
            }
            Err(e) => {
                tracing::warn!(
                    "通道 {}：npm 拉取失败（{}），回退使用内置 extensions/{}",
                    plugin_id,
                    e,
                    ext_folder
                );
                if !bundled_src.is_dir() {
                    let msg = format!(
                        "插件 {} 安装失败: {}。且无本地 extensions/{} 可回退。",
                        plugin_id, e, ext_folder
                    );
                    let _ = app.emit(
                        "install-progress",
                        InstallProgressEvent::failed(&stage_clone, &msg),
                    );
                    return Err(msg);
                }
                source_label = "本地复制（离线回退，建议联网重装）".to_string();
                let bundled_src_for_build = bundled_src.clone();
                let _ = app.emit(
                    "install-progress",
                    InstallProgressEvent::started(
                        &stage_clone,
                        &format!("正在编译插件 {} 的 TypeScript 源码…", plugin_id),
                    ),
                );
                if let Err(e2) = build_ts_extensions_blocking(
                    &data_dir,
                    bundled_src_for_build.to_str().unwrap_or(""),
                ) {
                    tracing::warn!(
                        "插件 {} bundled 源码编译失败（继续复制）: {}",
                        plugin_id,
                        e2
                    );
                }
                let _ = app.emit(
                    "install-progress",
                    InstallProgressEvent::started(
                        &stage_clone,
                        &format!(
                            "从 OpenClaw-CN 复制占位插件「{}」（extensions/{}）…",
                            plugin_id, ext_folder
                        ),
                    ),
                );
                let src = bundled_src.clone();
                let dst2 = dest.clone();
                tokio::task::spawn_blocking(move || copy_dir_all(&src, &dst2))
                    .await
                    .map_err(|e| format!("复制任务失败: {}", e))?
                    .map_err(|e2| {
                        let msg = format!("插件 {} 从本地复制失败: {}", plugin_id, e2);
                        let _ = app.emit(
                            "install-progress",
                            InstallProgressEvent::failed(&stage_clone, &msg),
                        );
                        msg
                    })?;
            }
        }
    } else if bundled_src.is_dir() {
        source_label = "本地复制（无需联网）".to_string();

        // 对 bundled 源码位置编译 TypeScript（生成 dist/），这对于 openclaw.extensions 指向 ./dist/index.js 的插件至关重要
        let bundled_src_for_build = bundled_src.clone();
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::started(
                &stage,
                &format!("正在编译插件 {} 的 TypeScript 源码…", plugin_id),
            ),
        );
        if let Err(e) =
            build_ts_extensions_blocking(&data_dir, bundled_src_for_build.to_str().unwrap_or(""))
        {
            tracing::warn!("插件 {} bundled 源码编译失败（继续复制）: {}", plugin_id, e);
        }

        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::started(
                &stage,
                &format!(
                    "从已安装的 OpenClaw-CN 复制插件「{}」（extensions/{}，无需联网）…",
                    plugin_id, ext_folder
                ),
            ),
        );
        let src = bundled_src.clone();
        let dst = dest.clone();
        tokio::task::spawn_blocking(move || copy_dir_all(&src, &dst))
            .await
            .map_err(|e| format!("复制任务失败: {}", e))?
            .map_err(|e| {
                let msg = format!("插件 {} 从本地复制失败: {}", plugin_id, e);
                let _ = app.emit(
                    "install-progress",
                    InstallProgressEvent::failed(&stage, &msg),
                );
                msg
            })?;
    } else {
        tracing::warn!(
            "插件 {} 不在 npm_first 列表且无 bundled 源码，将尝试 npm pack（ext_folder={}）",
            plugin_id,
            ext_folder
        );
        source_label = format!("npm 注册表（{}）", npm_name);
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::started(
                &stage,
                &format!("正在通过 npm registry 获取 {}（无需 git 登录）…", npm_name),
            ),
        );
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::detail(&stage, "正在执行 npm pack 下载（可能较慢），请稍候…"),
        );
        let base = data_dir.clone();
        let pkg = npm_name.to_string();
        let dst = dest.clone();
        tokio::task::spawn_blocking(move || npm_pack_unpack_blocking(&base, &pkg, &dst))
            .await
            .map_err(|e| format!("npm 安装任务失败: {}", e))?
            .map_err(|e| {
                let msg = format!(
                    "插件 {} 安装失败: {}。若尚未完成向导第二步，请先安装 OpenClaw-CN（本地将包含 extensions）。",
                    plugin_id, e
                );
                let _ = app.emit("install-progress", InstallProgressEvent::failed(&stage, &msg));
                msg
            })?;
    }

    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::started(
            &stage,
            &format!("正在安装插件 {} 的依赖（npm/pnpm install）…", plugin_id),
        ),
    );
    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::detail(&stage, "正在安装 node 依赖（可能较慢），请稍候…"),
    );

    let base = data_dir.clone();
    let cwd = plugin_path.clone();
    let pid = plugin_id.clone();
    let deps_install_err = tokio::task::spawn_blocking(move || install_plugin_deps_blocking(&base, &cwd, &pid, false))
        .await
        .map_err(|e| format!("依赖安装任务失败: {}", e))?;

    // 若通用依赖安装失败，且为飞书插件，尝试兜底安装核心依赖
    if let Err(deps_err) = deps_install_err {
        if plugin_id == "feishu" {
            let fb_err = feishu_node_sdk_fallback_install(&data_dir, &PathBuf::from(&plugin_path));
            if fb_err.is_ok() {
                tracing::warn!(
                    "飞书插件：通用 npm install 失败，但核心依赖 @larksuiteoapi/node-sdk 兜底安装成功"
                );
                let _ = app.emit(
                    "install-progress",
                    InstallProgressEvent::detail(
                        &stage,
                        "飞书：通用依赖安装失败，但核心 SDK 兜底安装成功，继续编译…",
                    ),
                );
            } else {
                let fb_msg = fb_err.unwrap_err();
                let _ = app.emit("install-progress", InstallProgressEvent::failed(&stage, &fb_msg));
                return Err(format!(
                    "飞书插件 npm install 失败（{}），核心 SDK 兜底也失败：{}\n\
                     请检查 config/app.yaml 中 registry: 是否为合法 https:// URL",
                    deps_err, fb_msg
                ));
            }
        } else {
            let _ = app.emit("install-progress", InstallProgressEvent::failed(&stage, &deps_err));
            return Err(format!("插件 {} 依赖安装失败: {}", plugin_id, deps_err));
        }
    }

    // 编译 TypeScript 扩展（生成 dist/，对 feishu 等插件至关重要）
    let base2 = data_dir.clone();
    let cwd2 = plugin_path.clone();
    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::started(&stage, "正在编译 TypeScript 扩展…"),
    );
    if let Err(e) = tokio::task::spawn_blocking(move || build_ts_extensions_blocking(&base2, &cwd2))
        .await
        .map_err(|e| format!("编译任务失败: {}", e))
        .and_then(|r| r)
    {
        // 编译失败不阻塞安装，但记录 warn
        let msg = format!(
            "插件 {} TypeScript 编译失败（插件仍可使用旧版 dist，但建议修复）: {}",
            plugin_id, e
        );
        let _ = app.emit(
            "install-progress",
            InstallProgressEvent::failed(&stage, &msg),
        );
        tracing::warn!("{}", msg);
    }

    // 将 data/plugins 目录写入 openclaw.json 的 plugins.load.paths，
    // 使网关能发现已安装的插件（bundled/global 之外的用户安装插件）
    if let Err(e) = sync_plugins_load_paths(&data_dir).await {
        tracing::warn!("同步 plugins.load.paths 失败（非致命）: {}", e);
    }

    // 完整同步：插件启用状态、飞书路由、skill 清理等，使插件立即生效（下次网关启动时）
    if let Err(e) =
        crate::commands::gateway::sync_openclaw_config_from_manager(&data_dir).await
    {
        tracing::warn!("同步网关配置失败（非致命）: {}", e);
    }

    let _ = app.emit(
        "install-progress",
        InstallProgressEvent::finished(
            &stage,
            &format!("插件 {} 安装成功（来源：{}）", plugin_id, source_label),
        ),
    );
    info!("插件 {} 安装完成", plugin_id);
    Ok(format!("插件 {} 安装成功", plugin_id))
}

// 卸载插件
#[tauri::command]
pub async fn uninstall_plugin(
    data_dir: tauri::State<'_, crate::AppState>,
    plugin_id: String,
) -> Result<String, String> {
    info!("卸载插件: {}", plugin_id);

    // Validate plugin_id against path traversal
    if plugin_id.contains("..") || plugin_id.contains('/') || plugin_id.contains('\\') {
        return Err("非法的插件ID".to_string());
    }

    let data_dir = data_dir.inner().get_data_dir();
    let plugin_path = format!("{}/plugins/{}", data_dir, plugin_id);

    if !std::path::Path::new(&plugin_path).exists() {
        return Err(format!("插件 {} 不存在", plugin_id));
    }

    tokio::fs::remove_dir_all(&plugin_path)
        .await
        .map_err(|e| format!("删除插件目录失败: {}", e))?;

    // 完整同步：移除插件条目、清理路由、skill 目录，使卸载立即生效
    if let Err(e) =
        crate::commands::gateway::sync_openclaw_config_from_manager(&data_dir).await
    {
        tracing::warn!("同步网关配置失败（非致命）: {}", e);
    }

    info!("插件 {} 卸载完成", plugin_id);
    Ok(format!("插件 {} 卸载成功", plugin_id))
}
