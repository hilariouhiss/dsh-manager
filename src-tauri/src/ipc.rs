//! 命令与事件的汇聚点（冻结契约见主计划 §3.4、§3.5）。

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;
use crate::error::{AppError, CmdResult};
use crate::m0::cancel::CancelToken;
use crate::m0::diagnostics::{self, DiagnosticsInput};
use crate::m0::logging;
use crate::m0::oplog::{Oplog, OplogEntry, OplogPage};
use crate::m0::settings::{self, Settings, SettingsStore};
use crate::m1::channels::{ChannelsCache, by_id};
use crate::m1::install_runner;
use crate::m1::installer::{
    InstallPhase, InstallRequest, TaskProgress, available_space_for, cancelled_error,
    download_and_verify,
};
use crate::m1::version_check::{
    AutoChannelSettings, CheckUpdatesReport, SharedOriginFacts, apply_auto_channel,
    auto_install_notice, check_updates as run_check_updates,
};
use crate::m3::instance_discovery::InstanceLister;
use crate::ports::package_manager::{PackageManager, PmInfo};
use crate::ports::registry::ChannelProbe;

/// 事件名常量（主计划 §3.5）。前端由 src/ipc/types.ts 的 EVENTS 镜像同名同值。
pub const EVENT_TASK_PROGRESS: &str = "task:progress";
pub const EVENT_INSTANCE_STATE: &str = "instance:state";
pub const EVENT_INSTANCE_OUTPUT: &str = "instance:output";
pub const EVENT_INSTANCE_URL: &str = "instance:url";
pub const EVENT_UPDATE_AVAILABLE: &str = "update:available";
pub const EVENT_OPLOG_ENTRY: &str = "oplog:entry";

/// 命令 15 的出参（主计划 §3.4）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub bytes_written: u64,
}

/// 把新产生的操作记录推给前端（主计划 §3.5 事件 oplog:entry）。
pub fn emit_oplog<R: tauri::Runtime>(app: &AppHandle<R>, entry: &OplogEntry) {
    if let Err(e) = app.emit(EVENT_OPLOG_ENTRY, entry) {
        log::warn!("推送 oplog:entry 失败：{e}");
    }
}

/// 命令 11：取当前生效设置。
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    Ok(state.settings.snapshot())
}

/// 命令 12：应用部分补丁、即时持久化并记一条操作记录（§3.7）。
#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: serde_json::Value,
) -> CmdResult<Settings> {
    let before = state.settings.snapshot();
    let (next, problems) = state.settings.apply_patch(&patch)?;

    let entry = settings::record_change(
        &state.oplog,
        state.settings.file(),
        &before,
        &next,
        logging::oplog_policy(&next),
    )?;
    if problems.is_empty() {
        log::info!("设置已更新");
    } else {
        log::warn!("设置补丁存在被回落或拒绝的字段：{}", problems.join("；"));
    }
    emit_oplog(&app, &entry);
    Ok(next)
}

/// 命令 16：分页读取操作记录（最新在前）。
#[tauri::command]
pub fn list_oplog(state: State<'_, AppState>, offset: usize, limit: usize) -> CmdResult<OplogPage> {
    state.oplog.read_page(offset, limit)
}

/// 命令 15：导出诊断包（不含凭据，NFR-6）。三个目录直接从 §3.3.2 冻结的 `AppState.paths` 取。
#[tauri::command]
pub fn export_diagnostics(
    state: State<'_, AppState>,
    target_path: String,
) -> CmdResult<ExportResult> {
    let settings = state.settings.snapshot();
    let page = state.oplog.read_page(0, diagnostics::OPLOG_TAIL)?;
    let input = DiagnosticsInput {
        paths: &state.paths,
        settings: &settings,
        entries: &page.entries,
        local_dsh_version: None,
        origin_verdict: None,
    };
    let bytes_written = diagnostics::export(&input, Path::new(&target_path))?;
    Ok(ExportResult { bytes_written })
}

