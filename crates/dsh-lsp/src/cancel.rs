//! 查询取消原语：上游 `lsp-stdio/src/abort.ts` 的 `AbortSignal` 等价物。
//!
//! 上游用 DOM `AbortSignal` 把取消从工具层（60s 预算）一路传进 provider 的宿主 I/O、
//! 队列等待与协议阶段；rustdsh 用「原子标志 + `Notify`」实现同语义：`abort()` 置位并
//! 唤醒所有等待者，`cancelled()` 非阻塞轮询，`wait()` 给 `select!` 当等待臂。
//!
//! 中止错误的文案与上游对齐：有 reason 透传 reason，没有 reason 时用
//! `'LSP query aborted'` 兜底（上游 `abortError` 的第三分支）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{LspError, LspErrorCode};

#[derive(Default)]
struct CancelInner {
    aborted: AtomicBool,
    notify: tokio::sync::Notify,
}

/// 一次查询的取消信号；克隆共享同一状态。
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位并唤醒所有等待者（幂等）。
    pub fn abort(&self) {
        self.0.aborted.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    /// 是否已取消（非阻塞）。
    pub fn cancelled(&self) -> bool {
        self.0.aborted.load(Ordering::SeqCst)
    }

    /// 等待取消发生（已取消则立即返回）。
    pub async fn wait(&self) {
        while !self.cancelled() {
            // notify_waiters 只唤醒「已在等待」的任务，先判标志再等的两段式避免丢唤醒。
            let notified = self.0.notify.notified();
            if self.cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// 已取消时返回上游同款中止错误。
    pub fn check(&self) -> Result<(), LspError> {
        if self.cancelled() {
            Err(abort_error())
        } else {
            Ok(())
        }
    }
}

/// 中止错误：上游 `abortError` 无 reason 分支的逐字文案。
pub fn abort_error() -> LspError {
    LspError::new(LspErrorCode::Timeout, "LSP query aborted")
}

impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancelToken {}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.cancelled())
            .finish()
    }
}