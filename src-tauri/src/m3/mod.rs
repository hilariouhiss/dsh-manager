pub mod instance_discovery;
pub mod instance_runner;
pub mod output_buffer;
pub mod url_extract;

use serde::Serialize;

/// 实例状态。取值与 §3.5 的 instance:state 载荷完全一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

/// 当前 Unix 纪元毫秒。后端**不解析**、不格式化时间，只做算术（§3.6 时间表示裁定）。
/// 时钟唯一来源是 M0 的 now_ms，这里只做类型转换。
pub fn now_ms() -> i64 {
    crate::m0::now_ms() as i64
}
