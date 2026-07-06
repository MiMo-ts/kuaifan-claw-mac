// 网关控制命令 — 启动真实的 openclaw-cn `gateway` 子命令，并把管理端配置写入安装目录下的 openclaw.json

use crate::commands::hidden_cmd;
use crate::commands::installer::reap_zombie_children;
use crate::commands::log::OPENCLAW_GATEWAY_LOG;
use crate::commands::robot::get_robot_system_prompt;
use crate::env_paths::{resolve_node, resolve_git};
use crate::models::GatewayStatus;
use crate::services::cipher::{decrypt_credential, CIPHER_PREFIX};
use serde_json::json;
use std::collections::HashSet;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};
use tracing::{info, warn};

/// 与 OpenClaw-CN 包内默认网关端口一致（app.yaml 未写 port 时的回退）
const FALLBACK_GATEWAY_PORT: u16 = 18789;

// ── 集中式通道元数据中心 ────────────────────────────────────────────────────────
// 描述每个 channel_type（管理端 YAML 里存的）与 OpenClaw 插件之间的映射关系。
//
// 重要字段：
//   openclaw_channel_id  — openclaw.json 中 channels.{id} 使用的键，也用于 bindings.match.channel
//   single_account      — true 表示插件只用根级凭证（channels.xxx.{appId/secret}），无需 accounts 嵌套
//   plugin_enabled_key   — plugins.entries.{key}.enabled 需要写入哪个插件条目
//   field_aliases       — YAML channel_config 中的字段名 → OpenClaw 期望的字段名
//
// 所有硬编码的通道判断都必须通过这个表驱动；新增插件时只需在这里注册。

/// 与 openclaw-cn dist/routing/session-key.js 中 normalizeAccountId 对齐，生成稳定账号 key
fn normalize_account_id(id: &str) -> String {
    id.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

/// 根据 channel_type 查元数据（未知类型返回 None）
fn get_channel_meta(yaml_channel: &str) -> Option<&'static ChannelMeta<'static>> {
    CHANNEL_META
        .iter()
        .find(|(k, _)| *k == yaml_channel)
        .map(|(_, m)| m)
}

/// 通道元数据
struct ChannelMeta<'a> {
    openclaw_channel_id: &'a str,
    single_account: bool,
    plugin_enabled_key: Option<&'a str>,
    /// YAML channel_config 中的字段名 → OpenClaw 期望的字段名（别名映射）
    field_aliases: &'a [(&'a str, &'a str)],
    yaml_channel_to_account_id: fn(&str) -> String,
}

const CHANNEL_META: &[(&str, ChannelMeta)] = &[
    // 飞书：内置通道；多账号，凭证在 channels.feishu.accounts.{id}
    (
        "feishu",
        ChannelMeta {
            openclaw_channel_id: "feishu",
            single_account: false,
            plugin_enabled_key: None,
            field_aliases: &[
                ("appId", "appId"),
                ("appSecret", "appSecret"),
                ("verificationToken", "verificationToken"),
                ("encryptKey", "encryptKey"),
                ("allowFrom", "allowFrom"),
                ("groupAllowFrom", "groupAllowFrom"),
                ("dmPolicy", "dmPolicy"),
                ("groupPolicy", "groupPolicy"),
            ],
            yaml_channel_to_account_id: |instance_id| normalize_account_id(instance_id),
        },
    ),
    // 企业微信：通道 id wecom；bundled stub manifest id 为 "wecom"（与 channel id 同），npm 官方包为 "wecom-openclaw-plugin"
    (
        "wxwork",
        ChannelMeta {
            openclaw_channel_id: "wecom",
            single_account: true,
            plugin_enabled_key: Some("wecom-openclaw-plugin"),
            field_aliases: &[
                ("botId", "botId"),
                ("agentId", "botId"), // 向导里填的是 agentId → 映射到 botId
                ("secret", "secret"),
                ("corpSecret", "secret"), // 旧 YAML
            ],
            yaml_channel_to_account_id: |_| "default".to_string(),
        },
    ),
    // 微信 ClawBot：插件 id openclaw-weixin；单账号（账号在 weixin auth 系统里）
    (
        "wechat_clawbot",
        ChannelMeta {
            openclaw_channel_id: "openclaw-weixin",
            single_account: true,
            plugin_enabled_key: Some("openclaw-weixin"),
            field_aliases: &[],
            yaml_channel_to_account_id: |_| "default".to_string(),
        },
    ),
    // QQ：插件 id qqbot；单账号，根级凭证；支持 token="AppID:Secret" 拼接格式
    (
        "qq",
        ChannelMeta {
            openclaw_channel_id: "qqbot",
            single_account: true,
            plugin_enabled_key: Some("qqbot"),
            field_aliases: &[
                ("appId", "appId"),
                ("clientSecret", "clientSecret"),
                ("appSecret", "clientSecret"),
                ("token", "token"), // CLI 风格 AppID:Secret
            ],
            yaml_channel_to_account_id: |_| "default".to_string(),
        },
    ),
    ];

/// 首次安装时生成随机 token（首次写入 app.yaml 后不再使用此值）
fn generate_secure_token() -> String {
    use uuid::Uuid;
    Uuid::new_v4().to_string().replace("-", "")
}

fn app_yaml_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("config").join("app.yaml")
}

fn openclaw_json_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir)
        .join("openclaw-cn")
        .join("openclaw.json")
}

fn instances_yaml_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir)
        .join("config")
        .join("instances.yaml")
}

/// 所有实例（含停用）的 `robot_id`，用于判断 `robots/` 下子目录是否仍被引用。
fn read_all_robot_ids_from_instances_yaml(data_dir: &str) -> HashSet<String> {
    let path = instances_yaml_path(data_dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };
    #[derive(serde::Deserialize)]
    struct InstRow {
        robot_id: String,
    }
    #[derive(serde::Deserialize)]
    struct Doc {
        #[serde(default)]
        instances: Vec<InstRow>,
    }
    let doc: Doc = match serde_yaml::from_str(&content) {
        Ok(d) => d,
        Err(_) => return HashSet::new(),
    };
    doc.instances.into_iter().map(|i| i.robot_id).collect()
}

/// 旧版曾把 `agent_id` 当作 workspace 路径段，在 `robots/` 下生成 `inst_*`、`wecom-inst_*` 等目录；
/// 与 YAML 里的 `robot_id`（通常为 `robot_*`）无关，实例删除后仍会残留。
fn is_legacy_wrong_robots_subdir_name(name: &str) -> bool {
    if name.starts_with("inst_") {
        return name["inst_".len()..].chars().all(|c| c.is_ascii_digit());
    }
        for (_, meta) in CHANNEL_META {
        if meta.single_account {
            let p = format!("{}-inst_", meta.openclaw_channel_id);
            if name.starts_with(&p) {
                return true;
            }
        }
    }
    false
}

/// 删除 `robots/` 下已不再作为任何实例 `robot_id` 的旧版误生成目录。
/// `.../robots/{robot_id}/skills` 形式的 extraDir → 返回 robot_id；其它路径返回 None（保留不删）。
fn extra_dir_robot_id(path: &str) -> Option<String> {
    let n = path.replace('\\', "/");
    let key = "/robots/";
    let i = n.find(key)?;
    let after = &n[i + key.len()..];
    let parts: Vec<&str> = after.split('/').collect();
    if parts.len() >= 2 && parts[1] == "skills" && !parts[0].is_empty() {
        return Some(parts[0].to_string());
    }
    None
}

/// 从 `skills.load.extraDirs` 去掉未被任何实例 `robot_id` 引用的机器人技能目录，避免「一个实例却加载多个机器人技能」。
fn prune_stale_skills_extra_dirs(base: &mut serde_json::Value, data_dir: &str) {
    let keep = read_all_robot_ids_from_instances_yaml(data_dir);
    let Some(extra) = base
        .get_mut("skills")
        .and_then(|s| s.get_mut("load"))
        .and_then(|l| l.get_mut("extraDirs"))
        .and_then(|e| e.as_array_mut())
    else {
        return;
    };
    extra.retain(|v| {
        let Some(s) = v.as_str() else {
            return true;
        };
        match extra_dir_robot_id(s) {
            Some(rid) => {
                let ok = keep.contains(&rid);
                if !ok {
                    info!(
                        "已从 skills.extraDirs 移除未被实例引用的目录: {} (robot_id={})",
                        s, rid
                    );
                }
                ok
            }
            None => true,
        }
    });
}

async fn cleanup_legacy_wrong_robot_workspace_dirs(data_dir: &str) {
    let keep = read_all_robot_ids_from_instances_yaml(data_dir);
    let root = PathBuf::from(data_dir).join("robots");
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = ent.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if keep.contains(&name) {
            continue;
        }
        if !is_legacy_wrong_robots_subdir_name(&name) {
            continue;
        }
        match tokio::fs::remove_dir_all(&path).await {
            Ok(_) => info!("已清理旧版误生成的 robots 目录: {}", path.display()),
            Err(e) => tracing::warn!("清理旧版 robots 目录失败 {}: {}", path.display(), e),
        }
    }
}

pub(crate) fn models_yaml_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("config").join("models.yaml")
}

/// 读取用户可能用记事本另存为 UTF-16 的 YAML（`read_to_string` 会直接失败）。
pub(crate) fn read_models_yaml_raw_utf8_or_utf16(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xFF, 0xFE]) && bytes.len() >= 2 {
        let slice = &bytes[2..];
        if slice.len() % 2 != 0 {
            return None;
        }
        let u16s: Vec<u16> = slice
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&u16s).ok();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) && bytes.len() >= 2 {
        let slice = &bytes[2..];
        if slice.len() % 2 != 0 {
            return None;
        }
        let u16s: Vec<u16> = slice
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&u16s).ok();
    }
    String::from_utf8(bytes).ok()
}

fn yaml_default_model_scalar_string(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => None,
        _ => v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    }
}

/// 从 instances.yaml 读取已启用实例，按平台分组构建 channels.{platform}.accounts 片段（JSON）。
/// 同时收集 { accountKey -> (instance_id, robot_id, instance_name, model, channel_type) } 供路由用。
/// 返回 (channels_patch_json, manager_accounts: Vec<ManagerAccount>)
fn read_channel_patches(data_dir: &str) -> (Option<serde_json::Value>, Vec<ManagerAccount>) {
    let path = instances_yaml_path(data_dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (None, vec![]),
    };

    #[derive(serde::Deserialize)]
    struct InstDoc {
        instances: Vec<InstEntry>,
    }
    #[derive(serde::Deserialize)]
    struct InstEntry {
        id: String,
        enabled: bool,
        name: String,
        robot_id: String,
        channel_type: String,
        #[serde(default)]
        channel_config: serde_json::Value,
        model: Option<serde_json::Value>,
        #[serde(default)]
        created_at: Option<String>,
    }

    let doc: InstDoc = match serde_yaml::from_str(&content) {
        Ok(d) => d,
        Err(_) => return (None, vec![]),
    };

    // 过滤出所有已启用的实例，并按创建时间排序。
    // 排序确保"首个实例"始终是最早创建的，而不是依赖 YAML 数组顺序（避免手动调整 YAML 导致不同实例变成 main）。
    let mut enabled_instances: Vec<_> = doc.instances.into_iter().filter(|i| i.enabled).collect();
    enabled_instances.sort_by(|a, b| {
        a.created_at
            .as_deref()
            .unwrap_or("")
            .cmp(b.created_at.as_deref().unwrap_or(""))
    });

    if enabled_instances.is_empty() {
        return (None, vec![]);
    }

    // 从 CHANNEL_META 收集所有已知通道 id，用于未知类型警告
    let known_channel_ids: std::collections::HashSet<&str> =
        CHANNEL_META.iter().map(|(k, _)| *k).collect();

    for inst in &enabled_instances {
        if !known_channel_ids.contains(inst.channel_type.as_str()) {
            tracing::warn!(
                "未知 channel_type \"{}\"（实例 {}），将跳过 agent 绑定",
                inst.channel_type,
                inst.name
            );
        }
    }

    // 按 channel_type 分组构建各平台的 accounts map
    let mut channels_obj = serde_json::Map::new();
    let mut manager_accounts = Vec::new();

    for inst in enabled_instances {
        let yaml_channel = inst.channel_type.as_str();
        let openclaw_channel = yaml_channel_to_openclaw_channel_id(yaml_channel);
        let account_id = channel_account_id(yaml_channel, &inst.id);
        let cc = &inst.channel_config;

        // 构建平台特定的凭证配置
        let acct = build_channel_account_config(yaml_channel, cc);

        let model_ref = inst.model.as_ref().and_then(|m| {
            let provider = m.get("provider")?.as_str()?;
            let name = m.get("model_name")?.as_str()?;
            Some(format!(
                "{}/{}",
                map_yaml_provider_to_openclaw_id(provider),
                name
            ))
        });

        // 根据 CHANNEL_META 判断凭证写入方式：
        // - single_account=true：插件只用根级凭证（channels.xxx.appId），不用 accounts 嵌套
        // - single_account=false：凭证放在 channels.xxx.accounts.{accountId}
        let (acct_for_binding_account_id, _ch_key) = if let Some(meta) =
            get_channel_meta(yaml_channel)
        {
            if meta.single_account {
                // 凭证展开写入 channels.{openclaw_channel} 根级
                let ch_entry = channels_obj
                    .entry(openclaw_channel.to_string())
                    .or_insert_with(|| json!({ "enabled": true }));
                if let Some(obj) = ch_entry.as_object_mut() {
                    for (k, v) in acct.clone() {
                        obj.insert(k, v);
                    }
                    obj.insert("enabled".to_string(), json!(true));
                }
                ("default".to_string(), openclaw_channel.to_string())
            } else {
                // 凭证嵌套在 accounts 下
                let ch_entry = channels_obj
                    .entry(openclaw_channel.to_string())
                    .or_insert_with(|| {
                        let mut m = serde_json::Map::new();
                        m.insert("enabled".to_string(), json!(true));
                        m.insert("accounts".to_string(), json!(serde_json::Map::new()));
                        serde_json::Value::Object(m)
                    });
                if let Some(obj) = ch_entry.as_object_mut() {
                    if let Some(acc_obj) = obj.get_mut("accounts").and_then(|a| a.as_object_mut()) {
                        acc_obj.insert(account_id.clone(), json!(acct));
                    }
                }
                (account_id.clone(), openclaw_channel.to_string())
            }
        } else {
            // 未知通道类型：尝试多账号模式
            let ch_entry = channels_obj
                .entry(openclaw_channel.to_string())
                .or_insert_with(|| {
                    let mut m = serde_json::Map::new();
                    m.insert("enabled".to_string(), json!(true));
                    m.insert("accounts".to_string(), json!(serde_json::Map::new()));
                    serde_json::Value::Object(m)
                });
            if let Some(obj) = ch_entry.as_object_mut() {
                if let Some(acc_obj) = obj.get_mut("accounts").and_then(|a| a.as_object_mut()) {
                    acc_obj.insert(account_id.clone(), json!(acct));
                }
            }
            (account_id.clone(), openclaw_channel.to_string())
        };

        // 单账号通道（QQ / 企微 / 微信 / 钉钉）插件层 accountId 固定为 "default"，但每个管理端实例
        // 必须独占 agents.list 中一条，否则 agent_id 会全部是 "default"，路由全部落到首个实例工作区。
        let agent_id = if get_channel_meta(yaml_channel)
            .map(|m| m.single_account)
            .unwrap_or(false)
        {
            normalize_account_id(&format!("{}-{}", openclaw_channel, inst.id))
        } else {
            account_id.clone()
        };

        manager_accounts.push(ManagerAccount {
            account_id: acct_for_binding_account_id,
            agent_id,
            instance_id: inst.id,
            instance_name: inst.name,
            robot_id: inst.robot_id,
            model_ref,
            channel_type: openclaw_channel.to_string(),
        });
    }

    if manager_accounts.is_empty() {
        return (None, vec![]);
    }

    let patch = json!({ "channels": serde_json::Value::Object(channels_obj) });
    (Some(patch), manager_accounts)
}

/// 管理端 `channel_type` 与 OpenClaw 插件通道 id 对齐。
fn yaml_channel_to_openclaw_channel_id(yaml_channel: &str) -> &str {
    get_channel_meta(yaml_channel)
        .map(|m| m.openclaw_channel_id)
        .unwrap_or(yaml_channel)
}

/// 根据 channel_type 和 instance_id 生成稳定的 accountId
fn channel_account_id(channel_type: &str, instance_id: &str) -> String {
    get_channel_meta(channel_type)
        .map(|m| (m.yaml_channel_to_account_id)(instance_id))
        .unwrap_or_else(|| normalize_account_id(&format!("{}-{}", channel_type, instance_id)))
}

