pub mod adapters;
pub mod error;
pub mod ipc;
pub mod m0;
pub mod m1;
pub mod m2;
pub mod m3;
pub mod ports;

use std::sync::Arc;

use tauri::Manager;

use crate::adapters::AllPackageManagers;
use crate::adapters::npm_registry::NpmRegistry;
use crate::adapters::sysinfo_probe::SysinfoProbe;
use crate::error::CmdResult;
use crate::m0::cancel::CancelRegistry;
use crate::m0::logging;
use crate::m0::oplog::Oplog;
use crate::m0::paths::DataPaths;
use crate::m0::settings::{self, SettingsStore};
use crate::m1::version_check::{OriginFacts, SharedOriginFacts};
use crate::m2::origin_detect::{self, OriginPaths, OriginReport, PER_SOURCE_TIMEOUT};
use crate::m3::instance_discovery::{InstanceInfo, InstanceLister, ProcessLister};
use crate::ports::package_manager::PackageManager;
use crate::ports::process::ProcessProbe;
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
    pub package_manager: Arc<dyn PackageManager>,
    pub process_probe: Arc<dyn ProcessProbe>,
    pub lister: Arc<dyn InstanceLister>,
    pub paths: DataPaths,
}

/// §3.4 命令 6：列出当前运行的 DSH Web 形态实例（无实例时返回空数组，不报错）。
/// 命令只认 InstanceLister 这个 trait，不认 M3 的实现类型——M1 的安装前检测也走同一个入口。
#[tauri::command]
fn list_instances(state: tauri::State<'_, AppState>) -> CmdResult<Vec<InstanceInfo>> {
    state.lister.list_web_instances()
}

/// §3.4 命令 5：识别本机 DSH 的安装来源，返回四值结论与依据清单。
///
/// 同一次探测同时写回 `SharedOriginFacts`：版本页不必先访问「安装来源」页，
/// 也能拿到本机版本与来源结论（裁定 3）。`Unidentified` 是正常返回值，不是错误。
#[tauri::command]
async fn detect_origin(
    state: tauri::State<'_, AppState>,
    facts: tauri::State<'_, SharedOriginFacts>,
) -> CmdResult<OriginReport> {
    let pm = state.package_manager.clone();
    let (report, fresh) = origin_detect::detect_and_gather(
        pm.as_ref(),
        &OriginPaths::for_this_machine(),
        PER_SOURCE_TIMEOUT,
    )
    .await;
    *facts
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = fresh;
    Ok(report)
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
            ipc::export_diagnostics,
            ipc::check_updates,
            ipc::set_auto_channel,
            ipc::start_install,
            ipc::cancel_task,
            list_instances,
            detect_origin
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

            // 来源识别尚未落地：默认「未识别」，check_updates 因此不产出任何版本结论。
            // T-O2 只更新这同一份共享状态，AppState 的冻结字段表保持不变。
            app.manage(SharedOriginFacts::new(std::sync::RwLock::new(
                OriginFacts::unidentified("来源识别尚未落地", None),
            )));
            let process_probe: Arc<dyn ProcessProbe> = Arc::new(SysinfoProbe::new());
            let lister: Arc<dyn InstanceLister> =
                Arc::new(ProcessLister::new(process_probe.clone()));
            app.manage(AppState {
                settings: Arc::new(settings),
                oplog: Arc::new(oplog),
                cancel: Arc::new(CancelRegistry::new()),
                registry: Arc::new(NpmRegistry::from_net()),
                package_manager: Arc::new(AllPackageManagers),
                process_probe,
                lister,
                paths,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
