//! 统一错误结构（冻结契约见主计划 §3.1、§3.2）。
//!
//! 序列化一律 camelCase（主计划 §3.4），因此 Rust 侧的 next_step 在 IPC 上是 nextStep。
//! 同时派生 Deserialize：§3.6 的 ChannelStatus.lastError 随通道缓存落盘后必须能读回（§7.4 修订 1）。

use serde::{Deserialize, Serialize};

/// 错误码常量表（主计划 §3.2）。前端 src/ipc/types.ts 的 ERROR_CODES 镜像同名同值，
/// 两侧一致性由 src/ipc/error-codes.test.ts 断言；新增错误码必须同时改两处。
pub const SETTINGS_CORRUPT: &str = "settings.corrupt";
pub const PATH_PERMISSION_DENIED: &str = "path.permission_denied";
pub const NETWORK_OFFLINE: &str = "network.offline";
pub const NETWORK_PROXY_AUTH: &str = "network.proxy_auth";
pub const NETWORK_TLS: &str = "network.tls";
pub const REGISTRY_PARSE: &str = "registry.parse";
pub const REGISTRY_HTTP: &str = "registry.http";
pub const DOWNLOAD_INSUFFICIENT_SPACE: &str = "download.insufficient_space";
pub const DOWNLOAD_INTEGRITY_MISMATCH: &str = "download.integrity_mismatch";
pub const DOWNLOAD_NO_INTEGRITY: &str = "download.no_integrity";
pub const DOWNLOAD_CANCELLED: &str = "download.cancelled";
pub const INSTALL_PM_MISSING: &str = "install.pm_missing";
pub const INSTALL_INSTANCES_RUNNING: &str = "install.instances_running";
pub const INSTALL_FAILED: &str = "install.failed";
pub const INSTANCE_ALREADY_RUNNING: &str = "instance.already_running";
pub const INSTANCE_SPAWN_FAILED: &str = "instance.spawn_failed";
pub const INSTANCE_PORT_IN_USE: &str = "instance.port_in_use";
pub const INSTANCE_EXITED_IMMEDIATELY: &str = "instance.exited_immediately";
pub const INSTANCE_STOP_TIMEOUT: &str = "instance.stop_timeout";
pub const INSTANCE_FORCED_STOP_REQUIRED: &str = "instance.forced_stop_required";
pub const INSTANCE_NOT_MANAGED: &str = "instance.not_managed";
pub const AUTOSTART_DENIED: &str = "autostart.denied";
pub const AUTOSTART_READ_FAILED: &str = "autostart.read_failed";
pub const TASK_NOT_FOUND: &str = "task.not_found";

/// 统一错误结构（主计划 §3.1）：原因 + 下一步 + 原始错误 + 是否可重试。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    /// 稳定的机器可读标识，见 §3.2 错误码表
    pub code: String,
    /// 面向用户的中文说明：发生了什么
    pub reason: String,
    /// 面向用户的中文建议：可以做什么
    pub next_step: String,
    /// 底层返回的原文，供复制反馈；不吞错、不改写
    pub raw: Option<String>,
    /// 决定界面是否给出重试入口
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: &str, reason: impl Into<String>, next_step: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            reason: reason.into(),
            next_step: next_step.into(),
            raw: None,
            retryable: false,
        }
    }

    pub fn with_raw(mut self, raw: impl Into<String>) -> Self {
        self.raw = Some(raw.into());
        self
    }

    pub fn retryable(mut self, yes: bool) -> Self {
        self.retryable = yes;
        self
    }
}

/// 命令返回值的统一外壳（主计划 §3.1）。
pub type CmdResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_five_camel_case_fields() {
        let err = AppError::new(NETWORK_OFFLINE, "无法连接到 npm 仓库", "检查网络后重试")
            .with_raw("dns error: no such host")
            .retryable(true);
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(
            value,
            json!({
                "code": "network.offline",
                "reason": "无法连接到 npm 仓库",
                "nextStep": "检查网络后重试",
                "raw": "dns error: no such host",
                "retryable": true,
            })
        );
    }

    #[test]
    fn builder_defaults_are_raw_none_and_not_retryable() {
        let err = AppError::new(TASK_NOT_FOUND, "找不到该任务", "刷新页面后重新发起操作");
        assert_eq!(err.raw, None);
        assert!(!err.retryable);
        assert_eq!(err.code, "task.not_found");
    }

    #[test]
    fn deserializes_the_camel_case_payload_written_to_the_channel_cache() {
        let payload = r#"{"code":"network.offline","reason":"网络请求未能完成","nextStep":"检查网络后重试","raw":null,"retryable":true}"#;
        let err: AppError = serde_json::from_str(payload).unwrap();
        assert_eq!(err.code, NETWORK_OFFLINE);
        assert_eq!(err.next_step, "检查网络后重试");
        assert_eq!(err.raw, None);
        assert!(err.retryable);
    }

    #[test]
    fn error_code_table_is_frozen() {
        let all = [
            SETTINGS_CORRUPT,
            PATH_PERMISSION_DENIED,
            NETWORK_OFFLINE,
            NETWORK_PROXY_AUTH,
            NETWORK_TLS,
            REGISTRY_PARSE,
            REGISTRY_HTTP,
            DOWNLOAD_INSUFFICIENT_SPACE,
            DOWNLOAD_INTEGRITY_MISMATCH,
            DOWNLOAD_NO_INTEGRITY,
            DOWNLOAD_CANCELLED,
            INSTALL_PM_MISSING,
            INSTALL_INSTANCES_RUNNING,
            INSTALL_FAILED,
            INSTANCE_ALREADY_RUNNING,
            INSTANCE_SPAWN_FAILED,
            INSTANCE_PORT_IN_USE,
            INSTANCE_EXITED_IMMEDIATELY,
            INSTANCE_STOP_TIMEOUT,
            INSTANCE_FORCED_STOP_REQUIRED,
            INSTANCE_NOT_MANAGED,
            AUTOSTART_DENIED,
            AUTOSTART_READ_FAILED,
            TASK_NOT_FOUND,
        ];
        assert_eq!(all.len(), 24, "主计划 §3.2 共 24 个错误码");
        for code in all {
            assert!(code.contains('.'), "{code} 缺少「域.动作」形式的前缀");
        }
        // §3.2 明确：三种「非错误状态」不得出现在错误码里
        for forbidden in ["unpublished", "unidentified", "not_running"] {
            assert!(!all.contains(&forbidden), "{forbidden} 不是错误码");
        }
    }
}