#[cfg(test)]
mod yaml_channel_mapping_tests {
    use super::yaml_channel_to_openclaw_channel_id;

    #[test]
    fn test_qq_maps_to_qqbot() {
        assert_eq!(yaml_channel_to_openclaw_channel_id("qq"), "qqbot");
    }

    #[test]
    fn test_wechat_clawbot_maps_to_openclaw_weixin() {
        assert_eq!(
            yaml_channel_to_openclaw_channel_id("wechat_clawbot"),
            "openclaw-weixin"
        );
    }

    #[test]
    fn test_feishu_passes_through() {
        assert_eq!(yaml_channel_to_openclaw_channel_id("feishu"), "feishu");
    }

    #[test]
    fn test_qq_passes_through() {
        assert_eq!(yaml_channel_to_openclaw_channel_id("qq"), "qqbot");
    }

    #[test]
    fn test_wxwork_maps_to_wecom() {
        assert_eq!(yaml_channel_to_openclaw_channel_id("wxwork"), "wecom");
    }
}

#[cfg(test)]
mod plugin_entries_normalize_tests {
    use super::normalize_plugin_entries_keys;
    use serde_json::json;

    #[test]
    fn merges_wecom_connector_into_wecom() {
        let mut base = json!({
            "plugins": {
                "entries": {
                    "wecom-connector": { "enabled": true },
                    "feishu": { "enabled": true }
                }
            }
        });
        normalize_plugin_entries_keys(&mut base);
        let e = base["plugins"]["entries"].as_object().unwrap();
        assert!(!e.contains_key("wecom-connector"));
        assert_eq!(e.get("wecom").unwrap()["enabled"], true);
        assert_eq!(e.get("feishu").unwrap()["enabled"], true);
    }

    #[test]
    fn merges_wecom_openclaw_plugin_into_wecom() {
        let mut base = json!({
            "plugins": {
                "entries": {
                    "wecom-openclaw-plugin": { "enabled": true },
                    "feishu": { "enabled": true }
                }
            }
        });
        normalize_plugin_entries_keys(&mut base);
        let e = base["plugins"]["entries"].as_object().unwrap();
        assert!(!e.contains_key("wecom-openclaw-plugin"));
        assert_eq!(e.get("wecom").unwrap()["enabled"], true);
        assert_eq!(e.get("feishu").unwrap()["enabled"], true);
    }

    #[test]
    fn merges_qq_into_qqbot() {
        let mut base = json!({
            "plugins": {
                "entries": {
                    "qq": { "enabled": true }
                }
            }
        });
        normalize_plugin_entries_keys(&mut base);
        let e = base["plugins"]["entries"].as_object().unwrap();
        assert!(!e.contains_key("qq"));
        assert_eq!(e.get("qqbot").unwrap()["enabled"], true);
    }
}

#[cfg(test)]
mod manager_agent_workspace_tests {
    use super::*;

    #[test]
    fn robot_workspace_rejects_path_traversal() {
        assert!(robot_workspace_path("D:/ORD/data", "x/../y").is_none());
        assert!(robot_workspace_path("D:/ORD/data", "a/b").is_none());
        let w = robot_workspace_path("D:/ORD/data", "robot_001").unwrap();
        assert!(w.replace('\\', "/").ends_with("robots/robot_001"));
    }

    #[test]
    fn orphan_detects_old_wecom_inst() {
        let mut cur = HashSet::new();
        cur.insert("wecom-inst_2".to_string());
        assert!(is_orphan_manager_agent_id("wecom-inst_1", &cur));
        assert!(!is_orphan_manager_agent_id("wecom-inst_2", &cur));
    }

    #[test]
    fn orphan_detects_old_feishu_inst_digits() {
        let mut cur = HashSet::new();
        cur.insert("inst_2".to_string());
        assert!(is_orphan_manager_agent_id("inst_1", &cur));
        assert!(!is_orphan_manager_agent_id("inst_2", &cur));
        assert!(!is_orphan_manager_agent_id("custom_bot", &cur));
    }

    #[test]
    fn extra_dir_robot_id_parses_windows_and_unix() {
        assert_eq!(
            super::extra_dir_robot_id(r"D:\ORD\data\robots\robot_stock_002\skills"),
            Some("robot_stock_002".to_string())
        );
        assert_eq!(
            super::extra_dir_robot_id("D:/ORD/data/robots/robot_stock_001/skills"),
            Some("robot_stock_001".to_string())
        );
        assert_eq!(super::extra_dir_robot_id("/custom/skills/foo"), None);
    }
}

/// 根据 channel_config 字段名 → OpenClaw 期望的字段名，应用别名映射并填入 acct。
/// 只填第一个匹配的别名；已存在的 key 不会被覆盖（别名列表靠前的优先）。
fn apply_field_aliases(
    cc: &serde_json::Value,
    aliases: &[(&str, &str)],
    acct: &mut serde_json::Map<String, serde_json::Value>,
) {
    // 先复制已有 key，避免在遍历 keys() 时同时修改 acct
    let existing_keys: Vec<String> = acct.keys().cloned().collect();
    let mut seen_keys: std::collections::HashSet<&str> =
        existing_keys.iter().map(|s| s.as_str()).collect();
    for (yaml_key, oc_key) in aliases {
        if seen_keys.contains(oc_key) {
            continue;
        }
        if let Some(v) = cc
            .get(*yaml_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            acct.insert(oc_key.to_string(), json!(v));
            seen_keys.insert(oc_key);
        }
    }
}

/// 根据平台类型，从 channel_config 中提取凭证字段，构建 OpenClaw 所需的账号配置对象。
/// 凭证字段映射规则集中在 CHANNEL_META 中。
fn build_channel_account_config(
    channel_type: &str,
    cc: &serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let mut acct = serde_json::Map::new();

    match channel_type {
        // 飞书：特殊处理 allowFrom/groupAllowFrom 多行字符串转数组
        "feishu" => {
            apply_field_aliases(
                cc,
                &[
                    ("appId", "appId"),
                    ("appSecret", "appSecret"),
                    ("verificationToken", "verificationToken"),
                    ("encryptKey", "encryptKey"),
                    ("dmPolicy", "dmPolicy"),
                    ("groupPolicy", "groupPolicy"),
                ],
                &mut acct,
            );
            if let Some(v) = cc
                .get("allowFrom")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let arr: Vec<_> = v.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
                if !arr.is_empty() {
                    acct.insert("allowFrom".to_string(), json!(arr));
                }
            }
            if let Some(v) = cc
                .get("groupAllowFrom")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let arr: Vec<_> = v.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
                if !arr.is_empty() {
                    acct.insert("groupAllowFrom".to_string(), json!(arr));
                }
            }
        }
        // QQ：先应用字段别名，再处理 token="AppID:Secret" 拼接格式（缺失字段时从 token 拆分）
        "qq" => {
            apply_field_aliases(
                cc,
                &[
                    ("appId", "appId"),
                    ("clientSecret", "clientSecret"),
                    ("appSecret", "clientSecret"),
                ],
                &mut acct,
            );
            if !acct.contains_key("appId") || !acct.contains_key("clientSecret") {
                if let Some(token) = cc
                    .get("token")
                    .and_then(|v| v.as_str())
                    .filter(|s| s.contains(':'))
                {
                    let parts: Vec<&str> = token.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        if !acct.contains_key("appId") {
                            acct.insert("appId".to_string(), json!(parts[0]));
                        }
                        if !acct.contains_key("clientSecret") {
                            acct.insert("clientSecret".to_string(), json!(parts[1]));
                        }
                    }
                }
            }
        }
        // 其他已知通道：使用 CHANNEL_META 的别名表
        _ => {
            if let Some(meta) = get_channel_meta(channel_type) {
                apply_field_aliases(cc, meta.field_aliases, &mut acct);
            } else {
                // 未知通道：原样复制非空字符串字段
                if let serde_json::Value::Object(obj) = cc {
                    for (k, v) in obj {
                        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                            acct.insert(k.clone(), json!(s));
                        }
                    }
                }
            }
        }
    }

    acct
}

#[derive(Clone)]
pub struct ManagerAccount {
    /// OpenClaw 路由匹配用的 accountId（单账号通道插件固定为 "default"）。
    pub account_id: String,
    /// agents.list 中的 id，每个已启用实例唯一；单账号通道下不等于 account_id。
    pub agent_id: String,
    #[allow(dead_code)]
    pub instance_id: String,
    pub instance_name: String,
    #[allow(dead_code)]
    pub robot_id: String,
    pub model_ref: Option<String>,
    pub channel_type: String,
}

/// 兼容性别名（代码中其他地方可能引用了 ManagerFeishuAccount）
#[allow(dead_code)]
pub type ManagerFeishuAccount = ManagerAccount;

/// Agent 工作区目录：`data/robots/{robot_id}`（与向导里选的机器人模板一致）。
/// 不得使用 `agent_id`（飞书多为 `inst_*`，单账号通道为 `wecom-inst_*` 等），否则会指到空目录导致无法回复。
fn robot_workspace_path(data_dir: &str, robot_id: &str) -> Option<String> {
    let rid = robot_id.trim();
    if rid.is_empty() || rid.contains("..") || rid.contains('/') || rid.contains('\\') {
        return None;
    }
    Some(
        PathBuf::from(data_dir)
            .join("robots")
            .join(rid)
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn agent_workspace_for_manager_account(data_dir: &str, acct: &ManagerAccount) -> String {
    robot_workspace_path(data_dir, &acct.robot_id).unwrap_or_else(|| {
        PathBuf::from(data_dir)
            .join("robots")
            .join(&acct.agent_id)
            .to_string_lossy()
            .replace('\\', "/")
    })
}

/// OpenClaw `agents.list[]` 为 strict schema，不接受顶层 `systemPrompt`（人设应放在 workspace 的 SOUL.md）。
fn strip_invalid_keys_from_agents_list(base: &mut serde_json::Value) {
    let Some(list) = base
        .get_mut("agents")
        .and_then(|a| a.get_mut("list"))
        .and_then(|l| l.as_array_mut())
    else {
        return;
    };
    for entry in list.iter_mut() {
        if let serde_json::Value::Object(obj) = entry {
            obj.remove("systemPrompt");
        }
    }
}

/// 同步管理端实例的人设到机器人工作区（与 `robot.rs` 中 SOUL.md 约定一致）。
fn sync_agent_workspace_soul(data_dir: &str, acct: &ManagerAccount) {
    let rel_ws = agent_workspace_for_manager_account(data_dir, acct);
    if rel_ws.contains("..") {
        return;
    }
    let dir = PathBuf::from(data_dir).join(Path::new(&rel_ws));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("创建 agent workspace 目录失败 {:?}: {}", dir, e);
        return;
    }
    let text = get_robot_system_prompt(&acct.robot_id);
    let soul_path = dir.join("SOUL.md");
    if let Err(e) = std::fs::write(&soul_path, text) {
        warn!("写入 SOUL.md 失败 {:?}: {}", soul_path, e);
    }
}

/// 已删除实例遗留的 agents.list 条目（仍在 JSON 里但不在本次 YAML 中）。
fn is_orphan_manager_agent_id(id: &str, current_agent_ids: &HashSet<String>) -> bool {
    if current_agent_ids.contains(id) {
        return false;
    }
    for (_, meta) in CHANNEL_META {
        if meta.single_account {
            let prefix = format!("{}-inst_", meta.openclaw_channel_id);
            if id.starts_with(&prefix) {
                return true;
            }
        }
    }
    // 飞书等：实例 id 常为 inst_<digits>
    if id.starts_with("inst_")
        && id
            .strip_prefix("inst_")
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    false
}

/// 同步所有已启用通道的凭证、agents.list 与 bindings 到 openclaw.json。
/// - 从 instances.yaml 读取所有已启用实例，按平台构建 channels.{type}.accounts；
/// - 移除与 `channels.*.accounts` 键或当前实例 account_id 对应的旧 agents/bindings，再追加新 entries。
/// - 兼容：仍删除 `inst-` 前缀的旧 agents / 以 `inst-` 为 accountId 的 bindings（历史双前缀账号）。
/// - 若当前无实例使用 "main" agent，清理孤立的 "main" entry。
fn sync_feishu_channel_and_routing(data_dir: &str, base: &mut serde_json::Value) {
    let (channels_patch, manager_accounts) = read_channel_patches(data_dir);
    strip_invalid_keys_from_agents_list(base);

    let current_agent_ids: HashSet<String> = manager_accounts
        .iter()
        .map(|a| a.agent_id.clone())
        .collect();

    // 合并「当前 openclaw.json 里各通道账号键」与「本次 YAML 中的 account_id」，用于识别管理端写入的 agents/bindings
    let mut managed_account_keys: HashSet<String> = HashSet::new();
    for a in &manager_accounts {
        managed_account_keys.insert(a.account_id.clone());
        managed_account_keys.insert(a.agent_id.clone());
    }
    if let Some(ch_map) = base.get("channels").and_then(|c| c.as_object()) {
        for (_ch, ch_val) in ch_map {
            if let Some(accs) = ch_val.get("accounts").and_then(|a| a.as_object()) {
                for k in accs.keys() {
                    managed_account_keys.insert(k.clone());
                }
            }
        }
    }

    if let Some(agents_list) = base
        .get_mut("agents")
        .and_then(|a| a.get_mut("list"))
        .and_then(|l| l.as_array_mut())
    {
        agents_list.retain(|entry| {
            let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
                return true;
            };
            if id.starts_with("inst-") {
                return false;
            }
            if managed_account_keys.contains(id) {
                return false;
            }
            if id == "main" && !current_agent_ids.contains("main") {
                return false;
            }
            if is_orphan_manager_agent_id(id, &current_agent_ids) {
                return false;
            }
            true
        });
    }

    if let Some(bindings_arr) = base.get_mut("bindings").and_then(|b| b.as_array_mut()) {
        bindings_arr.retain(|entry| {
            let acc = entry
                .get("match")
                .and_then(|m| m.get("accountId"))
                .and_then(|v| v.as_str());
            if let Some(a) = acc {
                if a.starts_with("inst-") {
                    return false;
                }
                if managed_account_keys.contains(a) {
                    return false;
                }
            }
            let agent_id = entry.get("agentId").and_then(|v| v.as_str());
            if let Some(aid) = agent_id {
                if is_orphan_manager_agent_id(aid, &current_agent_ids) {
                    return false;
                }
            }
            true
        });
    }

    // 追加新的 agents.list 与 bindings
    // 直接 get_mut 修改数组，避免 merge_json_deep 替换而非追加的问题。
    // 每个实例 agent_id 唯一，与 account_id 一致。
    // 人设写入各 workspace 的 SOUL.md（OpenClaw 从文件读取），勿写入 agents.list 非法键。
    if let Some(existing_agents) = base
        .get_mut("agents")
        .and_then(|a| a.get_mut("list"))
        .and_then(|l| l.as_array_mut())
    {
        let mut seen_agent_ids: HashSet<String> = HashSet::new();
        for acct in &manager_accounts {
            if !seen_agent_ids.insert(acct.agent_id.clone()) {
                continue; // 已写过此 agent_id，跳过
            }
            let workspace = agent_workspace_for_manager_account(data_dir, acct);
            let mut agent = serde_json::Map::new();
            agent.insert("id".to_string(), json!(&acct.agent_id));
            agent.insert("name".to_string(), json!(&acct.instance_name));
            agent.insert("workspace".to_string(), json!(&workspace));
            if let Some(ref mr) = acct.model_ref {
                agent.insert("model".to_string(), json!({ "primary": mr }));
            }
            existing_agents.push(json!(agent));
        }
    } else {
        let mut seen_agent_ids: HashSet<String> = HashSet::new();
        let mut new_agents = Vec::new();
        for acct in &manager_accounts {
            if !seen_agent_ids.insert(acct.agent_id.clone()) {
                continue;
            }
            let workspace = agent_workspace_for_manager_account(data_dir, acct);
            let mut agent = serde_json::Map::new();
            agent.insert("id".to_string(), json!(&acct.agent_id));
            agent.insert("name".to_string(), json!(&acct.instance_name));
            agent.insert("workspace".to_string(), json!(&workspace));
            if let Some(ref mr) = acct.model_ref {
                agent.insert("model".to_string(), json!({ "primary": mr }));
            }
            new_agents.push(json!(agent));
        }
        let mut agents_obj = serde_json::Map::new();
        agents_obj.insert("list".to_string(), serde_json::Value::Array(new_agents));
        let mut wrapper = serde_json::Map::new();
        wrapper.insert("agents".to_string(), serde_json::Value::Object(agents_obj));
        merge_json_deep(base, json!(wrapper));
    }

    if let Some(existing_bindings) = base.get_mut("bindings").and_then(|b| b.as_array_mut()) {
        for acct in &manager_accounts {
            existing_bindings.push(json!({
                "agentId": &acct.agent_id,
                "match": {
                    "channel": &acct.channel_type,
                    "accountId": &acct.account_id
                }
            }));
        }
    } else {
        let new_bindings: Vec<_> = manager_accounts
            .iter()
            .map(|acct| {
                json!({
                    "agentId": &acct.agent_id,
                    "match": {
                        "channel": &acct.channel_type,
                        "accountId": &acct.account_id
                    }
                })
            })
            .collect();
        let mut wrapper = serde_json::Map::new();
        wrapper.insert(
            "bindings".to_string(),
            serde_json::Value::Array(new_bindings),
        );
        merge_json_deep(base, json!(wrapper));
    }

    // 写入 channels：必须整表替换 accounts，并修剪已删除的 inst- 账号键
    let valid_ids: HashSet<String> = manager_accounts
        .iter()
        .map(|a| a.account_id.clone())
        .collect();
    if let Some(patch) = channels_patch {
        merge_channels_patch_replace_accounts(base, patch);
    }
    prune_stale_manager_channel_account_keys(base, &valid_ids);
    // 历史错误：曾把 YAML 的 wechat_clawbot 直接写入 channels，OpenClaw 只识别 openclaw-weixin，会导致网关启动失败
    remove_legacy_invalid_channel_entries(base);

    for acct in &manager_accounts {
        sync_agent_workspace_soul(data_dir, acct);
    }
}

