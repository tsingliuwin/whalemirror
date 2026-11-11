//! dsh-session-query — 跨会话检索纯面（上游 `session-query/session-query`
//! 的 rustdsh 等价：冷读全量扫描后端，无派生索引）。
//!
//! 语料由宿主从耐久日志物化（[`documents::CorpusSession`]）；提取/表面/
//! 过滤/摘录/检索/读事件都是可单测的纯函数。上游发货默认全文检索关闭
//! （sqlite openAt:never）；rustdsh 无组合系统，按 roadmap A4.4 直接建
//! opt-in 面。排序取上游 sqlite 排序键精神（match_count DESC、文档长度
//! ASC、time DESC、seq DESC）。偏差：无 cursor/generation（每次查询全量
//! 扫描，上游分页游标随派生索引而来）；事件行时间取事件体 time_ms（耐
//! 久信封 time 由宿主物化时带入，内存面缺省 None）。

pub mod documents;
pub mod filters;
pub mod presentation;
pub mod search;
pub mod snippet;
pub mod trace;

pub use documents::{build_documents, classify_surfaces, event_type_name, extract_event_text, CorpusSession, Surface};
pub use filters::{compile_text_filter, EventFilters, QueryError, SessionFilters};
pub use presentation::{
    availability_text, format_event_read, format_event_search, format_event_trace,
    format_session_search, format_session_trace, RESULT_CAP_LINE, MAX_SEARCH_RESULTS,
};
pub use search::{
    read_event, search_events, search_sessions, EventHit, EventSearchRequest, SessionHit,
    SessionSearchRequest,
};
pub use snippet::{make_snippet, normalize_whitespace, SNIPPET_MAX_CHARS};
pub use trace::{event_trace, session_lineage, EventTrace, SessionLineage};
