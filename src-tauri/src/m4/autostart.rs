//! M4 开机自启：登录项读写、启动行为参数、与系统状态一致性校验。
//!
//! 本模块只依赖 AutostartPort 这一个外部边界：真实实现是
//! adapters/autostart_sys.rs 的 SysAutostart（tauri-plugin-autostart），
//! 测试实现是本文件底部 #[cfg(test)] mod tests 的 FakeAutostart。

/// 登录项在系统中的名称。
///
/// Windows 上它是 HKCU\Software\Microsoft\Windows\CurrentVersion\Run 下的值名：
/// tauri-plugin-autostart 用它注册，adapters/autostart_sys.rs 用它拼出登录项标识。
pub const LOGIN_ENTRY_NAME: &str = "dsh-manager";

/// 写进登录项的启动参数（主计划 §5 T-A1「带启动参数」）。
///
/// 它标记「本次启动来自登录项」；具体启动行为由 settings.autostartMode 决定，
/// 原因见本子计划 §0.2（插件的启动参数在注册时冻结，进程内换不掉）。
pub const LOGIN_ENTRY_ARG: &str = "--autostart";

use serde::Serialize;

use crate::error::CmdResult;

/// `AutostartMode` 由 **M0** 定义（主计划 §3.7 冻结：`m0/settings.rs`，JSON 取值 "tray" | "window"），
/// 因为它是 `Settings.autostart_mode` 的字段类型而 `Settings` 属 M0——依赖箭头只能 M4 → M0。
/// 这里只做转出，让 M4 的调用方可以 `use crate::m4::autostart::AutostartMode;`。
/// 注意：§3.7 的枚举**没有** `Default` 实现，需要默认值时显式写 `AutostartMode::Tray`。
pub use crate::m0::settings::AutostartMode;

/// 命令 13/14 的出参（主计划 §3.4）。enabled 一律来自系统，不是本地配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    pub enabled: bool,
    pub mode: AutostartMode,
    pub entry_path: Option<String>,
}

/// 登录项边界。每个方法都必须能被假实现替换（§3.3 契约约束的同一条精神）。
pub trait AutostartPort: Send + Sync {
    /// 系统真实状态（FR-5.3、原则 P5）。读取失败返回 autostart.read_failed（可重试），
    /// **不得降级为「已关闭」**：读不出来不等于没开启。
    fn is_enabled(&self) -> CmdResult<bool>;
    /// 写入登录项；被系统拒绝时返回 autostart.denied（不可重试）。
    fn enable(&self) -> CmdResult<()>;
    /// 移除登录项（FR-5.4）；重复调用必须成功，不能因为「本来就不存在」而报错。
    fn disable(&self) -> CmdResult<()>;
    /// 登录项的位置标识，仅用于界面展示；拿不到时为 None。
    fn entry_path(&self) -> Option<String>;
}

/// 读取系统真实状态。**不读本地配置、不做缓存**：每调用一次就问一次系统。
pub fn get_state(port: &dyn AutostartPort, configured: AutostartMode) -> CmdResult<AutostartState> {
    let enabled = port.is_enabled()?;
    Ok(AutostartState {
        enabled,
        mode: configured,
        entry_path: if enabled { port.entry_path() } else { None },
    })
}