/// 删除 openclaw.json 中无效的遗留通道键（与插件注册的 channel id 不一致）。
/// 例如：曾把 YAML channel_type 直接作为 channels 键写过，插件实际注册的 id 可能不同。
fn remove_legacy_invalid_channel_entries(base: &mut serde_json::Value) {
    let Some(channels) = base.get_mut("channels").and_then(|c| c.as_object_mut()) else {
        return;
    };
    // 历史上把 channel_type 直接作为 channels 键写过，需清理这些旧键
    channels.remove("wechat_clawbot");
    channels.remove("qq");
    channels.remove("wxwork");

    // 单账号插件（additionalProperties: false）若误留 accounts 字段会校验失败
    // 遍历 CHANNEL_META 中所有 single_account=true 的通道，统一清理
    for (_, meta) in CHANNEL_META {
        if meta.single_account {
            if let Some(ch) = channels
                .get_mut(meta.openclaw_channel_id)
                .and_then(|v| v.as_object_mut())
            {
                ch.remove("accounts");
            }
        }
    }
}

/// 根据所有已启用实例对应的 openclaw channel id，统一设置各单账号通道的 enabled 状态。
/// - 有实例 → enabled: true
/// - 无实例 → enabled: false
fn sync_disable_stale_single_account_channels(
    base: &mut serde_json::Value,
    enabled_channel_ids: &HashSet<&str>,
) {
    if let Some(channels) = base.get_mut("channels").and_then(|c| c.as_object_mut()) {
        for (_, meta) in CHANNEL_META {
            let ch_key = meta.openclaw_channel_id;
            let should_be_enabled = enabled_channel_ids.contains(ch_key);
            if let Some(ch) = channels.get_mut(ch_key).and_then(|v| v.as_object_mut()) {
                let currently = ch.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                if should_be_enabled && !currently {
                    tracing::info!(
                        "通道「{}」有已启用实例，将 channels.{}.enabled 设为 true",
                        ch_key,
                        ch_key
                    );
                    ch.insert("enabled".into(), serde_json::Value::Bool(true));
                } else if !should_be_enabled && currently {
                    tracing::info!(
                        "通道「{}」无已启用实例，将 channels.{}.enabled 设为 false",
                        ch_key,
                        ch_key
                    );
                    ch.insert("enabled".into(), serde_json::Value::Bool(false));
                }
            }
        }
    }
}

