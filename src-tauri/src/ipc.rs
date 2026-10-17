//! 命令与事件的汇聚点（冻结契约见主计划 §3.4、§3.5）。

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;
use crate::error::{AppError, CmdResult};
use crate::m0::diagnostics::{self, DiagnosticsInput};
use crate::m0::logging;
use crate::m0::oplog::{Oplog, OplogEntry, OplogPage};
use crate::m0::settings::{self, Settings, SettingsStore};
use crate::m1::channels::{ChannelsCache, by_id};
use crate::m1::version_check::{
    AutoChannelSettings, CheckUpdatesReport, SharedOriginFacts, apply_auto_channel,
    auto_install_notice, check_updates as run_check_updates,
};

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

#[cfg(test)]
mod tests {
    use super::*;

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
