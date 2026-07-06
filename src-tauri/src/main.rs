// OpenClaw-CN Manager - Rust Backend

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bundled_env;
mod commands;
pub mod env_paths;
pub mod mirror;
mod models;
mod services;

use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub struct AppState {
    pub data_dir: Mutex<String>,
}

impl AppState {
    /// 安全获取 data_dir（从 poisoned mutex 恢复）
    pub fn get_data_dir(&self) -> String {
        self.data_dir.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn init_logging(data_dir: &std::path::Path) -> Result<WorkerGuard, String> {
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("Failed to create log directory: {}", e))?;

    let file_appender = tracing_appender::rolling::Builder::new()
        .filename_prefix("app")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&log_dir)
        .map_err(|e| format!("Failed to create log file appender: {}", e))?;

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true),
        )
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    Ok(guard)
}

/// 
/// 
fn strip_extended_prefix(p: &std::path::Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with("\\\\?\\") {
        PathBuf::from(&s[4..])
    } else {
        p.to_path_buf()
    }
}

/// 
fn diagnostics_dir() -> PathBuf {
    std::env::temp_dir().join("OpenClaw-CN-Manager")
}

fn write_diagnostic_file(name: &str, content: &str) {
    let dir = diagnostics_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(name), content);
}

/// 
fn data_root_is_writable(root: &std::path::Path) -> bool {
    if std::fs::create_dir_all(root).is_err() {
        return false;
    }
    let probe = root.join(".ocm_write_probe");
    match std::fs::write(&probe, b"1") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 
fn fallback_user_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir())
            .join("OpenClaw-CN Manager")
            .join("data")
    }
    #[cfg(target_os = "macos")]
    {
        // macOS: ~/Library/Application Support/OpenClaw-CN Manager/data
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join("Library").join("Application Support").join("OpenClaw-CN Manager").join("data"))
            .unwrap_or_else(|| std::env::temp_dir().join("OpenClaw-CN-Manager-data"))
    }
    #[cfg(target_os = "linux")]
    {
        // Linux: ~/.local/share/OpenClaw-CN Manager/data
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".local/share/OpenClaw-CN Manager/data"))
            .unwrap_or_else(|| std::env::temp_dir().join("OpenClaw-CN-Manager-data"))
    }
    #[cfg(target_os = "freebsd")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".local/share/OpenClaw-CN Manager/data"))
            .unwrap_or_else(|| std::env::temp_dir().join("OpenClaw-CN-Manager-data"))
    }
}

/// 
fn ensure_writable_release_data_dir(exe_path: &std::path::Path) -> PathBuf {
    let preferred = resolve_release_data_dir(exe_path);
    // 
    if std::env::var("OPENCLAW_CN_DATA_DIR")
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        return preferred;
    }
    if data_root_is_writable(&preferred) {
        return preferred;
    }
    let fb = fallback_user_data_dir();
    if data_root_is_writable(&fb) {
        write_diagnostic_file(
            "data-dir-fallback.txt",
            &format!(
                "Preferred data dir not writable, falling back to user dir.\nPreferred: {}\nCurrent: {}\n",
                preferred.display(),
                fb.display()
            ),
        );
        return fb;
    }
    // 
    preferred
}

/// 
/// 
/// 
/// 
/// 
///
///
///
fn resolve_release_data_dir(exe_path: &std::path::Path) -> std::path::PathBuf {
    // 1. 最高优先：环境变量强制指定
    if let Ok(ev) = std::env::var("OPENCLAW_CN_DATA_DIR") {
        let t = ev.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }

    let exe_dir = exe_path.parent().unwrap_or(exe_path);

    // 2. macOS 特殊处理：使用 ~/Library/Application Support 标准位置
    //    这样 .app 替换/重新编译时数据不会丢失。
    //    若旧版 .app/data/ 存在且新位置尚未初始化，自动迁移一次。
    #[cfg(target_os = "macos")]
    {
        let exe_str = exe_path.to_string_lossy();
        if exe_str.contains(".app/Contents/MacOS/") {
            // 标准 Mac 数据目录
            let standard_data_dir = fallback_user_data_dir();
            let standard_config = standard_data_dir.join("config");
            let _ = std::fs::create_dir_all(&standard_data_dir);

            // 一次性迁移：旧 .app/data/ → ~/Library/Application Support/...
            if !standard_config.exists() {
                if let Some(app_bundle_pos) = exe_str.find(".app/") {
                    let app_bundle_path = &exe_str[..app_bundle_pos + 5];
                    let old_bundle_data = PathBuf::from(app_bundle_path).join("data");
                    let old_bundle_config = old_bundle_data.join("config");
                    if old_bundle_config.exists() {
                        tracing::info!(
                            "Migrating legacy .app data: {} -> {}",
                            old_bundle_data.display(),
                            standard_data_dir.display()
                        );
                        if let Err(e) = copy_dir_recursive(&old_bundle_data, &standard_data_dir) {
                            tracing::warn!("Legacy data migration failed: {}", e);
                        }
                    }
                }
            }

            if standard_config.exists() {
                return standard_data_dir;
            }
        }
    }

    // 3. 便携模式：exe 同目录的 data/
    let portable_data_dir = exe_dir.join("data");
    let portable_config_dir = portable_data_dir.join("config");
    if portable_config_dir.exists() {
        return portable_data_dir;
    }
    let portable_flag = exe_dir.join("OpenClaw-CN.portable");
    if portable_flag.exists() {
        return portable_data_dir;
    }

    // 4. 兜底：使用系统标准目录
    fallback_user_data_dir()
}