/// `set_auto_channel` 的写入路径：校验、落盘并记一条设置变更记录（设计 §7.2）。
///
/// 与 `update_settings` 一致：新旧设置都交给 `settings::record_change`，
/// 单独拆出来是为了能在没有 Tauri 应用的情况下测试「设置变更可追溯」。
fn apply_auto_channel_settings(
    store: &SettingsStore,
    oplog: &Oplog,
    channel: Option<String>,
    enabled: bool,
) -> CmdResult<(Settings, OplogEntry)> {
    let before = store.snapshot();
    let current = AutoChannelSettings {
        auto_channel: before.auto_channel.clone(),
        auto_install: before.auto_install,
        confirm_before_install: before.confirm_before_install,
    };
    let channel = channel.unwrap_or_default();
    if enabled && by_id(&channel).is_none() {
        return Err(AppError::new(
            crate::error::REGISTRY_PARSE,
            format!("未定义的通道：{channel}。"),
            "请从界面提供的通道列表中选择。",
        )
        .retryable(false));
    }
    let next = apply_auto_channel(&current, &channel, enabled);
    // 只写这两个字段：autoChannel 与 autoInstall 必须**同时**落下，
    // 否则重启后会出现「开关开着但没有通道」或反之（FR-1.4 的单通道约束）。
    let patch = serde_json::json!({
        "autoChannel": next.auto_channel,
        "autoInstall": next.auto_install,
    });
    let (applied, _problems) = store.apply_patch(&patch)?;
    let entry = settings::record_change(
        oplog,
        store.file(),
        &before,
        &applied,
        logging::oplog_policy(&applied),
    )?;
    Ok((applied, entry))
}

/// §3.4 命令 2：设置自动安装通道，返回生效后的完整设置。
///
/// 前端**必须**在调用本命令之前完成切换确认（FR-1.4）；命令本身不再弹确认。
/// 写入路径：`apply_auto_channel` 算出实际要写的两个字段（单通道约束在这里生效），
/// 再交给 `SettingsStore::apply_patch` 校验 + 立即落盘（§3.3.2 禁止自建落盘），
/// 最后追加一条操作记录并推送 `oplog:entry`（设计 §7.2）。
#[tauri::command]
pub fn set_auto_channel(
    app: AppHandle,
    channel: Option<String>,
    enabled: bool,
    state: State<'_, AppState>,
) -> CmdResult<Settings> {
    let (applied, entry) =
        apply_auto_channel_settings(&state.settings, &state.oplog, channel, enabled)?;
    emit_oplog(&app, &entry);
    Ok(applied)
}

/// §3.4 命令 1：检查更新。入参 `{ channels: string[] }`（主计划 §3.4 冻结）。
///
/// 本机版本与来源结论都从 T-O2 写入的共享状态里取；M1 不自己探测包管理器，也不探测来源。
/// 检查完成后按 §3.5 的 update:available 判定决定是否推送自动安装提示。
#[tauri::command]
pub async fn check_updates(
    app: AppHandle,
    channels: Vec<String>,
    state: State<'_, AppState>,
    facts: State<'_, SharedOriginFacts>,
) -> CmdResult<CheckUpdatesReport> {
    let cache_file = state.paths.cache_file();
    let (previous, cache_error) = ChannelsCache::load(&cache_file);
    if let Some(problem) = cache_error {
        log::warn!("{problem}");
    }
    // 读锁只持有到克隆完成，不跨 await；中毒时取回内部值，不让一次旧 panic 卡住检查。
    let facts = facts
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let report = run_check_updates(
        state.registry.as_ref(),
        &facts,
        &channels,
        &previous,
        &cache_file,
        std::time::Duration::from_secs(8),
    )
    .await?;

    // 自动安装提示：只对「已开启自动安装且所选通道严格高于本机版本」推送（FR-1.4）。
    let settings = state.settings.snapshot();
    let auto_settings = AutoChannelSettings {
        auto_channel: settings.auto_channel,
        auto_install: settings.auto_install,
        confirm_before_install: settings.confirm_before_install,
    };
    // 推送失败不能把检查结果降级为失败：界面仍可直接展示检查结果
    if let Some(notice) = auto_install_notice(
        &auto_settings,
        facts.local_version.as_ref(),
        facts.version_applicable(),
        &report.channels,
    ) && let Err(e) = app.emit(EVENT_UPDATE_AVAILABLE, notice)
    {
        log::warn!("推送 update:available 失败：{e}");
    }
    Ok(report)
}

