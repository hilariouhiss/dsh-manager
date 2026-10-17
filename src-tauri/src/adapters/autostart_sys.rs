//! AutostartPort 的真实实现：把 tauri-plugin-autostart 隔在端口后面。
//!
//! 本文件是 M4 唯一接触外部世界的位置；应用层只认 m4::autostart::AutostartPort。

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::error::{AUTOSTART_DENIED, AUTOSTART_READ_FAILED, CmdResult};
use crate::m4::autostart::{AutostartPort, LOGIN_ENTRY_NAME};

/// Windows 上登录项落在 HKCU 的 Run 键下，值名是 LOGIN_ENTRY_NAME。
/// 依据：auto-launch 的 Windows 实现（https://docs.rs/auto-launch/0.5.0/src/auto_launch/windows.rs.html 的 AL_REGKEY）。
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// 写入登录项被系统拒绝（§3.2 autostart.denied｜常量 AUTOSTART_DENIED）。
fn denied(raw: impl std::fmt::Display) -> crate::error::AppError {
    crate::error::AppError::new(
        AUTOSTART_DENIED,
        "写入开机自启登录项被系统拒绝。",
        "请确认当前账户可写入 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run，或先用「任务管理器 → 启动」确认本应用未被策略禁用，然后重试。",
    )
    .with_raw(raw.to_string())
    .retryable(false)
}

/// 移除登录项被系统拒绝（同一个错误码 AUTOSTART_DENIED，文案指向「手动关掉」这条退路）。
fn remove_denied(raw: impl std::fmt::Display) -> crate::error::AppError {
    crate::error::AppError::new(
        AUTOSTART_DENIED,
        "移除开机自启登录项被系统拒绝。",
        "请用同一个账户重试；若仍失败，打开「任务管理器 → 启动」手动禁用本应用，再回到本页点「刷新」。",
    )
    .with_raw(raw.to_string())
    .retryable(false)
}

/// 读取登录项状态失败（§3.2 autostart.read_failed｜常量 AUTOSTART_READ_FAILED）。
///
/// 它与 AUTOSTART_DENIED 是**两个码**，理由有两条：
/// 1. 可重试性不同——读失败可重试，写入/移除被拒绝不可重试；
/// 2. 语义完全不同——「读不出来」不是一种状态，FR-5.3 要求以系统真实状态为准，
///    读失败**不得**降级为「已关闭」。
fn read_failed(raw: impl std::fmt::Display) -> crate::error::AppError {
    crate::error::AppError::new(
        AUTOSTART_READ_FAILED,
        "读取开机自启登录项状态失败。",
        "请确认当前账户可读取 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run，然后点「刷新」重试。",
    )
    .with_raw(raw.to_string())
    .retryable(true)
}

pub struct SysAutostart {
    app: AppHandle,
}

impl SysAutostart {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl AutostartPort for SysAutostart {
    fn is_enabled(&self) -> CmdResult<bool> {
        self.app.autolaunch().is_enabled().map_err(read_failed)
    }

    fn enable(&self) -> CmdResult<()> {
        self.app.autolaunch().enable().map_err(denied)
    }

    fn disable(&self) -> CmdResult<()> {
        self.app.autolaunch().disable().map_err(remove_denied)
    }

    fn entry_path(&self) -> Option<String> {
        Some(format!(r"{RUN_KEY}\{LOGIN_ENTRY_NAME}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_maps_to_autostart_denied_with_reason_and_next_step() {
        let err = denied("Access is denied. (os error 5)");
        assert_eq!(err.code, "autostart.denied", "错误码必须与主计划 §3.2 一致");
        assert!(
            err.reason.contains("登录项"),
            "reason 必须说明发生了什么，当前为：{}",
            err.reason
        );
        assert!(
            err.next_step.contains("任务管理器"),
            "next_step 必须给出可执行的下一步，当前为：{}",
            err.next_step
        );
        assert_eq!(
            err.raw.as_deref(),
            Some("Access is denied. (os error 5)"),
            "底层原文必须原样保留，不得吞错或改写"
        );
        assert!(!err.retryable, "autostart.denied 不可重试（§3.2）");
    }

    #[test]
    fn read_failure_has_its_own_code_and_is_retryable() {
        let err = read_failed("Access is denied. (os error 5)");
        assert_eq!(
            err.code, "autostart.read_failed",
            "读取失败用独立错误码（§3.2）"
        );
        assert!(err.retryable, "读取失败可重试：这正是两个码要分开的理由");
        assert!(
            err.reason.contains("读取"),
            "reason 必须说清是读取哪一项失败，当前为：{}",
            err.reason
        );
        assert!(
            err.next_step.contains("刷新"),
            "next_step 要指向界面上的动作"
        );
        assert_eq!(err.raw.as_deref(), Some("Access is denied. (os error 5)"));
        // 与写/移除被拒绝形成对照
        assert!(!denied("Access is denied. (os error 5)").retryable);
        assert!(!remove_denied("Access is denied. (os error 5)").retryable);
    }
}
