//! 检索面（上游引擎合同的具体后端）：冷读全量扫描 + 文本过滤 + 每会话
//! 最强命中分组 + 事件检索 + 事件读取。排序取上游 sqlite 排序键的精神
//! （match_count DESC, document_length ASC, time DESC, seq DESC）。

use crate::documents::{build_documents, CorpusSession, SearchDocument, Surface};
use crate::filters::{compile_text_filter, EventFilters, QueryError, SessionFilters};
use crate::snippet::{make_snippet, normalize_whitespace, SNIPPET_MAX_CHARS};

/// 一次命中（上游 SessionEventSearchHit 的本面子集 + match_count 供排序）。
#[derive(Clone, Debug)]
pub struct EventHit {
    pub session_id: String,
    pub seq: u64,
    pub event_type: &'static str,
    pub time_ms: Option<i64>,
    pub surface: Surface,
    pub snippet: String,
    pub match_count: usize,
}

/// 跨会话检索的一行（上游 SessionSearchHit：会话记录 + 最强命中）。
#[derive(Clone, Debug)]
pub struct SessionHit {
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub created_at_ms: Option<i64>,
    pub best: EventHit,
}

/// 跨会话检索请求（上游 SessionSearchRequest 的工具面子集）。
#[derive(Clone, Debug, Default)]
pub struct SessionSearchRequest {
    pub query: String,
    pub session_filters: SessionFilters,
    /// 检索事件维度过滤（上游 eventFilters——逐文档先过结构过滤再比文本）。
    pub event_filters: EventFilters,
    /// 返回上限（缺省 = 不限，调用方自带 max）。
    pub limit: Option<usize>,
}

/// 事件检索请求（上游 SessionEventSearchRequest）。
#[derive(Clone, Debug, Default)]
pub struct EventSearchRequest {
    pub session_id: String,
    pub query: String,
    pub filters: EventFilters,
    pub limit: Option<usize>,
}

/// 命中的排序键（上游 sqlite 排序键精神；DESC 各字段）。
fn rank_key(doc: &SearchDocument, match_count: usize) -> (usize, usize, i64, u64) {
    (match_count, usize::MAX - doc.text.chars().count(), doc.time_ms.unwrap_or(0), doc.seq)
}

fn matching_documents<'a>(
    docs: &'a [SearchDocument],
    matcher: &regex::Regex,
    filters: &EventFilters,
) -> Vec<(&'a SearchDocument, usize)> {
    docs.iter()
        .filter(|doc| filters.admits(doc))
        .filter_map(|doc| {
            let count = matcher.find_iter(&doc.text).count();
            (count > 0).then_some((doc, count))
        })
        .collect()
}

fn event_hit(session_id: &str, doc: &SearchDocument, match_count: usize, matcher: &regex::Regex) -> EventHit {
    let snippet = make_snippet(&normalize_whitespace(&doc.text), matcher, SNIPPET_MAX_CHARS);
    EventHit {
        session_id: session_id.to_string(),
        seq: doc.seq,
        event_type: doc.event_type,
        time_ms: doc.time_ms,
        surface: doc.surface,
        snippet,
        match_count,
    }
}

/// 跨会话检索（上游 `searchSessions`）：cwd/时间/血缘过滤 → 每会话最强
/// 命中 → 排序截断。空查询 = 结构过滤命中（上游 text 过滤必填由调用方
/// 保证——工具层 schema required）。
pub fn search_sessions(
    corpus: &[CorpusSession],
    request: &SessionSearchRequest,
) -> Result<Vec<SessionHit>, QueryError> {
    let matcher = compile_text_filter(&request.query)?;
    let mut hits: Vec<SessionHit> = Vec::new();
    for session in corpus {
        if !request.session_filters.admits(
            &session.id,
            session.cwd.as_deref(),
            session.parent_id.as_deref(),
            session.created_at_ms,
        ) {
            continue;
        }
        let docs = build_documents(session);
        let matches = matching_documents(&docs, &matcher, &request.event_filters);
        let Some((best_doc, best_count)) = matches
            .into_iter()
            .max_by_key(|(doc, count)| rank_key(doc, *count))
        else {
            continue;
        };
        hits.push(SessionHit {
            session_id: session.id.clone(),
            cwd: session.cwd.clone(),
            title: session.title.clone(),
            created_at_ms: session.created_at_ms,
            best: event_hit(&session.id, best_doc, best_count, &matcher),
        });
    }
    hits.sort_by(|a, b| {
        rank_key_of(&b.best).cmp(&rank_key_of(&a.best)).then_with(|| a.session_id.cmp(&b.session_id))
    });
    if let Some(limit) = request.limit {
        hits.truncate(limit);
    }
    Ok(hits)
}

