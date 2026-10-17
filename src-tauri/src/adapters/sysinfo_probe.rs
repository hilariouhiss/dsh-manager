//! 基于 sysinfo 的进程表。只读：本类型没有任何终止进程的方法（§3.3 契约约束）。

use std::sync::Mutex;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::error::CmdResult;
use crate::ports::process::{ProcInfo, ProcessProbe};

pub struct SysinfoProbe {
    sys: Mutex<System>,
}

impl Default for SysinfoProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoProbe {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new()),
        }
    }
}

impl ProcessProbe for SysinfoProbe {
    fn snapshot(&self) -> CmdResult<Vec<ProcInfo>> {
        let mut sys = self.sys.lock().expect("进程表互斥量被毒化");
        // 便捷方法 refresh_processes 的刷新集不包含命令行（cmd 的 UpdateKind 默认是 Never），
        // 只刷新 cmd 与 exe 两项：既拿到匹配所需的命令行，又不扩大 Windows API 面积（NFR-5）。
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        Ok(sys
            .processes()
            .values()
            .map(|p| ProcInfo {
                pid: p.pid().as_u32(),
                parent_pid: p.parent().map(|q| q.as_u32()),
                cmdline: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
                started_at_epoch_secs: match p.start_time() {
                    0 => None,
                    secs => Some(secs),
                },
                exe: p.exe().map(|e| e.to_path_buf()),
            })
            .collect())
    }

    fn is_alive(&self, pid: u32) -> bool {
        let mut sys = self.sys.lock().expect("进程表互斥量被毒化");
        let pid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        sys.process(pid).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_alive_reports_this_process_as_alive() {
        let probe = SysinfoProbe::new();
        assert!(probe.is_alive(std::process::id()), "当前进程必须被判为存活");
    }

    #[test]
    fn is_alive_reports_unknown_pid_as_dead() {
        let probe = SysinfoProbe::new();
        assert!(!probe.is_alive(u32::MAX), "不存在的进程必须为 false");
    }
}
