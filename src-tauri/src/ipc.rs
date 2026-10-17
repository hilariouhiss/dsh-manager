//! 命令与事件的汇聚点（冻结契约见主计划 §3.4、§3.5）。

use crate::error::{self, AppError, CmdResult};

/// 事件名常量（主计划 §3.5）。前端由 src/ipc/types.ts 的 EVENTS 镜像同名同值。
pub const EVENT_TASK_PROGRESS: &str = "task:progress";
pub const EVENT_INSTANCE_STATE: &str = "instance:state";
pub const EVENT_INSTANCE_OUTPUT: &str = "instance:output";
pub const EVENT_INSTANCE_URL: &str = "instance:url";
pub const EVENT_UPDATE_AVAILABLE: &str = "update:available";
pub const EVENT_OPLOG_ENTRY: &str = "oplog:entry";

/// 前后端联通验证用的临时命令。设置功能落地时连同 App.vue 的临时面板一并删除。
#[tauri::command]
pub fn ping(fail: Option<bool>) -> CmdResult<String> {
    if fail.unwrap_or(false) {
        return Err(AppError::new(
            error::NETWORK_OFFLINE,
            "这是一次用于验证错误呈现的故意失败",
            "无需处理：它证明失败路径已接通，重试入口应当可用。",
        )
        .with_raw("deliberate failure from the ping command")
        .retryable(true));
    }
    Ok("pong".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_pong() {
        assert_eq!(ping(None).unwrap(), "pong");
        assert_eq!(ping(Some(false)).unwrap(), "pong");
    }

    #[test]
    fn ping_fail_returns_structured_error() {
        let err = ping(Some(true)).unwrap_err();
        assert_eq!(err.code, error::NETWORK_OFFLINE);
        assert!(err.retryable);
        assert_eq!(
            err.raw.as_deref(),
            Some("deliberate failure from the ping command")
        );
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
}
