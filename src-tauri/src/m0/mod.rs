//! M0 公共支撑（设计文档 §3.1、§3.2）。本模块不承载任何业务判定。

pub mod diagnostics;
pub mod logging;
pub mod oplog;
pub mod paths;
pub mod settings;

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 Unix 纪元毫秒（主计划 §3.6「时间表示」：后端不解析时间，只取当前时刻）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