fn rank_key_of(hit: &EventHit) -> (usize, usize, i64, u64) {
    // 文本长度在 hit 上不再携带——以 snippet 长度近似次序稳定性足够（排序
    // 键的长度分量只在同会话内起决定作用，跨会话比较保留 count/time/seq）
    (hit.match_count, 0, hit.time_ms.unwrap_or(0), hit.seq)
}

/// 单会话事件检索（上游 `searchEvents`）：结构过滤 → 文本匹配 →
/// seq 降序（新近优先）→ 截断。
pub fn search_events(
    corpus: &[CorpusSession],
    request: &EventSearchRequest,
) -> Result<Vec<EventHit>, QueryError> {
    let matcher = compile_text_filter(&request.query)?;
    let session = corpus
        .iter()
        .find(|s| s.id == request.session_id)
        .ok_or_else(|| QueryError::new("SESSION_QUERY_SESSION_NOT_FOUND", "session not found"))?;
    let docs = build_documents(session);
    let mut hits: Vec<EventHit> = matching_documents(&docs, &matcher, &request.filters)
        .into_iter()
        .map(|(doc, count)| event_hit(&session.id, doc, count, &matcher))
        .collect();
    hits.sort_by(|a, b| b.seq.cmp(&a.seq));
    if let Some(limit) = request.limit {
        hits.truncate(limit);
    }
    Ok(hits)
}

