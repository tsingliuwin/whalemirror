//! 血缘面（上游 `traceSession` / `traceEvent` 的 rustdsh 等价）：会话祖
//! 先/后代行走 + 事件替换链（压缩影子区是 rustdsh 唯一的替换机制）。

use crate::documents::{classify_surfaces, event_type_name, CorpusSession};

/// 会话血缘走查（上游 SessionLineageTrace 的 rustdsh 等价材料）。
pub struct SessionLineage<'a> {
    pub target: &'a CorpusSession,
    /// 祖先链（parent 逐级上溯，最近在前）；rustdsh 语料 parent 缺省
    /// None → 目标即根。
    pub ancestors: Vec<&'a CorpusSession>,
    /// 祖先越界（可见语料断链——上游 [outside workspace boundary]）。
    pub ancestor_boundary: bool,
    /// 直接后代（parent == 目标 id，id 升序）。
    pub descendants: Vec<&'a CorpusSession>,
}

/// 会话血缘（上游 `traceSession`）：parent 链上溯 + corpus 内直接后代。
pub fn session_lineage<'a>(corpus: &'a [CorpusSession], target_id: &str) -> Option<SessionLineage<'a>> {
    let target = corpus.iter().find(|s| s.id == target_id)?;
    let mut ancestors = Vec::new();
    let mut ancestor_boundary = false;
    let mut cursor = target.parent_id.clone();
    let mut visited = std::collections::HashSet::new();
    visited.insert(target.id.clone());
    while let Some(parent) = cursor {
        if !visited.insert(parent.clone()) {
            break;
        }
        match corpus.iter().find(|s| s.id == parent) {
            Some(record) => {
                ancestors.push(record);
                cursor = record.parent_id.clone();
            }
            None => {
                ancestor_boundary = true;
                break;
            }
        }
    }
    let mut descendants: Vec<&CorpusSession> = corpus
        .iter()
        .filter(|s| s.parent_id.as_deref() == Some(target_id))
        .collect();
    descendants.sort_by(|a, b| a.id.cmp(&b.id));
    Some(SessionLineage { target, ancestors, ancestor_boundary, descendants })
}

/// 事件替换链观察（上游 SessionEventTraceObservation 的 rustdsh 等价）。
pub struct EventTrace {
    pub target_seq: u64,
    pub target_type: &'static str,
    pub target_surface: crate::documents::Surface,
    pub target_time_ms: Option<i64>,
    /// 替换目标的事件 seq（影子它的压缩事务；无 = None）。
    pub replaced_by: Option<u64>,
    /// 替换链（逐级被后续压缩再影子；含末端）。
    pub replacement_chain: Vec<u64>,
    /// 目标自身替换的事件 seq（目标是压缩事务时的影子区）。
    pub replaced_event_seqs: Vec<u64>,
    /// 直接引用源（上游 surfaceOp 归因；rustdsh 无该机制，恒空——偏差）。
    pub source_event_seqs: Vec<u64>,
    /// 直接派生事件（上游无对应机制，恒空——偏差）。
    pub derived_event_seqs: Vec<u64>,
}

