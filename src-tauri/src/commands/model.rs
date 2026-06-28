// 模型管理命令



use crate::models::ModelProvider;

use crate::services::cipher::CIPHER_PREFIX;

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use tokio::fs::OpenOptions;

use tokio::io::AsyncWriteExt;

use tracing::info;



#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct TokenUsageRecord {

    pub ts: String,

    pub provider: String,

    pub model: String,

    pub prompt_tokens: u32,

    pub completion_tokens: u32,

    pub total_tokens: u32,

    pub source: String,

}



fn usage_file_path(data_dir: &str) -> PathBuf {

    PathBuf::from(data_dir)

        .join("metrics")

        .join("token_usage.jsonl")

}



/// 与网关 `read_default_model_primary` 使用同一套读取逻辑（支持 UTF-8 / UTF-16 LE/BE 带 BOM）。

fn read_models_yaml_text_for_manager(data_dir: &str) -> Result<String, String> {

    let path = PathBuf::from(data_dir).join("config").join("models.yaml");

    crate::commands::gateway::read_models_yaml_raw_utf8_or_utf16(path.as_path()).ok_or_else(|| {

        format!(

            "读取 models.yaml 失败（{}）。文件可能不存在、被占用，或编码无法识别（请用 UTF-8 保存）。",

            path.display()

        )

    })

}



async fn write_token_usage(

    data_dir: &str,

    provider: &str,

    model: &str,

    prompt_tokens: u32,

    completion_tokens: u32,

    total_tokens: u32,

    source: &str,

) -> Result<(), String> {

    let file_path = usage_file_path(data_dir);

    tokio::fs::create_dir_all(file_path.parent().unwrap())

        .await

        .map_err(|e| format!("创建目录失败: {}", e))?;



    let record = TokenUsageRecord {

        ts: chrono::Utc::now().to_rfc3339(),

        provider: provider.to_string(),

        model: model.to_string(),

        prompt_tokens,

        completion_tokens,

        total_tokens,

        source: source.to_string(),

    };



    let line = serde_json::to_string(&record).map_err(|e| format!("序列化失败: {}", e))?;

    let mut file = OpenOptions::new()

        .create(true)

        .append(true)

        .open(&file_path)

        .await

        .map_err(|e| format!("打开文件失败: {}", e))?;



    file.write_all(format!("{}\n", line).as_bytes())

        .await

        .map_err(|e| format!("写入文件失败: {}", e))?;

    file.sync_all()

        .await

        .map_err(|e| format!("写入文件失败（sync）: {}", e))?;



    Ok(())

}



#[tauri::command]

pub async fn list_providers() -> Result<Vec<ModelProvider>, String> {

    // 返回所有模型供应商列表
    // 快泛API 的模型数量从 https://kuaifanio.cn/api/pricing 动态获取（5s 超时）
    let (kf_free, kf_total) = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fetch_models_from_pricing_api(),
    ).await
    {
        Ok(Ok(models)) if !models.is_empty() => {
            let free = models.iter().filter(|m| m.is_free).count();
            let total = models.len();
            tracing::info!("list_providers: KuaiFan {} free / {} total from pricing page", free, total);
            (free, total)
        }
        _ => {
            tracing::info!("list_providers: KuaiFan pricing page unavailable, using defaults");
            (5, 50)
        }
    };

    let providers = vec![

        ModelProvider {

            id: "kuaifan".to_string(),

            name: "快泛API".to_string(),

            enabled: true,

            api_key_configured: true,

            free_models_count: kf_free,

            total_models_count: kf_total,

        },

        ModelProvider {

            id: "openai".to_string(),

            name: "OpenAI".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 20,

        },

        ModelProvider {

            id: "anthropic".to_string(),

            name: "Claude（Anthropic）".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 10,

        },

        ModelProvider {

            id: "google".to_string(),

            name: "Google Gemini".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 10,

        },

        ModelProvider {

            id: "deepseek".to_string(),

            name: "DeepSeek".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 5,

        },

        ModelProvider {

            id: "minimax".to_string(),

            name: "MiniMax（M2.7 / M3 · 海螺）".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 24,

        },

        ModelProvider {

            id: "volc_ark".to_string(),

            name: "火山方舟 · 豆包".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 19,

        },

        ModelProvider {

            id: "nvidia".to_string(),

            name: "NVIDIA NIM".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 15,

        },

        ModelProvider {

            id: "xiaomi".to_string(),

            name: "小米 MiMo".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 3,

        },

        ModelProvider {

            id: "baidu".to_string(),

            name: "百度文心一言".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 1,

            total_models_count: 10,

        },

        ModelProvider {

            id: "aliyun".to_string(),

            name: "阿里通义千问".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 10,

        },

        ModelProvider {

            id: "zhipu".to_string(),

            name: "智谱 GLM".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 1,

            total_models_count: 8,

        },

        ModelProvider {

            id: "moonshot".to_string(),

            name: "Kimi（月之暗面）".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 1,

            total_models_count: 8,

        },

        ModelProvider {

            id: "grok".to_string(),

            name: "Grok (xAI)".to_string(),

            enabled: false,

            api_key_configured: false,

            free_models_count: 0,

            total_models_count: 5,

        },

        ModelProvider {

            id: "ollama".to_string(),

            name: "Ollama 本地模型".to_string(),

            enabled: false,

            api_key_configured: true,

            free_models_count: 100,

            total_models_count: 100,

        },

    ];



    Ok(providers)

}



#[tauri::command]

pub async fn get_provider_config(

    data_dir: tauri::State<'_, crate::AppState>,

    provider_id: String,

) -> Result<serde_json::Value, String> {

    let data_dir = data_dir.inner().get_data_dir();



    let content = read_models_yaml_text_for_manager(&data_dir)?;



    // 解析 models.yaml 中指定 provider_id 的配置块

    let lines: Vec<&str> = content.lines().collect();

    let mut in_provider_block = false;

    let mut block_lines: Vec<String> = Vec::new();

    let target = format!("{}:", provider_id);



    for line in lines {

        let trimmed = line.trim();

        // 匹配顶级 provider 块（如 `kuaifan:` 或 `volc_ark:`），

        // 但排除列表项（如 `    - id: "volc_ark-xxx"` 中的 volc_ark）。

        if !trimmed.starts_with('-')

            && !trimmed.starts_with("  -")

            && trimmed.starts_with(&target)

            && !trimmed.starts_with("default_model:")

        {

            in_provider_block = true;

        }

        if in_provider_block {

            // 遇到下一个 provider 定义或顶级 key 时停止

            if block_lines.len() > 1

                && (line.trim().starts_with('-')

                    || (!line.starts_with("  ")

                        && !line.starts_with('\t')

                        && !line.trim().is_empty()))

            {

                break;

            }

            block_lines.push(line.to_string());

        }

    }



    if block_lines.is_empty() {

        // 文件中无此供应商，返回默认值

        return Ok(serde_json::json!({

            "id": provider_id,

            "enabled": false,

            "api_key": "",

            "models": []

        }));

    }



    // 从 block_lines 中提取 api_key、enabled 和代理设置，并将加密的凭据解密后返回给前端

    let mut api_key = String::new();

    let mut enabled = false;

    let mut proxy_url = String::new();

    let mut proxy_username = String::new();

    let mut proxy_password = String::new();



    for line in &block_lines {

        let trimmed = line.trim();

        if trimmed.starts_with("api_key:") {

            api_key = trimmed

                .split(':')

                .nth(1)

                .unwrap_or("")

                .trim()

                .trim_matches('"')

                .to_string();

        } else if trimmed.starts_with("enabled:") {

            if let Some(val) = trimmed.split(':').nth(1) {

                enabled = val.trim() == "true";

            }

        } else if trimmed.starts_with("proxy_url:") {

            proxy_url = trimmed

                .split(':')

                .nth(1)

                .unwrap_or("")

                .trim()

                .trim_matches('"')

                .to_string();

        } else if trimmed.starts_with("proxy_username:") {

            proxy_username = trimmed

                .split(':')

                .nth(1)

                .unwrap_or("")

                .trim()

                .trim_matches('"')

                .to_string();

        } else if trimmed.starts_with("proxy_password:") {

            proxy_password = trimmed

                .split(':')

                .nth(1)

                .unwrap_or("")

                .trim()

                .trim_matches('"')

                .to_string();

        }

    }



    // 解密返回给前端的 api_key（前端不需要知道加密格式）

    if api_key.starts_with(CIPHER_PREFIX) {

        let api_key_clone = api_key.clone();

        let data_dir_str = data_dir.clone();

        let key = match crate::services::cipher::get_or_create_cipher_key_sync(&data_dir_str) {

            Ok(k) => k,

            Err(e) => {

                tracing::warn!("无法获取解密密钥: {}，返回原值", e);

                let fallback_key = [0u8; 32];

                fallback_key

            }

        };

        api_key = crate::services::cipher::decrypt_credential(&api_key_clone, &key)

            .unwrap_or_else(|| api_key_clone);

    }



    Ok(serde_json::json!({

        "id": provider_id,

        "enabled": enabled,

        "api_key": api_key,

        "proxy_url": proxy_url,

        "proxy_username": proxy_username,

        "proxy_password": proxy_password,

        "models": []

    }))

}



