//! 长任务取消令牌（主计划 §3.3 的 CancelToken、NFR-2；设计文档原则 P3）。
//!
//! 消除竞态的做法：用一个初始 0 许可的 Semaphore 当闸门，cancel 时 close()。
//! 关闭前调用 acquire() 会挂起，关闭后（无论先后）立即返回错误 —— 因此不存在
//! 「先检查标志、后注册等待」之间丢失唤醒的窗口。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

#[derive(Debug)]
struct CancelInner {
    cancelled: AtomicBool,
    gate: Semaphore,
}

/// 可克隆的取消令牌：克隆体共享同一取消状态。
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancelInner {
                cancelled: AtomicBool::new(false),
                gate: Semaphore::new(0),
            }),
        }
    }

    /// 幂等：重复调用不改变状态，也不报错。
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.gate.close();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 已取消则立即返回；否则等到取消发生（close 会唤醒全部等待者）。
    pub async fn cancelled(&self) {
        let _ = self.inner.gate.acquire().await;
    }

    /// 测试构造（主计划 §3.3.1 冻结）：不经过注册表，直接得到一个可手工触发的令牌。
    pub fn new_for_test() -> Self {
        Self::new()
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// 任务标识 → 令牌。
#[derive(Debug, Default)]
pub struct CancelRegistry {
    tokens: Mutex<HashMap<String, CancelToken>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个任务并返回它的令牌。同一 task_id 重复注册会先取消旧令牌，
    /// 否则旧的等待者会永久挂起。
    pub fn register(&self, task_id: &str) -> CancelToken {
        let token = CancelToken::new();
        let mut guard = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = guard.insert(task_id.to_owned(), token.clone()) {
            previous.cancel();
        }
        token
    }

    /// 取消并移除。返回 false 表示没有该任务（重复取消即这种情形）。
    pub fn cancel(&self, task_id: &str) -> bool {
        let mut guard = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.remove(task_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// 任务正常结束时清理，不触发取消。
    pub fn remove(&self, task_id: &str) {
        let mut guard = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(task_id);
    }

    pub fn len(&self) -> usize {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_token_is_not_cancelled() {
        let token = CancelToken::new_for_test();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let token = CancelToken::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_cancelled() {
        let token = CancelToken::new();
        token.cancel();
        let result = tokio::time::timeout(Duration::from_millis(50), token.cancelled()).await;
        assert!(result.is_ok(), "已取消的令牌必须立即返回");
    }

    #[tokio::test]
    async fn cancelled_waits_until_cancel_happens() {
        let token = CancelToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            true
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!handle.is_finished(), "未取消前等待者必须挂起");
        token.cancel();
        let joined = tokio::time::timeout(Duration::from_millis(200), handle).await;
        assert!(joined.unwrap().unwrap(), "取消必须唤醒等待者");
    }

    #[test]
    fn register_cancel_removes_token_and_marks_it() {
        let registry = CancelRegistry::new();
        let token = registry.register("install-1");
        assert_eq!(registry.len(), 1);

        assert!(registry.cancel("install-1"));
        assert!(token.is_cancelled());
        assert_eq!(registry.len(), 0, "取消后令牌必须被清理");

        assert!(!registry.cancel("install-1"), "重复取消幂等，返回 false");
        assert!(registry.is_empty());
    }

    #[test]
    fn remove_clears_without_cancelling() {
        let registry = CancelRegistry::new();
        let token = registry.register("install-2");
        registry.remove("install-2");
        assert!(registry.is_empty());
        assert!(!token.is_cancelled(), "正常结束的任务不应被标记为已取消");
        assert!(!registry.cancel("install-2"));
    }

    #[test]
    fn re_registering_same_id_cancels_the_previous_token() {
        let registry = CancelRegistry::new();
        let first = registry.register("install-3");
        let second = registry.register("install-3");
        assert!(
            first.is_cancelled(),
            "重复注册必须唤醒旧等待者，避免永久挂起"
        );
        assert!(!second.is_cancelled());
        assert_eq!(registry.len(), 1);
        assert!(registry.cancel("install-3"));
        assert!(second.is_cancelled());
        assert!(registry.is_empty());
    }
}
