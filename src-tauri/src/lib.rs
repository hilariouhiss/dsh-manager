pub mod adapters;
pub mod error;
pub mod ipc;
pub mod m0;
pub mod m1;
pub mod ports;

use std::sync::Arc;

use tauri::Manager;

use crate::adapters::npm_registry::NpmRegistry;
use crate::m0::cancel::CancelRegistry;
use crate::m0::logging;
use crate::m0::oplog::Oplog;
use crate::m0::paths::DataPaths;
use crate::m0::settings::{self, SettingsStore};
use crate::ports::registry::RegistrySource;

/// 装配点托管的统一状态。**字段名与顺序按主计划 §3.3.2 冻结**，任何任务都不得改名、
/// 不得增删已有字段；尚未落地的字段由各自产出任务按下列顺序追加：
/// 1 `settings: Arc<SettingsStore>`（T-S3）· 2 `oplog: Arc<Oplog>`（T-S5，本条）·
/// 3 `cancel: Arc<CancelRegistry>`（T-S6）· 4 `registry: Arc<dyn RegistrySource>`（T-V1）·
/// 5 `package_manager: Arc<dyn PackageManager>`（T-O1）· 6 `process_probe: Arc<dyn ProcessProbe>`（T-I1）·
/// 7 `lister: Arc<dyn InstanceLister>`（T-I1）· 8 `runner: Arc<InstanceRunner>`（T-I2）·
/// 9 `autostart: Arc<dyn AutostartPort>`（T-A1）· 10 `paths: DataPaths`（T-S3）。
pub struct AppState {
    pub settings: Arc<SettingsStore>,
    pub oplog: Arc<Oplog>,
    pub cancel: Arc<CancelRegistry>,
    pub registry: Arc<dyn RegistrySource>,
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
            ipc::update_settings,
            ipc::list_oplog,
            ipc::export_diagnostics
        ])
        .setup(|app| {
            let paths = DataPaths::resolve(app.handle())
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            paths
                .ensure_dirs()
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            let settings = SettingsStore::load_or_init(paths.settings_file())
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            let oplog = Oplog::new(paths.oplog_file());

            // 设置损坏：回落默认值 + 记一条操作记录 + 推事件，不阻断启动（§3.7、NFR-7）
            if let Some(raw) = settings.startup_problem() {
                log::warn!("设置文件存在问题，已回落默认值：{raw}");
                let entry = settings::record_corrupt(
                    &oplog,
                    settings.file(),
                    raw,
                    logging::oplog_policy(&settings.snapshot()),
                )
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
                ipc::emit_oplog(app.handle(), &entry);
            }

            app.manage(AppState {
                settings: Arc::new(settings),
                oplog: Arc::new(oplog),
                cancel: Arc::new(CancelRegistry::new()),
                registry: Arc::new(NpmRegistry::from_net()),
                paths,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
