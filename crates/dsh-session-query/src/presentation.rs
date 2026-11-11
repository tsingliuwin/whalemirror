//! 模型可见呈现（上游 `tool-session-query/src/presentation.ts` 模板逐字；
//! 动态字段由本面填充。ISO 时间走 `dsh_schedule::format_instant`）。

use crate::search::{EventHit, SessionHit};
use crate::trace::{EventTrace, SessionLineage};

/// 工具内部结果上限（上游 DEFAULT_MAX_SEARCH_RESULTS）。
pub const MAX_SEARCH_RESULTS: usize = 100;

/// 达到上限的收尾行（上游逐字）。
pub const RESULT_CAP_LINE: &str =
    "Result cap reached. Narrow the query or add filters to find additional matches.";

/// 可用性文案（上游 availabilityText；rustdsh 语料恒耐久——偏差：无
/// live/persisted 双旗）。
pub fn availability_text() -> &'static str {
    "persisted"
}

fn iso(time_ms: Option<i64>) -> String {
    time_ms
        .map(dsh_schedule::format_instant)
        .unwrap_or_else(|| "unknown".into())
}

fn title_or_fallback(title: Option<&str>) -> String {
    title.unwrap_or("untitled").to_string()
}

/// seq 列表行（上游 seqList：逗号 + 空格；空 = none）。
fn seq_list(seqs: &[u64]) -> String {
    if seqs.is_empty() {
        return "none".into();
    }
    seqs.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// 跨会话检索结果（上游 `formatSessionSearch` 逐字模板；capped = 达上限）。
pub fn format_session_search(hits: &[SessionHit], capped: bool) -> String {
    if hits.is_empty() {
        return "No prior session matches found.".into();
    }
    let mut lines = vec![format!("Session search results ({}):", hits.len()), String::new()];
    for (i, hit) in hits.iter().enumerate() {
        lines.push(format!(
            "{}. Session {} — {}",
            i + 1,
            hit.session_id,
            title_or_fallback(hit.title.as_deref())
        ));
        lines.push(format!("   Created: {}", iso(hit.created_at_ms)));
        lines.push(format!(
            "   Parent: {}",
            hit.parent_id.clone().unwrap_or_else(|| "root".to_string())
        ));
        lines.push(format!("   Availability: {}", availability_text()));
        lines.push(format!(
            "   Best match: seq {} | {} | {} | {}",
            hit.best.seq,
            hit.best.event_type,
            hit.best.surface.as_wire(),
            iso(hit.best.time_ms)
        ));
        lines.push(format!("   Snippet: {}", hit.best.snippet));
    }
    if capped {
        lines.push(RESULT_CAP_LINE.to_string());
    }
    lines.join("\n")
}

/// 单会话事件检索结果（上游 `formatEventSearch` 逐字模板）。
pub fn format_event_search(session_id: &str, title: Option<&str>, hits: &[EventHit], capped: bool) -> String {
    if hits.is_empty() {
        return "No prior event matches found.".into();
    }
    let mut lines = vec![
        format!("Session {} — {}", session_id, title_or_fallback(title)),
        format!("Event search results ({}):", hits.len()),
    ];
    for (i, hit) in hits.iter().enumerate() {
        lines.push(format!(
            "{}. seq {} | {} | {} | {}",
            i + 1,
            hit.seq,
            hit.event_type,
            hit.surface.as_wire(),
            iso(hit.time_ms)
        ));
        lines.push(format!("   Snippet: {}", hit.snippet));
    }
    if capped {
        lines.push(RESULT_CAP_LINE.to_string());
    }
    lines.join("\n")
}

/// 会话血缘（上游 `formatSessionTrace` 逐字模板）。
pub fn format_session_trace(lineage: &SessionLineage, corpus_len_hint: bool) -> String {
    let mut lines = vec![
        format!(
            "Session {} — {}",
            lineage.target.id,
            title_or_fallback(lineage.target.title.as_deref())
        ),
        format!("Created: {}", iso(lineage.target.created_at_ms)),
        format!("Availability: {}", availability_text()),
        String::new(),
        "Ancestors (nearest first):".to_string(),
    ];
    if lineage.ancestors.is_empty() && !lineage.ancestor_boundary {
        lines.push("- none (target is a root session)".to_string());
    }
    for record in &lineage.ancestors {
        lines.push(format!(
            "- {} — {} | {} | {}",
            record.id,
            title_or_fallback(record.title.as_deref()),
            iso(record.created_at_ms),
            availability_text()
        ));
    }
    if lineage.ancestor_boundary {
        lines.push("- [outside workspace boundary]".to_string());
    }
    lines.push(String::new());
    lines.push("Descendants:".to_string());
    if lineage.descendants.is_empty() {
        lines.push("- none".to_string());
    } else {
        for record in &lineage.descendants {
            lines.push(format!(
                "- {} — {} | {} | {}",
                record.id,
                title_or_fallback(record.title.as_deref()),
                iso(record.created_at_ms),
                availability_text()
            ));
        }
    }
    let _ = corpus_len_hint;
    lines.join("\n")
}

/// 事件血缘（上游 `formatEventTrace` 逐字模板）。
pub fn format_event_trace(session_id: &str, title: Option<&str>, trace: &EventTrace) -> String {
    [
        format!("Session {} — {}", session_id, title_or_fallback(title)),
        format!(
            "Target: seq {} | {} | {} | {}",
            trace.target_seq,
            trace.target_type,
            trace.target_surface.as_wire(),
            iso(trace.target_time_ms)
        ),
        format!(
            "Replaced by: {}",
            trace.replaced_by.map(|s| s.to_string()).unwrap_or_else(|| "none".into())
        ),
        format!("Replacement chain: {}", seq_list(&trace.replacement_chain)),
        format!("Events replaced by target: {}", seq_list(&trace.replaced_event_seqs)),
        format!("Events cited directly as sources: {}", seq_list(&trace.source_event_seqs)),
        format!("Direct derived events: {}", seq_list(&trace.derived_event_seqs)),
    ]
    .join("\n")
}

/// 事件读取（上游 `formatEventRead` 逐字模板：围栏 JSON + 前后邻行）。
pub fn format_event_read(
    session_id: &str,
    title: Option<&str>,
    seq: u64,
    target_json: &str,
    before: &[String],
    after: &[String],
) -> String {
    let mut lines = vec![
        format!("Session {} — {}", session_id, title_or_fallback(title)),
        format!("Target event seq {seq}:"),
        "```json".to_string(),
        target_json.to_string(),
        "```".to_string(),
    ];
    lines.push("Before:".to_string());
    if before.is_empty() {
        lines.push("- none".to_string());
    }
    for line in before {
        lines.push(line.clone());
    }
    lines.push("After:".to_string());
    if after.is_empty() {
        lines.push("- none".to_string());
    }
    for line in after {
        lines.push(line.clone());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(seq: u64) -> SessionHit {
        SessionHit {
            session_id: "session-1".into(),
            cwd: Some("/ws".into()),
            title: Some("标题".into()),
            parent_id: None,
            created_at_ms: Some(1_700_000_000_000),
            best: EventHit {
                session_id: "session-1".into(),
                seq,
                event_type: "user/message",
                time_ms: Some(1_700_000_000_000),
                surface: crate::Surface::Current,
                snippet: "命中摘录".into(),
                match_count: 1,
            },
        }
    }

    #[test]
    fn session_search_template_is_verbatim() {
        let text = format_session_search(&[hit(7)], false);
        assert!(text.starts_with("Session search results (1):\n"));
        assert!(text.contains("1. Session session-1 — 标题"));
        assert!(text.contains("   Created: "));
        assert!(text.contains("   Parent: root"));
        assert!(text.contains("   Availability: persisted"));
        assert!(text.contains("   Best match: seq 7 | user/message | current | "));
        assert!(text.contains("   Snippet: 命中摘录"));
        assert!(!text.contains(RESULT_CAP_LINE));
        assert_eq!(format_session_search(&[], false), "No prior session matches found.");
    }

    #[test]
    fn capped_search_appends_cap_line() {
        assert!(format_session_search(&[hit(1)], true).contains(RESULT_CAP_LINE));
    }

    #[test]
    fn event_trace_template_is_verbatim() {
        let trace = EventTrace {
            target_seq: 3,
            target_type: "user/message",
            target_surface: crate::Surface::Shadowed,
            target_time_ms: Some(1),
            replaced_by: Some(4),
            replacement_chain: vec![4, 6],
            replaced_event_seqs: Vec::new(),
            source_event_seqs: Vec::new(),
            derived_event_seqs: Vec::new(),
        };
        let text = format_event_trace("s1", None, &trace);
        assert!(text.contains("Target: seq 3 | user/message | shadowed | "));
        assert!(text.contains("Replaced by: 4"));
        assert!(text.contains("Replacement chain: 4, 6"));
        assert!(text.contains("Events replaced by target: none"));
        assert!(text.contains("Events cited directly as sources: none"));
        assert!(text.contains("Direct derived events: none"));
    }

    #[test]
    fn session_trace_template_is_verbatim() {
        let lineage = SessionLineage {
            target: &crate::CorpusSession {
                id: "t".into(),
                cwd: None,
                title: Some("目标".into()),
                parent_id: None,
                created_at_ms: Some(1),
                events: vec![],
            },
            ancestors: Vec::new(),
            ancestor_boundary: false,
            descendants: Vec::new(),
        };
        let text = format_session_trace(&lineage, false);
        assert!(text.contains("Session t — 目标"));
        assert!(text.contains("- none (target is a root session)"));
        assert!(text.contains("Descendants:\n- none"));
    }
}