/// 按请求写入系统，然后**回读系统真实状态**并如实返回。
///
/// 写失败时原样上抛端口返回的错误：不吞错、不记为成功、不降级为默认值。
pub fn set_state(
    port: &dyn AutostartPort,
    enabled: bool,
    configured: AutostartMode,
) -> CmdResult<AutostartState> {
    if enabled {
        port.enable()?;
    } else {
        port.disable()?;
    }
    // enabled 以回读的系统状态为准：系统没接受这次写入时，界面必须显示真相。
    let actual = port.is_enabled()?;
    Ok(AutostartState {
        enabled: actual,
        mode: configured,
        entry_path: if actual { port.entry_path() } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 假适配器：把 tauri-plugin-autostart 隔在端口后面。
    /// 它自己维护一个「系统状态」，可以模拟用户在系统侧改状态、系统忽略写入、写入被拒绝。
    struct FakeAutostart {
        system_enabled: Mutex<bool>,
        enable_calls: Mutex<u32>,
        disable_calls: Mutex<u32>,
        is_enabled_calls: Mutex<u32>,
        writes_ignored: Mutex<bool>,
        fail_enable_with: Mutex<Option<crate::error::AppError>>,
        fail_disable_with: Mutex<Option<crate::error::AppError>>,
        fail_read_with: Mutex<Option<crate::error::AppError>>,
    }

    impl Default for FakeAutostart {
        fn default() -> Self {
            Self {
                system_enabled: Mutex::new(false),
                enable_calls: Mutex::new(0),
                disable_calls: Mutex::new(0),
                is_enabled_calls: Mutex::new(0),
                writes_ignored: Mutex::new(false),
                fail_enable_with: Mutex::new(None),
                fail_disable_with: Mutex::new(None),
                fail_read_with: Mutex::new(None),
            }
        }
    }

    impl FakeAutostart {
        fn entry_path_value(&self) -> String {
            format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\{LOGIN_ENTRY_NAME}")
        }

        fn enable_calls(&self) -> u32 {
            *self.enable_calls.lock().unwrap()
        }

        fn disable_calls(&self) -> u32 {
            *self.disable_calls.lock().unwrap()
        }

        fn is_enabled_calls(&self) -> u32 {
            *self.is_enabled_calls.lock().unwrap()
        }

        /// 模拟「用户在系统设置里改了状态」：直接改假系统的状态，不经过本模块。
        fn change_system_state(&self, enabled: bool) {
            *self.system_enabled.lock().unwrap() = enabled;
        }

        /// 模拟「系统静默忽略写入」：enable/disable 不报错，但不改变系统状态。
        fn ignore_writes(&self) {
            *self.writes_ignored.lock().unwrap() = true;
        }

        fn fail_enable_with(&self, err: crate::error::AppError) {
            *self.fail_enable_with.lock().unwrap() = Some(err);
        }

        fn fail_disable_with(&self, err: crate::error::AppError) {
            *self.fail_disable_with.lock().unwrap() = Some(err);
        }

        fn fail_read_with(&self, err: crate::error::AppError) {
            *self.fail_read_with.lock().unwrap() = Some(err);
        }
    }

    impl AutostartPort for FakeAutostart {
        fn is_enabled(&self) -> CmdResult<bool> {
            *self.is_enabled_calls.lock().unwrap() += 1;
            if let Some(err) = self.fail_read_with.lock().unwrap().clone() {
                return Err(err);
            }
            Ok(*self.system_enabled.lock().unwrap())
        }

        fn enable(&self) -> CmdResult<()> {
            *self.enable_calls.lock().unwrap() += 1;
            if let Some(err) = self.fail_enable_with.lock().unwrap().clone() {
                return Err(err);
            }
            if !*self.writes_ignored.lock().unwrap() {
                *self.system_enabled.lock().unwrap() = true;
            }
            Ok(())
        }

        fn disable(&self) -> CmdResult<()> {
            *self.disable_calls.lock().unwrap() += 1;
            if let Some(err) = self.fail_disable_with.lock().unwrap().clone() {
                return Err(err);
            }
            if !*self.writes_ignored.lock().unwrap() {
                *self.system_enabled.lock().unwrap() = false;
            }
            Ok(())
        }

        fn entry_path(&self) -> Option<String> {
            Some(self.entry_path_value())
        }
    }

    /// 路径①：开启后系统状态为真。
    #[test]
    fn path1_enable_makes_system_report_enabled() {
        let fake = FakeAutostart::default();
        let state = set_state(&fake, true, AutostartMode::Tray).expect("写入登录项不应失败");
        assert!(state.enabled, "开启后返回的状态必须是已开启");
        assert!(fake.is_enabled().unwrap(), "假系统的真实状态必须是已开启");
        assert_eq!(fake.enable_calls(), 1, "必须恰好调用一次端口 enable");
        assert_eq!(
            state.mode,
            AutostartMode::Tray,
            "返回的启动行为必须是调用方给的值"
        );
        assert_eq!(
            state.entry_path,
            Some(fake.entry_path_value()),
            "已开启时必须给出登录项标识"
        );
    }

    /// 路径①的加强版：系统没真的接受写入时，绝不能返回「已开启」。
    #[test]
    fn path1_write_ignored_by_system_still_reports_disabled() {
        let fake = FakeAutostart::default();
        fake.ignore_writes();
        let state = set_state(&fake, true, AutostartMode::Tray).expect("端口没报错就不该返回 Err");
        assert!(
            !state.enabled,
            "系统没有真的开启时，绝不能返回已开启（禁止把失败记为成功）"
        );
        assert_eq!(state.entry_path, None, "系统未开启时不得给出登录项标识");
    }

    /// 路径②：关闭后登录项被移除。
    #[test]
    fn path2_disable_removes_login_entry() {
        let fake = FakeAutostart::default();
        fake.change_system_state(true); // 前置：系统里已经有登录项
        let state = set_state(&fake, false, AutostartMode::Window).expect("移除登录项不应失败");
        assert!(!state.enabled, "关闭后返回的状态必须是未开启");
        assert!(!fake.is_enabled().unwrap(), "假系统的真实状态必须是未开启");
        assert_eq!(fake.disable_calls(), 1, "必须恰好调用一次端口 disable");
        assert_eq!(fake.enable_calls(), 0, "关闭路径不得调用 enable");
        assert_eq!(state.entry_path, None, "关闭后不得残留登录项标识（FR-5.4）");
    }

    /// 路径②的加强版：本来就未开启时，关闭也必须成功且不报错。
    #[test]
    fn path2_disable_is_idempotent() {
        let fake = FakeAutostart::default();
        assert!(
            set_state(&fake, false, AutostartMode::Tray).is_ok(),
            "本来就未开启时，关闭也必须成功"
        );
        assert!(
            set_state(&fake, false, AutostartMode::Tray).is_ok(),
            "重复关闭必须成功"
        );
        assert_eq!(fake.disable_calls(), 2, "两次调用都必须真的走到端口");
    }

    /// 路径③：系统侧状态改变后，读取必须反映系统真实状态，而不是本地缓存。
    #[test]
    fn path3_reads_system_state_every_time() {
        let fake = FakeAutostart::default();
        assert!(
            !get_state(&fake, AutostartMode::Tray).unwrap().enabled,
            "初始必须是未开启"
        );
        // 模拟用户在系统设置里手动开启（不经过本模块）
        fake.change_system_state(true);
        let after = get_state(&fake, AutostartMode::Tray).expect("读取系统状态不应失败");
        assert!(
            after.enabled,
            "系统侧改动后，读取必须反映系统真实状态（FR-5.3）"
        );
        assert_eq!(
            fake.is_enabled_calls(),
            2,
            "每次读取都必须问一次系统，不得使用本地缓存"
        );
    }

    /// 路径③的另一半：用户在「任务管理器 → 启动」里禁用本应用后，界面必须跟上。
    #[test]
    fn path3_external_removal_is_reflected() {
        let fake = FakeAutostart::default();
        fake.change_system_state(true);
        assert!(get_state(&fake, AutostartMode::Window).unwrap().enabled);
        fake.change_system_state(false);
        let after = get_state(&fake, AutostartMode::Window).unwrap();
        assert!(
            !after.enabled,
            "系统侧删除/禁用后必须显示为关闭（E2E-6 前半）"
        );
        assert_eq!(after.entry_path, None, "未开启时不得给出登录项标识");
        assert_eq!(
            after.mode,
            AutostartMode::Window,
            "启动行为是本地设置，不随系统状态变化"
        );
    }

    /// 读取失败路径：get_state 必须报错，而不是返回「已关闭」。
    #[test]
    fn read_failure_is_an_error_not_a_state() {
        let fake = FakeAutostart::default();
        fake.change_system_state(true); // 系统里其实是有登录项的
        fake.fail_read_with(
            crate::error::AppError::new(
                crate::error::AUTOSTART_READ_FAILED,
                "读取开机自启登录项失败。",
                "请确认当前账户可读取 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run，然后点「刷新」重试。",
            )
            .with_raw("Access is denied. (os error 5)")
            .retryable(true),
        );
        let err = get_state(&fake, AutostartMode::Tray)
            .expect_err("读取失败必须返回 Err，不得降级为一种状态");
        assert_eq!(
            err.code, "autostart.read_failed",
            "读取失败用独立的错误码（§3.2）"
        );
        assert!(
            err.retryable,
            "读取失败可重试——这正是它与 autostart.denied 分成两个码的理由"
        );
        assert_eq!(err.raw.as_deref(), Some("Access is denied. (os error 5)"));
    }

    /// 写入成功后回读失败：也必须是 read_failed，绝不能报「已开启」。
    #[test]
    fn readback_failure_after_enable_is_an_error() {
        let fake = FakeAutostart::default();
        fake.fail_read_with(
            crate::error::AppError::new(
                crate::error::AUTOSTART_READ_FAILED,
                "读取开机自启登录项失败。",
                "请稍后点「刷新」重试。",
            )
            .retryable(true),
        );
        let err = set_state(&fake, true, AutostartMode::Tray).expect_err("回读失败必须返回 Err");
        assert_eq!(err.code, "autostart.read_failed");
        assert!(err.retryable, "回读失败可重试");
        assert_eq!(
            fake.enable_calls(),
            1,
            "写入本身已经发生，界面要如实说明「写成功但读不回来」"
        );
    }

    /// 路径④：写入被拒绝时，端口错误必须原样上抛，不得被吞掉或降级。
    #[test]
    fn path4_enable_failure_is_propagated_unchanged() {
        let fake = FakeAutostart::default();
        fake.fail_enable_with(
            crate::error::AppError::new(
                "autostart.denied",
                "写入开机自启登录项被系统拒绝。",
                "请检查是否被系统策略禁用，然后重试。",
            )
            .with_raw("Access is denied. (os error 5)")
            .retryable(false),
        );
        let err = set_state(&fake, true, AutostartMode::Tray).expect_err("写入失败时必须返回 Err");
        assert_eq!(err.code, "autostart.denied");
        assert_eq!(
            err.raw.as_deref(),
            Some("Access is denied. (os error 5)"),
            "原始错误必须原样保留"
        );
        assert!(!err.retryable, "autostart.denied 不可重试（§3.2）");
        assert!(
            !fake.is_enabled().unwrap(),
            "写失败后系统状态仍是未开启，界面不得显示为已开启"
        );
    }

    /// 路径④的另一半：移除被拒绝时同样原样上抛。
    #[test]
    fn path4_disable_failure_is_propagated_unchanged() {
        let fake = FakeAutostart::default();
        fake.fail_disable_with(crate::error::AppError::new(
            "autostart.denied",
            "移除开机自启登录项被系统拒绝。",
            "请用同一个账户重试。",
        ));
        let err = set_state(&fake, false, AutostartMode::Tray).expect_err("移除失败时必须返回 Err");
        assert_eq!(err.code, "autostart.denied");
    }
}
