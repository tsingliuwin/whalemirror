//! dsh-goal — 目标域（A1.3）：服务面与模型面工具。
//!
//! 上游四包的 Rust 等价：
//! - `packages/goal/goal`（纯面）→ `dsh-session::goal`（批① 已落：类型、严格
//!   解码器、状态机校验、承认轮折叠）。
//! - `packages/goal/goal`（GoalService）→ 本 crate [`store`]（单会话目标的
//!   CAS 变换 + 进程本地 activation + 耐久提交缝）。
//! - `packages/goal/tool-goal` → 本 crate [`tools`]（`get_goal`/`create_goal`/
//!   `update_goal` 三工具 + authority 武装门 + wrapup 上下文渲染）。
//! - `packages/goal/goal-round-driver` + `command-goal` → 后续批（宿主接线）。

pub mod authority;
pub mod store;
pub mod tools;
pub mod wrapup;

pub use authority::{
    CompletionAuthority, AuthorityProbe, completion_authority, is_matching_goal_round,
    require_direct_human,
};
pub use store::{
    CommitSink, CreateGoalRequest, EditGoalRequest, GoalActivation, GoalChanged, GoalError,
    GoalStore, GoalView, codes,
};
pub use tools::{DEFAULT_BLOCKED_AFTER_CONSECUTIVE_ROUNDS, GoalTools};
pub use wrapup::render_wrapup_context;