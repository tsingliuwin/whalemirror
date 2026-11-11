//! 过滤面（上游 `filters.ts`）：字面全文过滤编译（大小写不敏感、空白
//! 弹性）+ 会话/事件维度的结构过滤闭集。

use crate::documents::Surface;
use regex::Regex;

/// 查询错误（上游 SessionQueryError 词表的本面子集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub code: &'static str,
    pub message: String,
}

impl QueryError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// 字面全文过滤（上游 `compileSessionTextFilter`）：trim → 按空白切分 →
/// 逐段正则转义 → `\s+` 连接 → 大小写不敏感。空查询报错（逐字）。
pub fn compile_text_filter(text: &str) -> Result<Regex, QueryError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(QueryError::new(
            "SESSION_QUERY_INVALID_FILTER",
            "session text filter must contain non-whitespace text",
        ));
    }
    let joined = trimmed
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    Regex::new(&format!(r"(?i){joined}"))
        .map_err(|error| QueryError::new("SESSION_QUERY_INVALID_FILTER", error.to_string()))
}

/// 会话维度过滤闭集（上游 SessionResultFilter 的工具面子集；None = 不过滤）。
#[derive(Clone, Debug, Default)]
pub struct SessionFilters {
    pub session_ids: Vec<String>,
    pub cwds: Vec<String>,
    pub parent_session_ids: Vec<String>,
    /// 创建时间下/上界（含端；unix ms）。
    pub created_from_ms: Option<i64>,
    pub created_to_ms: Option<i64>,
}

impl SessionFilters {
    /// 会话记录是否通过全部子句（同子句内 OR、子句间 AND——上游同语义）。
    pub fn admits(
        &self,
        id: &str,
        cwd: Option<&str>,
        parent_id: Option<&str>,
        created_at_ms: Option<i64>,
    ) -> bool {
        if !self.session_ids.is_empty() && !self.session_ids.iter().any(|s| s == id) {
            return false;
        }
        if !self.cwds.is_empty()
            && !self
                .cwds
                .iter()
                .any(|c| Some(c.as_str()) == cwd)
        {
            return false;
        }
        if !self.parent_session_ids.is_empty()
            && !self
                .parent_session_ids
                .iter()
                .any(|p| Some(p.as_str()) == parent_id)
        {
            return false;
        }
        if let Some(from) = self.created_from_ms {
            if created_at_ms.is_none_or(|t| t < from) {
                return false;
            }
        }
        if let Some(to) = self.created_to_ms {
            if created_at_ms.is_none_or(|t| t > to) {
                return false;
            }
        }
        true
    }
}

/// 事件维度过滤闭集（上游 SessionEventResultFilter 的工具面子集）。
#[derive(Clone, Debug, Default)]
pub struct EventFilters {
    pub seq_from: Option<u64>,
    pub seq_to: Option<u64>,
    pub time_from_ms: Option<i64>,
    pub time_to_ms: Option<i64>,
    pub event_types: Vec<String>,
    pub surfaces: Vec<Surface>,
}

impl EventFilters {
    /// 事件记录是否通过（事件行时间缺承载时按不过滤该维处理）。
    pub fn admits(&self, doc: &crate::documents::SearchDocument) -> bool {
        if let Some(from) = self.seq_from {
            if doc.seq < from {
                return false;
            }
        }
        if let Some(to) = self.seq_to {
            if doc.seq > to {
                return false;
            }
        }
        if let Some(time) = doc.time_ms {
            if let Some(from) = self.time_from_ms {
                if time < from {
                    return false;
                }
            }
            if let Some(to) = self.time_to_ms {
                if time > to {
                    return false;
                }
            }
        }
        if !self.event_types.is_empty() && !self.event_types.iter().any(|t| t == doc.event_type) {
            return false;
        }
        if !self.surfaces.is_empty() && !self.surfaces.iter().any(|s| *s == doc.surface) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_filter_is_case_insensitive_and_whitespace_flexible() {
        let re = compile_text_filter("  fix   login retry ").unwrap();
        // 空白弹性：词间任意空白（含换行）都算一段
        assert!(re.is_match("Fix\nLOGIN\nretry"));
        assert!(re.is_match("please FIX login retry now"));
        // 但词序相接（无空白）不算
        assert!(!re.is_match("FixLOGIN retry"));
        assert!(!re.is_match("fixlogin retry"));
    }

    #[test]
    fn regex_metachars_are_literal() {
        let re = compile_text_filter("a.c (x)").unwrap();
        // 元字符按字面：点就是点
        assert!(re.is_match("A.C (X)"));
        assert!(re.is_match("a.c (x)"));
        assert!(!re.is_match("A C (X)"), "点不吞空白");
        assert!(!re.is_match("axc (x)"));
    }

    #[test]
    fn empty_query_is_rejected_verbatim() {
        let error = compile_text_filter("   ").unwrap_err();
        assert_eq!(error.code, "SESSION_QUERY_INVALID_FILTER");
        assert_eq!(
            error.message,
            "session text filter must contain non-whitespace text"
        );
    }

    #[test]
    fn session_filters_or_within_and_across() {
        let filters = SessionFilters {
            session_ids: vec!["a".into(), "b".into()],
            cwds: vec!["/ws".into()],
            ..Default::default()
        };
        assert!(filters.admits("a", Some("/ws"), None, None));
        assert!(!filters.admits("c", Some("/ws"), None, None), "id 子句 OR 外即拒");
        assert!(!filters.admits("a", Some("/other"), None, None), "子句间 AND");
    }
}