/// 读一个事件的完整 JSON 与邻行摘要（上游 `session_event_read`）：
/// 返回 (目标事件 JSON, before 行, after 行)；行形 `- seq {n} | {type} |
/// {time}` + 语义文本（缺文本 = `(no semantic text)`）。
pub fn read_event(
    corpus: &[CorpusSession],
    session_id: &str,
    seq: u64,
    before: usize,
    after: usize,
) -> Option<(String, Vec<String>, Vec<String>)> {
    let session = corpus.iter().find(|s| s.id == session_id)?;
    let shadowed = crate::documents::classify_surfaces(&session.events);
    let pos = session.events.iter().position(|(s, _, _)| *s == seq)?;
    let (_, event, time) = &session.events[pos];
    let target_json = serde_json::to_string_pretty(event).unwrap_or_else(|_| "{}".into());
    let neighbor_line = |(s, ev, t): &(u64, dsh_session::SessionEvent, Option<i64>)| -> String {
        let semantic = crate::documents::extract_event_text(ev);
        let mut line = format!(
            "- seq {} | {} | {}",
            s,
            crate::documents::event_type_name(ev),
            t.map(|ms| dsh_schedule::format_instant(ms))
                .unwrap_or_else(|| "unknown".into()),
        );
        if semantic.is_empty() {
            line.push_str(" | (no semantic text)");
        } else {
            for line_text in semantic.lines() {
                line.push_str("\n  ");
                line.push_str(line_text);
            }
        }
        line
    };
    let before_lines: Vec<String> = session.events[pos.saturating_sub(before)..pos]
        .iter()
        .map(&neighbor_line)
        .collect();
    let after_lines: Vec<String> = session.events[pos + 1..=(pos + after).min(session.events.len() - 1)]
        .iter()
        .map(&neighbor_line)
        .collect();
    let _ = &shadowed;
    Some((target_json, before_lines, after_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::CorpusSession;
    use dsh_llm::{Message, MessageSource, Role};
    use dsh_session::SessionEvent;

    fn user_msg(text: &str) -> SessionEvent {
        SessionEvent::UserMessage(Message::new(
            Role::User,
            vec![dsh_llm::ContentBlock::text(text)],
            MessageSource::User,
        ))
    }

    fn assistant_msg(text: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            interrupted: false,
            time_ms: None,
            usage: None,
            message: Message::new(
                Role::Assistant,
                vec![dsh_llm::ContentBlock::text(text)],
                MessageSource::Model {
                    provider: "mock".into(),
                    model: "mock".into(),
                    replay_state: None,
                },
            ),
        }
    }

    fn session(id: &str, cwd: &str, events: Vec<(u64, SessionEvent, Option<i64>)>) -> CorpusSession {
        CorpusSession {
            id: id.into(),
            cwd: Some(cwd.into()),
            title: None,
            parent_id: None,
            created_at_ms: Some(1_700_000_000_000),
            events,
        }
    }

    #[test]
    fn session_search_groups_by_session_with_best_event() {
        let corpus = vec![
            session(
                "session-a",
                "/ws",
                vec![
                    (1, user_msg("fix login retry flow"), Some(1000)),
                    (2, assistant_msg("fixed the login retry, login retry again"), Some(2000)),
                ],
            ),
            session("session-b", "/other", vec![(1, user_msg("unrelated"), Some(3000))]),
        ];
        let hits = search_sessions(
            &corpus,
            &SessionSearchRequest {
                query: "login retry".into(),
                session_filters: SessionFilters { cwds: vec!["/ws".into()], ..Default::default() },
                event_filters: EventFilters::default(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1, "cwd 过滤外会话缺席");
        assert_eq!(hits[0].session_id, "session-a");
        assert_eq!(hits[0].best.seq, 2, "同会话最强命中（match_count 高者）");
        assert!(hits[0].best.snippet.contains("login retry"));
    }

    #[test]
    fn session_search_orders_by_rank_and_truncates() {
        let mk = |id: &str, seq: u64| {
            session(id, "/ws", vec![(seq, user_msg("needle here"), Some(1000 + seq as i64))])
        };
        let corpus: Vec<_> = vec![mk("s1", 1), mk("s2", 2), mk("s3", 3)];
        let hits = search_sessions(
            &corpus,
            &SessionSearchRequest {
                query: "needle".into(),
                session_filters: SessionFilters::default(),
                event_filters: EventFilters::default(),
                limit: Some(2),
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].session_id, "s3", "同分按 time/seq 降序");
    }

    #[test]
    fn event_search_filters_and_orders_desc() {
        let corpus = vec![session(
            "s1",
            "/ws",
            vec![
                (1, user_msg("alpha needle"), Some(1)),
                (2, assistant_msg("beta"), Some(2)),
                (3, user_msg("gamma NEEDLE"), Some(3)),
            ],
        )];
        let hits = search_events(
            &corpus,
            &EventSearchRequest {
                session_id: "s1".into(),
                query: "needle".into(),
                filters: EventFilters {
                    surfaces: vec![Surface::Current],
                    ..Default::default()
                },
                limit: Some(1),
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 3, "新近优先");
    }

    #[test]
    fn event_search_missing_session_errors() {
        let corpus = vec![session("s1", "/ws", vec![(1, user_msg("x"), None)])];
        let error = search_events(
            &corpus,
            &EventSearchRequest { session_id: "zz".into(), query: "x".into(), ..Default::default() },
        )
        .unwrap_err();
        assert_eq!(error.code, "SESSION_QUERY_SESSION_NOT_FOUND");
    }

    #[test]
    fn shadowed_surface_classified_from_compaction() {
        let corpus = vec![session(
            "s1",
            "/ws",
            vec![
                (1, user_msg("old needle"), Some(1)),
                (
                    2,
                    SessionEvent::Compaction {
                        before_seq: 2,
                        summary: "压缩".into(),
                        compaction_id: "c1".into(),
                        shadowed_start: 1,
                        shadowed_seqs: vec![1],
                        shadowed_token_count: 10,
                        provider: "mock".into(),
                        model: "mock".into(),
                    },
                    Some(2),
                ),
            ],
        )];
        let hits = search_events(
            &corpus,
            &EventSearchRequest {
                session_id: "s1".into(),
                query: "needle".into(),
                filters: EventFilters {
                    surfaces: vec![Surface::Shadowed],
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].surface, Surface::Shadowed);
    }

    #[test]
    fn read_event_returns_json_and_neighbor_lines() {
        let corpus = vec![session(
            "s1",
            "/ws",
            vec![
                (1, user_msg("first"), Some(1)),
                (2, user_msg("target needle"), Some(2)),
                (3, assistant_msg("after"), Some(3)),
            ],
        )];
        let (json, before, after) = read_event(&corpus, "s1", 2, 1, 1).unwrap();
        assert!(json.contains("user-message"), "内存面 serde kebab tag");
        assert_eq!(before.len(), 1);
        assert!(before[0].starts_with("- seq 1 | user/message |"));
        assert!(before[0].contains("first"));
        assert_eq!(after.len(), 1);
        assert!(after[0].contains("assistant/message"));
    }
}
