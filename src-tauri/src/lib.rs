pub mod error;
pub mod ipc;
pub mod m0;

use std::sync::Arc;

use tauri::Manager;

use crate::m0::paths::DataPaths;
use crate::m0::settings::SettingsStore;

/// 装配点托管的统一状态（T-S1 已按主计划 §3.3.2 冻结形状，本步给出带字段的版本）。
/// 字段名与顺序冻结：settings / oplog / cancel / registry / package_manager /
/// process_probe / lister / runner / autostart / paths。
pub struct AppState {
    pub settings: Arc<SettingsStore>,
    pub paths: DataPaths,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_log::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            ipc::get_settings,
            ipc::update_settings
        ])
        .setup(|app| {
            let paths = DataPaths::resolve(app.handle())
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            paths
                .ensure_dirs()
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            let settings = SettingsStore::load_or_init(paths.settings_file())
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            if let Some(raw) = settings.startup_problem() {
                log::warn!("设置文件存在问题，已回落默认值：{raw}");
            }
            app.manage(AppState {
                settings: Arc::new(settings),
                paths,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
