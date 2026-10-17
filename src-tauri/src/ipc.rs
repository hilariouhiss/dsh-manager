//! 命令与事件的汇聚点（冻结契约见主计划 §3.4、§3.5）。

use tauri::State;

use crate::AppState;
use crate::error::CmdResult;
use crate::m0::settings::Settings;

/// 事件名常量（主计划 §3.5）。前端由 src/ipc/types.ts 的 EVENTS 镜像同名同值。
pub const EVENT_TASK_PROGRESS: &str = "task:progress";
pub const EVENT_INSTANCE_STATE: &str = "instance:state";
pub const EVENT_INSTANCE_OUTPUT: &str = "instance:output";
pub const EVENT_INSTANCE_URL: &str = "instance:url";
pub const EVENT_UPDATE_AVAILABLE: &str = "update:available";
pub const EVENT_OPLOG_ENTRY: &str = "oplog:entry";

/// 取当前生效设置（主计划 §3.4 命令 11）。
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    Ok(state.settings.snapshot())
}

/// 应用部分补丁并即时持久化（主计划 §3.4 命令 12、§3.7）。
#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    patch: serde_json::Value,
) -> CmdResult<Settings> {
    let (next, problems) = state.settings.apply_patch(&patch)?;
    if problems.is_empty() {
        log::info!("设置已更新");
    } else {
        log::warn!("设置补丁存在被回落或拒绝的字段：{}", problems.join("；"));
    }
    Ok(next)
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