/// 
///
/// 
/// 
/// 
/// 
/// 
///
/// 
#[cfg(windows)]
fn msi_bootstrap(exe_path: &std::path::Path) {
    // 
    let exe_dir = exe_path.parent().unwrap_or(exe_path);
    if exe_dir.join("data").join("config").exists() {
        return;
    }

    // 
    const HKEY_CURRENT_USER: u32 = 0x80000001;
    extern "system" {
        fn RegOpenKeyExW(
            hKey: *mut std::ffi::c_void,
            lpSubKey: *const u16,
            ulOptions: u32,
            samDesired: u32,
            phkResult: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn RegQueryValueExW(
            hKey: *mut std::ffi::c_void,
            lpValueName: *const u16,
            lpReserved: *mut std::ffi::c_void,
            lpType: *mut u32,
            lpData: *mut u8,
            lpcbData: *mut u32,
        ) -> i32;
        fn RegCloseKey(hKey: *mut std::ffi::c_void) -> i32;
    }

    const KEY_READ: u32 = 0x20019;
    let subkey: Vec<u16> = "Software\\openclaw-cn\\OpenClaw-CN Manager\0"
        .encode_utf16()
        .collect();
    let value_name: Vec<u16> = "InstallDir\0".encode_utf16().collect();
    let mut hkey: *mut std::ffi::c_void = std::ptr::null_mut();
    let ret = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER as *mut std::ffi::c_void,
            subkey.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if ret != 0 {
        return;
    }
    let mut data_buf = [0u16; 512];
    let mut data_len = (data_buf.len() * 2) as u32;
    let mut reg_type: u32 = 0;
    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut reg_type,
            data_buf.as_mut_ptr().cast(),
            &mut data_len,
        )
    };
    unsafe { RegCloseKey(hkey); };
    if ret != 0 || reg_type != 1 {
        // REG_SZ == 1
        return;
    }
    // 
    let char_count = (data_len as usize / 2).saturating_sub(1);
    let install_dir_str = String::from_utf16_lossy(&data_buf[..char_count]);
    let install_dir = std::path::Path::new(&install_dir_str);
    if install_dir.components().count() < 2 {
        return;
    }

    // Create data/config (MSI bootstrap supplement)?
    let config_dir = install_dir.join("data").join("config");
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        tracing::warn!("MSI bootstrap failed to create data/config: {} (continuing)", e);
        return;
    }
    tracing::info!("MSI bootstrap created data/config in install dir: {}", config_dir.display());
}

/// 
/// 
/// - Windows/Linux: {exe_dir}/resources/
/// - macOS app bundle: {app_bundle}/Contents/Resources/
fn resolve_resource_dir(exe_path: &std::path::Path) -> Option<PathBuf> {
    let exe_str = exe_path.to_string_lossy();

    #[cfg(target_os = "macos")]
    {
        //
        if exe_str.contains(".app/Contents/MacOS/") {
            if let Some(app_bundle_pos) = exe_str.find(".app/") {
                let app_bundle_path = &exe_str[..app_bundle_pos + 5];
                let resource_dir = PathBuf::from(app_bundle_path).join("Contents").join("Resources");
                if resource_dir.exists() {
                    return Some(resource_dir);
                }
            }
        }
    }

    // 
    let default_resource = exe_path.parent()?.join("resources");
    if default_resource.exists() {
        return Some(default_resource);
    }

    None
}

