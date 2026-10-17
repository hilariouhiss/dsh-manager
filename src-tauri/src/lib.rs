pub mod adapters;
pub mod error;
pub mod ipc;
pub mod m0;
pub mod m1;
pub mod m2;
pub mod m3;
pub mod m4;
pub mod ports;

use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter as _, Manager};
use tauri_plugin_dialog::{
    DialogExt as _, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use crate::adapters::AllPackageManagers;
use crate::adapters::npm_registry::NpmRegistry;
use crate::adapters::sysinfo_probe::SysinfoProbe;
use crate::error::{AppError, CmdResult};
use crate::m0::cancel::CancelRegistry;
use crate::m0::logging;
use crate::m0::oplog::Oplog;
use crate::m0::paths::DataPaths;
use crate::m0::settings::{self, SettingsStore};
use crate::m1::version_check::{OriginFacts, SharedOriginFacts};
use crate::m2::origin_detect::{self, OriginPaths, OriginReport, PER_SOURCE_TIMEOUT};
use crate::m3::instance_discovery::{InstanceInfo, InstanceLister, ProcessLister};
use crate::m3::instance_runner::{
    self, InstanceEmitter, InstanceOutputPayload, InstanceRunner, InstanceStatePayload,
    InstanceUrlPayload,
};
use crate::m3::output_buffer::OutputLine;
use crate::m4::autostart::AutostartPort;
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
    pub runner: Arc<InstanceRunner>,
    pub autostart: Arc<dyn AutostartPort>,
    pub paths: DataPaths,
}

/// §3.4 命令 6：列出当前运行的 DSH Web 形态实例（无实例时返回空数组，不报错）。
/// 命令只认 InstanceLister 这个 trait，不认 M3 的实现类型——M1 的安装前检测也走同一个入口。
#[tauri::command]
fn list_instances(state: tauri::State<'_, AppState>) -> CmdResult<Vec<InstanceInfo>> {
    state.lister.list_web_instances()
}

/// 真实的事件出口：每次 emit 直接经 `AppHandle` 发出去。
/// 它由 `InstanceRunner` 持有，所以 `AppState` 不需要事件字段。
struct TauriEmitter {
    app: tauri::AppHandle,
}