/// 将 `plugins.entries` 的键与各插件 manifest `id` 对齐，避免网关报 plugin id mismatch。
///
/// - `wecom-connector` → `wecom-openclaw-plugin`（保留原始插件 ID）
/// - `qq` / `qq-connector` → `qqbot`（@sliverp/qqbot）
/// - `dingtalk`（误用通道名）→ `dingtalk-connector`
fn normalize_plugin_entries_keys(base: &mut serde_json::Value) {
    let Some(plugins) = base.get_mut("plugins").and_then(|p| p.as_object_mut()) else {
        return;
    };
    let Some(entries) = plugins.get_mut("entries").and_then(|e| e.as_object_mut()) else {
        return;
    };

    fn merge_plugin_entry(
        entries: &mut serde_json::Map<String, serde_json::Value>,
        key: &str,
        incoming: serde_json::Value,
    ) {
        if let serde_json::Value::Object(in_obj) = incoming {
            match entries.get_mut(key) {
                Some(serde_json::Value::Object(existing)) => {
                    let en_new = in_obj
                        .get("enabled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    let en_old = existing
                        .get("enabled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    existing.insert("enabled".to_string(), json!(en_new || en_old));
                    for (k, v) in in_obj {
                        if k == "enabled" {
                            continue;
                        }
                        existing.entry(k).or_insert(v);
                    }
                }
                None => {
                    entries.insert(key.to_string(), serde_json::Value::Object(in_obj));
                }
                _ => {
                    entries.insert(key.to_string(), serde_json::Value::Object(in_obj));
                }
            }
        }
    }

    for (legacy, canonical) in [
        ("wecom-connector", "wecom-openclaw-plugin"),
        ("qq-connector", "qqbot"),
    ] {
        if let Some(v) = entries.remove(legacy) {
            merge_plugin_entry(entries, canonical, v);
        }
    }
}

fn yaml_map_key(name: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(name.to_string())
}

/// 从已同步的 `openclaw.json` 读取网关鉴权 Token（用于与运行中进程对齐）
pub(crate) fn read_gateway_auth_token_from_openclaw_json(data_dir: &str) -> Option<String> {
    let path = openclaw_json_path(data_dir);
    let s = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let t = v
        .get("gateway")?
        .get("auth")?
        .get("token")?
        .as_str()?
        .trim();
    if t.len() >= 16 {
        Some(t.to_string())
    } else {
        None
    }
}

/// 管理端拉取网关 WS（用量统计等）时使用的 Token：优先与 `openclaw.json` 中 `gateway.auth.token` 一致（即实际监听进程的配置）
pub(crate) fn resolve_gateway_ws_token(data_dir: &str) -> String {
    if let Some(t) = read_gateway_auth_token_from_openclaw_json(data_dir) {
        return t;
    }
    let (_, t, _) = read_app_gateway_from_yaml(data_dir);
    t
}

/// 读取 `config/app.yaml` 中网关相关字段（宽松解析，缺省使用合理默认）
///
/// **重要**：若缺少有效 `gateway.token`，会生成随机 Token 并**写回 app.yaml**。
/// 否则在同一次 `start_gateway` 中 `sync_openclaw_config_from_manager` 与 `spawn_gateway_process` 各读一次会得到两个不同随机串，
/// 导致 `gateway.auth.token` 与 `OPENCLAW_GATEWAY_TOKEN` 不一致 → 用量页 / CLI 握手报 `token mismatch`。
pub(crate) fn read_app_gateway_from_yaml(data_dir: &str) -> (u16, String, String) {
    let path = app_yaml_path(data_dir);
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: serde_yaml::Value = match serde_yaml::from_str::<serde_yaml::Value>(&raw) {
        Ok(v) if v.is_mapping() => v,
        _ => serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    };
    let root = doc
        .as_mapping_mut()
        .expect("read_app_gateway_from_yaml: root mapping");

    let gk = yaml_map_key("gateway");
    if !root.contains_key(&gk) {
        root.insert(
            gk.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let gw_slot = root.get_mut(&gk).expect("gateway key");
    if !gw_slot.is_mapping() {
        *gw_slot = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let gw = gw_slot.as_mapping_mut().expect("gateway mapping");

    let port = gw
        .get(&yaml_map_key("port"))
        .and_then(yaml_to_u16)
        .unwrap_or(FALLBACK_GATEWAY_PORT);

    let host = gw
        .get(&yaml_map_key("host"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let had_valid_yaml_token = gw
        .get(&yaml_map_key("token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().len() >= 16)
        .unwrap_or(false);

    let mut token_opt: Option<String> = gw
        .get(&yaml_map_key("token"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| s.len() >= 16)
        .map(|s| s.to_string());

    if token_opt.is_none() {
        token_opt = read_gateway_auth_token_from_openclaw_json(data_dir);
    }

    let token = token_opt.unwrap_or_else(generate_secure_token);

    if !had_valid_yaml_token {
        gw.insert(
            yaml_map_key("token"),
            serde_yaml::Value::String(token.clone()),
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(out) = serde_yaml::to_string(&doc) {
            let _ = std::fs::write(&path, out);
        }
    }

    (port, token, host)
}

fn yaml_to_u16(v: &serde_yaml::Value) -> Option<u16> {
    v.as_u64()
        .and_then(|u| u16::try_from(u).ok())
        .or_else(|| v.as_i64().and_then(|i| i.try_into().ok()))
}

fn gateway_bind_from_host(host: &str) -> (serde_json::Value, Option<String>) {
    let h = host.trim().to_lowercase();
    if h == "127.0.0.1" || h == "localhost" || h == "::1" {
        return (json!("loopback"), None);
    }
    if h == "0.0.0.0" {
        return (json!("lan"), None);
    }
    (json!("custom"), Some(host.trim().to_string()))
}

/// 将管理端 `models.yaml` 中的默认模型合并进 OpenClaw 的 `agents.defaults.model.primary`
///
/// OpenClaw `resolveConfiguredModelRef`（`dist/agents/model-selection.js`）：若 `primary` 不含 `/`，
/// 会回退为 `anthropic/{id}`，导致 MiniMax 等厂商被错误路由。因此必须写入 `provider/model`。
pub(crate) fn read_default_model_primary(data_dir: &str) -> Option<String> {
    let path = models_yaml_path(data_dir);
    let raw = read_models_yaml_raw_utf8_or_utf16(&path)?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let doc: serde_yaml::Value = match serde_yaml::from_str(raw) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                "解析 models.yaml 失败，无法读取默认模型（{}）：{}",
                path.display(),
                e
            );
            return None;
        }
    };
    let dm = doc.get("default_model")?;
    let name = yaml_default_model_scalar_string(dm.get("model_name")?)?;
    let yaml_provider_opt = dm
        .get("provider")
        .and_then(|v| yaml_default_model_scalar_string(v));
    // model_name 含 `/` 时：OpenRouter 的 ID 多为 `qwen/...`、`google/...`，必须写成
    // `openrouter/qwen/...`，否则 OpenClaw 会把首段 `qwen` 归一成 `qwen-portal`（OAuth），触发 Unknown model。
    if name.contains('/') {
        if let Some(ref yp) = yaml_provider_opt {
            let ocp = map_yaml_provider_to_openclaw_id(yp);
            if ocp == "openrouter" && !name.starts_with("openrouter/") {
                return Some(format!("openrouter/{}", name));
            }
        }
        return Some(name.to_string());
    }
    let yaml_provider = yaml_provider_opt?;
    let openclaw_provider = map_yaml_provider_to_openclaw_id(&yaml_provider);
    Some(format!("{}/{}", openclaw_provider, name))
}

/// 与 [`read_default_model_primary`] 同逻辑，但返回可诊断错误（用于前端保存时直接提示）。
pub(crate) fn diagnose_default_model_primary(data_dir: &str) -> Result<String, String> {
    let path = models_yaml_path(data_dir);
    if !path.exists() {
        return Err(format!("models.yaml 不存在：{}", path.display()));
    }
    let raw = read_models_yaml_raw_utf8_or_utf16(&path).ok_or_else(|| {
        format!(
            "读取 models.yaml 失败（{}）。文件可能被占用或编码无法识别（请用 UTF-8 保存）。",
            path.display()
        )
    })?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let doc: serde_yaml::Value = serde_yaml::from_str(raw).map_err(|e| {
        format!(
            "解析 models.yaml 失败（{}）：{}。请确认 YAML 缩进/引号正确，且不要用记事本另存为 UTF-16。",
            path.display(),
            e
        )
    })?;
    let dm = doc
        .get("default_model")
        .ok_or_else(|| format!("models.yaml 缺少 default_model 节（{}）", path.display()))?;
    let name = yaml_default_model_scalar_string(dm.get("model_name").ok_or_else(|| {
        format!("default_model 缺少 model_name（{}）", path.display())
    })?)
    .ok_or_else(|| format!("default_model.model_name 为空（{}）", path.display()))?;
    let yaml_provider_opt = dm
        .get("provider")
        .and_then(|v| yaml_default_model_scalar_string(v));
    if name.contains('/') {
        if let Some(ref yp) = yaml_provider_opt {
            let ocp = map_yaml_provider_to_openclaw_id(yp);
            if ocp == "openrouter" && !name.starts_with("openrouter/") {
                return Ok(format!("openrouter/{}", name));
            }
        }
        return Ok(name);
    }
    let yaml_provider = yaml_provider_opt.ok_or_else(|| {
        format!("default_model 缺少 provider（{}）", path.display())
    })?;
    let openclaw_provider = map_yaml_provider_to_openclaw_id(&yaml_provider);
    Ok(format!("{}/{}", openclaw_provider, name))
}

/// 管理端 `models.yaml` 中的 provider id 与 OpenClaw 内置 catalog 的 provider 名对齐
fn map_yaml_provider_to_openclaw_id(yaml_provider: &str) -> &str {
    match yaml_provider {
        "volc_ark" => "volcengine",
        other => other,
    }
}

/// 从 `models.yaml` 读取某供应商的 `api_key`。
/// - 优先：`providers.<id>.api_key`（结构化配置）
/// - 回退：若 providers.<id> 不存在（如 volc_ark vs volcengine），尝试别名映射
/// - 最后回退：顶层 `<id>.api_key`（旧格式兼容）
fn yaml_provider_api_key(doc: &serde_yaml::Value, provider: &str) -> Option<String> {
    fn key_from_block(block: &serde_yaml::Value) -> Option<String> {
        let s = block.get("api_key")?.as_str()?.trim();
        if s.is_empty() {
            return None;
        }
        Some(s.to_string())
    }
    // 直接查找（模板中已有的 key 如 openrouter、minimax 等）
    if let Some(p) = doc.get("providers").and_then(|x| x.get(provider)) {
        if let Some(k) = key_from_block(p) {
            return Some(k);
        }
    }
    // 别名映射：volc_ark <-> volcengine（模板中用 volcengine，UI 用 volc_ark）
    let alias = match provider {
        "volc_ark" => "volcengine",
        "volcengine" => "volc_ark",
        _ => provider,
    };
    if alias != provider {
        if let Some(p) = doc.get("providers").and_then(|x| x.get(alias)) {
            if let Some(k) = key_from_block(p) {
                return Some(k);
            }
        }
    }
    // 顶层回退（旧格式兼容）
    doc.get(provider).and_then(key_from_block)
}

/// 读取 default_model 对应 provider 的 API Key，供启动网关时注入环境变量（与 openclaw `model-auth.js` / pi-ai 一致）。
///
/// `models.yaml` 中密钥常以 `enc:` 前缀加密存储（与「测试连接」一致）；此处必须在注入子进程环境变量前解密，
/// 否则网关内的 Node 会把密文当作 Bearer Token，供应商返回空内容或 401，控制 UI 即报「模型未返回文本」。
fn read_default_model_api_key(data_dir: &str) -> Option<String> {
    let path = models_yaml_path(data_dir);
    let raw = read_models_yaml_raw_utf8_or_utf16(&path)?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let doc: serde_yaml::Value = serde_yaml::from_str(raw).ok()?;
    let dm = doc.get("default_model")?;
    let provider = yaml_default_model_scalar_string(dm.get("provider")?)?;
    let key = yaml_provider_api_key(&doc, &provider)?;
    Some(decrypt_models_yaml_api_key_for_gateway_env(data_dir, &key))
}

fn decrypt_models_yaml_api_key_for_gateway_env(data_dir: &str, api_key: &str) -> String {
    if !api_key.starts_with(CIPHER_PREFIX) {
        return api_key.to_string();
    }
    match crate::services::cipher::get_or_create_cipher_key_sync(data_dir) {
        Ok(k) => decrypt_credential(api_key, &k).unwrap_or_else(|| {
            warn!(
                "默认模型 API Key 为加密格式但解密失败，网关将无法正确调用供应商（请检查 data_dir 与机器绑定密钥是否一致）"
            );
            api_key.to_string()
        }),
        Err(e) => {
            warn!(
                "无法读取解密密钥（{}），加密格式的默认模型 API Key 无法注入网关环境变量",
                e
            );
            api_key.to_string()
        }
    }
}

fn read_default_model_provider(data_dir: &str) -> Option<String> {
    let path = models_yaml_path(data_dir);
    let raw = read_models_yaml_raw_utf8_or_utf16(&path)?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let doc: serde_yaml::Value = serde_yaml::from_str(raw).ok()?;
    let dm = doc.get("default_model")?;
    yaml_default_model_scalar_string(dm.get("provider")?)
}

/// OpenClaw-CN / pi-ai 使用的常见环境变量名（与 dist/agents/model-auth.js 对齐）
fn provider_api_key_env_var(provider: &str) -> Option<&'static str> {
    match provider.to_lowercase().as_str() {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "google" => Some("GEMINI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "nvidia" => Some("NVIDIA_API_KEY"),
        "moonshot" => Some("MOONSHOT_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "volcengine" | "volc_ark" => Some("VOLCENGINE_API_KEY"),
        "dashscope" => Some("DASHSCOPE_API_KEY"),
        "siliconflow" => Some("SILICONFLOW_API_KEY"),
        "baidu" => Some("BAIDU_API_KEY"),
        "xiaomi" => Some("XIAOMI_API_KEY"),
        "zhipu" => Some("BIGMODEL_API_KEY"),
        "kuaifan" => Some("KUAIFAN_API_KEY"),
        _ => None,
    }
}

pub(crate) fn merge_json_deep(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(tobj), serde_json::Value::Object(pobj)) => {
            for (k, pv) in pobj {
                if let Some(tv) = tobj.get_mut(&k) {
                    merge_json_deep(tv, pv);
                } else {
                    tobj.insert(k, pv);
                }
            }
        }
        (t, p) => *t = p,
    }
}

/// 将管理端生成的 `channels` 片段合并进 openclaw.json：对 patch 中出现的每个通道 **整表替换** `accounts`。
/// 若仍用 `merge_json_deep`，已删除实例的旧 accountId 会残留在 `accounts` 里，飞书等会为每个 key 各拉一条 WebSocket，表现为重复连接、收消息异常。
fn merge_channels_patch_replace_accounts(base: &mut serde_json::Value, patch: serde_json::Value) {
    let Some(patch_root) = patch.as_object() else {
        return;
    };
    let Some(p_channels) = patch_root.get("channels").and_then(|c| c.as_object()) else {
        return;
    };
    let Some(base_root) = base.as_object_mut() else {
        tracing::warn!("openclaw.json 根类型应为 object，无法合并 channels patch");
        return;
    };
    let Some(t_channels) = base_root
        .entry("channels".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        tracing::warn!("channels 字段应为 object，无法合并 patch");
        return;
    };

    for (ch_name, ch_patch) in p_channels {
        let Some(ch_obj) = ch_patch.as_object() else {
            continue;
        };
        let entry = t_channels
            .entry(ch_name.clone())
            .or_insert_with(|| json!({}));
        let e = match entry.as_object_mut() {
            Some(obj) => obj,
            None => {
                tracing::warn!("channel entry 应为 object 类型，跳过");
                continue;
            }
        };
        for (k, v) in ch_obj {
            if k == "accounts" {
                continue;
            }
            e.insert(k.clone(), v.clone());
        }
        if let Some(acc) = ch_obj.get("accounts") {
            // 关键修复：accounts 整表替换时，**保留**每个账号的敏感字段
            // (allowFrom, appSecret, dmPolicy, groupPolicy 等) — 这些是用户/扫码流程
            // 写入的，不应被 sync YAML 覆盖。
            // 合并策略：
            //   1. patch account 上的新字段（agent_id, model_ref 等）覆盖 base
            //   2. base account 上 patch 没有的字段保留（allowFrom, appSecret, dmPolicy 等）
            let mut merged = acc.clone();
            if let Some(merged_obj) = merged.as_object_mut() {
                if let Some(base_acc) = e.get("accounts").and_then(|a| a.as_object()) {
                    for (base_key, base_acc_id) in base_acc {
                        let Some(base_acc_obj) = base_acc_id.as_object() else {
                            continue;
                        };
                        let patch_acc_obj = merged_obj
                            .entry(base_key.clone())
                            .or_insert_with(|| json!({}))
                            .as_object_mut();
                        if let Some(patch_acc_obj) = patch_acc_obj {
                            for (k, v) in base_acc_obj {
                                // patch 没有但 base 有的字段 → 保留
                                if !patch_acc_obj.contains_key(k) {
                                    patch_acc_obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
            e.insert("accounts".to_string(), merged);
        }
    }
}

/// 删除各通道 `accounts` 下仍残留、但已不在管理端实例列表中的账号（无论前缀格式）。
/// 修复：不再依赖 "inst-" 前缀判断，而是精确检查 accountId 是否在当前有效集合中。
/// 这样非飞书通道的 "qq-inst_xxx" / "telegram-inst_xxx" 等格式也能被正确清理。
fn prune_stale_manager_channel_account_keys(
    base: &mut serde_json::Value,
    valid_ids: &HashSet<String>,
) {
    let Some(channels) = base.get_mut("channels").and_then(|c| c.as_object_mut()) else {
        return;
    };
    // 收集 channel names 以避免在迭代中直接对 channels 调用 get_mut（嵌套可变借用）
    let ch_names: Vec<String> = channels.keys().cloned().collect();
    for ch_name in ch_names {
        // 取出的 ch_val 是 &mut Value，后续对 fields 的 get_mut 不再触发 channels 的借用
        let ch_val = match channels.get_mut(&ch_name) {
            Some(v) => v,
            None => continue,
        };
        let acc_map = match ch_val {
            serde_json::Value::Object(fields) => match fields.get_mut("accounts") {
                Some(serde_json::Value::Object(a)) => a,
                _ => continue,
            },
            _ => continue,
        };
        let stale: Vec<String> = acc_map
            .keys()
            .filter(|k| !valid_ids.contains(k.as_str()))
            .cloned()
            .collect();
        for k in stale {
            acc_map.remove(&k);
        }
    }
}

/// 将管理端 `app.yaml` / `models.yaml` 写入 `{data_dir}/openclaw-cn/openclaw.json`（深度合并，保留 skills 等已有字段）
pub async fn sync_openclaw_config_from_manager(data_dir: &str) -> Result<(), String> {
    let openclaw_dir = PathBuf::from(data_dir).join("openclaw-cn");
    tokio::fs::create_dir_all(&openclaw_dir)
        .await
        .map_err(|e| format!("创建 openclaw-cn 目录失败: {}", e))?;

    let cfg_path = openclaw_json_path(data_dir);
    let mut base: serde_json::Value = if cfg_path.exists() {
        let s = tokio::fs::read_to_string(&cfg_path)
            .await
            .map_err(|e| format!("读取 openclaw.json 失败: {}", e))?;
        serde_json::from_str(&s).unwrap_or(json!({}))
    } else {
        json!({})
    };

    let (port, token, host) = read_app_gateway_from_yaml(data_dir);
    let (bind, custom_host) = gateway_bind_from_host(&host);

    // OpenClaw-CN 0.1.9+：未设置 gateway.mode 时 CLI 会直接退出（见 gateway-cli/run.js）
    let mut gateway_patch = json!({
        "mode": "local",
        "port": port,
        "auth": { "token": token },
        "bind": bind,
    });
    if let Some(h) = custom_host {
        if let Some(obj) = gateway_patch.as_object_mut() {
            obj.insert("customBindHost".to_string(), json!(h));
        }
    }

    let mut patch = json!({ "gateway": gateway_patch });

    // 禁用一组本 app 用不到、且在 Mac 上会因缺依赖 / TS 解析器不支持 `??` 而刷错误的扩展。
    // （不删目录是为了保留升级 openclaw-cn 后能复用的可能性；deny 列表是 openclaw 官方支持的扩展控制机制。）
    merge_json_deep(
        &mut patch,
        json!({
            "plugins": {
                "deny": [
                    "nostr",                // 缺 nostr-tools
                    "tlon",                  // 缺 @urbit/http-api
                    "matrix",                // 缺 matrix-bot-sdk
                    "memory-lancedb",        // 缺 @lancedb/lancedb
                    "diagnostics-otel",      // 缺 @opentelemetry/api
                    "minimax-portal-auth"    // 含 ES2020 `??`，老版 esbuild/swc 解析失败
                ]
            }
        }),
    );

    if let Some(primary) = read_default_model_primary(data_dir) {
        merge_json_deep(
            &mut patch,
            json!({
                "agents": {
                    "defaults": {
                        "model": { "primary": primary }
                    }
                }
            }),
        );
    }

    // 与 openclaw-cn `dist/config/port-defaults.js` / `dist/browser/config.js` 一致：
    //   browser 控制服务端口 = gateway.port + 2
    //   Chrome 扩展中继（HTTP/WS，供扩展连接）= 控制端口 + 1（即 gateway + 3，如 8080→8082→8083）
    // 管理端此前未写入 `browser` 时，默认仅有 clawd CDP 配置，网关侧 `ensureExtensionRelayForProfiles`
    // 不会为 `driver=extension` 拉起中继 → 8083 不监听、扩展无法连接。
    // 此处强制同步启用并声明 `profiles.chrome` 和 `profiles.edge`，保证中继随「启动网关」一并就绪。
    let browser_control_port = port.saturating_add(2);
    let browser_relay_port = browser_control_port.saturating_add(1);
    merge_json_deep(
        &mut patch,
        json!({
            "browser": {
                "enabled": true,
                "defaultProfile": "clawd",
                "controlUrl": format!("http://127.0.0.1:{browser_control_port}"),
                "profiles": {
                    "chrome": {
                        "driver": "extension",
                        "cdpUrl": format!("http://127.0.0.1:{browser_relay_port}"),
                        "color": "#00AA00"
                    },
                    "edge": {
                        "driver": "extension",
                        "cdpUrl": format!("http://127.0.0.1:{browser_relay_port}"),
                        "color": "#0078D4"
                    }
                }
            }
        }),
    );

    // 启用 HTTP /v1/chat/completions 端点
    merge_json_deep(
        &mut patch,
        json!({
            "gateway": {
                "http": {
                    "endpoints": {
                        "chatCompletions": { "enabled": true }
                    }
                }
            }
        }),
    );

    merge_json_deep(&mut base, patch);

    // 从 models.yaml 读取所有供应商 API Key，以明文写入 openclaw.json 的 models.providers.{id}.apiKey
    inject_provider_api_keys_into_openclaw_json(&mut base, data_dir);

    // 实时拉取快泛 API 模型列表并注入 openclaw.json
    inject_kuaifan_models_from_pricing(&mut base).await;

    normalize_plugin_entries_keys(&mut base);

    // plugins.entries 由网关自动管理，不手动写入

    // 避免从 custom 切回 loopback/lan 后仍残留 customBindHost
    if bind != json!("custom") {
        if let Some(g) = base.get_mut("gateway").and_then(|x| x.as_object_mut()) {
            g.remove("customBindHost");
        }
    }

    // OpenClaw WS：CLI 类客户端握手失败时常提示将 `gateway.remote.token` 与 `gateway.auth.token` 对齐；合并后统一写入，避免只改端口/重启后两端不一致
    if let Some(g) = base.get_mut("gateway").and_then(|x| x.as_object_mut()) {
        let auth_tok = g
            .get("auth")
            .and_then(|a| a.get("token"))
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|s| s.len() >= 16)
            .map(|s| s.to_string())
            .unwrap_or_else(|| token.clone());
        let port_for_remote = g
            .get("port")
            .and_then(|x| x.as_u64())
            .and_then(|u| u16::try_from(u).ok())
            .unwrap_or(port);
        let remote = g.entry("remote".to_string()).or_insert_with(|| json!({}));
        if let Some(ro) = remote.as_object_mut() {
            ro.insert("token".to_string(), json!(auth_tok));
            let url_empty = ro
                .get("url")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            if url_empty {
                ro.insert(
                    "url".to_string(),
                    json!(format!("ws://127.0.0.1:{}/", port_for_remote)),
                );
            }
        }
    }

    // 将所有供应商的特有模型（含 baseUrl/apiKey）写入 openclaw.json，
    // 避免网关启动时被 ensureOpenClawModelsJson 覆盖导致模型路由失效。
    upsert_manager_provider_catalogs_into_openclaw_json(&mut base);

    // 飞书通道凭证 + 路由同步：channels.feishu.accounts / agents.list / bindings
    sync_feishu_channel_and_routing(data_dir, &mut base);

    // 防止多次创建机器人后 extraDirs 堆积，导致网关加载到其它模板机器人的技能
    prune_stale_skills_extra_dirs(&mut base, data_dir);

    // 从 CHANNEL_META 找出所有需要显式启用插件的通道，检查是否有已启用的实例，
    // 若有则写入 plugins.entries.{key}.enabled = true。
    // 注意：read_channel_patches 只需调用一次，不要重复调用。
    // channel_type 字段存的是 openclaw_channel_id（如 "qqbot"，而非 YAML 原始值 "qq"）。
    let (_patch, manager_accounts) = read_channel_patches(data_dir);
    let enabled_channel_ids: std::collections::HashSet<&str> = manager_accounts
        .iter()
        .map(|a| a.channel_type.as_str())
        .collect();
    // 遍历 CHANNEL_META：检查每个单账号通道是否有已启用实例，统一处理 enabled 的写回
    sync_disable_stale_single_account_channels(&mut base, &enabled_channel_ids);
    // 设置 plugins.load.paths 指向用户插件目录 (统一使用正斜杠)
    let plugins_dir = data_dir.trim_end_matches(|c| c == '/' || c == '\\').to_string() + "/plugins";
    if let Some(pl) = base.get_mut("plugins") {
        if let Some(obj) = pl.as_object_mut() {
            obj.insert("load".to_string(), json!({"paths": [&plugins_dir]}));
        }
    }
    // plugins.entries 由网关自动管理，不手动写入

    let pretty = serde_json::to_string_pretty(&base)
        .map_err(|e| format!("序列化 openclaw.json 失败: {}", e))?;
    // OpenOptions + sync_all 确保写入原子性和数据落盘
    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&cfg_path)
        .await
        .map_err(|e| format!("打开 openclaw.json 失败: {}", e))?;
    let mut file = file;
    file.write_all(pretty.as_bytes())
        .await
        .map_err(|e| format!("写入 openclaw.json 失败: {}", e))?;
    file.sync_all()
        .await
        .map_err(|e| format!("sync openclaw.json 失败: {}", e))?;

    // 清掉历史上 workspace=robots/{agent_id} 留下的空壳目录（与当前 robot_id 无关）
    cleanup_legacy_wrong_robot_workspace_dirs(data_dir).await;

    Ok(())
}

fn read_port_from_openclaw_json(data_dir: &str) -> Option<u16> {
    let path = openclaw_json_path(data_dir);
    let s = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("gateway")?
        .get("port")?
        .as_u64()
        .and_then(|u| u16::try_from(u).ok())
}

/// 用于界面展示与控制台链接的 HTTP 端口
pub(crate) fn resolve_gateway_http_port(data_dir: &str) -> u16 {
    read_port_from_openclaw_json(data_dir).unwrap_or_else(|| read_app_gateway_from_yaml(data_dir).0)
}

fn openclaw_state_dir(data_dir: &str) -> PathBuf {
    // 直接使用 openclaw-cn 作为状态目录，确保插件凭证路径一致
    PathBuf::from(data_dir).join("openclaw-cn")
}

/// 与 [`crate::commands::model::static_provider_models`] 对齐，供 OpenClaw 控制台 `models.json` 补齐。
/// 网关启动时 `ensureOpenClawModelsJson` 会用内置目录重写 agent/models.json，
/// 把尚未收录的模型写进 openclaw.json，由 JS 与内置表按 id 合并。
#[derive(Clone, Copy)]
struct ProviderCatalogEntry {
    id: &'static str,
    name: &'static str,
    context_window: u64,
    vision: bool,
}

/// 各供应商模型目录（与 static_provider_models 分支一一对应）
fn manager_provider_catalog(provider_id: &str) -> Option<&'static [ProviderCatalogEntry]> {
    match provider_id {
        "minimax" => Some(&[
            ProviderCatalogEntry {
                id: "MiniMax-M2.7",
                name: "MiniMax M2.7",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "MiniMax-M2.7-highspeed",
                name: "MiniMax M2.7 高速",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "MiniMax-M2.5",
                name: "MiniMax M2.5（标准）",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "MiniMax-M2.5-highspeed",
                name: "MiniMax M2.5 高速",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "MiniMax-M2.1",
                name: "MiniMax M2.1",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "MiniMax-M2-her",
                name: "MiniMax M2-Her（角色）",
                context_window: 204_800,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "abab6.5s-chat",
                name: "abab6.5s（兼容轻量）",
                context_window: 245_000,
                vision: false,
            },
        ]),
        "volc_ark" => Some(&[
            ProviderCatalogEntry {
                id: "doubao-seed-2-0-pro-260215",
                name: "豆包 Seed 2.0 Pro（旗舰·复杂推理/长链任务）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-2-0-code-preview-260215",
                name: "豆包 Seed 2.0 Code（编程·IDE 工具集成）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-2-0-lite-260215",
                name: "豆包 Seed 2.0 Lite（均衡·生产级负载）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-2-0-mini-260215",
                name: "豆包 Seed 2.0 Mini（低延迟·高并发）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-1-8-251228",
                name: "豆包 Seed 1.8（旗舰·多模态 Agent）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-1-6-250615",
                name: "豆包 Seed 1.6",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-seed-1-6-thinking-250615",
                name: "豆包 Seed 1.6 Thinking（深度推理）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-1-5-pro-256k-250115",
                name: "豆包 1.5 Pro 256K（长上文）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-1-5-pro-32k-250115",
                name: "豆包 1.5 Pro 32K",
                context_window: 32_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-1-5-thinking-pro-250428",
                name: "豆包 1.5 Thinking Pro（推理）",
                context_window: 32_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-1-5-vision-pro-32k-250115",
                name: "豆包 1.5 Vision Pro 32K（多模态对话）",
                context_window: 32_000,
                vision: true,
            },
            ProviderCatalogEntry {
                id: "doubao-1-5-lite-32k-250115",
                name: "豆包 1.5 Lite 32K（轻量）",
                context_window: 32_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "doubao-lite-32k-character-240828",
                name: "豆包 Lite 32K Character（轻量）",
                context_window: 32_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "deepseek-v3-250324",
                name: "DeepSeek V3（方舟接入）",
                context_window: 128_000,
                vision: false,
            },
        ]),
        "nvidia" => Some(&[
            ProviderCatalogEntry {
                id: "meta/llama-4-maverick-17b-128e-instruct",
                name: "Llama 4 Maverick 17B 128E（NIM 文档示例）",
                context_window: 131_072,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "meta/llama-4-scout-17b-16e-instruct",
                name: "Llama 4 Scout 17B 16E",
                context_window: 131_072,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "meta/llama-3.1-405b-instruct",
                name: "Llama 3.1 405B Instruct",
                context_window: 131_072,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "meta/llama-3.1-8b-instruct",
                name: "Llama 3.1 8B Instruct（轻量）",
                context_window: 131_072,
                vision: false,
            },
        ]),
        "aliyun" => Some(&[
            ProviderCatalogEntry {
                id: "qwen3-max",
                name: "通义千问 Qwen3-Max（百炼文档旗舰线）",
                context_window: 1_000_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "qwen-plus",
                name: "通义千问 Plus",
                context_window: 1_000_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "qwen-long",
                name: "通义千问 Long（超长）",
                context_window: 10_000_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "qwen-flash",
                name: "通义千问 Flash（轻量）",
                context_window: 1_000_000,
                vision: false,
            },
        ]),
        "zhipu" => Some(&[
            ProviderCatalogEntry {
                id: "glm-4.6",
                name: "GLM-4.6（智谱文档当前旗舰）",
                context_window: 200_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "glm-4-plus",
                name: "GLM-4 Plus（稳定高配）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "glm-4-long",
                name: "GLM-4 Long（长文）",
                context_window: 1_000_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "glm-4-flash",
                name: "GLM-4 Flash（轻量）",
                context_window: 128_000,
                vision: false,
            },
        ]),
        "moonshot" => Some(&[
            ProviderCatalogEntry {
                id: "kimi-k2.5",
                name: "Kimi K2.5（Moonshot 文档当前主推）",
                context_window: 256_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "kimi-k2-thinking",
                name: "Kimi K2 Thinking（推理）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "kimi-k2-turbo-preview",
                name: "Kimi K2 Turbo Preview",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "moonshot-v1-128k",
                name: "moonshot-v1-128k（经典长文·轻量）",
                context_window: 128_000,
                vision: false,
            },
        ]),
        "baidu" => Some(&[
            ProviderCatalogEntry {
                id: "ernie-5.0",
                name: "ERNIE 5.0（千帆文档旗舰线）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "ernie-5.0-thinking-latest",
                name: "ERNIE 5.0 Thinking（推理）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "ernie-4.0-turbo-128k",
                name: "ERNIE 4.0 Turbo 128K（兼容）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "ernie-speed-128k",
                name: "ERNIE Speed（轻量）",
                context_window: 128_000,
                vision: false,
            },
        ]),
        "xiaomi" => Some(&[
            ProviderCatalogEntry {
                id: "mimo-v2-pro",
                name: "MiMo V2 Pro（示例·高配）",
                context_window: 128_000,
                vision: false,
            },
            ProviderCatalogEntry {
                id: "mimo-v2-flash",
                name: "MiMo V2 Flash（示例·轻量）",
                context_window: 128_000,
                vision: false,
            },
        ]),
        // OpenRouter 模型 ID 由线上目录与用户自选决定，此处仅保证 upsert 会写入连接参数；
        // 任意模型名由 pi-embedded-runner `resolveModel` 的 providerCfg fallback 解析。
        "openrouter" => Some(&[] as &[ProviderCatalogEntry]),
        _ => None,
    }
}

/// OpenClaw 内置模型 id 集合（按供应商分组，仅用于去重检测；未列在此处的模型均视为"管理器特有"）
fn openclaw_builtin_model_ids(provider_id: &str) -> std::collections::HashSet<&'static str> {
    match provider_id {
        "minimax" => [
            "MiniMax-M2.7",
            "MiniMax-M2.7-highspeed",
            "MiniMax-M2.5",
            "MiniMax-M2.5-highspeed",
            "MiniMax-M2.1",
            "MiniMax-M2.1-highspeed",
            "MiniMax-M2",
            "MiniMax-VL-01",
            "MiniMax-M2-her",
            "abab6.5s-chat",
        ]
        .into_iter()
        .collect(),
        _ => std::collections::HashSet::new(),
    }
}

/// 供应商默认定价（单位：$ / 1M tokens，与 OpenClaw models.json cost 字段对应）
fn provider_default_pricing(provider_id: &str) -> (i32, i32, i32, i32) {
    match provider_id {
        "minimax" => (15, 60, 2, 10),
        "volc_ark" => (15, 60, 0, 0),
        "nvidia" => (15, 60, 0, 0),
        "aliyun" => (15, 60, 0, 0),
        "zhipu" => (15, 60, 0, 0),
        "moonshot" => (15, 60, 0, 0),
        "baidu" => (15, 60, 0, 0),
        "xiaomi" => (15, 60, 0, 0),
        "openrouter" => (0, 0, 0, 0),
        _ => (0, 0, 0, 0),
    }
}

fn build_provider_model_json(provider_id: &str, e: &ProviderCatalogEntry) -> serde_json::Value {
    let input = if e.vision {
        json!(["text", "image"])
    } else {
        json!(["text"])
    };
    let (input_cost, output_cost, cache_read, cache_write) = provider_default_pricing(provider_id);
    json!({
        "id": e.id,
        "name": e.name,
        "reasoning": false,
        "input": input,
        "cost": {
            "input": input_cost,
            "output": output_cost,
            "cacheRead": cache_read,
            "cacheWrite": cache_write
        },
        "contextWindow": e.context_window,
        "maxTokens": 8192u32
    })
}

/// 将管理端特有（OpenClaw 内置目录暂无）的模型追加到 `openclaw.json`，覆盖所有已知供应商。
/// 从 models.yaml 读取所有供应商的 API Key（解密后明文），写入 openclaw.json 的 models.providers.{id}.apiKey。
/// 仅当 models.yaml 中存在对应 provider 且 api_key 非空时才覆盖。
fn inject_provider_api_keys_into_openclaw_json(base: &mut serde_json::Value, data_dir: &str) {
    let models_root = match base
        .pointer_mut("/models/providers")
        .and_then(|v| v.as_object_mut())
    {
        Some(p) => p,
        None => return,
    };

    let yaml_path = models_yaml_path(data_dir);
    let raw = match read_models_yaml_raw_utf8_or_utf16(&yaml_path) {
        Some(r) => r,
        None => return,
    };
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let doc: serde_yaml::Value = match serde_yaml::from_str(raw) {
        Ok(d) => d,
        Err(_) => return,
    };
    let providers_yaml = match doc.get("providers") {
        Some(serde_yaml::Value::Mapping(m)) => m,
        _ => return,
    };

    for (provider_key, provider_val) in providers_yaml {
        let pid = match provider_key.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let encrypted = match provider_val.get("api_key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => continue,
        };
        let plain = if encrypted.starts_with(CIPHER_PREFIX) {
            match crate::services::cipher::get_or_create_cipher_key_sync(data_dir) {
                Ok(k) => decrypt_credential(&encrypted, &k).unwrap_or(encrypted),
                Err(_) => encrypted,
            }
        } else {
            encrypted
        };

        if let Some(provider_obj) = models_root.get_mut(&pid).and_then(|v| v.as_object_mut()) {
            provider_obj.insert("apiKey".to_string(), json!(plain));
            info!("Injected plaintext API key for provider: {}", pid);
        }
    }
}

/// 从 https://kuaifanio.cn/api/pricing 实时拉取快泛 API 模型列表，写入 openclaw.json。
async fn inject_kuaifan_models_from_pricing(base: &mut serde_json::Value) {
    let models = match crate::commands::model::fetch_models_from_pricing_api().await {
        Ok(m) if !m.is_empty() => m,
        _ => {
            tracing::warn!("inject_kuaifan_models: pricing API unavailable, skipping");
            return;
        }
    };

    let models_root = match base
        .pointer_mut("/models/providers")
        .and_then(|v| v.as_object_mut())
    {
        Some(p) => p,
        None => return,
    };

    let arr: Vec<serde_json::Value> = models
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
            })
        })
        .collect();

    let provider_entry = models_root
        .entry("kuaifan".to_string())
        .or_insert_with(|| json!({}));
    if let Some(obj) = provider_entry.as_object_mut() {
        obj.entry("baseUrl".to_string())
            .or_insert(json!("https://kuaifanio.cn/v1"));
        obj.entry("api".to_string())
            .or_insert(json!("openai-completions"));
        obj.insert("models".to_string(), json!(arr));
    }

    tracing::info!(
        "inject_kuaifan_models: {} models from pricing API",
        models.len()
    );
}

fn upsert_manager_provider_catalogs_into_openclaw_json(base: &mut serde_json::Value) {
    use std::collections::HashSet;

    let Some(root_obj) = base.as_object_mut() else {
        return;
    };
    let models_entry = root_obj
        .entry("models".to_string())
        .or_insert_with(|| json!({}));
    if !models_entry.is_object() {
        *models_entry = json!({});
    }
    let Some(models_root) = models_entry.as_object_mut() else {
        return;
    };
    models_root.entry("mode").or_insert_with(|| json!("merge"));

    let Some(providers) = models_root
        .entry("providers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return;
    };

    // 遍历所有已知供应商（kuaifan 模型由 inject_kuaifan_models_from_pricing 实时拉取）
    let all_provider_ids = [
        "minimax",
        "volc_ark",
        "nvidia",
        "aliyun",
        "zhipu",
        "moonshot",
        "baidu",
        "xiaomi",
        "openrouter",
    ];

    for provider_id in all_provider_ids {
        let Some(catalog) = manager_provider_catalog(provider_id) else {
            continue;
        };

        // OpenClaw 内置目录已有的模型 id（用于去重）
        let builtin_ids = openclaw_builtin_model_ids(provider_id);

        let provider_obj = providers
            .entry(provider_id.to_string())
            .or_insert_with(|| json!({}));

        if !provider_obj.is_object() {
            *provider_obj = json!({});
        }
        let Some(provider_map) = provider_obj.as_object_mut() else {
            continue;
        };

        // 写入连接参数（OpenClaw 配置 schema 要求）
        let (base_url, api_type, api_key_env) = match provider_id {
            "minimax" => (
                "https://api.minimax.chat/v1",
                "openai-completions",
                "MINIMAX_API_KEY",
            ),
            "volc_ark" => (
                "https://ark.cn-beijing.volces.com/api/v3",
                "openai-completions",
                "VOLCENGINE_API_KEY",
            ),
            "nvidia" => (
                "https://integrate.api.nvidia.com/v1",
                "openai-completions",
                "NVIDIA_API_KEY",
            ),
            "aliyun" => (
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "openai-completions",
                "DASHSCOPE_API_KEY",
            ),
            "zhipu" => (
                "https://open.bigmodel.cn/api/paas/v4",
                "openai-completions",
                "BIGMODEL_API_KEY",
            ),
            "moonshot" => (
                "https://api.moonshot.cn/v1",
                "openai-completions",
                "MOONSHOT_API_KEY",
            ),
            "baidu" => (
                "https://qianfan.baidubce.com/v2",
                "openai-completions",
                "BAIDU_API_KEY",
            ),
            "xiaomi" => (
                "https://api.xiaomi.com/v1",
                "openai-completions",
                "XIAOMI_API_KEY",
            ),
            "openrouter" => (
                "https://openrouter.ai/api/v1",
                "openai-completions",
                "OPENROUTER_API_KEY",
            ),
            _ => continue,
        };

        provider_map
            .entry("baseUrl".to_string())
            .or_insert_with(|| json!(base_url));
        provider_map
            .entry("api".to_string())
            .or_insert_with(|| json!(api_type));
        provider_map
            .entry("apiKey".to_string())
            .or_insert_with(|| json!(api_key_env));

        // 追加管理器特有模型
        let models_arr = provider_map
            .entry("models".to_string())
            .or_insert_with(|| json!([]));
        if !models_arr.is_array() {
            *models_arr = json!([]);
        }
        let Some(models_list) = models_arr.as_array_mut() else {
            continue;
        };

        let mut seen: HashSet<String> = models_list
            .iter()
            .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_string))
            .collect();

        for e in catalog {
            if builtin_ids.contains(e.id) || seen.contains(e.id) {
                continue;
            }
            seen.insert(e.id.to_string());
            models_list.push(build_provider_model_json(provider_id, e));
        }
    }
}

fn abs_path_str(p: &Path) -> Result<String, String> {
    let p = if p.exists() {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    let mut s = p.to_string_lossy().to_string();
    // Windows canonicalize 会生成 \\?\ 扩展路径；传给子进程作 cwd 时 CMD 不支持，Node 内 spawn cmd 也会告警并落到错误目录。
    #[cfg(windows)]
    {
        const VERBATIM: &str = r"\\?\";
        const UNC_VERBATIM: &str = r"\\?\UNC\";
        if s.starts_with(UNC_VERBATIM) {
            s = format!(r"\\{}", &s[UNC_VERBATIM.len()..]);
        } else if s.starts_with(VERBATIM) {
            s = s[VERBATIM.len()..].to_string();
        }
    }
    Ok(s)
}

/// 与 openclaw-cn `dist/config/port-defaults.js` 一致：网关端口 + 偏移得到桥接 / 浏览器 / Canvas 等默认端口。
fn gateway_listen_ports_to_clear(main: u16) -> Vec<u16> {
    let mut v = Vec::new();
    for offset in [0u16, 1, 2, 4] {
        if let Some(p) = main.checked_add(offset) {
            v.push(p);
        }
    }
    v
}

/// Windows：结束占用指定 TCP 端口的监听进程（用于 PID 已失效或 taskkill 未杀干净时）。
#[cfg(windows)]
fn kill_windows_processes_listening_on_port(port: u16) {
    let ps = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Get-NetTCPConnection -LocalPort {} -State Listen -ErrorAction SilentlyContinue | \
         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }}",
        port
    );
    let out = hidden_cmd::powershell()
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
        .output();
    if let Ok(o) = &out {
        if !o.status.success() {
            tracing::warn!(
                "按端口 {} 清理监听进程(PowerShell)未完全成功: {}",
                port,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
}

/// Windows：`Get-NetTCPConnection` 在部分环境不可用或拿不到 OwningProcess 时，用 netstat 解析 PID 再 taskkill。
#[cfg(windows)]
fn kill_windows_processes_listening_on_port_netstat_fallback(port: u16) {
    let out = hidden_cmd::cmd()
        .args(["/C", &format!("netstat -ano | findstr :{}", port)])
        .output();
    let Ok(o) = out else {
        return;
    };
    if !o.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for line in text.lines() {
        let upper = line.to_uppercase();
        if !upper.contains("LISTENING") {
            continue;
        }
        if let Some(pid_str) = line.split_whitespace().last() {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if pid > 0 {
                    pids.insert(pid);
                }
            }
        }
    }
    for pid in pids {
        let _ = hidden_cmd::cmd()
            .args(["/C", &format!("taskkill /PID {} /F /T", pid)])
            .output();
    }
}

#[cfg(windows)]
fn kill_windows_gateway_listen_ports_all_methods(port: u16) {
    kill_windows_processes_listening_on_port(port);
    kill_windows_processes_listening_on_port_netstat_fallback(port);
}

/// Unix：按端口结束 LISTEN 进程（无 lsof 时静默跳过）
/// 使用 lsof + pgrep 双重检测，并 kill 进程树（-P 杀子进程）避免残留
#[cfg(not(windows))]
fn kill_unix_listeners_on_port(port: u16) {
    // 第一轮：lsof 按端口查找 LISTEN 进程，kill 进程树
    let script = format!(
        "pids=$(lsof -ti tcp:{} -sTCP:LISTEN 2>/dev/null); \
         if [ -n \"$pids\" ]; then \
           for p in $pids; do kill -9 -$p 2>/dev/null || kill -9 $p 2>/dev/null; done; \
         fi; true",
        port
    );
    let _ = Command::new("sh").args(["-c", &script]).output();

    // 第二轮：pgrep 按命令名查找 node gateway 进程（覆盖 lsof 未检测到的情况）
    // 匹配多种启动方式：node dist/entry.js gateway、openclaw-cn、clawdbot-gateway 等
    let script2 = "pids=$(pgrep -f 'node dist/entry(-hidden)?\\.js gateway' 2>/dev/null); \
                    if [ -n \"$pids\" ]; then \
                      for p in $pids; do kill -9 -$p 2>/dev/null || kill -9 $p 2>/dev/null; done; \
                    fi; \
                    pids2=$(pgrep -f 'clawdbot-gateway' 2>/dev/null); \
                    if [ -n \"$pids2\" ]; then \
                      for p in $pids2; do kill -9 -$p 2>/dev/null || kill -9 $p 2>/dev/null; done; \
                    fi; true";
    let _ = Command::new("sh").args(["-c", script2]).output();
}

fn gateway_process_log_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir)
        .join("logs")
        .join(OPENCLAW_GATEWAY_LOG)
}

/// 读取网关日志尾部，供启动失败时提示用户（完整日志仍在「设置」中查看）。
fn read_gateway_log_tail(data_dir: &str, max_chars: usize) -> Option<String> {
    let path = gateway_process_log_path(data_dir);
    let s = std::fs::read_to_string(&path).ok()?;
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() <= max_chars {
        return Some(t.to_string());
    }
    Some(t[t.len() - max_chars..].to_string())
}

fn log_gateway_spawn_banner(data_dir: &str) {
    let log_dir = PathBuf::from(data_dir).join("logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        tracing::warn!("创建网关日志目录失败: {}", e);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(gateway_process_log_path(data_dir))
    {
        let _ = writeln!(
            f,
            "\n======== {} 启动网关进程 =========",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
    }
}

fn attach_gateway_stdio_to_log(cmd: &mut Command, data_dir: &str) -> Result<(), String> {
    let log_dir = PathBuf::from(data_dir).join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建日志目录: {}", e))?;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(gateway_process_log_path(data_dir))
        .map_err(|e| format!("打开网关日志文件: {}", e))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("网关日志句柄: {}", e))?;
    cmd.stdout(Stdio::from(log_file));
    cmd.stderr(Stdio::from(log_err));
    Ok(())
}

/// 快速检测端口是否被占用（单次尝试，500ms 超时）
async fn is_port_listen(port: u16) -> bool {
    let addr = (Ipv4Addr::LOCALHOST, port);
    matches!(
        tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// 等待本机 TCP 端口可连接（网关 HTTP/WS 就绪）。
/// 首次安装后 Node 冷启动、读盘较慢时可能超过 20s，故默认尝试次数较多。
async fn wait_for_local_port_listen(port: u16, max_attempts: u32) -> bool {
    let addr = (Ipv4Addr::LOCALHOST, port);
    for _ in 0..max_attempts {
        match tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(_)) => return true,
            _ => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
    false
}

/// 停止网关相关进程：先按端口清理所有残留监听（包括无状态文件的进程），再按状态文件 PID 结束进程树，最后删除状态文件。
fn stop_gateway_processes_best_effort(data_dir: &str) {
    let status_file = format!("{}/gateway.status", data_dir);
    let port = resolve_gateway_http_port(data_dir);

    // 第一步：按端口清理所有残留监听（无状态文件时也执行，确保关闭网关时子进程全清）
    #[cfg(windows)]
    {
        for p in gateway_listen_ports_to_clear(port) {
            kill_windows_gateway_listen_ports_all_methods(p);
        }
    }

    #[cfg(not(windows))]
    {
        for p in gateway_listen_ports_to_clear(port) {
            kill_unix_listeners_on_port(p);
        }
    }

    // 第二步：按状态文件 PID 再杀一次（覆盖第一步未清的边缘情况）
    // macOS 上 child.id() 返回的是 sh 父进程 PID，需同时杀其子进程树
    if let Ok(content) = std::fs::read_to_string(&status_file) {
        if let Some(pid_str) = content.strip_prefix("pid:") {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                #[cfg(windows)]
                {
                    let out = hidden_cmd::cmd()
                        .args(["/C", &format!("taskkill /PID {} /F /T", pid)])
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {
                            tracing::info!("已结束网关进程树 PID {}", pid);
                        }
                        Ok(o) => {
                            let err = String::from_utf8_lossy(&o.stderr);
                            tracing::warn!("taskkill PID {} 未成功: {}", pid, err.trim());
                        }
                        Err(e) => tracing::warn!("taskkill 调用失败: {}", e),
                    }
                }
                #[cfg(not(windows))]
                {
                    // 先杀进程组（负 PID = 杀整个进程组），覆盖 sh → node 子进程树
                    let _ = Command::new("kill")
                        .args(["-9", &format!("-{}", pid)])
                        .output();
                    // 再杀单个 PID 兜底（进程组可能不匹配时）
                    let _ = Command::new("kill")
                        .args(["-9", &pid.to_string()])
                        .output();
                }
            }
        }
    }

    // 第三步：再次按端口兜底清理（覆盖 /T 未能完全清掉的子进程）
    #[cfg(windows)]
    {
        for p in gateway_listen_ports_to_clear(port) {
            kill_windows_gateway_listen_ports_all_methods(p);
        }
    }

    #[cfg(not(windows))]
    {
        for p in gateway_listen_ports_to_clear(port) {
            kill_unix_listeners_on_port(p);
        }
    }

    // 第四步：兜底 — 杀所有残留的 openclaw gateway node 进程
    #[cfg(not(windows))]
    {
        // 匹配多种进程名：node dist/entry.js gateway（原始命令行）、
        // openclaw-cn（process.title 设置的名称）、clawdbot-gateway（CLI 启动的网关）
        let _ = Command::new("sh")
            .args([
                "-c",
                "pkill -9 -f 'node dist/entry(-hidden)?\\.js gateway' 2>/dev/null; \
                 pkill -9 -f 'openclaw-cn.*gateway' 2>/dev/null; \
                 pkill -9 -f 'clawdbot-gateway' 2>/dev/null; true",
            ])
            .output();
    }

    let _ = std::fs::remove_file(&status_file)
        .map_err(|e| tracing::debug!("删除网关状态文件失败（非致命）: {}", e));
}

#[tauri::command]
pub async fn get_gateway_status(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<GatewayStatus, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let status_file = format!("{}/gateway.status", data_dir);

    let port = resolve_gateway_http_port(&data_dir);
    let mut running = Path::new(&status_file).exists();
    if running {
        let addr = (Ipv4Addr::LOCALHOST, port);
        match tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(_)) => {}
            _ => {
                // 刚写入状态文件后端口可能尚未 listen，短窗口内不误删
                let recently_written = std::fs::metadata(&status_file)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        SystemTime::now()
                            .duration_since(t)
                            .unwrap_or(Duration::ZERO)
                            < Duration::from_secs(15)
                    })
                    .unwrap_or(true);
                if !recently_written {
                    running = false;
                    let _ = tokio::fs::remove_file(&status_file).await;
                }
            }
        }
    }

    let version = if running {
        let openclaw_dir = format!("{}/openclaw-cn", data_dir);
        let pkg_path = format!("{}/package.json", openclaw_dir);
        tokio::fs::read_to_string(&pkg_path)
            .await
            .ok()
            .and_then(|content| {
                serde_json::from_str::<serde_json::Value>(&content)
                    .ok()
                    .and_then(|v| {
                        v.get("version")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                    })
            })
    } else {
        None
    };

    let uptime_seconds = if running {
        if let Ok(metadata) = std::fs::metadata(&status_file) {
            if let Ok(modified) = metadata.modified() {
                SystemTime::now()
                    .duration_since(modified)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        }
    } else {
        0
    };

    Ok(GatewayStatus {
        running,
        version,
        port,
        uptime_seconds,
        memory_mb: 0.0,
        instances_running: 0,
    })
}