/// 
/// 
/// 
fn migrate_resources_on_first_run(data_dir_abs: &PathBuf, exe_path: &PathBuf) {
    if !cfg!(debug_assertions) {
        // 资源目录在 Tauri 2 打包后位于：
        //   Windows/Linux: {exe_dir}/resources/resources/data/
        //   macOS app bundle: {app_bundle}/Contents/Resources/resources/data/
        // 原因：tauri.conf.json 中 `bundle.resources` 列出的 `resources/data` 等条目
        //      会被 Tauri 保留 `resources/` 前缀放进 bundle 资源根。
        let resource_dir = resolve_resource_dir(exe_path).map(|p| p.join("resources").join("data"));

        if let Some(resource_dir) = resource_dir {
            let migrated_marker = data_dir_abs.join(".migrated");
            let resource_version_file = resource_dir.join(".resource_version");

            // Read resource package version (written by build.rs)
            let expected_version = std::fs::read_to_string(&resource_version_file)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "0".to_string());

            let current_version = std::fs::read_to_string(&migrated_marker)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            if current_version != expected_version && resource_dir.exists() {
                tracing::info!(
                    "First run detected data migration: resources v{} -> data v{}",
                    expected_version,
                    current_version
                );

                if let Err(e) = copy_dir_recursive(&resource_dir, data_dir_abs) {
                    tracing::warn!("Migration resources/data failed: {} (continuing)", e);
                } else {
                    tracing::info!("resources/data migration completed");

                    if let Err(e) = std::fs::write(&migrated_marker, &expected_version) {
                        tracing::warn!("Failed to write migration marker: {}", e);
                    }
                }
            } else if current_version == expected_version {
                tracing::info!(
                    "Data migration already completed (v{}); skipping",
                    current_version
                );
            } else {
                tracing::info!(
                    "Resource dir not found at {}; skipping migration",
                    resource_dir.display()
                );
            }
        } else {
            tracing::warn!("resolve_resource_dir returned None; skipping migration");
        }
    }
}

/// 
/// 
const RESOURCE_MIGRATE_SKIP_DIRS: &[&str] = &[
    "backups",
    "logs",
    "metrics",
    "openclaw-cn",
    "plugins",
    "robots",
    "instances",
    "openclaw-state",
];

fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let name = entry.file_name();
        if ty.is_dir() {
            let n = name.to_string_lossy();
            if RESOURCE_MIGRATE_SKIP_DIRS.iter().any(|s| *s == n.as_ref()) {
                tracing::info!(
                    "Migration resources/data: skipped user data dir {}",
                    n
                );
                continue;
            }
        }
        let dst_path = dst.join(&name);
        if ty.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            // 
            if name.to_string_lossy() == "models.yaml" && dst_path.exists() {
                tracing::info!(
                    "Migration resources/data: keeping existing user config {}",
                    dst_path.display()
                );
                continue;
            }
            // 
            if let Some(parent) = dst_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn main() {
    // 
    write_diagnostic_file(
        "start.log",
        &format!(
            "main() entered\nexe={:?}\n",
            std::env::current_exe().unwrap_or_default()
        ),
    );
    std::panic::set_hook(Box::new(move |panic_info| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let msg = format!(
            "[{}s] PANIC: {}\nBacktrace:\n{:?}\n",
            ts,
            panic_info,
            std::backtrace::Backtrace::capture()
        );
        write_diagnostic_file("crash.log", &msg);
    }));

    let exe_path = std::env::current_exe()
        .ok()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // 
    // 
    // 
    // 
    #[cfg(windows)]
    {
        msi_bootstrap(&exe_path);
    }

    // 
    // 
    // 
    //
    // 
    // 
    // 
    let data_dir_abs: PathBuf = if cfg!(debug_assertions) {
        // 
        PathBuf::from(r"D:\ORD\data")
    } else {
        ensure_writable_release_data_dir(&exe_path)
    };

    let data_dir = data_dir_abs.clone();
    let _ = std::fs::create_dir_all(&data_dir);

    // 
    let data_dir_for_state = strip_extended_prefix(&data_dir_abs)
        .to_string_lossy()
        .to_string();

    let _guard = match init_logging(&data_dir_abs) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("Log initialization failed: {} (continuing without file logging)", e);
            None
        }
    };

    tracing::info!("App started");
    tracing::info!("Data directory: {}", data_dir_for_state);
    tracing::info!(
        "Node will be installed to: {}",
        strip_extended_prefix(&data_dir_abs)
            .join("env")
            .join("node")
            .display()
    );
    tracing::info!(
        "Git will be installed to: {}",
        strip_extended_prefix(&data_dir_abs)
            .join("env")
            .join("git")
            .display()
    );

    let dirs = [
        "config",
        "instances",
        "backups",
        "logs",
        "plugins",
        "robots",
        "metrics",
        "env",
    ];
    for dir in dirs {
        let path = data_dir_abs.join(dir);
        if let Err(e) = std::fs::create_dir_all(&path) {
            tracing::warn!("Failed to create directory {}: {}", path.display(), e);
        }
    }

    // 
    migrate_resources_on_first_run(&data_dir_abs, &exe_path);

    // 
    match services::invite_code::is_invite_code_validated(&data_dir_abs) {
        Ok(true) => {
            tracing::info!("App started");
        },
        Ok(false) => {
            tracing::info!("App started");
            // 
            // 
        },
        Err(e) => {
            tracing::error!("Error: {}", e);
            // 
            tracing::info!("App started");
        },
    }

    tauri::Builder::default()
        // 
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            data_dir: Mutex::new(data_dir_for_state),
        })
        .invoke_handler(tauri::generate_handler![
            commands::env::check_node_version,
            commands::env::check_git_version,
            commands::env::check_npm_version,
            commands::env::check_pnpm_installation,
            commands::env::check_homebrew,
            commands::env::get_app_version,
            commands::env::check_network_connectivity,
            commands::env::check_disk_space,
            commands::env::run_env_check,
            commands::env::run_env_auto_fix,
            commands::installer::install_node,
            commands::installer::install_homebrew,
            commands::installer::install_pnpm,
            commands::installer::install_git,
            commands::installer::install_openclaw,
            commands::installer::get_openclaw_version,
            commands::installer::get_openclaw_cn_status,
            commands::installer::get_openclaw_install_status,
            commands::installer::start_openclaw_background_install,
            commands::plugin::list_plugins,
            commands::plugin::check_plugin_installed,
            commands::plugin::install_plugin,
            commands::plugin::reinstall_plugin_deps,
            commands::plugin::uninstall_plugin,
            // plugin_framework — 声明式插件框架 + 各平台快捷绑定
            commands::plugin_framework::get_plugin_manifests,
            commands::plugin_framework::get_plugin_manifest,
            commands::plugin_framework::get_plugin_auth_config,
            commands::plugin_framework::get_plugin_credentials,
            commands::plugin_framework::validate_plugin_credentials,
            commands::plugin_framework::install_plugin_cli,
            commands::plugin_framework::uninstall_plugin_fw,
            // 钉钉 device_code 快捷绑定
            commands::plugin_framework::start_device_auth,
            commands::plugin_framework::poll_device_auth,
            // 通用 QR 扫码
            commands::plugin_framework::start_qrcode_auth,
            commands::plugin_framework::poll_qrcode_auth,
            // 微信 ilink QR 快捷绑定
            commands::plugin_framework::start_wechat_cli_bind,
            commands::plugin_framework::poll_wechat_cli_bind,
            commands::plugin_framework::cancel_wechat_cli_bind,
            commands::plugin_framework::save_wechat_bot_token,
            // 飞书 Device Authorization Grant 快捷绑定
            commands::plugin_framework::start_feishu_quick_bind,
            commands::plugin_framework::poll_feishu_quick_bind,
            commands::model::list_providers,
            commands::model::get_provider_config,
            commands::model::save_provider_config,
            commands::model::test_model_connection,
            commands::model::get_default_model,
            commands::model::set_default_model,
            commands::model::list_models,
            commands::robot::list_robot_templates,
            commands::robot::get_robot_skills,
            commands::robot::get_robot_mcp_recommendations,
            commands::robot::download_skills,
            commands::robot::download_skill_retry,
            commands::robot::create_robot,
            commands::robot::list_robots,
            commands::instance::list_instances,
            commands::instance::get_instance,
            commands::instance::create_instance,
            commands::instance::update_instance,
            commands::instance::delete_instance,
            commands::instance::toggle_instance,
            commands::gateway::get_gateway_status,
            commands::gateway::get_gateway_ws_info,
            commands::gateway::start_gateway,
            commands::gateway::stop_gateway,
            commands::gateway::restart_gateway,
            commands::gateway::proxy_gateway_chat,
            commands::gateway::open_openclaw_console,
            commands::gateway::get_gateway_usage,
            commands::backup::list_backups,
            commands::backup::create_backup,
            commands::backup::restore_backup,
            commands::backup::delete_backup,
            commands::backup::export_config,
            commands::backup::import_config,
            commands::config::get_app_config,
            commands::config::save_app_config,
            commands::config::get_data_dir,
            commands::config::get_config_paths,
            commands::log::read_logs,
            commands::log::clear_logs,
            commands::log::read_runtime_logs_tail,
            commands::log::clear_openclaw_gateway_log,
            commands::system::open_folder,
            commands::system::open_manager_config_dir,
            commands::system::open_url,
            commands::system::open_openclaw_config,
            commands::system::get_system_info,
            commands::system::download_update,
            commands::system::fetch_versions,
            commands::system::get_lan_info,
            commands::system::set_gateway_host,
            commands::system::get_device_list,
            commands::system::submit_connect_request,
            commands::system::approve_device,
            commands::system::deny_device,
            commands::system::block_device,
            commands::system::get_remote_gateway,
            commands::system::set_remote_gateway,
            commands::usage::record_token_usage,
            commands::usage::get_token_usage_summary,
            commands::usage::get_token_usage_events,
            commands::usage::record_detailed_usage,
            commands::usage::get_usage_by_model,
            commands::usage::get_usage_by_provider,
            commands::usage::get_provider_pricing,
            commands::usage::calculate_usage_cost,
            commands::monitoring::get_monitoring_summary,
            commands::monitoring::get_realtime_metrics,
            commands::monitoring::get_model_metrics,
            commands::monitoring::record_request_metrics,
            commands::monitoring::reset_monitoring,
            commands::monitoring::get_cost_budgets,
            commands::monitoring::save_cost_budget,
            commands::monitoring::delete_cost_budget,
            commands::monitoring::check_cost_budget,
            commands::monitoring::get_unacknowledged_alerts,
            commands::monitoring::acknowledge_alert,
            commands::monitoring::reset_budget,
            commands::monitoring::load_budgets,
            commands::monitoring::create_cost_budget,
            // 
            commands::feishu_wizard::get_feishu_wizard_guide,
            commands::feishu_wizard::open_feishu_url,
            commands::feishu_wizard::probe_feishu,
            commands::feishu_wizard::get_feishu_ws_info,
            // 
            commands::invite::get_machine_fingerprint,
            commands::invite::validate_and_bind_invite_code,
            commands::invite::is_invite_code_validated,
            // Auth commands
            commands::auth::login,
            commands::auth::register,
            commands::auth::logout,
            commands::auth::save_api_key,
            commands::auth::check_auth,
            commands::auth::get_self,
            commands::auth::get_user_id,
            // Token commands
            commands::token::auto_configure_api_key,        ])
        .setup(|app| {
            tracing::info!("Tauri app initialization complete");

            #[cfg(desktop)]
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder};

                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let show = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &quit])?;

                let icon = app.default_window_icon().cloned();
                match icon {
                    Some(icon) => {
                        let _tray = TrayIconBuilder::new()
                            .icon(icon)
                            .menu(&menu)
                            .tooltip("快泛Claw")
                            .on_menu_event(|app, event| match event.id.as_ref() {
                                "quit" => {
                                    app.exit(0);
                                }
                                "show" => {
                                    if let Some(w) = app.get_webview_window("main") {
                                        let _ = w.show();
                                        let _ = w.set_focus();
                                    }
                                }
                                _ => {}
                            })
                            // 
                            .on_tray_icon_event(|tray, event| {
                                if let tauri::tray::TrayIconEvent::Click {
                                    button: MouseButton::Left,
                                    button_state: MouseButtonState::Up,
                                    ..
                                } = event
                                {
                                    let app = tray.app_handle();
                                    if let Some(w) = app.get_webview_window("main") {
                                        let _ = w.show();
                                        let _ = w.set_focus();
                                    }
                                }
                            })
                            .build(app)
                            .map_err(|e| tracing::warn!("Tray icon warning: {}", e))
                            .ok();
                    }
                    None => {
                        tracing::warn!("App icon not found, tray skipped");
                    }
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    // 
                    let has_tray = window.app_handle().default_window_icon().is_some();
                    if has_tray {
                        let _ = window.hide();
                        api.prevent_close();
                        tracing::info!("App started");
                    } else {
                        tracing::info!("Tray unavailable, allowing close");
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!("Error: {}", e);
            std::process::exit(1);
        });
}