/// 命令 3 的出参（主计划 §3.4）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartInstallResult {
    pub task_id: String,
}

/// 命令 4 的出参（主计划 §3.4）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTaskResult {
    pub accepted: bool,
}

/// 下载校验完成、安装开始前的取消守卫（NFR-2 / 裁定 d）。
///
/// `start_install` 先注销任务再查令牌：取消若发生在注销前会被令牌记下，这里拦截并删除
/// 已验证的安装包，返回与下载阶段相同的 `download.cancelled`，绝不进入安装。
fn reject_if_cancelled_after_verify(token: &CancelToken, tarball: &Path) -> CmdResult<()> {
    if !token.is_cancelled() {
        return Ok(());
    }
    let _ = std::fs::remove_file(tarball);
    Err(cancelled_error())
}

/// 下载校验完成后的安装步骤（T-V6）：运行检测 → 安装 → 复核 → 清理 → 记一条操作记录。
///
/// 依赖全部显式传入（临时包、通道/版本、包管理器、实例枚举、操作记录），
/// 因此不建 Tauri 应用也能单测；事件推送留在命令层，测试不需要假 AppHandle。
async fn install_after_download(
    tarball: &Path,
    target: &str,
    pms: &[PmInfo],
    pm: &dyn PackageManager,
    lister: &dyn InstanceLister,
    oplog: &Oplog,
    policy: logging::RollPolicy,
) -> (CmdResult<install_runner::InstallReport>, OplogEntry) {
    let result = install_runner::run(tarball, pms, pm, lister).await;
    let entry = match &result {
        Ok(report) => {
            // 安装已完成：清理失败只记日志，不能把一次成功的安装改判为失败（FR-1.6）
            if let Err(e) = install_runner::cleanup_after_success(report, tarball) {
                log::warn!("安装成功但清理临时包失败：{}：{}", e.code, e.reason);
            }
            let mut entry = OplogEntry::ok("install", target);
            entry.raw = Some(format!(
                "installed_before={} -> installed_after={}",
                report.installed_before.as_deref().unwrap_or("未安装"),
                report.installed_after.as_deref().unwrap_or("未读到"),
            ));
            entry
        }
        Err(e) => OplogEntry::fail(
            "install",
            target,
            &e.code,
            e.raw.clone().unwrap_or_else(|| e.reason.clone()),
        ),
    };
    // 操作记录落盘失败只记日志：用户最关心的是安装本身的结果（FR-1.6）
    if let Err(e) = oplog.append(&entry, policy) {
        log::warn!("写入安装操作记录失败：{}：{}", e.code, e.reason);
    }
    (result, entry)
}