/// 返回网关 WebSocket 连接信息（URL + token），供前端直连使用
#[tauri::command]
pub async fn get_gateway_ws_info(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<serde_json::Value, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let port = resolve_gateway_http_port(&data_dir);
    let token = resolve_gateway_ws_token(&data_dir);
    Ok(serde_json::json!({
        "url": format!("ws://127.0.0.1:{}", port),
        "token": token,
    }))
}

/// 供 `start_gateway` 与「仅路径」场景复用（例如新建微信实例后需在无 `State` 时重启网关）。
pub async fn start_gateway_with_data_dir_path(data_dir: &str) -> Result<String, String> {
    let openclaw_dir = format!("{}/openclaw-cn", data_dir);

    // 安全网 1：reap 任何 zombie 子进程
    // 上次启动的 gateway 若被 SIGKILL 而父进程没及时 wait，PID 会变成 (node) 僵尸。
    // openclaw-cn 的 lock 库用 kill(pid, 0) 检测存活 → 僵尸仍被认为"活"，导致新进程
    // 等 5s 后报 "lock timeout"。这里用 SIGCHLD 通知父进程 reap 所有 zombie。
    #[cfg(unix)]
    reap_zombie_children();

    // 安全网 2：每次启动网关前都尝试清理一次冲突的全局 launchd 服务
    // （com.clawdbot.gateway 来自旧的 `npm install -g @dragonforce2010/openclaw-cn`，
    // 它会与本 app 抢端口 18789 并持续刷 "Missing config" 错误）。
    crate::commands::installer::cleanup_stale_clawdbot_launchd_service();

    if !Path::new(&openclaw_dir).exists() {
        return Err("OpenClaw-CN 未安装，请先完成安装向导".to_string());
    }

    let entry = Path::new(&openclaw_dir).join("dist").join("entry.js");
    if !entry.exists() {
        return Err("openclaw-cn 安装不完整：缺少 dist/entry.js".to_string());
    }

    // 确保 entry-hidden.js 存在（隐藏子进程窗口的 wrapper）
    let hidden_entry = Path::new(&openclaw_dir).join("dist").join("entry-hidden.js");
    if !hidden_entry.exists() {
        let hidden_js = r#"import cp from "node:child_process";
const oSpawn=cp.spawn,oExec=cp.exec,oExecFile=cp.execFile,oFork=cp.fork;
const w=(o)=>o&&typeof o==="object"&&!("windowsHide"in o)?{...o,windowsHide:true}:o||{windowsHide:true};
cp.spawn=function(c,a,o){if(typeof a==="function"){o=a;a=[]}if(typeof o==="function")o={};return oSpawn(c,a||[],w(o))};
cp.exec=function(c,o,b){if(typeof o==="function"){b=o;o={}}return oExec(c,w(o||{}),b)};
cp.execFile=function(c,a,o,b){if(typeof a==="function"){b=a;a=[];o={}}if(typeof o==="function"){b=o;o={}}return oExecFile(c,a||[],w(o||{}),b)};
cp.fork=function(m,a,o){return oFork(m,a||[],w(o||{}))};
await import("./entry.js");
"#;
        let _ = tokio::fs::write(&hidden_entry, hidden_js).await;
    }

    sync_openclaw_config_from_manager(data_dir).await?;

    // 未配置默认大模型时阻止启动：避免网关静默回退使用 Claude（或上游默认），导致用户以为已配的模型未生效。
    // OpenClaw `resolveConfiguredModelRef` 只在 `agents.defaults.model.primary` 存在时覆盖上游默认，
    // 若 models.yaml 的 default_model 为空，则不会写入该字段，网关会保留上游的 anthropic/... 默认。
    if read_default_model_primary(data_dir).is_none() {
        let models_path = crate::commands::gateway::models_yaml_path(data_dir);
        let path_display = models_path.display().to_string();
        let hint = if models_path.exists() {
            format!(
                "程序正在读取的 models.yaml 为：\n{}\n\n\
                 若你已在别处编辑过配置但仍提示此项，多半是编辑了错误路径（例如安装目录下的 resources\\data，那是只读模板）。\
                 请用「模型配置」页保存一次，或设置环境变量 OPENCLAW_CN_DATA_DIR 指向你的数据目录；\
                 便携模式可在 exe 同目录放置空文件 OpenClaw-CN.portable，数据将写入 exe 旁 data\\ 目录。\n\n\
                 若文件编码为 UTF-16，请另存为 UTF-8；查看日志中「解析 models.yaml 失败」可确认是否 YAML 损坏。",
                path_display
            )
        } else {
            format!(
                "models.yaml 不存在：\n{}\n\n请完成向导「大模型配置」或从备份恢复。",
                path_display
            )
        };
        return Err(format!(
            "未配置默认大模型，网关无法启动。\n\n{}\n\n\
             请在「大模型配置」中选择供应商与模型、勾选「设为全局默认」并保存；\
             保存后 default_model.provider 与 model_name 均不能为空。",
            hint
        ));
    }

    if let Err(e) =
        crate::commands::installer::patch_openclaw_gateway_localhost_usage(&openclaw_dir).await
    {
        tracing::warn!(
            "网关用量本机授权补丁未应用（用量页可能提示缺少权限）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_sessions_usage_aggregate_fix(&openclaw_dir).await
    {
        tracing::warn!(
            "sessions.usage 汇总补丁未应用（用量页可能仍只统计前 N 个会话）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_sessions_usage_session_display_fallbacks(&openclaw_dir)
            .await
    {
        tracing::warn!(
            "sessions.usage 会话渠道/模型展示回退未应用（最近会话表可能仍显示 —）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_session_cost_usage_utc_and_discover(&openclaw_dir).await
    {
        tracing::warn!(
            "session-cost-usage.js UTC + discoverAllSessions 补丁未应用（日趋势可能少 1-2 天）: {}",
            e
        );
    }
    if let Err(e) = crate::commands::installer::patch_sessions_usage_all_agents(&openclaw_dir).await
    {
        tracing::warn!(
            "sessions.usage 全 agent 发现补丁未应用（28/29 号数据可能仍缺失）: {}",
            e
        );
    }
    // 修复 npm 包缺失的 dist 文件（如 openclaw-weixin.js 等导致的 ERR_MODULE_NOT_FOUND）
    if let Err(e) =
        crate::commands::installer::patch_openclaw_broken_modules(&openclaw_dir).await
    {
        tracing::warn!(
            "openclaw-cn 损坏模块补丁未应用（网关可能因 ERR_MODULE_NOT_FOUND 启动失败）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_shell_utils_drop_node_powershell(&openclaw_dir).await
    {
        tracing::warn!(
            "Windows shell-utils node-e 补丁未应用（exec 读 .env 等可能仍失败）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_shell_utils_windows_exec_cmd_quoting(&openclaw_dir).await
    {
        tracing::warn!(
            "Windows exec type 引号补丁未应用（tools 读文件可能失败）: {}",
            e
        );
    }
    if let Err(e) =
        crate::commands::installer::patch_bash_tools_exec_windows_command_normalize(&openclaw_dir)
            .await
    {
        tracing::warn!("Windows bash-tools.exec 规范化补丁未应用: {}", e);
    }
    if let Err(e) =
        crate::commands::installer::patch_shell_utils_windows_bat_exec_normalize(&openclaw_dir)
            .await
    {
        tracing::warn!("Windows .bat/cmd exec 规范化补丁未应用: {}", e);
    }

    // 按 instances.yaml 自动准备通道插件（复制 extensions、依赖、编译 dist），并同步 plugins.load.paths
    crate::commands::plugin::ensure_plugins_for_enabled_instances(data_dir).await;

    let listen_port = resolve_gateway_http_port(data_dir);

    // 检查网关是否已在运行（状态文件 + 端口双重检测），避免重复启动
    {
        let status = get_gateway_status_internal(data_dir).await.unwrap_or_default();
        if status.running {
            info!("网关已在运行（端口 {}），跳过启动", listen_port);
            return Ok(format!("网关已在运行，端口 {}", listen_port));
        }
        // 状态文件可能尚未写入，直接检测端口
        if is_port_listen(listen_port).await {
            info!("端口 {} 已被占用，网关可能已在运行，创建状态文件", listen_port);
            // 创建状态文件以便 get_gateway_status 能正确检测
            let status_file = format!("{}/gateway.status", data_dir);
            let _ = tokio::fs::write(&status_file, "pid:external").await;
            return Ok(format!("网关已在运行，端口 {}", listen_port));
        }
    }

    // 启动前清理残留进程，避免端口冲突
    for round in 1..=5 {
        info!("启动前：清理残留进程（第{}轮）", round);
        stop_gateway_processes_best_effort(data_dir);

        // 额外清理：杀掉所有 clawdbot-gateway 和僵尸 node 进程
        #[cfg(not(windows))]
        {
            let _ = Command::new("sh").args([
                "-c",
                "pkill -9 -f 'clawdbot-gateway' 2>/dev/null; \
                 pkill -9 -f 'node dist/entry.*gateway' 2>/dev/null; \
                 pkill -9 -f 'openclaw-cn.*gateway' 2>/dev/null; true",
            ]).output();
        }

        tokio::time::sleep(Duration::from_millis(1000)).await;

        let port_free = !is_port_listen(listen_port).await;
        if port_free {
            info!("端口 {} 已释放，可以启动网关", listen_port);
            break;
        }
        tracing::warn!("端口 {} 仍被占用，进行第{}轮清理", listen_port, round);
    }

    log_gateway_spawn_banner(data_dir);

    let (_, token, _) = read_app_gateway_from_yaml(data_dir);
    let config_abs = abs_path_str(&openclaw_json_path(data_dir))?;
    let state_dir = openclaw_state_dir(data_dir);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .map_err(|e| format!("创建 OpenClaw 状态目录失败: {}", e))?;
    let state_abs = abs_path_str(&state_dir)?;

    let mut child =
        spawn_gateway_process(&openclaw_dir, &config_abs, &state_abs, &token, data_dir)?;

    let status_file = format!("{}/gateway.status", data_dir);
    tokio::fs::write(&status_file, format!("pid:{}", child.id()))
        .await
        .map_err(|e| format!("写入状态文件失败: {}", e))?;

    // listen_port 已在清理循环中声明
    // 约 100 × (500ms 连接超时 + 150ms 间隔) ≈ 65s 量级上限，覆盖冷启动
    if !wait_for_local_port_listen(listen_port, 100).await {
        let _ = tokio::fs::remove_file(&status_file).await;
        #[cfg(windows)]
        {
            for p in gateway_listen_ports_to_clear(listen_port) {
                kill_windows_gateway_listen_ports_all_methods(p);
            }
        }
        let exit_note = match child.try_wait() {
            Ok(Some(status)) => format!("网关 Node 进程已退出（退出码 {:?}）。", status.code()),
            Ok(None) => {
                "网关进程仍在运行，但在预期时间内未在 127.0.0.1 上接受 TCP 连接。".to_string()
            }
            Err(e) => format!("无法查询子进程状态: {}。", e),
        };
        let log_tail = read_gateway_log_tail(data_dir, 1200)
            .map(|t| format!("\n\n—— logs/{} 末尾 ——\n{}", OPENCLAW_GATEWAY_LOG, t))
            .unwrap_or_default();
        return Err(format!(
            "启动失败：{} 当前检测端口: {}。{}\n\n若端口被占用请先「关闭网关」或结束占用该端口的程序；若进程已崩溃请根据上方日志排查 openclaw.json / 模型配置 / Node 版本（需 ≥22）。仍失败请检查防火墙。",
            exit_note, listen_port, log_tail
        ));
    }

    info!(
        "网关启动成功，PID: {}，监听端口 {}",
        child.id(),
        listen_port
    );
    Ok(format!(
        "网关启动成功，PID: {}，端口 {}",
        child.id(),
        listen_port
    ))
}

/// 新建/更新实例并写入 `openclaw.json` 后，若网关进程仍在使用旧配置，新通道（如 openclaw-weixin / qqbot / wecom）不会生效。
/// 仅在检测到网关已运行时做一次「停 → 起」，与 UI「重启网关」等价。
pub async fn restart_gateway_if_running_for_wechat_config(data_dir: &str) -> Result<(), String> {
    let status = get_gateway_status_internal(data_dir).await?;
    if !status.running {
        return Ok(());
    }
    info!("通道配置已写入且网关正在运行：将重启网关以加载最新 openclaw.json 与插件");
    stop_gateway_processes_best_effort(data_dir);
    tokio::time::sleep(Duration::from_secs(2)).await;
    start_gateway_with_data_dir_path(data_dir).await?;
    Ok(())
}

#[tauri::command]
pub async fn start_gateway(data_dir: tauri::State<'_, crate::AppState>) -> Result<String, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let port = resolve_gateway_http_port(&data_dir);

    // 先检查状态文件
    let status = get_gateway_status_internal(&data_dir).await.unwrap_or_default();
    if status.running {
        info!("网关已在运行（端口 {}），跳过重复启动", status.port);
        return Ok(format!("网关已在运行，端口 {}", status.port));
    }

    // 状态文件可能不存在，直接检测端口
    if is_port_listen(port).await {
        info!("端口 {} 已被占用，网关可能已在运行，创建状态文件", port);
        // 创建状态文件以便 get_gateway_status 能正确检测
        let status_file = format!("{}/gateway.status", data_dir);
        let _ = tokio::fs::write(&status_file, "pid:external").await;
        return Ok(format!("网关已在运行，端口 {}", port));
    }

    info!("启动网关...");
    start_gateway_with_data_dir_path(&data_dir).await
}

/// 直接运行 `node dist/entry.js gateway`（npm 包通常不含 `scripts/run-node.mjs`，`npm run start` 无法启动网关）
fn spawn_gateway_process(
    openclaw_dir: &str,
    config_abs: &str,
    state_abs: &str,
    gateway_token: &str,
    data_dir: &str,
) -> Result<std::process::Child, String> {
    #[cfg(windows)]
    {
        // 优先级：1. 系统 PATH 中的 node → 2. 内置 node
        // 注意：resolve_node 已验证系统 node 可用（通过 Command::new("node") 测试），
        // 所以 is_system=true 时 node_exe 一定存在；只有内置时才需要 .exists() 检查
        let (node_exe, is_system) = resolve_node(data_dir);
        let bundled_exists = crate::env_paths::node_exists(&crate::env_paths::env_root(data_dir));

        // 系统 node 由 resolve_node 内部验证过；内置 node 才需要 .exists() 检查
        if !is_system && !node_exe.exists() {
            return Err(format!(
                "Node.js 未找到（系统: {}, 内置: {}）。\
                 请先在环境检查中安装 Node.js，或将官方 zip 解压到 data/env/node。",
                if is_system { "可用" } else { "不可用" },
                if bundled_exists { "存在" } else { "不存在" }
            ));
        }

        let system_path = std::env::var("PATH").unwrap_or_default();
        let (git_exe_path, git_is_system) = resolve_git(data_dir);

        // 只有使用内置 node/git 时才 prepend 其目录
        let new_path = if is_system {
            system_path
        } else {
            let node_parent = node_exe
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let git_prepend = if git_is_system || !git_exe_path.exists() {
                String::new()
            } else {
                git_exe_path
                    .parent()
                    .map(|p| format!("{};", p.to_string_lossy()))
                    .unwrap_or_default()
            };
            format!("{}{}{}", git_prepend, node_parent, system_path)
        };

        let mut cmd = hidden_cmd::hidden_command(&node_exe);
        let gw_http_port = resolve_gateway_http_port(data_dir);
        cmd.args(["dist/entry-hidden.js", "gateway"])
            .current_dir(openclaw_dir)
            .env("PATH", &new_path)
            .env("OPENCLAW_CONFIG_PATH", config_abs)
            .env("OPENCLAW_STATE_DIR", state_abs)
            .env("OPENCLAW_GATEWAY_TOKEN", gateway_token)
            .env("OPENCLAW_GATEWAY_PORT", gw_http_port.to_string())
            .env("OPENCLAW_NO_RESPAWN", "1");

        if let (Some(provider), Some(key)) = (
            read_default_model_provider(data_dir),
            read_default_model_api_key(data_dir),
        ) {
            if let Some(var) = provider_api_key_env_var(&provider) {
                cmd.env(var, key);
            }
        }

        attach_gateway_stdio_to_log(&mut cmd, data_dir)?;

        cmd.spawn().map_err(|e| {
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                "Node.js 未找到，请确认已在系统 PATH 中（运行 `node -v` 验证），或重新安装 Node.js"
            } else {
                "启动网关失败"
            };
            format!("{}: {}", hint, e)
        })
    }

    #[cfg(not(windows))]
    {
        // Mac GUI 子进程不会继承用户的完整 PATH（只有 /usr/bin:/bin:/usr/sbin:/sbin），
        // 找不到 node / git / openclaw-cn 的 bin 工具。必须用 build_deps_env_path 主动
        // prepend 内置或系统 node/git 目录（同时追加 /usr/local/bin 和 /opt/homebrew/bin），
        // 否则子进程 `sh -c "node ..."` 会 `sh: node: command not found` 然后立刻退出。
        let new_path = crate::env_paths::build_deps_env_path(data_dir);

        let mut c = Command::new("sh");
        c.arg("-c")
            .arg("node dist/entry.js gateway")
            .current_dir(openclaw_dir)
            .env("PATH", &new_path)
            .env("OPENCLAW_CONFIG_PATH", config_abs)
            .env("OPENCLAW_STATE_DIR", state_abs)
            .env("OPENCLAW_GATEWAY_TOKEN", gateway_token)
            .env(
                "OPENCLAW_GATEWAY_PORT",
                resolve_gateway_http_port(data_dir).to_string(),
            )
            .env("OPENCLAW_NO_RESPAWN", "1")
            .env("OPENCLAW_ALLOW_MULTI_GATEWAY", "1");

        if let (Some(provider), Some(key)) = (
            read_default_model_provider(data_dir),
            read_default_model_api_key(data_dir),
        ) {
            if let Some(var) = provider_api_key_env_var(&provider) {
                c.env(var, key);
            }
        }

        attach_gateway_stdio_to_log(&mut c, data_dir)?;

        c.spawn().map_err(|e| format!("启动网关失败: {}", e))
    }
}

#[tauri::command]
pub async fn open_openclaw_console(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let data_dir = data_dir.inner().get_data_dir();
    sync_openclaw_config_from_manager(&data_dir).await?;
    let port = resolve_gateway_http_port(&data_dir);
    let token = resolve_gateway_ws_token(&data_dir);
    let token_trim = token.trim();
    let base = format!("http://127.0.0.1:{}/control-ui", port);
    // 与 `openclaw-cn dashboard` 一致：Control UI 需带 `?token=` 才会把令牌写入 localStorage 并完成 WS 握手，
    // 否则浏览器直接打开裸 URL 会报 1008 gateway token missing。
    let open_url = if token_trim.len() >= 16 {
        format!("{}?token={}", base, urlencoding::encode(token_trim))
    } else {
        base.clone()
    };
    crate::commands::system::open_url_in_default_browser(&open_url)?;
    Ok(if token_trim.len() >= 16 {
        format!("已打开 OpenClaw 控制台：{}（链接已附带网关令牌）", base)
    } else {
        format!(
            "已打开 OpenClaw 控制台：{}。未检测到有效网关令牌，请在控制台「设置」中粘贴与 Manager 提示一致的令牌。",
            base
        )
    })
}

#[tauri::command]
pub async fn stop_gateway(data_dir: tauri::State<'_, crate::AppState>) -> Result<String, String> {
    info!("停止网关...");

    let data_dir = data_dir.inner().get_data_dir();
    stop_gateway_processes_best_effort(&data_dir);
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    info!("网关已停止");
    Ok("网关已停止".to_string())
}

#[tauri::command]
pub async fn restart_gateway(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    info!("重启网关...");

    let _ = stop_gateway(data_dir.clone()).await;
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    start_gateway(data_dir).await
}

/// 从 OpenClaw 网关 WebSocket JSON-RPC 拉取用量（`usage.*` / `sessions.usage*`）。
/// 用量接口不在 HTTP 上提供；此前误用 HTTP 会拿到 Control UI 的 HTML，导致 JSON 解析失败。
/// type: "status" | "cost" | "sessions" | "sessions-timeseries"
#[tauri::command]
pub async fn get_gateway_usage(
    data_dir: tauri::State<'_, crate::AppState>,
    usage_type: String,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let data_dir = data_dir.inner().get_data_dir();

    let status = get_gateway_status_internal(&data_dir).await?;
    if !status.running {
        return Err("网关未运行，请先启动网关".to_string());
    }

    let port = status.port;
    let token = crate::commands::gateway::resolve_gateway_ws_token(&data_dir);

    // 用量相关 RPC 仅通过网关 WebSocket 提供；HTTP 同路径会落入 Control UI SPA，返回 HTML 导致 JSON 解析失败。
    let p = params.unwrap_or(serde_json::Value::Null);
    let (rpc_method, rpc_params): (&str, serde_json::Value) = match usage_type.as_str() {
        "status" => ("usage.status", serde_json::Value::Null),
        "cost" => ("usage.cost", p),
        "sessions" => {
            let mut m = serde_json::Map::new();
            for k in [
                "key",
                "startDate",
                "endDate",
                "limit",
                "includeContextWeight",
            ] {
                if let Some(v) = p.get(k) {
                    m.insert(k.to_string(), v.clone());
                }
            }
            ("sessions.usage", serde_json::Value::Object(m))
        }
        "sessions-timeseries" => {
            let key = p.get("key").and_then(|v| v.as_str()).unwrap_or("");
            ("sessions.usage.timeseries", json!({ "key": key }))
        }
        _ => return Err(format!("未知的用量类型: {}", usage_type)),
    };

    crate::commands::gateway_ws::call_gateway_method(port, &token, rpc_method, rpc_params).await
}

async fn get_gateway_status_internal(data_dir: &str) -> Result<GatewayStatus, String> {
    use std::net::Ipv4Addr;
    use std::path::Path;
    use std::time::SystemTime;

    let status_file = format!("{}/gateway.status", data_dir);

    let port = crate::commands::gateway::resolve_gateway_http_port(data_dir);
    let mut running = Path::new(&status_file).exists();
    if running {
        let addr = (Ipv4Addr::LOCALHOST, port);
        match tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(_)) => {}
            _ => {
                let recently_written = std::fs::metadata(&status_file)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        SystemTime::now()
                            .duration_since(t)
                            .unwrap_or(Duration::ZERO)
                            < Duration::from_secs(15)
                    })
                    .unwrap_or(true);
                if !recently_written {
                    running = false;
                    let _ = tokio::fs::remove_file(&status_file).await;
                }
            }
        }
    }

    let uptime_seconds = if running {
        std::fs::metadata(&status_file)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    } else {
        0
    };

    Ok(GatewayStatus {
        running,
        version: None,
        port,
        uptime_seconds,
        memory_mb: 0.0,
        instances_running: 0,
    })
}

// ── HTTP proxy: 绕过 CORS，由 Rust 代理请求到网关 ──

#[tauri::command]
pub async fn proxy_gateway_chat(
    data_dir: tauri::State<'_, crate::AppState>,
    port: u16,
    model: String,
    messages: Vec<serde_json::Value>,
    stream: Option<bool>,
) -> Result<String, String> {
    let data_base = data_dir.inner().get_data_dir();

    tracing::info!("proxy_gateway_chat: model={}", model);

    // 读取 models.yaml 获取 kuaifan API Key（明文）
    let models_yaml_path = std::path::Path::new(&data_base).join("config").join("models.yaml");
    let api_key = std::fs::read_to_string(&models_yaml_path).ok().and_then(|raw| {
        let doc: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
        let key = doc.get("providers")?.get("kuaifan")?.get("api_key")?.as_str()?;
        if key.starts_with("enc:") {
            crate::services::cipher::get_or_create_cipher_key_sync(&data_base).ok()
                .and_then(|k| decrypt_credential(key, &k))
        } else {
            Some(key.to_string())
        }
    }).unwrap_or_default();

    // 去掉 provider 前缀，只传模型名给上游
    let upstream_model = if let Some(idx) = model.find('/') {
        model[idx+1..].to_string()
    } else {
        model.clone()
    };

    let use_stream = stream.unwrap_or(true);
    let body = serde_json::json!({
        "model": upstream_model,
        "messages": messages,
        "stream": use_stream,
        "max_tokens": 4096,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;

    let mut builder = client
        .post("https://kuaifanio.cn/v1/chat/completions")
        .json(&body);
    if !api_key.is_empty() {
        builder = builder.header("Authorization", format!("Bearer {}", api_key));
    }

    tracing::info!("proxy_gateway_chat: calling kuaifan API model={} stream={}", upstream_model, use_stream);
    let resp = builder.send().await.map_err(|e| {
        tracing::error!("proxy_gateway_chat: request failed: {}", e);
        format!("请求失败: {}", e)
    })?;
    let status = resp.status();

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::error!("proxy_gateway_chat: error response: {}", &text[..text.len().min(300)]);
        return Err(format!("HTTP {}: {}", status.as_u16(), &text[..text.len().min(300)]));
    }

    if use_stream {
        // 流式模式：读取 SSE 并拼接内容
        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取流响应: {}", e))? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();
                if line.starts_with("data: ") {
                    let data = &line[6..];
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if let Some(delta) = json.get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("delta"))
                        {
                            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                full_content.push_str(content);
                            }
                        }
                    }
                }
            }
        }
        // 构造非流式格式响应
        let response = serde_json::json!({
            "id": "chatcmpl-proxy",
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": upstream_model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": full_content
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            }
        });
        let text = serde_json::to_string(&response).unwrap_or_default();
        tracing::info!("proxy_gateway_chat: stream assembled, content_len={}", full_content.len());
        Ok(text)
    } else {
        // 非流式模式
        let text = resp.text().await.map_err(|e| {
            tracing::error!("proxy_gateway_chat: read response failed: {}", e);
            format!("读取响应: {}", e)
        })?;
        tracing::info!("proxy_gateway_chat: HTTP {} body_len={}", status.as_u16(), text.len());
        Ok(text)
    }
}

