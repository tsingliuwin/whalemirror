//! dsh-spill — 超预算文本外置存储（上游 `packages/spill/*` 的等价面）。
//!
//! 三个包合成一个 crate：
//! - `spill`：`SpillLocator` / `SpillRef` / `save_text` 词汇；
//! - `spill-local`：本机文件后端（进程私有根、会话分目录、不可逆拆分安全
//!   路径段、独占写）；
//! - `spill-policy`：工具结果的 token 预算保留（head/tail 预览 + 省略通知），
//!   另含 `compaction/tool-result-pruner` 的确定性 head/middle/tail 剪枝。
//!
//! 语义要点（与上游对齐）：
//! - 定位符是**不透明句柄**，消费方只渲染 `retrievalHint`，不解析；
//! - 存储失败 / 无属主 / 无后端 → **保留原文**（策略永不吞掉结果）；
//! - 保留 = 头尾各占一半预算，中间用 `[...]` 断开，图片整块保留或整块省略；
//! - 通知本身放不进预算 → 保留原文（绝不产生更大的结果）。

pub mod local;
pub mod policy;
pub mod prune;
pub mod retention;

pub use policy::SpillPolicy;

/// 定位符（上游 `SpillLocator`，不透明字符串）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillLocator(pub String);

impl SpillLocator {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 一次外置的产物：定位符 + 字节数 + 后端给的取回提示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillRef {
    pub locator: SpillLocator,
    pub bytes: usize,
    /// 取回提示（本机后端固定文案：`Use read with offset/limit, or grep this path to search within it.`）
    pub retrieval_hint: String,
}

/// 本机后端的取回提示（上游 spill-local 渲染文案）。
pub const RETRIEVAL_HINT: &str =
    "Use read with offset/limit, or grep this path to search within it.";

/// 产物来源（描述性，不参与访问控制）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpillSource {
    Tool { tool_name: String, call_id: String, label: String },
    SessionReference { session_id: String, label: String },
}

/// 一次保存请求。
#[derive(Clone, Debug)]
pub struct SaveTextSpill {
    /// 属主会话 id（后端据此分目录）。
    pub session_id: String,
    pub source: SpillSource,
    /// 调用方建议的文件名（后端会转成单段安全名——只是提示，不是路径）。
    pub suggested_name: String,
    /// 全文（UTF-8）。
    pub content: String,
}
