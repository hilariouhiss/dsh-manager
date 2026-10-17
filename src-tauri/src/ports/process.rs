use std::path::PathBuf;

use serde::Serialize;

use crate::error::CmdResult;

/// 一条进程快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    /// 已按空白切分的完整命令行
    pub cmdline: Vec<String>,
    /// Unix 纪元秒；无法读取时为 None（受保护进程）
    pub started_at_epoch_secs: Option<u64>,
    pub exe: Option<PathBuf>,
}

/// 进程枚举端口。**只读**：本 trait 没有任何写方法（§3.3 契约约束）。
pub trait ProcessProbe: Send + Sync {
    /// 全量快照。单项读取失败跳过该项，不整体失败（FR-2.5 精神）。
    fn snapshot(&self) -> CmdResult<Vec<ProcInfo>>;
    /// 复核某进程是否仍存活（停止后复核用，FR-3.4、E2E-7）。
    fn is_alive(&self, pid: u32) -> bool;
}