// ============================================================
// 测试模块：实例 → Agent 映射逻辑
//
// 与 read_channel_patches 对齐：
//   - 多账号通道：binding accountId == agent_id == channel_account_id
//   - 单账号通道：binding accountId 为 "default"，agent_id 为 "{openclaw_channel}-{instance_id}" 规范化
//   - 已禁用实例不参与；排序规则与生产相同（created_at 升序，缺省按稳定顺序）
// ============================================================

#[cfg(test)]
mod instance_agent_mapping_tests {
    use std::collections::HashSet;

    fn yaml_to_openclaw_ch(yaml_channel: &str) -> String {
        match yaml_channel {
            "qq" => "qqbot".to_string(),
            "wxwork" => "wecom".to_string(),
            "wechat_clawbot" => "openclaw-weixin".to_string(),
            _ => yaml_channel.to_string(),
        }
    }

    fn is_single_account_yaml(yaml_channel: &str) -> bool {
        matches!(
            yaml_channel,
            "qq" | "wxwork" | "wechat_clawbot"
        )
    }

    /// 复制 read_channel_patches 的核心逻辑（无文件 I/O），用于独立验证。
    /// 返回 (instance_id, binding_account_id, agent_id)。
    fn compute_agent_ids(yaml_content: &str) -> Vec<(String, String, String)> {
        #[derive(serde::Deserialize)]
        struct InstDoc {
            instances: Vec<InstEntry>,
        }
        #[derive(serde::Deserialize)]
        struct InstEntry {
            id: String,
            enabled: bool,
            channel_type: String,
            #[serde(default)]
            created_at: Option<String>,
        }

        fn normalize_account_id(id: &str) -> String {
            id.trim()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .take(64)
                .collect()
        }

        fn channel_account_id(channel_type: &str, instance_id: &str) -> String {
            match channel_type {
                "feishu" => normalize_account_id(instance_id),
                "qq" | "wxwork" | "wechat_clawbot" | "dingtalk" => "default".to_string(),
                _ => normalize_account_id(&format!("{}-{}", channel_type, instance_id)),
            }
        }

        let doc: InstDoc = serde_yaml::from_str(yaml_content).unwrap();
        let mut enabled: Vec<_> = doc.instances.into_iter().filter(|i| i.enabled).collect();
        enabled.sort_by(|a, b| {
            a.created_at
                .as_deref()
                .unwrap_or("")
                .cmp(b.created_at.as_deref().unwrap_or(""))
        });

        enabled
            .into_iter()
            .map(|inst| {
                let yaml_ch = inst.channel_type.as_str();
                let openclaw_ch = yaml_to_openclaw_ch(yaml_ch);
                let acc = channel_account_id(yaml_ch, &inst.id);
                let binding_id = if is_single_account_yaml(yaml_ch) {
                    "default".to_string()
                } else {
                    acc.clone()
                };
                let agent_id = if is_single_account_yaml(yaml_ch) {
                    normalize_account_id(&format!("{}-{}", &openclaw_ch, inst.id))
                } else {
                    acc.clone()
                };
                (inst.id, binding_id, agent_id)
            })
            .collect()
    }

