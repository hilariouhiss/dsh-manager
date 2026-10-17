//! 假进程表。记录进程枚举的调用次数，供「停止后必须复核」这类断言使用。

use std::sync::Mutex;

use crate::error::{AppError, CmdResult, PATH_PERMISSION_DENIED};
use crate::ports::process::{ProcInfo, ProcessProbe};

#[derive(Default)]
pub struct FakeProbe {
    pub table: Vec<ProcInfo>,
    /// Some 时 is_alive 直接查这个清单；None 时按 table 判断。
    pub alive: Option<Vec<u32>>,
    /// 为真时 snapshot 返回错误，用于「复核无法完成」用例。
    pub fail_snapshot: bool,
    snapshot_calls: Mutex<usize>,
}

impl FakeProbe {
    pub fn new(table: Vec<ProcInfo>) -> Self {
        Self {
            table,
            ..Default::default()
        }
    }

    pub fn with_alive(mut self, pids: Vec<u32>) -> Self {
        self.alive = Some(pids);
        self
    }

    pub fn failing_snapshot(mut self) -> Self {
        self.fail_snapshot = true;
        self
    }

    pub fn snapshot_calls(&self) -> usize {
        *self.snapshot_calls.lock().unwrap()
    }

    /// 模拟「停止之后进程消失」。
    pub fn remove_pid(&mut self, pid: u32) {
        self.table.retain(|p| p.pid != pid);
    }
}

impl ProcessProbe for FakeProbe {
    fn snapshot(&self) -> CmdResult<Vec<ProcInfo>> {
        *self.snapshot_calls.lock().unwrap() += 1;
        if self.fail_snapshot {
            return Err(AppError::new(
                PATH_PERMISSION_DENIED,
                "假实现注入：进程枚举失败",
                "这是一条测试用错误，不需要处理。",
            ));
        }
        Ok(self.table.clone())
    }

    fn is_alive(&self, pid: u32) -> bool {
        match &self.alive {
            Some(list) => list.contains(&pid),
            None => self.table.iter().any(|p| p.pid == pid),
        }
    }
}