/// §3.4 命令 3：开始安装。返回 taskId 后由 `task:progress` 事件推进度。
///
/// 步骤顺序不可颠倒（T-V5 要点）：空间与 HTTPS 检查 → 经端口下载 → 进度 → 校验。
/// 下载走 `RegistrySource::download`，**不直接使用 `m0::net::client()`**（§3.3、§7.4 #4）。
///
/// 入参扁平为 `{ channel, version }`：§3.4 冻结的是两个顶层键，不是 `{ args: {...} }`。
#[tauri::command]
pub async fn start_install(
    channel: String,
    version: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<StartInstallResult> {
    let release = match state.registry.probe(&channel).await {
        ChannelProbe::Published(r) => r,
        ChannelProbe::Unpublished => {
            return Err(AppError::new(
                crate::error::REGISTRY_PARSE,
                format!("通道 {channel} 当前没有已发布的版本可安装。"),
                "先点「检查更新」，等该通道发布后再试。",
            )
            .retryable(true));
        }
        ChannelProbe::Failed(e) => return Err(e),
    };
    if release.version.to_string() != version {
        return Err(AppError::new(
            crate::error::REGISTRY_PARSE,
            format!(
                "通道 {channel} 的最新版本已变为 {}，与请求的 {version} 不一致。",
                release.version
            ),
            "重新点「检查更新」后再发起安装。",
        )
        .retryable(true));
    }

    // §3.6：临时包路径由 DataPaths::tarball 净化产出（作用域包名里的 / 换成 -）
    let dest = state.paths.tarball(
        crate::m1::channels::PACKAGE_NAME,
        &release.version.to_string(),
    );
    // 审计重要-10：查的必须是**真正要写入的那个目录**所在的卷
    let available_bytes = available_space_for(&dest);
    let req = InstallRequest {
        channel,
        release,
        dest,
        available_bytes,
        // 端口不提前回报 Content-Length：解包体积已计入，压缩包字节数未知
        tarball_bytes: None,
    };

    let task_id = format!("install-{}", crate::m1::version_check::now_epoch_ms());
    let token = state.cancel.register(&task_id);
    // 下载要拿走令牌；留一份克隆用于「校验完成、安装开始前」的取消检查（NFR-2）。
    let pre_install = token.clone();
    let emit_app = app.clone();
    let emit_task = task_id.clone();
    let on_progress = move |p: TaskProgress| {
        let _ = emit_app.emit(
            EVENT_TASK_PROGRESS,
            serde_json::json!({
                "taskId": emit_task,
                "percent": p.percent,
                "remainingMs": p.remaining_ms,
                "phase": p.phase.as_str(),
            }),
        );
    };
    let verified =
        match download_and_verify(state.registry.as_ref(), &req, token, on_progress).await {
            Ok(outcome) => outcome,
            Err(e) => {
                state.cancel.remove(&task_id);
                return Err(e);
            }
        };

    // 安装阶段不可取消：先注销任务，之后 cancel_task 会如实返回 accepted=false（NFR-2）。
    // 注销后再查令牌，与 CancelRegistry::cancel 的互斥语义一起消除「取消被接受但仍安装」的窗口。
    state.cancel.remove(&task_id);
    // 下载/校验期间用户取消：不得进入安装；已验证的包按 §3.6 删除（NFR-2）。
    reject_if_cancelled_after_verify(&pre_install, &verified.path)?;

    if let Err(e) = app.emit(
        EVENT_TASK_PROGRESS,
        serde_json::json!({
            "taskId": task_id,
            "percent": 100,
            "remainingMs": null,
            "phase": InstallPhase::Installing.as_str(),
        }),
    ) {
        log::warn!("推送 task:progress（installing）失败：{e}");
    }

    let pms = state.package_manager.available().await;
    let policy = logging::oplog_policy(&state.settings.snapshot());
    let target = format!("{}/{}", req.channel, req.release.version);
    let (result, entry) = install_after_download(
        &verified.path,
        &target,
        &pms,
        state.package_manager.as_ref(),
        state.lister.as_ref(),
        &state.oplog,
        policy,
    )
    .await;
    emit_oplog(&app, &entry);
    result.map(|_report| StartInstallResult { task_id })
}

/// §3.4 命令 4：取消任务。重复取消安全（NFR-2）。
#[tauri::command]
pub fn cancel_task(task_id: String, state: State<'_, AppState>) -> CmdResult<CancelTaskResult> {
    Ok(CancelTaskResult {
        accepted: state.cancel.cancel(&task_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m0::oplog::OplogResult;
    use crate::m0::paths::{DataPaths, temp_root};
    use crate::m3::InstanceState;
    use crate::m3::instance_discovery::{InstanceInfo, InstanceLister};
    use crate::ports::package_manager::{InstallOutcome, PackageManager, PmInfo, PmKind};
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeLister {
        running: Vec<InstanceInfo>,
    }

    impl FakeLister {
        fn empty() -> Self {
            Self { running: vec![] }
        }

        fn with_running(pid: u32) -> Self {
            Self {
                running: vec![InstanceInfo {
                    instance_id: format!("fake-{pid}"),
                    managed: false,
                    pid,
                    started_at_ms: Some(1_760_000_000_000),
                    uptime_ms: Some(1_000),
                    url: Some("http://127.0.0.1:3080".to_string()),
                    state: InstanceState::Running,
                }],
            }
        }
    }

    impl InstanceLister for FakeLister {
        fn list_web_instances(&self) -> crate::error::CmdResult<Vec<InstanceInfo>> {
            Ok(self.running.clone())
        }
    }

    struct RecordingPm {
        versions: Mutex<Vec<Option<semver::Version>>>,
        install: Mutex<Option<crate::error::CmdResult<InstallOutcome>>>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingPm {
        fn new(
            versions: Vec<Option<&str>>,
            install: crate::error::CmdResult<InstallOutcome>,
        ) -> Self {
            Self {
                versions: Mutex::new(
                    versions
                        .into_iter()
                        .map(|v| v.map(|s| semver::Version::parse(s).unwrap()))
                        .collect(),
                ),
                install: Mutex::new(Some(install)),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl PackageManager for RecordingPm {
        async fn available(&self) -> Vec<PmInfo> {
            vec![pm_info()]
        }

        async fn installed_version(
            &self,
            _pm: &PmInfo,
        ) -> crate::error::CmdResult<Option<semver::Version>> {
            self.calls
                .lock()
                .unwrap()
                .push("installed_version".to_string());
            let mut versions = self.versions.lock().unwrap();
            Ok(if versions.len() > 1 {
                versions.remove(0)
            } else {
                versions.first().cloned().flatten()
            })
        }

        async fn install_from_tarball(
            &self,
            _pm: &PmInfo,
            _tarball: &Path,
        ) -> crate::error::CmdResult<InstallOutcome> {
            self.calls
                .lock()
                .unwrap()
                .push("install_from_tarball".to_string());
            self.install
                .lock()
                .unwrap()
                .take()
                .expect("安装最多只应被调用一次")
        }
    }

    fn pm_info() -> PmInfo {
        PmInfo {
            kind: PmKind::Npm,
            exe: PathBuf::from("C:\\Program Files\\nodejs\\npm.cmd"),
            version: "11.6.2".to_string(),
            global_root: PathBuf::from("C:\\Users\\me\\AppData\\Roaming\\npm"),
        }
    }

    fn policy() -> logging::RollPolicy {
        logging::RollPolicy {
            max_bytes: 1024 * 1024,
            keep_files: 3,
        }
    }

    /// 一次安装的夹具：目录、操作记录与**已校验的**临时包。
    fn install_fixture(tag: &str, version: &str) -> (DataPaths, Oplog, PathBuf) {
        let paths = DataPaths::under_root(&temp_root(tag));
        paths.ensure_dirs().unwrap();
        let oplog = Oplog::new(paths.oplog_file());
        let tarball = crate::m1::install_runner::tarball_path(
            &paths,
            &semver::Version::parse(version).unwrap(),
        );
        std::fs::write(&tarball, b"pretend verified tarball").unwrap();
        (paths, oplog, tarball)
    }

    #[tokio::test]
    async fn failing_install_keeps_the_tarball_and_records_one_failing_oplog_entry() {
        let (_paths, oplog, tarball) = install_fixture("ipc-install-fail", "0.2.0");
        let pm = RecordingPm::new(
            vec![Some("0.2.0-rc.2")],
            Err(crate::error::AppError::new(
                crate::error::INSTALL_FAILED,
                "npm 安装失败。",
                "按提示手工安装。",
            )
            .with_raw("npm ERR! code EACCES\nnpm ERR! permission denied")),
        );
        let lister = FakeLister::empty();

        let (result, entry) = install_after_download(
            &tarball,
            "latest/0.2.0",
            &[pm_info()],
            &pm,
            &lister,
            &oplog,
            policy(),
        )
        .await;

        let e = result.unwrap_err();
        assert_eq!(e.code, crate::error::INSTALL_FAILED);
        assert!(
            e.next_step.contains("npm install -g"),
            "失败必须给出手工安装命令：{}",
            e.next_step
        );
        assert!(tarball.exists(), "安装失败必须保留安装包（FR-1.6）");

        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 1, "一次安装只记一条操作记录");
        let logged = &page.entries[0];
        assert_eq!(logged.action, "install");
        assert_eq!(logged.target, "latest/0.2.0");
        assert_eq!(logged.result, OplogResult::Fail);
        assert_eq!(logged.code.as_deref(), Some(crate::error::INSTALL_FAILED));
        assert!(
            logged.raw.as_deref().unwrap_or_default().contains("EACCES"),
            "raw 必须是安装子进程原文：{:?}",
            logged.raw
        );
        assert_eq!(&entry, logged, "推送的条目就是落盘的那条");
    }

    #[tokio::test]
    async fn instances_running_refuses_before_calling_the_package_manager() {
        let (_paths, oplog, tarball) = install_fixture("ipc-install-running", "0.2.0");
        let pm = RecordingPm::new(
            vec![Some("0.2.0-rc.2")],
            Ok(InstallOutcome {
                exit_code: 0,
                output: "added 1 package".to_string(),
            }),
        );
        let lister = FakeLister::with_running(4242);

        let (result, entry) = install_after_download(
            &tarball,
            "latest/0.2.0",
            &[pm_info()],
            &pm,
            &lister,
            &oplog,
            policy(),
        )
        .await;

        let e = result.unwrap_err();
        assert_eq!(e.code, crate::error::INSTALL_INSTANCES_RUNNING);
        assert!(e.retryable);
        assert!(
            pm.calls().is_empty(),
            "有实例运行时包管理器一个方法都不该被调用：{:?}",
            pm.calls()
        );
        assert!(tarball.exists(), "被拒时不得删除安装包");
        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].result, OplogResult::Fail);
        assert_eq!(
            page.entries[0].code.as_deref(),
            Some(crate::error::INSTALL_INSTANCES_RUNNING)
        );
        assert_eq!(&entry, &page.entries[0]);
    }

    #[tokio::test]
    async fn successful_install_cleans_the_tarball_and_records_before_and_after() {
        let (_paths, oplog, tarball) = install_fixture("ipc-install-ok", "0.2.0");
        let pm = RecordingPm::new(
            vec![Some("0.2.0-rc.2"), Some("0.2.1-alpha.4")],
            Ok(InstallOutcome {
                exit_code: 0,
                output: "added 1 package".to_string(),
            }),
        );
        let lister = FakeLister::empty();

        let (result, entry) = install_after_download(
            &tarball,
            "latest/0.2.0",
            &[pm_info()],
            &pm,
            &lister,
            &oplog,
            policy(),
        )
        .await;

        let report = result.expect("安装成功");
        assert_eq!(report.installed_before.as_deref(), Some("0.2.0-rc.2"));
        assert_eq!(report.installed_after.as_deref(), Some("0.2.1-alpha.4"));
        assert!(!tarball.exists(), "成功后临时包必须清理");

        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].result, OplogResult::Ok);
        assert_eq!(page.entries[0].code, None);
        let raw = page.entries[0].raw.as_deref().expect("成功也要留前后版本");
        assert!(
            raw.contains("0.2.0-rc.2") && raw.contains("0.2.1-alpha.4") && raw.contains("->"),
            "raw 要能看出 installed_before -> installed_after：{raw}"
        );
        assert_eq!(&entry, &page.entries[0]);
    }

    /// 校验后取消（裁定 d / NFR-2）：守卫删除已验证的安装包并返回 download.cancelled，
    /// 安装 helper 与包管理器都不会被触达——这正是 `start_install` 校验后的第一道闸门。
    #[tokio::test]
    async fn cancelling_during_verify_refuses_the_install_without_touching_the_package_manager() {
        let (_paths, oplog, tarball) = install_fixture("ipc-install-cancel-after-verify", "0.2.0");
        let token = CancelToken::new_for_test();
        token.cancel();
        let pm = RecordingPm::new(
            vec![Some("0.2.0-rc.2")],
            Ok(InstallOutcome {
                exit_code: 0,
                output: "added 1 package".to_string(),
            }),
        );
        let lister = FakeLister::empty();

        // 与 start_install 相同的顺序：守卫不通过时绝不执行下面的安装步骤
        let e = match reject_if_cancelled_after_verify(&token, &tarball) {
            Ok(()) => install_after_download(
                &tarball,
                "latest/0.2.0",
                &[pm_info()],
                &pm,
                &lister,
                &oplog,
                policy(),
            )
            .await
            .0
            .expect_err("取消后绝不允许执行安装"),
            Err(e) => e,
        };

        assert_eq!(e.code, crate::error::DOWNLOAD_CANCELLED);
        assert!(e.retryable, "取消后允许重新点击安装");
        assert!(!tarball.exists(), "取消后必须删除已验证的安装包（NFR-2）");
        assert!(
            pm.calls().is_empty(),
            "取消后包管理器一个方法都不该被调用：{:?}",
            pm.calls()
        );
    }

    #[test]
    fn not_cancelled_after_verify_keeps_the_verified_tarball_for_install() {
        let (_paths, _oplog, tarball) = install_fixture("ipc-install-not-cancelled", "0.2.0");
        let token = CancelToken::new_for_test();

        reject_if_cancelled_after_verify(&token, &tarball).expect("未取消时必须放行");

        assert!(tarball.exists(), "未取消时安装包必须留给安装步骤");
    }

    #[test]
    fn event_names_match_frozen_contract() {
        let names = [
            EVENT_TASK_PROGRESS,
            EVENT_INSTANCE_STATE,
            EVENT_INSTANCE_OUTPUT,
            EVENT_INSTANCE_URL,
            EVENT_UPDATE_AVAILABLE,
            EVENT_OPLOG_ENTRY,
        ];
        assert_eq!(
            names,
            [
                "task:progress",
                "instance:state",
                "instance:output",
                "instance:url",
                "update:available",
                "oplog:entry",
            ]
        );
    }

    #[test]
    fn set_auto_channel_is_recorded_in_oplog() {
        use crate::m0::paths::{DataPaths, temp_root};

        let paths = DataPaths::under_root(&temp_root("ipc-auto-channel-oplog"));
        paths.ensure_dirs().unwrap();
        let store = SettingsStore::load_or_init(paths.settings_file()).unwrap();
        let oplog = Oplog::new(paths.oplog_file());

        let (first, first_entry) =
            apply_auto_channel_settings(&store, &oplog, Some("latest".to_owned()), true).unwrap();
        assert_eq!(first_entry.action, "settings.update");
        assert_eq!(first.auto_channel.as_deref(), Some("latest"));

        let (second, second_entry) =
            apply_auto_channel_settings(&store, &oplog, Some("next".to_owned()), true).unwrap();
        assert_eq!(second.auto_channel.as_deref(), Some("next"));
        let raw = second_entry.raw.as_deref().expect("应含前后值");
        assert!(raw.contains("\"latest\""), "旧通道应出现在记录中：{raw}");
        assert!(raw.contains("\"next\""), "新通道应出现在记录中：{raw}");
        assert!(raw.contains("→"), "应展示旧值 → 新值：{raw}");

        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 2, "每次通道切换记一条");
    }
}
