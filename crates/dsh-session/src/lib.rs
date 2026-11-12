//! dsh-session — the append-only session event log.
//!
//! Mirrors [`packages/core/session`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/session):
//! the durable `SessionEvent` stream, the in-memory store, `derive_messages()`
//! projection of model-visible history, and the `request/header` /
//! `request/context` epochs that make a request reconstructable from the log.

pub mod catalog_names;
pub mod goal;
pub mod session;

pub use catalog_names::{
    AGENT_NAME_MAX_LEN, ROOT_AGENT_NAME, SUBAGENT_CATALOG_VERSION, CatalogConflict, CatalogRow,
    catalog_mode_allowed, catalog_mode_requires_label, catalog_names,
    catalog_version_modes, catalog_version_requires_name, find_catalog_conflict, is_agent_name,
};

pub use goal::{
    GOAL_CHANGE_VERSION, FoldedGoal, GoalBlockReason, GoalChange, GoalClear, GoalFoldState,
    GoalId, GoalMessageSource, GoalPhase, GoalRef, GoalSnapshot, GoalSnapshotChange,
    apply_goal_change, apply_goal_event, decode_goal_change, fold_goal,
};

pub use session::{
    CancelCause, EpochHeader, HeaderReason, ImageOffloadTarget, PresentedFile, RequestContext,
    JobRecord, JobSettlement, Session, SessionEntry, SessionEvent, SessionStatsTotals, TitleSource,
    TodoEntry, TurnChangesWatcher, TurnEndReason, WorkspaceChangedFile,
    session_stats_fold,
};