    /// 验证：单个已启用实例 → agent_id = account_id
    #[test]
    fn test_single_instance_gets_own_agent_id() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "inst_001");
        assert_eq!(result[0].2, "inst_001", "单实例也独占独立 agent");
        assert_eq!(result[0].0, "inst_001");
    }

    /// 验证：同 channel 两实例 → 各用各的 account_id / agent_id
    #[test]
    fn test_same_channel_two_instances() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, "inst_001");
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].1, "inst_002");
        assert_eq!(result[1].2, "inst_002");
    }

    /// 验证：同 channel 三实例
    #[test]
    fn test_same_channel_three_instances() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
  - id: inst_003
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].2, "inst_002");
        assert_eq!(result[2].2, "inst_003");
    }

    /// 验证：不同 channel_type 各自 account_id 不冲突
    #[test]
    fn test_different_channels_have_distinct_agent_ids() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: qq
  - id: inst_003
    enabled: true
    channel_type: wxwork
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].2, "qqbot-inst_002");
        assert_eq!(result[2].2, "wecom-inst_003");
        let ids: HashSet<_> = result.iter().map(|(_, _, a)| a.as_str()).collect();
        assert_eq!(ids.len(), 3);
    }

    /// 验证：混合同 channel 和不同 channel
    #[test]
    fn test_mixed_channels() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
  - id: inst_003
    enabled: true
    channel_type: qq
  - id: inst_004
    enabled: true
    channel_type: feishu
  - id: inst_005
    enabled: true
    channel_type: qq
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].2, "inst_002");
        assert_eq!(result[3].2, "inst_004");
        assert_eq!(result[2].2, "qqbot-inst_003");
        assert_eq!(result[4].2, "qqbot-inst_005");
    }

    /// 验证：禁用实例不参与
    #[test]
    fn test_disabled_instances_ignored() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: false
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
  - id: inst_003
    enabled: false
    channel_type: feishu
  - id: inst_004
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].2, "inst_002");
        assert_eq!(result[0].0, "inst_002");
        assert_eq!(result[1].2, "inst_004");
        assert_eq!(result[1].0, "inst_004");
    }

    /// 验证：空 instances 列表
    #[test]
    fn test_empty_instances() {
        let yaml = r#"instances: []"#;
        let result = compute_agent_ids(yaml);
        assert!(result.is_empty());
    }

    /// 验证：只有禁用的实例
    #[test]
    fn test_all_disabled() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: false
    channel_type: feishu
  - id: inst_002
    enabled: false
    channel_type: qq