/// 将 models.yaml 内容中的指定 provider api_key 进行 upsert，返回新内容。

/// 当 provider 块已存在时替换 api_key: 行；当块存在但无 api_key 时追加一行；

/// 当块不存在时，查找 providers: 块并在块内追加新 provider（不在根级追加）。

/// UI 供应商 ID → models.yaml 中实际 key 的别名映射。

/// 模板中 volcengine，UI 中 volc_ark；需要相互回退。

fn yaml_id_alias(id: &str) -> Option<&'static str> {

    match id {

        "volc_ark" => Some("volcengine"),

        "volcengine" => Some("volc_ark"),

        _ => None,

    }

}



fn upsert_provider_api_key(content: &str, provider_id: &str, api_key: &str) -> String {

    let target_header = format!("{}:", provider_id);



    let lines: Vec<&str> = content.lines().collect::<Vec<_>>();

    let mut block_start: Option<usize> = None;

    let mut block_end: Option<usize> = None;



    // 第一遍：直接查找 provider_id

    for (i, line) in lines.iter().enumerate() {

        let trimmed = line.trim();

        if !trimmed.starts_with('-')

            && !trimmed.starts_with("  -")

            && trimmed.starts_with(&target_header)

            && !trimmed.starts_with("default_model:")

        {

            block_start = Some(i);

        } else if let Some(start) = block_start {

            if i > start

                && (!line.starts_with("  ") && !line.starts_with('\t'))

                && !trimmed.is_empty()

            {

                block_end = Some(i);

                break;

            }

        }

    }



    // 第二遍：别名回退（如 volc_ark 未找到，尝试 volcengine）

    if block_start.is_none() {

        if let Some(alias) = yaml_id_alias(provider_id) {

            let alias_header = format!("{}:", alias);

            for (i, line) in lines.iter().enumerate() {

                let trimmed = line.trim();

                if !trimmed.starts_with('-')

                    && !trimmed.starts_with("  -")

                    && trimmed.starts_with(&alias_header)

                    && !trimmed.starts_with("default_model:")

                {

                    block_start = Some(i);

                } else if let Some(start) = block_start {

                    if i > start

                        && (!line.starts_with("  ") && !line.starts_with('\t'))

                        && !trimmed.is_empty()

                    {

                        block_end = Some(i);

                        break;

                    }

                }

            }

        }

    }



    let api_key_line_inside = format!("    api_key: \"{}\"", api_key);



    match (block_start, block_end) {

        (Some(start), end_opt) => {

            let end = end_opt.unwrap_or(lines.len());

            let mut new_lines: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();

            let in_block = &lines[start..end];

            let mut has_api_key = false;

            for line in in_block {

                if line.trim().starts_with("api_key:") {

                    has_api_key = true;

                    break;

                }

            }

            if has_api_key {

                for line in in_block {

                    if line.trim().starts_with("api_key:") {

                        new_lines.push(api_key_line_inside.clone());

                    } else {

                        new_lines.push(line.to_string());

                    }

                }

            } else {

                let mut block_lines: Vec<String> = in_block.iter().map(|s| s.to_string()).collect();

                let insert_pos = block_lines.len().saturating_sub(

                    block_lines.iter().rev().take_while(|s| s.trim().is_empty()).count(),

                );

                block_lines.insert(insert_pos.max(1), api_key_line_inside.clone());

                new_lines.extend(block_lines);

            }

            new_lines.extend(lines[end..].iter().map(|s| s.to_string()));

            new_lines.join("\n")

        }

        (None, _) => upsert_append_provider_inside_providers_block(content, provider_id, &api_key_line_inside),

    }

}



/// 在 provider block 中更新或插入 proxy_url / proxy_username / proxy_password 字段

fn upsert_provider_proxy_config(

    content: &str,

    provider_id: &str,

    proxy_url: &str,

    proxy_username: &str,

    proxy_password: &str,

) -> String {

    let target_header = format!("{}:", provider_id);



    let lines: Vec<&str> = content.lines().collect::<Vec<_>>();

    let mut block_start: Option<usize> = None;

    let mut block_end: Option<usize> = None;



    // 第一遍：直接查找 provider_id

    for (i, line) in lines.iter().enumerate() {

        let trimmed = line.trim();

        if !trimmed.starts_with('-')

            && !trimmed.starts_with("  -")

            && trimmed.starts_with(&target_header)

            && !trimmed.starts_with("default_model:")

        {

            block_start = Some(i);

        } else if let Some(start) = block_start {

            if i > start

                && (!line.starts_with("  ") && !line.starts_with('\t'))

                && !trimmed.is_empty()

            {

                block_end = Some(i);

                break;

            }

        }

    }



    // 第二遍：别名回退

    if block_start.is_none() {

        if let Some(alias) = yaml_id_alias(provider_id) {

            let alias_header = format!("{}:", alias);

            for (i, line) in lines.iter().enumerate() {

                let trimmed = line.trim();

                if !trimmed.starts_with('-')

                    && !trimmed.starts_with("  -")

                    && trimmed.starts_with(&alias_header)

                    && !trimmed.starts_with("default_model:")

                {

                    block_start = Some(i);

                } else if let Some(start) = block_start {

                    if i > start

                        && (!line.starts_with("  ") && !line.starts_with('\t'))

                        && !trimmed.is_empty()

                    {

                        block_end = Some(i);

                        break;

                    }

                }

            }

        }

    }



    let proxy_url_line = format!("    proxy_url: \"{}\"", proxy_url);

    let proxy_username_line = format!("    proxy_username: \"{}\"", proxy_username);

    let proxy_password_line = format!("    proxy_password: \"{}\"", proxy_password);



    match (block_start, block_end) {

        (Some(start), end_opt) => {

            let end = end_opt.unwrap_or(lines.len());

            let mut new_lines: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();

            let in_block: Vec<&str> = lines[start..end].to_vec();

            let mut has_proxy_url = false;

            let mut has_proxy_username = false;

            let mut has_proxy_password = false;



            for line in &in_block {

                let trimmed = line.trim();

                if trimmed.starts_with("proxy_url:") {

                    has_proxy_url = true;

                } else if trimmed.starts_with("proxy_username:") {

                    has_proxy_username = true;

                } else if trimmed.starts_with("proxy_password:") {

                    has_proxy_password = true;

                }

            }



            let mut out_block: Vec<String> = Vec::new();

            for line in &in_block {

                let trimmed = line.trim();

                if trimmed.starts_with("proxy_url:") {

                    out_block.push(proxy_url_line.clone());

                } else if trimmed.starts_with("proxy_username:") {

                    out_block.push(proxy_username_line.clone());

                } else if trimmed.starts_with("proxy_password:") {

                    out_block.push(proxy_password_line.clone());

                } else {

                    out_block.push(line.to_string());

                }

            }



            // 如果没有找到对应的行，则追加

            if !has_proxy_url {

                let insert_pos = out_block.len().saturating_sub(

                    out_block.iter().rev().take_while(|s| s.trim().is_empty()).count(),

                );

                out_block.insert(insert_pos.max(1), proxy_url_line);

            }

            if !has_proxy_username {

                let insert_pos = out_block.len().saturating_sub(

                    out_block.iter().rev().take_while(|s| s.trim().is_empty()).count(),

                );

                out_block.insert(insert_pos.max(1), proxy_username_line);

            }

            if !has_proxy_password {

                let insert_pos = out_block.len().saturating_sub(

                    out_block.iter().rev().take_while(|s| s.trim().is_empty()).count(),

                );

                out_block.insert(insert_pos.max(1), proxy_password_line);

            }



            new_lines.extend(out_block);

            new_lines.extend(lines[end..].iter().map(|s| s.to_string()));

            new_lines.join("\n")

        }

        (None, _) => {

            // provider 不存在，跳过代理设置（应由 save_provider_config 先创建 provider）

            content.to_string()

        }

    }

}



