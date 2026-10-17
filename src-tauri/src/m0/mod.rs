//! M0 公共支撑（设计文档 §3.1、§3.2）。本模块不承载任何业务判定。

pub mod cancel;
pub mod diagnostics;
pub mod logging;
pub mod net;
pub mod oplog;
pub mod paths;
pub mod settings;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 下载进度回调：已下载字节数、总字节数（未知时为 None）。
///
/// 主计划 §3.3.1 冻结了它的形状与 `new` 构造。这里用 newtype 而不是裸类型别名，
/// 因为 Rust 不允许给 `Arc<dyn Fn…>` 这类外部类型写固有 impl（§3.3.1 的 `impl ProgressSink`
/// 只有在它是本地类型时才成立）。端口适配器周期性调用 `report`，调用方把它转成
/// task:progress 事件（主计划 §3.5）。
#[derive(Clone)]
pub struct ProgressSink(Arc<dyn Fn(u64, Option<u64>) + Send + Sync>);

impl ProgressSink {
    /// 把一个闭包包成 ProgressSink。
    pub fn new(f: impl Fn(u64, Option<u64>) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// 上报一次进度。
    pub fn report(&self, downloaded: u64, total: Option<u64>) {
        (self.0)(downloaded, total)
    }
}

/// 当前 Unix 纪元毫秒（主计划 §3.6「时间表示」：后端不解析时间，只取当前时刻）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn progress_sink_is_send_sync_and_callable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProgressSink>();

        let seen = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&seen);
        let sink = ProgressSink::new(move |downloaded, total| {
            assert_eq!(total, Some(1024));
            counter.fetch_add(downloaded, Ordering::SeqCst);
        });
        sink.report(512, Some(1024));
        sink.report(512, Some(1024));
        assert_eq!(seen.load(Ordering::SeqCst), 1024);
    }

    #[test]
    fn now_ms_is_a_plausible_unix_epoch_millisecond() {
        let now = now_ms();
        assert!(now > 1_577_836_800_000, "应晚于 2020-01-01");
        assert!(now < 4_102_444_800_000, "应早于 2100-01-01");
    }
}