/// 事件替换链（上游 `traceEvent`）：压缩影子区是 rustdsh 唯一替换机制。
pub fn event_trace(session: &CorpusSession, seq: u64) -> Option<EventTrace> {
    let shadowed = classify_surfaces(&session.events);
    let (_, target_event, target_time) = session.events.iter().find(|(s, _, _)| *s == seq)?;
    let target_surface = crate::documents::surface_of(seq, &shadowed, target_event);
    // 影子目标的压缩事务（首个 shadowed_seqs 含 seq 的 compaction）
    let shadowing_compaction = session.events.iter().find_map(|(s, event, _)| match event {
        dsh_session::SessionEvent::Compaction { shadowed_seqs, .. }
            if shadowed_seqs.contains(&seq) =>
        {
            Some(*s)
        }
        _ => None,
    });
    // 替换链：压缩事务自己再被后续压缩影子 → 逐级下走
    let mut replacement_chain = Vec::new();
    let mut cursor = shadowing_compaction;
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = cursor {
        if !seen.insert(current) {
            break;
        }
        replacement_chain.push(current);
        cursor = session.events.iter().find_map(|(s, event, _)| match event {
            dsh_session::SessionEvent::Compaction { shadowed_seqs, .. }
                if shadowed_seqs.contains(&current) =>
            {
                Some(*s)
            }
            _ => None,
        });
    }
    // 目标自身是压缩事务 → 它替换的影子区
    let replaced_event_seqs = match target_event {
        dsh_session::SessionEvent::Compaction { shadowed_seqs, .. } => shadowed_seqs.clone(),
        _ => Vec::new(),
    };
    Some(EventTrace {
        target_seq: seq,
        target_type: event_type_name(target_event),
        target_surface,
        target_time_ms: *target_time,
        replaced_by: shadowing_compaction,
        replacement_chain,
        replaced_event_seqs,
        source_event_seqs: Vec::new(),
        derived_event_seqs: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{Message, MessageSource, Role};
    use dsh_session::SessionEvent;

    fn user_msg(text: &str) -> SessionEvent {
        SessionEvent::UserMessage(Message::new(
            Role::User,
            vec![dsh_llm::ContentBlock::text(text)],
            MessageSource::User,
        ))
    }

    fn compaction(id: &str, shadowed: Vec<u64>) -> SessionEvent {
        SessionEvent::Compaction {
            before_seq: shadowed.iter().copied().max().unwrap_or(0) + 1,
            summary: "压缩摘要".into(),
            compaction_id: id.into(),
            shadowed_start: shadowed.iter().copied().min().unwrap_or(0),
            shadowed_seqs: shadowed,
            shadowed_token_count: 10,
            provider: "mock".into(),
            model: "mock".into(),
        }
    }

    #[test]
    fn session_lineage_walks_parents_and_collects_descendants() {
        let corpus = vec![
            CorpusSession {
                id: "root".into(),
                cwd: Some("/ws".into()),
                title: None,
                parent_id: None,
                created_at_ms: Some(1),
                events: vec![],
            },
            CorpusSession {
                id: "child".into(),
                cwd: Some("/ws".into()),
                title: None,
                parent_id: Some("root".into()),
                created_at_ms: Some(2),
                events: vec![],
            },
            CorpusSession {
                id: "grand".into(),
                cwd: Some("/ws".into()),
                title: None,
                parent_id: Some("child".into()),
                created_at_ms: Some(3),
                events: vec![],
            },
        ];
        let lineage = session_lineage(&corpus, "grand").unwrap();
        assert_eq!(lineage.ancestors.len(), 2);
        assert_eq!(lineage.ancestors[0].id, "child", "最近在前");
        assert_eq!(lineage.ancestors[1].id, "root");
        assert!(!lineage.ancestor_boundary);
        let root_lineage = session_lineage(&corpus, "root").unwrap();
        assert_eq!(root_lineage.descendants.len(), 1);
        assert_eq!(root_lineage.descendants[0].id, "child");
        assert!(root_lineage.ancestors.is_empty());
    }

    #[test]
    fn broken_parent_chain_marks_boundary() {
        let corpus = vec![CorpusSession {
            id: "orphan".into(),
            cwd: Some("/ws".into()),
            title: None,
            parent_id: Some("missing".into()),
            created_at_ms: Some(1),
            events: vec![],
        }];
        let lineage = session_lineage(&corpus, "orphan").unwrap();
        assert!(lineage.ancestors.is_empty());
        assert!(lineage.ancestor_boundary);
    }

    #[test]
    fn event_trace_follows_compaction_replacement() {
        let session = CorpusSession {
            id: "s1".into(),
            cwd: None,
            title: None,
            parent_id: None,
            created_at_ms: None,
            events: vec![
                (1, user_msg("旧上下文"), Some(1)),
                (2, compaction("c1", vec![1]), Some(2)),
                (3, user_msg("新对话"), Some(3)),
                (4, compaction("c2", vec![2]), Some(4)),
            ],
        };
        // 旧消息 → 被压缩 c1 替换；c1 又被 c2 影子 → 链 [2, 4]
        let trace = event_trace(&session, 1).unwrap();
        assert_eq!(trace.replaced_by, Some(2));
        assert_eq!(trace.replacement_chain, vec![2, 4]);
        assert!(trace.replaced_event_seqs.is_empty());
        // 压缩事件自己：replaced = 其影子区
        let compaction_trace = event_trace(&session, 2).unwrap();
        assert_eq!(compaction_trace.replaced_event_seqs, vec![1]);
        assert_eq!(compaction_trace.replaced_by, Some(4));
    }
}