/// 当 providers: 块内找不到目标 provider 时，将新块追加到 providers: 块内部。

/// 追加位置：providers: 块的最后一个已有 provider 之后（而非根级末尾）。

fn upsert_append_provider_inside_providers_block(content: &str, provider_id: &str, api_key_line: &str) -> String {

    let lines: Vec<&str> = content.lines().collect::<Vec<_>>();



    let providers_header_idx = lines.iter().position(|l| l.trim() == "providers:");



    if let Some(pidx) = providers_header_idx {

        let mut last_provider_end: Option<usize> = None;

        let mut in_provider = false;

        let mut current_provider_indent = 0usize;



        for (i, line) in lines.iter().enumerate().skip(pidx + 1) {

            if !in_provider {

                if (line.starts_with("  ") && !line.starts_with("    "))

                    && !line.trim().starts_with('#')

                    && !line.trim().is_empty()

                    && !line.trim().starts_with("default_model")

                {

                    in_provider = true;

                    current_provider_indent = line.len() - line.trim_start().len();

                }

                continue;

            }



            if i == lines.len() - 1 {

                last_provider_end = Some(i);

                break;

            }



            let next_raw = lines[i + 1];

            let next_indent = next_raw.len() - next_raw.trim_start().len();



            if next_indent <= current_provider_indent && !next_raw.trim().is_empty() {

                last_provider_end = Some(i);

                break;

            }

        }



        if let Some(insert_after) = last_provider_end {

            let mut new_lines: Vec<String> = lines[..=insert_after].iter().map(|s| s.to_string()).collect();

            new_lines.push(format!("  {}:", provider_id));

            new_lines.push(api_key_line.to_string());

            new_lines.extend(lines[insert_after + 1..].iter().map(|s| s.to_string()));

            return new_lines.join("\n");

        }



        let mut new_lines: Vec<String> = lines[..=pidx].iter().map(|s| s.to_string()).collect();

        new_lines.push(format!("  {}:", provider_id));

        new_lines.push(api_key_line.to_string());

        new_lines.extend(lines[pidx + 1..].iter().map(|s| s.to_string()));

        return new_lines.join("\n");

    }



    let separator = if content.trim().is_empty() { "" } else { "\n" };

    format!(

        "{}{}providers:\n  {}:\n{}\n",

        content.trim_end(),

        separator,

        provider_id,

        api_key_line

    )

}

#[tauri::command]

pub async fn save_provider_config(

    data_dir: tauri::State<'_, crate::AppState>,

    provider_id: String,

    api_key: String,

    proxy_url: Option<String>,

    proxy_username: Option<String>,

    proxy_password: Option<String>,

) -> Result<String, String> {

    info!("保存供应商配置: {}", provider_id);



    let data_dir = data_dir.inner().get_data_dir();

    let config_path = PathBuf::from(&data_dir).join("config").join("models.yaml");



    let content = read_models_yaml_text_for_manager(&data_dir)?;



    // 新凭据直接加密后写入（而非明文）

    let encrypted_api_key = tokio::task::spawn_blocking({

        let data_dir_clone = data_dir.clone();

        let api_key_clone = api_key.clone();

        move || {

            let key = crate::services::cipher::get_or_create_cipher_key_sync(&data_dir_clone)

                .map_err(|e| format!("Failed to get encryption key: {}", e))?;

            Ok::<_, String>(crate::services::cipher::encrypt_credential(&api_key_clone, &key))

        }

    })

    .await

    .map_err(|e| format!("Key task failed: {}", e))?

    .map_err(|e| e)?;



    // 先更新 api_key

    let new_content = upsert_provider_api_key(&content, &provider_id, &encrypted_api_key);



    // 再更新代理设置（如果提供）

    let final_content = if proxy_url.is_some() || proxy_username.is_some() || proxy_password.is_some() {

        upsert_provider_proxy_config(

            &new_content,

            &provider_id,

            proxy_url.as_deref().unwrap_or(""),

            proxy_username.as_deref().unwrap_or(""),

            proxy_password.as_deref().unwrap_or(""),

        )

    } else {

        new_content

    };



    // Write with sync_all to avoid data loss

    let mut f = OpenOptions::new()

        .create(true)

        .write(true)

        .truncate(true)

        .open(&config_path)

        .await

        .map_err(|e| format!("Failed to open config file: {}", e))?;

    f.write_all(final_content.as_bytes())

        .await

        .map_err(|e| format!("Failed to write config: {}", e))?;

    f.sync_all()

        .await

        .map_err(|e| format!("Failed to sync config: {}", e))?;



    Ok(format!("Provider {} config saved", provider_id))

}



async fn test_openai_compatible_chat(

    url: &str,

    data_dir: &str,

    provider: &str,

    api_key: &str,

    model_name: &str,

    proxy_url: Option<&str>,

    proxy_username: Option<&str>,

    proxy_password: Option<&str>,

) -> Result<serde_json::Value, String> {

    let mut client_builder = reqwest::Client::builder()

        .timeout(std::time::Duration::from_secs(45));



    // 配置代理（如果提供）

    if let Some(p_url) = proxy_url {

        if !p_url.is_empty() {

            let mut proxy = reqwest::Proxy::http(p_url).map_err(|e| e.to_string())?;

            if let Some(user) = proxy_username {

                if !user.is_empty() {

                    proxy = proxy.basic_auth(user, proxy_password.unwrap_or(""));

                }

            }

            client_builder = client_builder.proxy(proxy);

        }

    }



    let client = client_builder.build().map_err(|e| e.to_string())?;

    let response = client

        .post(url)

        .header("Authorization", format!("Bearer {}", api_key))

        .header("Content-Type", "application/json")

        .json(&serde_json::json!({

            "model": model_name,

            "messages": [{"role": "user", "content": "Hi"}],

            "max_tokens": 12

        }))

        .send()

        .await

        .map_err(|e| format!("请求失败: {}", e))?;



    if response.status().is_success() {

        if let Ok(body) = response.json::<serde_json::Value>().await {

            let dir_clone = data_dir.to_string();

            let provider_clone = provider.to_string();

            let model_clone = model_name.to_string();

            if let Some(usage) = body.get("usage") {

                let prompt_tokens = usage

                    .get("prompt_tokens")

                    .and_then(|v| v.as_u64())

                    .unwrap_or(0) as u32;

                let completion_tokens = usage

                    .get("completion_tokens")

                    .and_then(|v| v.as_u64())

                    .unwrap_or(0) as u32;

                let total_tokens = usage

                    .get("total_tokens")

                    .and_then(|v| v.as_u64())

                    .unwrap_or(0) as u32;

                let _handle = tokio::spawn(async move {

                    if let Err(e) = write_token_usage(

                        &dir_clone,

                        &provider_clone,

                        &model_clone,

                        prompt_tokens,

                        completion_tokens,

                        total_tokens,

                        "test_connection",

                    )

                    .await

                    {

                        tracing::warn!("记录 token 用量失败: {}", e);

                    }

                });

            } else {

                // 部分供应商成功响应里不含 usage，仍记一条便于仪表盘时间线更新（合计为 0）

                let _handle = tokio::spawn(async move {

                    if let Err(e) = write_token_usage(

                        &dir_clone,

                        &provider_clone,

                        &model_clone,

                        0,

                        0,

                        0,

                        "test_connection_no_usage",

                    )

                    .await

                    {

                        tracing::warn!("记录 token 用量失败: {}", e);

                    }

                });

            }

        }

        Ok(serde_json::json!({

            "success": true,

            "message": "连接成功"

        }))

    } else {

        let err = response.text().await.unwrap_or_default();

        Err(format!("连接失败: {}", err))

    }

}





