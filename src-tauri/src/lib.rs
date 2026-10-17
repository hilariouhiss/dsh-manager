pub mod error;
pub mod ipc;

/// 装配点托管的统一状态。**字段名与顺序按主计划 §3.3.2 冻结**：后续每条任务线只往这个
/// 结构体里追加自己那一行，不改形状、不改名、不删已有字段。完整顺序：
/// 1 `settings: Arc<SettingsStore>`（T-S3）· 2 `oplog: Arc<Oplog>`（T-S5）·
/// 3 `cancel: Arc<CancelRegistry>`（T-S6）· 4 `registry: Arc<dyn RegistrySource>`（T-V1）·
/// 5 `package_manager: Arc<dyn PackageManager>`（T-O1）· 6 `process_probe: Arc<dyn ProcessProbe>`（T-I1）·
/// 7 `lister: Arc<dyn InstanceLister>`（T-I1）· 8 `runner: Arc<InstanceRunner>`（T-I2）·
/// 9 `autostart: Arc<dyn AutostartPort>`（T-A1）· 10 `paths: DataPaths`（T-S3，§3.6 的三个目录）。
///
/// 本步用单元结构体占位：上面 10 个字段的类型在本步**一个都还不存在**（`DataPaths` 与
/// `SettingsStore` 由 T-S3 产出）。形状先冻结在这里，字段随其类型出现由产出任务逐行补齐——
/// 这样 T-V1/T-O1/T-I1/T-I2/T-A1 五条线不会各自发明一个 `AppState`。
pub struct AppState;

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
        .invoke_handler(tauri::generate_handler![ipc::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