"#;
        let result = compute_agent_ids(yaml);
        assert!(result.is_empty());
    }

    /// 验证：非飞书 channel 的 account_id 格式
    #[test]
    fn test_non_feishu_account_id_format() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: qq
  - id: inst_002
    enabled: true
    channel_type: wxwork
  - id: inst_003
    enabled: true
    channel_type: wechat_clawbot
  - id: inst_004
    enabled: true
    channel_type: qq
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].1, "default");
        assert_eq!(result[1].1, "default");
        assert_eq!(result[2].1, "default");
        assert_eq!(result[3].1, "default");
        assert_eq!(result[0].2, "qqbot-inst_001");
        assert_eq!(result[1].2, "wecom-inst_002");
        assert_eq!(result[2].2, "openclaw-weixin-inst_003");
        assert_eq!(result[3].2, "qqbot-inst_004");
        assert_ne!(
            result[0].1, result[0].2,
            "单账号通道 binding 为 default，agent_id 应独立"
        );
    }

    /// 验证：同 channel 多实例在无 created_at 时顺序稳定（与生产 sort 一致）
    #[test]
    fn test_order_by_yaml_appearance() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
  - id: inst_003
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "inst_001");
        assert_eq!(result[0].1, "inst_001");
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].0, "inst_002");
        assert_eq!(result[1].1, "inst_002");
        assert_eq!(result[1].2, "inst_002");
        assert_eq!(result[2].0, "inst_003");
        assert_eq!(result[2].1, "inst_003");
        assert_eq!(result[2].2, "inst_003");
    }

    /// 验证：多实例各自 agent_id 唯一（feishu x2 + qq x1 → 3 条）
    #[test]
    fn test_agent_list_deduplication() {
        let yaml = r#"
instances:
  - id: feishu_01
    index: 1
    enabled: true
    channel_type: feishu
  - id: feishu_02
    index: 2
    enabled: true
    channel_type: feishu
  - id: qq_01
    index: 3
    enabled: true
    channel_type: qq
"#;
        let accounts = compute_agent_ids(yaml);
        assert_eq!(accounts.len(), 3);

        let by_id: std::collections::HashMap<String, _> = accounts
            .iter()
            .map(|(id, acc, agent)| (id.clone(), (acc.clone(), agent.clone())))
            .collect();

        assert_eq!(
            by_id.get("feishu_01").map(|(_, a)| a.as_str()),
            Some("feishu_01"),
            "实际: {:?}",
            by_id
        );
        assert_eq!(
            by_id.get("feishu_02").map(|(_, a)| a.as_str()),
            Some("feishu_02"),
            "实际: {:?}",
            by_id
        );
        assert_eq!(
            by_id.get("qq_01").map(|(_, a)| a.as_str()),
            Some("qqbot-qq_01"),
            "实际: {:?}",
            by_id
        );

        let mut seen: HashSet<String> = HashSet::new();
        let agent_ids_written: Vec<String> = accounts
            .iter()
            .filter_map(|(_, _, agent_id)| {
                if seen.insert(agent_id.clone()) {
                    Some(agent_id.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(agent_ids_written.len(), 3, "实际: {:?}", agent_ids_written);
        assert!(agent_ids_written.contains(&"feishu_01".to_string()));
        assert!(agent_ids_written.contains(&"feishu_02".to_string()));
        assert!(agent_ids_written.contains(&"qqbot-qq_01".to_string()));
    }

    /// 验证：bindings 数量 == 账户数量（每个账户一条 binding）
    #[test]
    fn test_bindings_count_equals_accounts_count() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_002
    enabled: true
    channel_type: feishu
  - id: inst_003
    enabled: true
    channel_type: qq
  - id: inst_004
    enabled: true
    channel_type: wxwork
  - id: inst_005
    enabled: true
    channel_type: wechat_clawbot
"#;
        let accounts = compute_agent_ids(yaml);
        // 每个 account 都有独立 binding
        assert_eq!(accounts.len(), 5, "binding 数应等于已启用账户数");
    }

    /// 验证：飞书实例 accountId 格式为 inst-{instance_id}
    #[test]
    fn test_feishu_account_id_normalization() {
        let yaml = r#"
instances:
  - id: inst_abc_123_XYZ
    enabled: true
    channel_type: feishu
  - id: UPPER_CASE_ID
    enabled: true
    channel_type: feishu
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result[0].1, "inst_abc_123_xyz", "应转小写");
        assert_eq!(result[1].1, "upper_case_id", "应转小写");
    }

    /// 验证：大规模场景（20 实例跨 4 channel，agent_id 全局唯一）
    #[test]
    fn test_large_scale_mixed_channels() {
        let mut instances = String::from("instances:\n");
        let channels = ["feishu", "qq", "wxwork", "wechat_clawbot"];
        for i in 0..20 {
            let ch = channels[i % channels.len()];
            instances.push_str(&format!(
                "  - id: inst_{:03}\n    enabled: true\n    channel_type: {}\n",
                i, ch
            ));
        }
        let result = compute_agent_ids(&instances);
        assert_eq!(result.len(), 20);

        for i in 0..20 {
            let ch = channels[i % channels.len()];
            let (expected_binding, expected_agent) = match ch {
                "feishu" => {
                    let a = format!("inst_{:03}", i);
                    (a.clone(), a)
                },
                "qq" => ("default".to_string(), format!("qqbot-inst_{:03}", i)),
                "wxwork" => ("default".to_string(), format!("wecom-inst_{:03}", i)),
                "wechat_clawbot" => ("default".to_string(), format!("openclaw-weixin-inst_{:03}", i)),
                _ => unreachable!(),
            };
            assert_eq!(result[i].1, expected_binding, "binding accountId i={}", i);
            assert_eq!(result[i].2, expected_agent, "agent_id i={}", i);
        }

        let mut all: Vec<_> = result.iter().map(|(_, _, a)| a.clone()).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 20);
    }

    /// 验证：相同 instance id 在不同 channel 下 account_id / agent_id 仍不同
    #[test]
    fn test_no_cross_channel_collision() {
        let yaml = r#"
instances:
  - id: inst_001
    enabled: true
    channel_type: feishu
  - id: inst_001
    enabled: true
    channel_type: qq
"#;
        let result = compute_agent_ids(yaml);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, "inst_001");
        assert_eq!(result[1].1, "default");
        assert_ne!(result[0].1, result[1].1, "binding accountId 应互不重复");
        assert_eq!(result[0].2, "inst_001");
        assert_eq!(result[1].2, "qqbot-inst_001");
        assert_ne!(result[0].2, result[1].2);
    }

    /// 验证：同步时 agents.list 清理逻辑（inst- 前缀 + managed 键）
    #[test]
    fn test_old_inst_agents_cleaned() {
        use std::collections::HashSet;

        let old_agents = serde_json::json!([
            { "id": "main", "name": "Main Agent" },
            { "id": "custom-agent", "name": "Custom" },
            { "id": "inst-inst_001", "name": "Old Feishu" },
            { "id": "inst-old-deleted", "name": "Deleted" }
        ]);

        let mut managed_keys: HashSet<String> = HashSet::new();
        managed_keys.insert("inst-inst_001".to_string());
        managed_keys.insert("inst-old-deleted".to_string());
        let current_agent_ids: HashSet<String> = HashSet::new();

        let cleaned: Vec<_> = old_agents
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                let Some(id) = e.get("id").and_then(|v| v.as_str()) else {
                    return true;
                };
                if id.starts_with("inst-") {
                    return false;
                }
                if managed_keys.contains(id) {
                    return false;
                }
                if id == "main" && !current_agent_ids.contains("main") {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        assert_eq!(cleaned.len(), 1);
        assert_eq!(
            cleaned[0].get("id").and_then(|v| v.as_str()),
            Some("custom-agent")
        );
    }

    /// 验证：bindings 清理（inst- accountId + managed 键）
    #[test]
    fn test_old_inst_bindings_cleaned() {
        use std::collections::HashSet;

        let old_bindings = serde_json::json!([
            { "agentId": "main", "match": { "channel": "feishu", "accountId": "feishu-account" } },
            { "agentId": "custom", "match": { "channel": "qq", "accountId": "qq-account" } },
            { "agentId": "inst-inst_001", "match": { "channel": "feishu", "accountId": "inst-inst_001" } },
            { "agentId": "x", "match": { "channel": "qq", "accountId": "inst-old-deleted" } }
        ]);

        let mut managed_keys: HashSet<String> = HashSet::new();
        managed_keys.insert("inst-old-deleted".to_string());

        let cleaned: Vec<_> = old_bindings
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                let acc = e
                    .get("match")
                    .and_then(|m| m.get("accountId"))
                    .and_then(|v| v.as_str());
                if let Some(a) = acc {
                    if a.starts_with("inst-") {
                        return false;
                    }
                    if managed_keys.contains(a) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        assert_eq!(cleaned.len(), 2);
        assert_eq!(
            cleaned[0]
                .get("match")
                .and_then(|m| m.get("accountId"))
                .and_then(|v| v.as_str()),
            Some("feishu-account")
        );
        assert_eq!(
            cleaned[1]
                .get("match")
                .and_then(|m| m.get("accountId"))
                .and_then(|v| v.as_str()),
            Some("qq-account")
        );
    }

}