impl TauriEmitter {
    fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl InstanceEmitter for TauriEmitter {
    fn emit_state(&self, payload: InstanceStatePayload) {
        let _ = self.app.emit(crate::ipc::EVENT_INSTANCE_STATE, payload);
    }

    fn emit_output(&self, payload: InstanceOutputPayload) {
        let _ = self.app.emit(crate::ipc::EVENT_INSTANCE_OUTPUT, payload);
    }

    fn emit_url(&self, payload: InstanceUrlPayload) {
        let _ = self.app.emit(crate::ipc::EVENT_INSTANCE_URL, payload);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartInstanceOutput {
    instance_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListOutputOutput {
    lines: Vec<OutputLine>,
    next_seq: u64,
    truncated: bool,
}

/// §3.4 命令 7：用本工具的实例启动 DSH Web，返回实例标识。
#[tauri::command]
async fn start_instance(
    state: tauri::State<'_, AppState>,
    args: Vec<String>,
    work_dir: String,
) -> CmdResult<StartInstanceOutput> {
    let launcher = crate::adapters::resolve_on_path("dsh").ok_or_else(|| {
        AppError::new(
            crate::error::INSTANCE_SPAWN_FAILED,
            "PATH 中找不到 dsh 启动器，无法启动实例",
            "先在本机命令行执行 dsh --version 确认 DSH 已安装；若找不到，请到「版本与更新」页安装后再试。",
        )
    })?;

    let settings = state.settings.snapshot();

    // FR-3.1 与 §7.1 偏差 D-B 的落点：设置里的默认启动参数（["--no-open"]）先于本次请求的参数。
    // 少了这一句，dsh 会自己打开浏览器，autoOpenBrowser 再打开一次——T-E2「只打开一次」必失败。
    let merged_args = instance_runner::merge_launch_args(&settings.launch_args, &args);

    // FR-3.1：工作目录留空时回落到设置里的 workDir（§3.7 保证它不是空白串）。
    let work_dir = if work_dir.trim().is_empty() {
        settings.work_dir.clone()
    } else {
        work_dir
    };

    let instance_id = format!("inst-{}", crate::m3::now_ms());
    // §3.6 的实例日志落在 DataPaths::log_dir() 下（不要自己拼 "logs" 子目录）。
    let log_path = state.paths.instance_log(&instance_id);

    let spec = instance_runner::build_start_spec(
        &launcher,
        &merged_args,
        std::path::Path::new(&work_dir),
        &instance_id,
        settings.output_buffer_lines as usize,
        &log_path,
        u64::from(settings.log_max_mib) * 1024 * 1024,
        settings.log_keep_files,
    );

    let id = instance_runner::start(&state.runner, state.process_probe.as_ref(), spec).await?;

    Ok(StartInstanceOutput { instance_id: id })
}

/// §3.4 命令 8：停止实例。三档确认强度见 §3.9 D-3：
/// 外部实例必须显式确认、先礼后兵（不带 /F）、超时不自动强制、停止后必定复核。
#[tauri::command]
async fn stop_instance(
    state: tauri::State<'_, AppState>,
    instance_id: String,
    force: bool,
    confirm_external: bool,
) -> CmdResult<instance_runner::StopResult> {
    // TaskKill 无状态：装配点不需要托管它，也不需要往 AppState 里加字段。
    let kill = instance_runner::TaskKill::real();
    instance_runner::stop(
        state.runner.as_ref(),
        state.process_probe.as_ref(),
        &kill,
        instance_runner::StopRequest {
            instance_id,
            force,
            confirm_external,
        },
        instance_runner::STOP_WAIT,
        instance_runner::FORCE_WAIT,
    )
    .await
}

/// §3.4 命令 9：按 `fromSeq` 取输出分页。
#[tauri::command]
fn list_output(
    state: tauri::State<'_, AppState>,
    instance_id: String,
    from_seq: u64,
) -> CmdResult<ListOutputOutput> {
    let (lines, next_seq, truncated) =
        instance_runner::list_output(state.runner.as_ref(), &instance_id, from_seq)?;
    Ok(ListOutputOutput {
        lines,
        next_seq,
        truncated,
    })
}

/// §3.4 命令 10：把实例日志导出到用户选定的路径，返回写入字节数。
#[tauri::command]
fn export_output(
    state: tauri::State<'_, AppState>,
    instance_id: String,
    target_path: String,
) -> CmdResult<ipc::ExportResult> {
    let bytes_written = instance_runner::export_output(
        state.runner.as_ref(),
        &instance_id,
        std::path::Path::new(&target_path),
    )?;
    Ok(ipc::ExportResult { bytes_written })
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

/// 退出前的阻塞式三选一（§3.9「应用退出时的例外」第 3 步）。
///
/// 返回按钮文案，交给 map_exit_choice 翻译。默认按钮是「强制终止」，但**必须由用户点**；
/// 直接关掉对话框时按「取消退出」处理（不可逆动作上保守才是对的）。
fn ask_force_or_keep(
    app: &tauri::AppHandle,
    pending: &[instance_runner::UnstoppableInstance],
) -> String {
    let result = app
        .dialog()
        .message(format!(
            "以下实例在等待时限内没有退出：\n{}\n\n强制终止可能丢失它正在进行的任务。要怎么做？",
            instance_runner::describe_instances(pending)
        ))
        .title("实例未响应")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            instance_runner::BTN_FORCE.to_string(),
            instance_runner::BTN_KEEP.to_string(),
            instance_runner::BTN_CANCEL.to_string(),
        ))
        .blocking_show_with_result();

    match result {
        MessageDialogResult::Custom(label) => label,
        MessageDialogResult::Yes | MessageDialogResult::Ok => {
            instance_runner::BTN_FORCE.to_string()
        }
        MessageDialogResult::No => instance_runner::BTN_KEEP.to_string(),
        MessageDialogResult::Cancel => instance_runner::BTN_CANCEL.to_string(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 登录项里写固定的启动参数；启动行为由 settings.autostartMode 决定（见子计划 §0.2）
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg(m4::autostart::LOGIN_ENTRY_ARG)
                .build(),
        )
        .plugin(tauri_plugin_log::Builder::new().build())
        .on_window_event(|window, event| {
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let app = window.app_handle().clone();
            // 先把句柄取出来，别让 State 借用跟着 app 进异步块。
            let (runner, probe, oplog, marker, policy, stop_on_exit) = {
                let state = app.state::<AppState>();
                let settings = state.settings.snapshot();
                (
                    state.runner.clone(),
                    state.process_probe.clone(),
                    state.oplog.clone(),
                    instance_runner::pending_exit_file(&state.paths.cache_file()),
                    logging::oplog_policy(&settings),
                    settings.stop_instance_on_exit,
                )
            };

            // 没开这个设置、或本来就没有本工具实例：按默认行为直接关（FR-4.4）。
            if !stop_on_exit || runner.running_ids().is_empty() {
                return;
            }

            // 先拦住这次关闭，做完决定再真的退（§3.9「应用退出时的例外」）。
            api.prevent_close();

            tauri::async_runtime::spawn(async move {
                // TaskKill 无状态，现造一个即可（AppState 里没有这个字段，也不该有）。
                let kill = instance_runner::TaskKill::real();
                match instance_runner::try_stop_all_on_exit(
                    runner.as_ref(),
                    probe.as_ref(),
                    &kill,
                    true,
                )
                .await
                {
                    // 第 2 步：停干净了（或本来无事可做）→ 直接退出。
                    instance_runner::ExitOutcome::NothingToDo
                    | instance_runner::ExitOutcome::AllStopped(_) => app.exit(0),

                    // 第 3 步：阻塞式三选一。默认按钮是「强制终止」，但**必须由用户点**。
                    instance_runner::ExitOutcome::NeedsDecision(pending) => {
                        match instance_runner::map_exit_choice(&ask_force_or_keep(&app, &pending)) {
                            instance_runner::ExitChoice::Force => {
                                // 用户点了才允许补 /F（§3.9 第 3 步）。
                                let stopped = instance_runner::force_stop_pending(
                                    runner.as_ref(),
                                    probe.as_ref(),
                                    &kill,
                                    &pending,
                                )
                                .await;
                                // /F 也可能停不下来：剩下的实例必须按「保留运行并退出」收尾，
                                // 否则应用退出后它就成了没有存档、没有操作记录的孤儿。
                                let leftovers =
                                    instance_runner::remaining_after_force(&pending, &stopped);
                                if !leftovers.is_empty() {
                                    log::error!(
                                        "强制终止后仍有实例未停止，按「保留运行并退出」处理：{}",
                                        instance_runner::describe_instances(&leftovers)
                                    );
                                    let entry =
                                        instance_runner::forced_stop_failed_oplog_entry(&leftovers);
                                    if let Err(e) = oplog.append(&entry, policy) {
                                        log::warn!(
                                            "记录「强制终止失败」的操作记录失败：{}：{}",
                                            e.code,
                                            e.reason
                                        );
                                    } else {
                                        ipc::emit_oplog(&app, &entry);
                                    }
                                    if let Err(e) = instance_runner::remember_skipped_instances(
                                        &marker,
                                        &leftovers,
                                    ) {
                                        log::warn!(
                                            "写下次启动提示的存档失败：{}：{}",
                                            e.code,
                                            e.reason
                                        );
                                    }
                                }
                                app.exit(0);
                            }
                            instance_runner::ExitChoice::KeepAndExit => {
                                // 第 4 步：如实记一条操作记录，并留下下次启动要提示的存档。
                                let entry = instance_runner::keep_running_oplog_entry(&pending);
                                if let Err(e) = oplog.append(&entry, policy) {
                                    log::warn!("记录「保留运行并退出」失败：{}：{}", e.code, e.reason);
                                } else {
                                    ipc::emit_oplog(&app, &entry);
                                }
                                if let Err(e) =
                                    instance_runner::remember_skipped_instances(&marker, &pending)
                                {
                                    log::warn!("写下次启动提示的存档失败：{}：{}", e.code, e.reason);
                                }
                                app.exit(0);
                            }
                            // 取消退出：什么都不做，窗口保持打开（prevent_close 已经生效）。
                            instance_runner::ExitChoice::CancelExit => {}
                        }
                    }
                }
            });
        })
        .invoke_handler(tauri::generate_handler![
            ipc::get_settings,
            ipc::update_settings,
            ipc::get_autostart_state,
            ipc::set_autostart,
            ipc::list_oplog,
            ipc::export_diagnostics,
            ipc::check_updates,
            ipc::set_auto_channel,
            ipc::start_install,
            ipc::cancel_task,
            list_instances,
            detect_origin,
            start_instance,
            stop_instance,
            list_output,
            export_output
        ])
        .setup(|app| {
            let paths = DataPaths::resolve(app.handle())
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            paths
                .ensure_dirs()
                .map_err(|e| format!("{}：{}", e.code, e.reason))?;
            let settings = Arc::new(
                SettingsStore::load_or_init(paths.settings_file())
                    .map_err(|e| format!("{}：{}", e.code, e.reason))?,
            );
            let oplog = Arc::new(Oplog::new(paths.oplog_file()));

            // 设置损坏：回落默认值 + 记一条操作记录 + 推事件，不阻断启动（§3.7、NFR-7）
            if let Some(raw) = settings.startup_problem() {
                log::warn!("设置文件存在问题，已回落默认值：{raw}");
                let entry = settings::record_corrupt(
                    oplog.as_ref(),
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
            // FR-3.6 输出触发的操作记录出口：真实装配写 oplog 并推 oplog:entry 事件。
            let runner = Arc::new(
                InstanceRunner::new(Arc::new(TauriEmitter::new(app.handle().clone())))
                    .with_failure_recorder({
                        let oplog = oplog.clone();
                        let settings = settings.clone();
                        let handle = app.handle().clone();
                        Arc::new(move |entry: &crate::m0::oplog::OplogEntry| {
                            let policy = logging::oplog_policy(&settings.snapshot());
                            match oplog.append(entry, policy) {
                                Ok(()) => ipc::emit_oplog(&handle, entry),
                                Err(e) => log::warn!(
                                    "记录实例启动失败的操作记录失败：{}：{}",
                                    e.code,
                                    e.reason
                                ),
                            }
                        })
                    }),
            );
            // lister 与 runner 共享同一份受管登记，实例枚举才能把 cmd.exe 外壳下的 node 认成本工具实例。
            let lister: Arc<dyn InstanceLister> = Arc::new(ProcessLister::new(
                process_probe.clone(),
                runner.managed_registry(),
            ));
            app.manage(AppState {
                settings: settings.clone(),
                oplog: oplog.clone(),
                cancel: Arc::new(CancelRegistry::new()),
                registry: Arc::new(NpmRegistry::from_net()),
                package_manager: Arc::new(AllPackageManagers),
                process_probe,
                lister,
                runner,
                // M4 的登录项端口：命令 13/14 通过它读写系统（适配器只在装配点出现）
                autostart: Arc::new(crate::adapters::autostart_sys::SysAutostart::new(
                    app.handle().clone(),
                )),
                paths,
            });

            // §3.9 第 4 步：上次退出时若选了「保留运行并退出」，这里如实提示一次并清掉存档。
            // 提示必须在后台线程里弹：blocking_show 不能阻塞主线程，而 setup 就跑在主线程上。
            let notice_marker = instance_runner::pending_exit_file(
                &app.state::<AppState>().paths.cache_file(),
            );
            match instance_runner::take_startup_notice(&notice_marker) {
                Ok(pending) if !pending.is_empty() => {
                    let handle = app.handle().clone();
                    let who = instance_runner::describe_instances(&pending);
                    std::thread::spawn(move || {
                        handle
                            .dialog()
                            .message(format!(
                                "上次退出时还有实例未能停止：\n{who}\n\n可以在「运行实例」页现在停止它。"
                            ))
                            .title("上次退出时实例未停止")
                            .blocking_show();
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("读取上次退出时的未停止实例记录失败：{}：{}", e.code, e.reason);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
