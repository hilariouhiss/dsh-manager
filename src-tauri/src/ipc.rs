//! 命令与事件的汇聚点（冻结契约见主计划 §3.4、§3.5）。

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;
use crate::error::CmdResult;
use crate::m0::diagnostics::{self, DiagnosticsInput};
use crate::m0::logging;
use crate::m0::oplog::{OplogEntry, OplogPage};
use crate::m0::settings::{self, Settings};
use crate::m1::channels::ChannelsCache;
use crate::m1::version_check::{
    CheckUpdatesReport, SharedOriginFacts, check_updates as run_check_updates,
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

/// §3.4 命令 1：检查更新。入参 `{ channels: string[] }`（主计划 §3.4 冻结）。
///
/// 本机版本与来源结论都从 T-O2 写入的共享状态里取；M1 不自己探测包管理器，也不探测来源。
#[tauri::command]
pub async fn check_updates(
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
    run_check_updates(
        state.registry.as_ref(),
        &facts,
        &channels,
        &previous,
        &cache_file,
        std::time::Duration::from_secs(8),
    )
    .await
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
}