#[tauri::command]

pub async fn test_model_connection(

    data_dir: tauri::State<'_, crate::AppState>,

    provider: String,

    model_name: String,

    api_key: String,

    proxy_url: Option<String>,

    proxy_username: Option<String>,

    proxy_password: Option<String>,

) -> Result<serde_json::Value, String> {

    info!("测试模型连接: {} / {}", provider, model_name);

    let data_dir_clone = data_dir.inner().get_data_dir();



    // =============================================================================

    // 各供应商测试 URL 与官方文档对照（每次修改前请同步更新此注释块）

    //

    // openai:        https://api.openai.com/v1/chat/completions

    //                 官方：https://platform.openai.com/docs/api-reference/introduction

    // anthropic:     https://api.anthropic.com/v1/messages  （特殊路径，非 Chat）

    //                 官方：https://docs.anthropic.com/en/api/messages

    // google:        https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}

    //                 官方：https://ai.google.dev/gemini-api/docs

    // deepseek:      https://api.deepseek.com/v1/chat/completions

    //                 官方：https://api-docs.deepseek.com/

    // minimax:       https://api.minimax.chat/v1/chat/completions

    //                 官方（OpenAI兼容）：https://platform.minimax.io/docs/guides/text-chat

    //                 备选域名（部分账号）：https://api.minimax.io/v1

    // volc_ark:      https://ark.cn-beijing.volces.com/api/v3/chat/completions

    //                 官方：https://www.volcengine.com/docs/82379/1298459（Base URL及鉴权）

    //                          https://www.volcengine.com/docs/82379/1494384（对话API）

    //                 注意：Seedream/Seedance 的 OpenAI-compatible 路径仍为 /v3/chat/completions，

    //                       但 prompt 格式与纯对话不同；用标准对话 prompt 测试会返回能力不匹配。

    //                 图片生成独立API：https://www.volcengine.com/docs/82379/1541523

    //                 视频生成独立API：https://www.volcengine.com/docs/82379/1520757

    // nvidia:        https://integrate.api.nvidia.com/v1/chat/completions

    //                 官方：https://docs.nvidia.com/nim/

    // aliyun:        https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions

    //                 官方：https://help.aliyun.com/document_detail/25183868.html

    // zhipu:         https://open.bigmodel.cn/api/paas/v4/chat/completions

    //                 官方：https://open.bigmodel.cn/dev/api

    // moonshot:      https://api.moonshot.cn/v1/chat/completions

    //                 官方：https://platform.moonshot.cn/docs

    // xiaomi:        https://api.xiaomi.com/v1/chat/completions  （需官方确认，当前占位）

    //                 官方：https://platform.xiaomi.com/  （如有）

    // kuaifan:       https://kuaifanio.cn/v1/chat/completions

    //                 官方：https://kuaifanio.cn

    // ollama:        http://localhost:11434/api/generate  （本地，无需 Key）

    // =============================================================================



    // 根据不同的供应商进行测试

    match provider.as_str() {

        "kuaifan" => {

            let client = reqwest::Client::new();

            let response = client

                .post("https://kuaifanio.cn/v1/chat/completions")

                .header("Authorization", format!("Bearer {}", api_key))

                .header("Content-Type", "application/json")

                .json(&serde_json::json!({

                    "model": model_name,

                    "messages": [{"role": "user", "content": "Hello"}],

                    "max_tokens": 10

                }))

                .send()

                .await

                .map_err(|e| format!("请求失败: {}", e))?;



            if response.status().is_success() {

                // 解析 usage 并记录

                if let Ok(body) = response.json::<serde_json::Value>().await {

                    let dir_clone = data_dir_clone.clone();

                    let provider_clone = provider.clone();

                    let model_clone = model_name.clone();

                    if let Some(usage) = body.get("usage") {

                        let prompt_tokens = usage

                            .get("prompt_tokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;

                        let completion_tokens = usage

                            .get("completion_tokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;

                        let total_tokens = usage

                            .get("total_tokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;



                        let _handle = tokio::spawn(async move {

                            if let Err(e) = write_token_usage(

                                &dir_clone,

                                &provider_clone,

                                &model_clone,

                                prompt_tokens,

                                completion_tokens,

                                total_tokens,

                                "test_connection",

                            )

                            .await

                            {

                                tracing::warn!("记录 token 用量失败: {}", e);

                            }

                        });

                    } else {

                        let _handle = tokio::spawn(async move {

                            if let Err(e) = write_token_usage(

                                &dir_clone,

                                &provider_clone,

                                &model_clone,

                                0,

                                0,

                                0,

                                "test_connection_no_usage",

                            )

                            .await

                            {

                                tracing::warn!("记录 token 用量失败: {}", e);

                            }

                        });

                    }

                }



                Ok(serde_json::json!({

                    "success": true,

                    "message": "连接成功"

                }))

            } else {

                let error = response.text().await.unwrap_or_default();

                Err(format!(

                    "连接失败: {}",

                    error

                ))

            }

        }

        "ollama" => {

            let client = reqwest::Client::new();

            let response = client

                .post("http://localhost:11434/api/generate")

                .json(&serde_json::json!({

                    "model": model_name,

                    "prompt": "Hello",

                    "stream": false

                }))

                .send()

                .await

                .map_err(|e| format!("Ollama 连接失败: {}", e))?;



            if response.status().is_success() {

                let dir_clone = data_dir_clone.clone();

                let provider_clone = provider.clone();

                let model_clone = model_name.clone();

                // Ollama 响应无标准 usage 字段，记一条 0-token 行便于仪表盘时间线更新

                let _handle = tokio::spawn(async move {

                    if let Err(e) = write_token_usage(

                        &dir_clone,

                        &provider_clone,

                        &model_clone,

                        0,

                        0,

                        0,

                        "test_connection",

                    )

                    .await

                    {

                        tracing::warn!("记录 Ollama token 用量失败: {}", e);

                    }

                });

                Ok(serde_json::json!({

                    "success": true,

                    "message": "Ollama 连接成功"

                }))

            } else {

                Err("Ollama 未运行或模型不存在".to_string())

            }

        }

        "openai" => {

            test_openai_compatible_chat(

                "https://api.openai.com/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "anthropic" => {

            let client = reqwest::Client::builder()

                .timeout(std::time::Duration::from_secs(45))

                .build()

                .map_err(|e| e.to_string())?;

            let response = client

                .post("https://api.anthropic.com/v1/messages")

                .header("x-api-key", &api_key)

                .header("anthropic-version", "2023-06-01")

                .header("Content-Type", "application/json")

                .json(&serde_json::json!({

                    "model": model_name,

                    "max_tokens": 20,

                    "messages": [{"role": "user", "content": "Hi"}]

                }))

                .send()

                .await

                .map_err(|e| format!("Anthropic 请求失败: {}", e))?;



            if response.status().is_success() {

                let dir_clone = data_dir_clone.clone();

                let provider_clone = provider.clone();

                let model_clone = model_name.clone();

                // Anthropic 响应无标准 usage，记 0-token 行便于仪表盘时间线更新

                let _handle = tokio::spawn(async move {

                    if let Err(e) = write_token_usage(

                        &dir_clone,

                        &provider_clone,

                        &model_clone,

                        0,

                        0,

                        0,

                        "test_connection",

                    )

                    .await

                    {

                        tracing::warn!("记录 Anthropic token 用量失败: {}", e);

                    }

                });

                Ok(serde_json::json!({

                    "success": true,

                    "message": "Claude API 连接成功"

                }))

            } else {

                Err(format!(

                    "连接失败: {}",

                    response.text().await.unwrap_or_default()

                ))

            }

        }

        "google" => {

            let url = format!(

                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",

                model_name, api_key

            );

            let mut client_builder = reqwest::Client::builder()

                .timeout(std::time::Duration::from_secs(45));



            // 配置代理（如果提供）

            if let Some(ref p_url) = proxy_url {

                if !p_url.is_empty() {

                    let mut proxy = reqwest::Proxy::http(p_url).map_err(|e| e.to_string())?;

                    if let Some(ref user) = proxy_username {

                        if !user.is_empty() {

                            proxy = proxy.basic_auth(user, proxy_password.as_deref().unwrap_or(""));

                        }

                    }

                    client_builder = client_builder.proxy(proxy);

                }

            }



            let client = client_builder.build().map_err(|e| e.to_string())?;

            let response = client

                .post(url)

                .header("Content-Type", "application/json")

                .json(&serde_json::json!({

                    "contents": [{"parts": [{"text": "Hi"}]}]

                }))

                .send()

                .await

                .map_err(|e| format!("Gemini 请求失败: {}", e))?;



            if response.status().is_success() {

                let dir_clone = data_dir_clone.clone();

                let provider_clone = provider.clone();

                let model_clone = model_name.clone();

                // Gemini 响应含 usage.promptTokens / completionTokens / totalTokens，尝试解析

                if let Ok(body) = response.json::<serde_json::Value>().await {

                    if let Some(usage) = body.get("usage") {

                        let prompt_tokens = usage

                            .get("promptTokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;

                        let completion_tokens = usage

                            .get("completionTokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;

                        let total_tokens = usage

                            .get("totalTokens")

                            .and_then(|v| v.as_u64())

                            .unwrap_or(0) as u32;

                        let _handle = tokio::spawn(async move {

                            if let Err(e) = write_token_usage(

                                &dir_clone,

                                &provider_clone,

                                &model_clone,

                                prompt_tokens,

                                completion_tokens,

                                total_tokens,

                                "test_connection",

                            )

                            .await

                            {

                                tracing::warn!("记录 Gemini token 用量失败: {}", e);

                            }

                        });

                    } else {

                        let _handle = tokio::spawn(async move {

                            if let Err(e) = write_token_usage(

                                &dir_clone,

                                &provider_clone,

                                &model_clone,

                                0,

                                0,

                                0,

                                "test_connection",

                            )

                            .await

                            {

                                tracing::warn!("记录 Gemini token 用量失败: {}", e);

                            }

                        });

                    }

                } else {

                    let _handle = tokio::spawn(async move {

                        if let Err(e) = write_token_usage(

                            &dir_clone,

                            &provider_clone,

                            &model_clone,

                            0,

                            0,

                            0,

                            "test_connection",

                        )

                        .await

                        {

                            tracing::warn!("记录 Gemini token 用量失败: {}", e);

                        }

                    });

                }

                Ok(serde_json::json!({

                    "success": true,

                    "message": "Gemini API 连接成功"

                }))

            } else {

                Err(format!(

                    "连接失败: {}",

                    response.text().await.unwrap_or_default()

                ))

            }

        }

        "deepseek" => {

            test_openai_compatible_chat(

                "https://api.deepseek.com/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "minimax" => {

            test_openai_compatible_chat(

                "https://api.minimax.chat/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "volc_ark" => {

            // 方舟对话模型走 OpenAI-compatible 路径（文档：https://www.volcengine.com/docs/82379/1494384）

            // 注意：Seedream/Seedance 走同一 Base URL，但 prompt 格式不同；

            //       若因模型不支持对话能力而失败，以下会追加对应 API 文档链接。

            let chat_result = test_openai_compatible_chat(

                "https://ark.cn-beijing.volces.com/api/v3/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await;



            match chat_result {

                Ok(r) => Ok(r),

                Err(e) => {

                    let is_media_model = model_name.starts_with("doubao-seedream")

                        || model_name.starts_with("doubao-seedance");

                    if is_media_model {

                        Err(format!(

                            "{}（Seedream/Seedance 为生图/生视频模型，不支持标准对话 prompt；\

                             请使用图片生成 API：https://www.volcengine.com/docs/82379/1541523 \

                             或视频生成 API：https://www.volcengine.com/docs/82379/1520757）",

                            e

                        ))

                    } else {

                        Err(format!(

                            "{}（方舟对话 API 文档：https://www.volcengine.com/docs/82379/1494384）",

                            e

                        ))

                    }

                }

            }

        }

        "nvidia" => {

            test_openai_compatible_chat(

                "https://integrate.api.nvidia.com/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "aliyun" => {

            test_openai_compatible_chat(

                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "zhipu" => {

            test_openai_compatible_chat(

                "https://open.bigmodel.cn/api/paas/v4/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "moonshot" => {

            test_openai_compatible_chat(

                "https://api.moonshot.cn/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "grok" => {

            test_openai_compatible_chat(

                "https://api.x.ai/v1/chat/completions",

                &data_dir_clone,

                &provider,

                &api_key,

                &model_name,

                proxy_url.as_deref(),

                proxy_username.as_deref(),

                proxy_password.as_deref(),

            )

            .await

        }

        "baidu" => Ok(serde_json::json!({

            "success": true,

            "message": "百度千帆需在控制台创建应用并绑定模型；此处已保存 Key，请在千帆侧验证模型名与权限"

        })),

        "xiaomi" => {

            // 小米 MiMo 连通性测试

            let client = reqwest::Client::new();

            let response = client

                .post("https://api.xiaomi.com/v1/chat/completions")

                .header("Authorization", format!("Bearer {}", api_key))

                .header("Content-Type", "application/json")

                .json(&serde_json::json!({

                    "model": model_name,

                    "messages": [{"role": "user", "content": "Hello"}],

                    "max_tokens": 10

                }))

                .send()

                .await

                .map_err(|e| format!("请求失败: {}", e))?;



            if response.status().is_success() {

                let dir_clone = data_dir_clone.clone();

                let provider_clone = provider.clone();

                let model_clone = model_name.clone();

                let body: serde_json::Value = response.json().await.unwrap_or_default();

                let usage = body.get("usage");

                if let Some(u) = usage {

                    let prompt_tokens =

                        u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                    let completion_tokens = u

                        .get("completion_tokens")

                        .and_then(|v| v.as_u64())

                        .unwrap_or(0) as u32;

                    let total_tokens =

                        u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                    let _handle = tokio::spawn(async move {

                        if let Err(e) = write_token_usage(

                            &dir_clone,

                            &provider_clone,

                            &model_clone,

                            prompt_tokens,

                            completion_tokens,

                            total_tokens,

                            "test_connection",

                        )

                        .await

                        {

                            tracing::warn!("记录小米 MiMo token 用量失败: {}", e);

                        }

                    });

                } else {

                    let _handle = tokio::spawn(async move {

                        if let Err(e) = write_token_usage(

                            &dir_clone,

                            &provider_clone,

                            &model_clone,

                            0,

                            0,

                            0,

                            "test_connection",

                        )

                        .await

                        {

                            tracing::warn!("记录小米 MiMo token 用量失败: {}", e);

                        }

                    });

                }

                Ok(serde_json::json!({

                    "success": true,

                    "message": "小米 MiMo 连接成功",

                    "usage": usage

                }))

            } else {

                let error = response.text().await.unwrap_or_default();

                Err(format!("连接失败: {}", error))

            }

        }

        _ => {

            // 其他供应商暂时返回成功

            Ok(serde_json::json!({

                "success": true,

                "message": format!("{} 连接配置已保存", provider)

            }))

        }

    }

}



/// 查询当前默认模型（与 set_default_model 写入格式对应）

#[derive(Debug, Clone, serde::Serialize)]

pub struct DefaultModel {

    pub provider: Option<String>,

    pub model_name: Option<String>,

}



/// 从 models.yaml 文本解析 `default_model` 块（与 `upsert_default_model_block` 的块边界规则一致）。

/// 注意：块结束须用**原始行**的缩进判断；若误用 `trimmed.starts_with("  ")`，则 `provider:` 去缩进后

/// 不以两个空格开头，会立刻误判为块外并 break，导致永远读不到默认模型（网关用 serde 能读到，前端却读不到）。

fn parse_default_model_from_models_yaml_content(content: &str) -> DefaultModel {

    let mut provider = None;

    let mut model_name = None;

    let mut in_default_block = false;



    for line in content.lines() {

        let trimmed = line.trim();

        if trimmed == "default_model:" {

            in_default_block = true;

            continue;

        }

        if in_default_block {

            if !trimmed.is_empty() && !line.starts_with("  ") && !line.starts_with('\t') {

                break;

            }

            if trimmed.starts_with("provider:") || trimmed.starts_with("provider :") {

                provider = trimmed

                    .splitn(2, ':')

                    .nth(1)

                    .map(|s| s.trim().trim_matches('"').to_string());

            } else if trimmed.starts_with("model_name:") || trimmed.starts_with("model_name :") {

                model_name = trimmed

                    .splitn(2, ':')

                    .nth(1)

                    .map(|s| s.trim().trim_matches('"').to_string());

            }

        }

    }



    DefaultModel {

        provider,

        model_name,

    }

}



#[tauri::command]

pub async fn get_default_model(

    data_dir: tauri::State<'_, crate::AppState>,

) -> Result<DefaultModel, String> {

    let data_dir = data_dir.inner().get_data_dir();



    let content = read_models_yaml_text_for_manager(&data_dir)?;

    let content = content.strip_prefix('\u{feff}').unwrap_or(content.as_str());



    Ok(parse_default_model_from_models_yaml_content(content))

}



#[tauri::command]

pub async fn set_default_model(

    data_dir: tauri::State<'_, crate::AppState>,

    provider: String,

    model_name: String,

    // 可选：同时把该供应商的 api_key 一并写入（用户选模型时一并触发）。
    // 若为 None / 空字符串，则不动供应商的 api_key 块。
    api_key: Option<String>,

    proxy_url: Option<String>,

    proxy_username: Option<String>,

    proxy_password: Option<String>,

) -> Result<String, String> {

    let provider = provider.trim().to_string();

    let model_name = model_name.trim().to_string();

    if provider.is_empty() || model_name.is_empty() {

        return Err(

            "设置默认模型失败：供应商或模型名为空。请在大模型页重新点选列表中的模型后再保存。"

                .to_string(),

        );

    }

    info!("设置默认模型: {} / {}", provider, model_name);



    let data_dir = data_dir.inner().get_data_dir();

    let config_path = PathBuf::from(&data_dir).join("config").join("models.yaml");



    let content = read_models_yaml_text_for_manager(&data_dir)?;



    // 解析 YAML 并重建，仅替换 default_model 块内的 provider 和 model_name。

    // 之前的实现对整个文件遍历，会错误替换 providers.*.provider 等无关行。

    let mut new_content = upsert_default_model_block(&content, &provider, &model_name);



    // 一次性合并写入：若调用方传了 api_key / proxy，也同步写进 providers.<provider> 块，

    // 避免「选完模型后还要再单独点保存供应商」的两步操作。

    let key_opt = api_key.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let proxy_opt = proxy_url.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let proxy_user_opt = proxy_username.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let proxy_pass_opt = proxy_password.as_deref().map(str::trim).filter(|s| !s.is_empty());

    if key_opt.is_some() || proxy_opt.is_some() || proxy_user_opt.is_some() || proxy_pass_opt.is_some() {

        // 加密 api_key（与 save_provider_config 同一套加密）

        if let Some(plain_key) = key_opt {

            let data_dir_for_key = data_dir.clone();

            let plain_key_owned = plain_key.to_string();

            let encrypted = tokio::task::spawn_blocking(move || -> Result<String, String> {

                let key = crate::services::cipher::get_or_create_cipher_key_sync(&data_dir_for_key)

                    .map_err(|e| format!("获取加密密钥失败: {}", e))?;

                Ok(crate::services::cipher::encrypt_credential(&plain_key_owned, &key))

            })

            .await

            .map_err(|e| format!("加密任务失败: {}", e))??;

            new_content = upsert_provider_api_key(&new_content, &provider, &encrypted);

        }

        if proxy_opt.is_some() || proxy_user_opt.is_some() || proxy_pass_opt.is_some() {

            new_content = upsert_provider_proxy_config(

                &new_content,

                &provider,

                proxy_opt.unwrap_or(""),

                proxy_user_opt.unwrap_or(""),

                proxy_pass_opt.unwrap_or(""),

            );

        }

    }



    // 写入后 sync_all：避免用户机上"保存提示成功/失败不一致"

    let mut f = OpenOptions::new()

        .create(true)

        .write(true)

        .truncate(true)

        .open(&config_path)

        .await

        .map_err(|e| format!("保存配置失败（打开文件）: {}", e))?;

    f.write_all(new_content.as_bytes())

        .await

        .map_err(|e| format!("保存配置失败（写入）: {}", e))?;

    f.sync_all()

        .await

        .map_err(|e| format!("保存配置失败（sync）: {}", e))?;



    // 与网关启动前检查使用同一套逻辑，避免「前端 toast 成功但 read_default_model_primary 仍为 None」的假成功

    if crate::commands::gateway::read_default_model_primary(&data_dir).is_none() {

        let diag = crate::commands::gateway::diagnose_default_model_primary(&data_dir)

            .err()

            .unwrap_or_else(|| "未知原因".to_string());

        return Err(format!(

            "设置默认模型失败：已写入磁盘，但网关仍无法读取到有效 default_model。\n\n{}",

            diag

        ));

    }



    // models.yaml 已更新，立即将默认模型同步到 openclaw.json（修复：保存后必须同步，否则网关永远用默认 Claude）

    crate::commands::gateway::sync_openclaw_config_from_manager(&data_dir)

        .await

        .map_err(|e| format!("同步网关配置失败: {}", e))?;



    Ok(format!("默认模型已设置为 {} / {}", provider, model_name))

}



/// 在 models.yaml 内容中找到 default_model 块并替换 provider / model_name。

/// 若 default_model 块不存在则追加。

fn upsert_default_model_block(content: &str, provider: &str, model_name: &str) -> String {

    let lines: Vec<&str> = content.lines().collect::<Vec<_>>();



    let has_default_model = lines.iter().any(|l| l.trim() == "default_model:");



    if !has_default_model {

        let sep = if content.trim().is_empty() { "" } else { "\n" };

        return format!(

            "{}{}default_model:\n  provider: \"{}\"\n  model_name: \"{}\"",

            content.trim_end(),

            sep,

            provider,

            model_name

        );

    }



    let mut new_lines: Vec<String> = Vec::new();

    let mut in_block = false;

    let mut replaced_provider = false;

    let mut replaced_model = false;



    for line in &lines {

        let trimmed = line.trim();



        if trimmed == "default_model:" {

            new_lines.push(line.to_string());

            in_block = true;

            continue;

        }



        if in_block {

            // 遇到非缩进行（块结束标记）时退出块模式

            if !trimmed.is_empty() && !line.starts_with("  ") && !line.starts_with('\t') {

                if !replaced_provider {

                    new_lines.push(format!("  provider: \"{}\"", provider));

                }

                if !replaced_model {

                    new_lines.push(format!("  model_name: \"{}\"", model_name));

                }

                replaced_provider = true;

                replaced_model = true;

                in_block = false;

                new_lines.push(line.to_string());

                continue;

            }



            if !replaced_provider && (trimmed.starts_with("provider:") || trimmed.starts_with("provider :")) {

                new_lines.push(format!("  provider: \"{}\"", provider));

                replaced_provider = true;

            } else if !replaced_model

                && (trimmed.starts_with("model_name:") || trimmed.starts_with("model_name :"))

            {

                new_lines.push(format!("  model_name: \"{}\"", model_name));

                replaced_model = true;

            } else {

                new_lines.push(line.to_string());

            }

        } else {

            new_lines.push(line.to_string());

        }

    }



    // default_model 在文件末尾时的兜底追加

    if in_block {

        if !replaced_provider {

            new_lines.push(format!("  provider: \"{}\"", provider));

        }

        if !replaced_model {

            new_lines.push(format!("  model_name: \"{}\"", model_name));

        }

    }



    new_lines.join("\n")

}



/// 模型列表中的单个模型条目

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ModelEntry {

    pub id: String,

    pub name: String,

    /// 近似上下文窗口（tokens），用于展示

    pub context_window: Option<usize>,

    /// 是否免费

    pub is_free: bool,

    /// 备注，如 "推荐"、"最新"

    pub badge: Option<String>,

}



fn me(

    id: &str,

    name: &str,

    context_window: Option<usize>,

    is_free: bool,

    badge: Option<&str>,

) -> ModelEntry {

    ModelEntry {

        id: id.to_string(),

        name: name.to_string(),

        context_window,

        is_free,

        badge: badge.map(|s| s.to_string()),

    }

}



/// 各云厂商常用模型静态目录（与控制台命名对齐；方舟可填推理接入点 ID 作为自定义模型）

fn static_provider_models(provider_id: &str) -> Vec<ModelEntry> {

    match provider_id {

        // ===== OpenAI ? https://platform.openai.com/docs/models | ??: https://api.openai.com/v1 =====

        "openai" => vec![

            me("gpt-5.5", "GPT-5.5（OpenAI旗舰模型）128K上下文）", Some(128000), false, Some("Default")),

            me("gpt-5.4", "GPT-5.4（高性价比·128K上下文）", Some(128000), false, Some("Default")),

            me("gpt-5.4-mini", "GPT-5.4 mini（轻量快速·128K上下文）", Some(128000), false, Some("Default")),

        ],

        // ===== Anthropic · https://docs.anthropic.com | 端点: https://api.anthropic.com/v1/messages（兼容OpenAI格式）=====

        "anthropic" => vec![

            me("claude-sonnet-4-6", "Claude Sonnet 4.6（旗舰模型·200K上下文）", Some(200000), false, Some("Default")),

            me("claude-opus-4-5", "Claude Opus 4.5（深度推理·200K上下文）", Some(200000), false, Some("Default")),

            me("claude-sonnet-4-5-20250929", "Claude Sonnet 4.5（平衡版·200K上下文）", Some(200000), false, Some("Default")),

            me("claude-haiku-4-5-20251001", "Claude Haiku 4.5（轻量快速·200K上下文）", Some(200000), false, Some("Default")),

        ],

        // ===== Google · https://ai.google.dev | 端点: https://generativelanguage.googleapis.com/v1beta（兼容OpenAI格式）=====

        "google" => vec![

            me("gemini-3.1-pro-preview", "Gemini 3.1 Pro（旗舰模型·1M上下文）", Some(1048576), false, Some("Default")),

            me("gemini-3-flash-preview", "Gemini 3 Flash（极速响应·1M上下文）", Some(1048576), false, Some("Default")),

            me("gemini-2.5-pro", "Gemini 2.5 Pro（深度推理·1M上下文）", Some(1048576), false, Some("Default")),

            me("gemini-2.5-flash", "Gemini 2.5 Flash（快速响应·1M上下文）", Some(1048576), false, Some("Default")),

        ],

        // ===== DeepSeek - https://api-docs.deepseek.com | China: https://api.deepseek.com/v1 (OpenAI Compatible)=====

        // deepseek-chat / deepseek-reasoner -> V4 Flash (2026-07-24)

        "deepseek" => vec![

            me("deepseek-v4-pro", "DeepSeek V4 Pro (Flagship - 1M Context - Deep Reasoning)", Some(1000000), false, Some("Default")),

            me("deepseek-v4-flash", "DeepSeek V4 Flash (1M Context - Fast)", Some(1000000), false, Some("Default")),

            me("deepseek-chat", "DeepSeek V4 Flash (Legacy ID Compatible)", Some(1000000), false, Some("Default")),

            me("deepseek-reasoner", "DeepSeek V4 Flash (Reasoning - Legacy ID)", Some(1000000), false, Some("Default")),

        ],

        // ===== MiniMax - https://platform.minimaxi.com | China: https://api.minimax.chat/v1 (OpenAI Compatible)=====

        "minimax" => vec![

            me("MiniMax-M3", "MiniMax M3 (Flagship - 200K Context)", Some(204800), false, Some("Default")),
            me("MiniMax-M3-highspeed", "MiniMax M3 (High Speed)", Some(204800), false, None),

            me("MiniMax-M2.7", "MiniMax M2.7 (Flagship - 200K Context)", Some(204800), false, Some("Default")),
            me("MiniMax-M2.7-highspeed", "MiniMax M2.7 (High Speed)", Some(204800), false, None),

        ],

        // ===== Volcengine Ark - Doubao - https://www.volcengine.com/docs/82379 | China: https://ark.cn-beijing.volces.com/api/v3 (OpenAI Compatible)=====

        "volc_ark" => vec![

            me("__volc_custom_ep__", "Volcengine Custom Endpoint (Input ID like ep-xxxx)", None, false, Some("Default")),

            me("doubao-seed-2-0-pro-260215", "Doubao Seed 2.0 Pro (Flagship - 256K Context)", Some(256000), false, Some("Default")),

            me("doubao-seed-2-0-code-preview-260215", "Doubao Seed 2.0 Code (Coding - 256K Context)", Some(256000), false, Some("Default")),

            me("doubao-seed-2-0-lite-260215", "Doubao Seed 2.0 Lite (Light - 256K Context)", Some(256000), false, Some("Default")),

            me("doubao-seed-2-0-mini-260215", "Doubao Seed 2.0 Mini (Tiny - 256K Context)", Some(256000), false, Some("Default")),

            me("doubao-seed-1-8-251228", "Doubao Seed 1.8 (Agent - 256K Context)", Some(256000), false, None),

            me("deepseek-v3-250324", "DeepSeek V3 (Classic - 128K Context)", Some(128000), false, Some("Default")),

            me("doubao-seedream-5-0-260128", "Doubao Seedream 5.0 (Image Gen - Chat Only)", None, false, Some("Default")),

            me("doubao-seedance-2-0-260128", "Doubao Seedance 2.0 (Video Gen - Chat Only)", None, false, Some("Default")),

        ],

        // ===== NVIDIA NIM - https://build.nvidia.com | Endpoint: https://integrate.api.nvidia.com/v1 (OpenAI Compatible)=====

        "nvidia" => vec![

            me("meta/llama-4-maverick-17b-128e-instruct", "Llama 4 Maverick 17B?128K?", Some(131072), false, Some("Default")),

            me("meta/llama-4-scout-17b-16e-instruct", "Llama 4 Scout 17B?128K?", Some(131072), false, Some("Default")),

            me("meta/llama-3.1-405b-instruct", "Llama 3.1 405B Instruct?128K?", Some(131072), false, None),

            me("meta/llama-3.1-8b-instruct", "Llama 3.1 8B Instruct?128K?", Some(131072), false, Some("Default")),

        ],

        // ===== Alibaba Qwen - https://help.aliyun.com/zh/model-studio | China: https://dashscope.aliyuncs.com/compatible-mode/v1 =====

        // 2026-06 ??: qwen3.7-max / qwen3.7-plus / qwen3.6-flash

        "aliyun" => vec![

            me("qwen3.7-max", "Qwen3.7-Max (Flagship - 1M Context)", Some(1000000), false, Some("Default")),

            me("qwen3.7-plus", "Qwen3.7-Plus (1M Context - Value)", Some(1000000), false, Some("Default")),

            me("qwen3.6-flash", "Qwen3.6-Flash (Fast - 1M Context)", Some(1000000), false, Some("Default")),

            me("qwen-long", "Qwen-Long (Ultra Long - 10M Context)", Some(10000000), false, Some("Default")),

        ],

        // ===== Zhipu GLM - https://open.bigmodel.cn | China: https://open.bigmodel.cn/api/paas/v4 (OpenAI Compatible)=====

        // 2026-06: GLM-5.2 (1M Context - Deep Reasoning)

        "zhipu" => vec![

            me("glm-5.2", "GLM-5.2 (Flagship - 1M Context - Deep Reasoning)", Some(1000000), false, Some("Default")),

            me("glm-5.1", "GLM-5.1 (1M Context - Stable)", Some(1000000), false, Some("Default")),

            me("glm-5-turbo", "GLM-5 Turbo (Fast - 1M Context)", Some(1000000), false, Some("Default")),

            me("glm-4.7-flash", "GLM-4.7 Flash (Free - 200K Context)", Some(200000), true, Some("Default")),

        ],

        // ===== Kimi (Moonshot) - https://platform.moonshot.cn | China: https://api.moonshot.cn/v1 (OpenAI Compatible)=====

        // 2026-06: K2.6 Flagship + K2.7 Code (Coding Specialist)

        "moonshot" => vec![

            me("kimi-k2.7-code", "Kimi K2.7 Code (Coding - 256K Context)", Some(256000), false, Some("Default")),

            me("kimi-k2.6", "Kimi K2.6 (Flagship - 256K Context)", Some(256000), false, Some("Default")),

            me("kimi-k2-thinking", "Kimi K2 Thinking (Deep Reasoning - 128K Context)", Some(128000), false, Some("Default")),

        ],

        // ===== Grok (xAI) - https://console.x.ai | Endpoint: https://api.x.ai/v1 (OpenAI Compatible)=====

        "grok" => vec![

            me("grok-3", "Grok 3 (xAI Flagship - 128K Context)", Some(131072), false, Some("Default")),

            me("grok-2", "Grok 2 (128K Context)", Some(131072), false, Some("Default")),

        ],

        // ===== Baidu ERNIE - https://console.bce.baidu.com/qianfan | China: https://qianfan.baidubce.com/v2 (OpenAI Compatible)=====

        "baidu" => vec![

            me("ernie-5.0", "ERNIE 5.0 (Flagship - 128K Context)", Some(128000), false, Some("Default")),

            me("ernie-5.0-thinking-latest", "ERNIE 5.0 Thinking (Deep Reasoning - 128K Context)", Some(128000), false, Some("Default")),

            me("ernie-4.0-turbo-128k", "ERNIE 4.0 Turbo 128K (Fast)", Some(128000), false, Some("Default")),

            me("ernie-speed-128k", "ERNIE Speed (Fast Lite - 128K Context)", Some(128000), false, Some("Default")),

        ],

        // ===== Xiaomi MiMo - https://platform.xiaomi.com | China: https://api.xiaomi.com/v1 (OpenAI Compatible)=====

        // 2026-06: MiMo V2.5 Pro (Xiaomi Flagship)

        "xiaomi" => vec![

            me("mimo-v2.5-pro", "MiMo V2.5 Pro (Flagship - 128K Context)", Some(128000), false, Some("Default")),
            me("mimo-v2.5", "MiMo V2.5 (Balanced - 128K Context)", Some(128000), false, Some("Default")),

        ],

        _ => Vec::new(),

    }

}



/// Fetch model lists from providers in real-time

/// - Ollama local: http://localhost:11434/api/tags

/// - KuaiFan: https://kuaifanio.cn/pricing (public JS bundle, no API key needed)

/// - Other providers use static lists

#[tauri::command]

pub async fn list_models(

    provider_id: String,

    api_key: Option<String>,

) -> Result<Vec<ModelEntry>, String> {

    match provider_id.as_str() {

        "ollama" => list_ollama_models().await,

        "kuaifan" => list_kuaifan_models(api_key.as_deref()).await,

        other => Ok(static_provider_models(other)),

    }

}



/// KuaiFan model list fetched in real-time from https://kuaifanio.cn/api/pricing

async fn list_kuaifan_models(_api_key: Option<&str>) -> Result<Vec<ModelEntry>, String> {
    match fetch_models_from_pricing_api().await {
        Ok(models) if !models.is_empty() => {
            tracing::info!("KuaiFan: {} models from /api/pricing", models.len());
            Ok(models)
        }
        Ok(_) => {
            tracing::info!("KuaiFan: /api/pricing returned empty model list");
            Ok(Vec::new())
        }
        Err(e) => {
            tracing::warn!("KuaiFan: /api/pricing fetch failed: {}", e);
            Ok(Vec::new())
        }
    }
}

/// Fetch published models from https://kuaifanio.cn/api/pricing (public endpoint)
pub async fn fetch_models_from_pricing_api() -> Result<Vec<ModelEntry>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;

    let resp = client
        .get("https://kuaifanio.cn/api/pricing")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("JSON: {}", e))?;

    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No 'data' array in response".to_string())?;

    let models: Vec<ModelEntry> = data
        .iter()
        .filter_map(|item| {
            let id = item.get("model_name").and_then(|v| v.as_str())?;
            if id.is_empty() {
                return None;
            }
            let is_free = match (
                item.get("model_price").and_then(|v| v.as_f64()),
                item.get("model_ratio").and_then(|v| v.as_f64()),
            ) {
                (Some(price), Some(ratio)) => price == 0.0 && ratio == 0.0,
                _ => false,
            };
            let badge = item
                .get("supported_endpoint_types")
                .and_then(|v| v.as_array())
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|s| !s.is_empty());
            Some(ModelEntry {
                id: id.to_string(),
                name: id.to_string(),
                context_window: None,
                is_free,
                badge,
            })
        })
        .collect();

    if models.is_empty() {
        Err("No published models found in /api/pricing".into())
    } else {
        Ok(models)
    }
}








async fn list_ollama_models() -> Result<Vec<ModelEntry>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;

    let resp = client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .map_err(|e| format!("Ollama connection error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Ollama: HTTP {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("JSON: {}", e))?;

    let models: Vec<ModelEntry> = body
        .get("models")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let id = item.get("name").and_then(|v| v.as_str())?;
                    Some(ModelEntry {
                        id: id.to_string(),
                        name: id.to_string(),
                        context_window: None,
                        is_free: true,
                        badge: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if models.is_empty() {
        return Err("Ollama: No models found. Run `ollama pull <model>` first.".into());
    }

    Ok(models)
}
