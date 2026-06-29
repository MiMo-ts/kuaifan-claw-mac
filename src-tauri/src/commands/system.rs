// 系统命令

use crate::commands::hidden_cmd;
use crate::models::SystemInfo;
use std::process::Command;

/// 在系统默认浏览器中打开 URL（供其他模块复用，避免重复平台分支）
pub(crate) fn open_url_in_default_browser(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        hidden_cmd::cmd()
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn open_folder(path: String) -> Result<String, String> {
    #[cfg(windows)]
    {
        Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }

    Ok(format!("已打开: {}", path))
}

/// 打开管理端配置目录（data/config）
#[tauri::command]
pub async fn open_manager_config_dir(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let config_path = std::path::PathBuf::from(&data_dir).join("config");

    // 确保目录存在
    tokio::fs::create_dir_all(&config_path)
        .await
        .map_err(|e| format!("创建配置目录失败: {}", e))?;

    #[cfg(windows)]
    {
        // 使用 cmd /c start 打开目录更可靠
        hidden_cmd::cmd()
            .args(["/C", "start", "", &config_path.display().to_string()])
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&config_path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&config_path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }
    Ok(format!("已打开: {}", config_path.display()))
}

#[tauri::command]
pub async fn open_url(url: String) -> Result<String, String> {
    open_url_in_default_browser(&url)?;
    Ok(format!("已打开: {}", url))
}

#[tauri::command]
pub async fn open_openclaw_config(
    data_dir: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let data_dir = data_dir.inner().get_data_dir();
    let openclaw_dir = format!("{}/openclaw-cn", data_dir);
    let config_path = format!("{}/openclaw.json", openclaw_dir);

    // 保证目录存在
    tokio::fs::create_dir_all(&openclaw_dir)
        .await
        .map_err(|e| format!("创建目录失败: {}", e))?;

    crate::commands::gateway::sync_openclaw_config_from_manager(&data_dir)
        .await
        .map_err(|e| format!("同步 OpenClaw 配置失败: {}", e))?;

    // 若文件不存在，写入最小合法 JSON
    if !std::path::Path::new(&config_path).exists() {
        tokio::fs::write(&config_path, "{}")
            .await
            .map_err(|e| format!("写入空配置失败: {}", e))?;
    }

    // 用默认程序打开文件（Windows: start；macOS: open；Linux: xdg-open）
    #[cfg(windows)]
    {
        hidden_cmd::cmd()
            .args(["/C", "start", "", &config_path])
            .spawn()
            .map_err(|e| format!("打开文件失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&config_path)
            .spawn()
            .map_err(|e| format!("打开文件失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&config_path)
            .spawn()
            .map_err(|e| format!("打开文件失败: {}", e))?;
    }

    Ok(format!("已打开: {}", config_path))
}

#[tauri::command]
pub async fn download_update(url: String) -> Result<String, String> {
    use hidden_cmd::cmd;

    // 获取临时下载目录
    let temp_dir = std::env::temp_dir();
    let file_name = url.split('/').last().unwrap_or("update.exe");
    let temp_path = temp_dir.join(file_name);

    // 使用 curl 下载文件
    #[cfg(windows)]
    {
        cmd()
            .args(["/C", "curl", "-L", "-o", &temp_path.display().to_string(), &url])
            .spawn()
            .map_err(|e| format!("下载失败: {}", e))?;
    }

    Ok(format!("下载完成: {}", temp_path.display()))
}

#[tauri::command]
pub async fn get_system_info() -> Result<SystemInfo, String> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "Unknown".to_string());

    #[cfg(windows)]
    {
        let output = hidden_cmd::cmd()
            .args(["/C", "systeminfo"])
            .output();

        let (total_memory, available_memory) = if let Ok(out) = output {
            let info = String::from_utf8_lossy(&out.stdout);
            let total = info
                .lines()
                .find(|l| l.contains("Total Physical Memory"))
                .map(|l| {
                    let num: String = l.chars().filter(|c| c.is_ascii_digit()).collect();
                    num.parse::<u64>().unwrap_or(0) / 1024
                })
                .unwrap_or(0);
            let avail = info
                .lines()
                .find(|l| l.contains("Available Physical Memory"))
                .map(|l| {
                    let num: String = l.chars().filter(|c| c.is_ascii_digit()).collect();
                    num.parse::<u64>().unwrap_or(0) / 1024
                })
                .unwrap_or(0);
            (total, avail)
        } else {
            (0, 0)
        };

        Ok(SystemInfo {
            os: "Windows".to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_count: num_cpus::get(),
            total_memory_mb: total_memory,
            available_memory_mb: available_memory,
            hostname,
        })
    }

    #[cfg(not(windows))]
    {
        Ok(SystemInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_count: num_cpus::get(),
            total_memory_mb: 0,
            available_memory_mb: 0,
            hostname,
        })
    }
}

#[tauri::command]
pub async fn fetch_versions() -> Result<String, String> {
    let url = "https://kuaifanclaw.cn/api/public/packages";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求版本信息失败: {}", e))?;
    let text = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;
    Ok(text)
}

use std::net::ToSocketAddrs;

#[derive(serde::Serialize)]
pub struct LanInfo {
    pub local_ips: Vec<String>,
    pub gateway_host: String,
    pub gateway_port: u16,
    pub gateway_running: bool,
    pub connection_urls: Vec<String>,
}

#[tauri::command]
pub fn get_lan_info(data_dir: String) -> Result<LanInfo, String> {
    let app_yaml = std::path::PathBuf::from(&data_dir).join("app.yaml");
    let raw = std::fs::read_to_string(&app_yaml).unwrap_or_default();
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

    let port = doc.get("gateway").and_then(|g| g.get("port")).and_then(|v| v.as_u64()).unwrap_or(8080) as u16;
    let host = doc.get("gateway").and_then(|g| g.get("host")).and_then(|v| v.as_str()).unwrap_or("127.0.0.1").to_string();

    let status_file = std::path::PathBuf::from(&data_dir).join("gateway.status");
    let running = status_file.exists()
        && std::fs::read_to_string(&status_file)
            .map(|s| s.trim().to_lowercase().contains("running"))
            .unwrap_or(false);

    let local_ips = get_local_lan_ips();
    let mut connection_urls = Vec::new();
    for ip in &local_ips {
        connection_urls.push(format!("http://{}:{}", ip, port));
    }

    Ok(LanInfo {
        local_ips,
        gateway_host: host,
        gateway_port: port,
        gateway_running: running,
        connection_urls,
    })
}

#[tauri::command]
pub fn set_gateway_host(data_dir: String, host: String) -> Result<String, String> {
    let app_yaml = std::path::PathBuf::from(&data_dir).join("app.yaml");
    let raw = std::fs::read_to_string(&app_yaml).unwrap_or_default();
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let Some(root) = doc.as_mapping_mut() {
        let gk = serde_yaml::Value::String("gateway".into());
        if !root.contains_key(&gk) {
            root.insert(gk.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        if let Some(gw) = root.get_mut(&gk).and_then(|v| v.as_mapping_mut()) {
            gw.insert(
                serde_yaml::Value::String("host".into()),
                serde_yaml::Value::String(host.clone()),
            );
        }
    }
    let yaml_str = serde_yaml::to_string(&doc).map_err(|e| format!("YAML 序列化失败: {}", e))?;
    std::fs::write(&app_yaml, yaml_str).map_err(|e| format!("写入配置失败: {}", e))?;
    Ok(format!("网关绑定地址已设为 {}", host))
}

// ─── 多设备互联：设备管理 ───

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct DeviceEntry {
    pub ip: String,
    pub name: String,
    pub status: String, // pending / approved / denied / blocked
    pub first_seen: String,
    pub last_seen: String,
}

fn device_list_path(data_dir: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(data_dir).join("devices.json")
}

fn load_devices(data_dir: &str) -> Vec<DeviceEntry> {
    let path = device_list_path(data_dir);
    if path.exists() {
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn save_devices(data_dir: &str, devices: &[DeviceEntry]) {
    let path = device_list_path(data_dir);
    if let Ok(json) = serde_json::to_string_pretty(devices) {
        let _ = std::fs::write(&path, json);
    }
}

#[tauri::command]
pub fn get_device_list(data_dir: String) -> Result<Vec<DeviceEntry>, String> {
    Ok(load_devices(&data_dir))
}

#[tauri::command]
pub fn submit_connect_request(data_dir: String, ip: String, name: String) -> Result<String, String> {
    let mut devices = load_devices(&data_dir);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    if let Some(existing) = devices.iter_mut().find(|d| d.ip == ip) {
        if existing.status == "blocked" {
            return Err("该设备已被拉黑".to_string());
        }
        existing.name = name;
        existing.last_seen = now;
        existing.status = "pending".to_string();
    } else {
        devices.push(DeviceEntry {
            ip,
            name,
            status: "pending".to_string(),
            first_seen: now.clone(),
            last_seen: now,
        });
    }
    save_devices(&data_dir, &devices);
    Ok("连接请求已提交".to_string())
}

#[tauri::command]
pub fn approve_device(data_dir: String, ip: String) -> Result<String, String> {
    let mut devices = load_devices(&data_dir);
    if let Some(d) = devices.iter_mut().find(|d| d.ip == ip) {
        d.status = "approved".to_string();
        d.last_seen = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        save_devices(&data_dir, &devices);
        Ok("已允许设备接入".to_string())
    } else {
        Err("设备不存在".to_string())
    }
}

#[tauri::command]
pub fn deny_device(data_dir: String, ip: String) -> Result<String, String> {
    let mut devices = load_devices(&data_dir);
    if let Some(d) = devices.iter_mut().find(|d| d.ip == ip) {
        d.status = "denied".to_string();
        d.last_seen = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        save_devices(&data_dir, &devices);
        Ok("已拒绝设备接入".to_string())
    } else {
        Err("设备不存在".to_string())
    }
}

#[tauri::command]
pub fn block_device(data_dir: String, ip: String) -> Result<String, String> {
    let mut devices = load_devices(&data_dir);
    if let Some(d) = devices.iter_mut().find(|d| d.ip == ip) {
        d.status = "blocked".to_string();
        d.last_seen = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        save_devices(&data_dir, &devices);
        Ok("已拉黑设备".to_string())
    } else {
        Err("设备不存在".to_string())
    }
}

fn get_local_lan_ips() -> Vec<String> {
    let mut ips = Vec::new();
    // 通过连接外部地址来探测本机出口 IP
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        socket.set_read_timeout(Some(std::time::Duration::from_millis(100))).ok();
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = socket.local_addr() {
                let ip = local.ip().to_string();
                if ip != "0.0.0.0" && ip != "127.0.0.1" {
                    ips.push(ip);
                }
            }
        }
    }
    // 也尝试通过主机名解析
    if let Ok(hostname) = hostname::get() {
        if let Ok(host_str) = hostname.into_string() {
            if let Ok(addrs) = (host_str.as_str(), 0).to_socket_addrs() {
                for addr in addrs {
                    let ip = addr.ip();
                    if ip.is_ipv4() && !ip.is_loopback() {
                        let s = ip.to_string();
                        if !ips.contains(&s) {
                            ips.push(s);
                        }
                    }
                }
            }
        }
    }
    if ips.is_empty() {
        ips.push("127.0.0.1".to_string());
    }
    ips
}

#[derive(serde::Serialize)]
pub struct RemoteGatewayInfo {
    pub connected: bool,
    pub url: String,
}

#[tauri::command]
pub fn get_remote_gateway(data_dir: String) -> Result<RemoteGatewayInfo, String> {
    let app_yaml = std::path::PathBuf::from(&data_dir).join("app.yaml");
    let raw = std::fs::read_to_string(&app_yaml).unwrap_or_default();
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    let url = doc
        .get("gateway")
        .and_then(|g| g.get("remote_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(RemoteGatewayInfo {
        connected: !url.is_empty(),
        url,
    })
}

#[tauri::command]
pub fn set_remote_gateway(data_dir: String, url: String) -> Result<String, String> {
    let app_yaml = std::path::PathBuf::from(&data_dir).join("app.yaml");
    let raw = std::fs::read_to_string(&app_yaml).unwrap_or_default();
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let Some(root) = doc.as_mapping_mut() {
        let gk = serde_yaml::Value::String("gateway".into());
        if !root.contains_key(&gk) {
            root.insert(gk.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        if let Some(gw) = root.get_mut(&gk).and_then(|v| v.as_mapping_mut()) {
            if url.is_empty() {
                gw.remove(&serde_yaml::Value::String("remote_url".into()));
            } else {
                gw.insert(
                    serde_yaml::Value::String("remote_url".into()),
                    serde_yaml::Value::String(url.clone()),
                );
            }
        }
    }
    let yaml_str = serde_yaml::to_string(&doc).map_err(|e| format!("YAML 序列化失败: {}", e))?;
    std::fs::write(&app_yaml, yaml_str).map_err(|e| format!("写入配置失败: {}", e))?;

    if url.is_empty() {
        Ok("已断开远程网关".to_string())
    } else {
        Ok(format!("已连接到远程网关: {}", url))
    }
}
