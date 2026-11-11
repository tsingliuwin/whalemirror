//! 连续重复调用提醒（上游 `guard/repeat-tool-reminder` 的 rustdsh 等价面）。
//!
//! 咨询性侦测、从不否决：每次工具调用尝试在 post-execute 位计数（拒绝/
//! 未知的调用同样计数——模型连续重锤被拒调用正是要打断的循环），连击长度
//! 恰命中阈值时产一条提醒文本，由宿主循环注入为
//! user/plugin `repeat-tool-reminder` notice（V4 wire
//! `{kind:'repeat-tool-reminder', form:'notice', summary}`）。
//! 用户插话重置连击链（语境变了，跨插话的重复不是循环）。

use serde_json::Value;
use std::collections::HashSet;
use std::sync::Mutex;

/// 默认触发阈值（上游 Config 默认；部署组合 cordis.patch.yml 同值）。
pub const DEFAULT_THRESHOLDS: &[u64] = &[3, 5, 8];

/// 默认详述提醒参数预览上限（上游 `argumentsPreviewChars` 默认）。
pub const DEFAULT_PREVIEW_CHARS: usize = 500;

/// 首档温和提醒（上游 `GENTLE_REMINDER`，逐字；keyed 到 `thresholds[0]`
/// 而非字面计数——自定义首档保持温和→详述的升级节奏）。
pub const GENTLE_REMINDER: &str = "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.";

/// 后续档详述提醒（上游 `detailedReminder`，逐字模板）。
pub fn detailed_reminder(tool_name: &str, count: u64, canonical_arguments: &str) -> String {
    format!(
        "Repeated tool call detected:\n- tool: {tool_name}\n- consecutive_calls: {count}\n- arguments: {canonical_arguments}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered."
    )
}

/// 配置（上游 `Config`；rustdsh 部署组合用默认值，字段留给后续接线）。
#[derive(Clone, Debug)]
pub struct RepeatGuardConfig {
    pub thresholds: Vec<u64>,
    /// 追踪的工具名 `*` 通配模式；空 = 全追踪。
    pub include: Vec<String>,
    /// 对连击链透明的工具名模式（既不计数也不重置）。
    pub exclude: Vec<String>,
    pub preview_chars: usize,
}

impl Default for RepeatGuardConfig {
    fn default() -> Self {
        Self {
            thresholds: DEFAULT_THRESHOLDS.to_vec(),
            include: Vec::new(),
            exclude: Vec::new(),
            preview_chars: DEFAULT_PREVIEW_CHARS,
        }
    }
}

impl RepeatGuardConfig {
    /// fail-loud 校验（上游 `validateThresholds` + preview 检查，文案逐字）
    /// 并返回升序阈值（升级规则读 `thresholds[0]` 为温和档，序在此归一）。
    pub fn validated(self) -> Result<ValidatedConfig, String> {
        if self.thresholds.is_empty() {
            return Err("repeat-tool-reminder: `thresholds` must not be empty".into());
        }
        for value in &self.thresholds {
            if *value < 2 {
                return Err(format!(
                    "repeat-tool-reminder: invalid threshold {value} — every threshold must be an integer >= 2"
                ));
            }
        }
        if self.thresholds.iter().collect::<HashSet<_>>().len() != self.thresholds.len() {
            return Err("repeat-tool-reminder: `thresholds` must not contain duplicates".into());
        }
        if self.preview_chars < 1 {
            return Err(format!(
                "repeat-tool-reminder: invalid argumentsPreviewChars {} — must be an integer >= 1",
                self.preview_chars
            ));
        }
        let mut thresholds = self.thresholds;
        thresholds.sort_unstable();
        Ok(ValidatedConfig {
            thresholds,
            include: self.include,
            exclude: self.exclude,
            preview_chars: self.preview_chars,
        })
    }
}

/// 校验后的配置（阈值升序）。
#[derive(Clone, Debug)]
pub struct ValidatedConfig {
    pub thresholds: Vec<u64>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub preview_chars: usize,
}

/// 深键排序（上游 `sortJsonValue`）：仅键序不同的两个参数对象规范化为
/// 同一形态。JSON 值域即全输入域（无 bigint/环/undefined 路径）。
pub fn sort_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_json_value).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sort_json_value(&map[key]));
            }
            Value::Object(sorted)
        }
        other => other.clone(),
    }
}

/// 参数的规范串形（上游 `canonicalize`）：深键排序后 stringify；畸形 JSON
/// 的原始串按字符串字面量入键（上游 raw-string 回退同语义）。
pub fn canonicalize(raw_arguments: &str) -> String {
    let value = match serde_json::from_str::<Value>(raw_arguments) {
        Ok(v) => sort_json_value(&v),
        Err(_) => Value::String(raw_arguments.to_string()),
    };
    serde_json::to_string(&value).unwrap_or_else(|_| format!("\"{raw_arguments}\""))
}

/// 锚定 `*` 通配匹配（上游 `wildcardToRegExp` 语义：`*` → 任意序列，
/// 其余字符字面，整体锚定）。无 regex 依赖的手写实现。
pub fn wildcard_matches(pattern: &str, name: &str) -> bool {
    match pattern.find('*') {
        None => pattern == name,
        Some(star) => {
            let prefix = &pattern[..star];
            let rest = &pattern[star + 1..];
            let Some(after_prefix) = name.strip_prefix(prefix) else {
                return false;
            };
            // `*` 吞掉任意长度（含 0），余下模式继续锚定匹配
            (0..=after_prefix.len())
                .any(|skip| after_prefix.is_char_boundary(skip) && wildcard_matches(rest, &after_prefix[skip..]))
        }
    }
}

/// 详述提醒的参数头截断（上游 `previewArguments`，逐字标记；只界模型可见
/// 文本——链键恒用完整规范串）。按字符截断（上游为 UTF-16 码元，ASCII 同形）。
pub fn preview_arguments(canonical: &str, cap: usize) -> String {
    let total = canonical.chars().count();
    if total <= cap {
        return canonical.to_string();
    }
    let head: String = canonical.chars().take(cap).collect();
    format!("{head}… (+{} more chars)", total - cap)
}

/// 一个 agent 的连击链：上次被追踪调用的身份键与连击长度
/// （上游 `WeakMap<Agent, Chain>`；rustdsh 单 agent 单链）。
struct Chain {
    key: String,
    count: u64,
}

/// 守卫：配置 + 连击链。宿主循环持有一份；`observe` 在每次工具调用尝试
/// 完成后调用（post-execute 位），`reset` 在含 user 源输入的步前调用。
pub struct RepeatChainGuard {
    config: ValidatedConfig,
    chain: Mutex<Option<Chain>>,
}

/// 一次命中阈值产出的提醒（正文 + notice 摘要）。
pub struct ReminderText {
    pub text: String,
    pub summary: String,
}

impl RepeatChainGuard {
    /// 部署组合默认配置（校验失败在构造即 panic——上游 load fail-loud）。
    pub fn with_defaults() -> Self {
        Self::new(RepeatGuardConfig::default())
    }

    /// fail-loud 构造（上游 `apply` 的 load 校验同语义）。
    pub fn new(config: RepeatGuardConfig) -> Self {
        let validated = config.validated().expect("repeat-tool-reminder: config");
        Self { config: validated, chain: Mutex::new(None) }
    }

    /// 工具是否入链（上游 `tracked`：不在 include（非空时）或在 exclude
    /// 的调用对链透明——既不计数也不重置）。
    fn tracked(&self, tool_name: &str) -> bool {
        if !self.config.include.is_empty()
            && !self.config.include.iter().any(|p| wildcard_matches(p, tool_name))
        {
            return false;
        }
        !self.config.exclude.iter().any(|p| wildcard_matches(p, tool_name))
    }

    /// 清零连击链（上游 agent/pre-step 重置钩：本步输入含 user 源消息）。
    pub fn reset(&self) {
        *self.chain.lock().unwrap() = None;
    }

    /// 为一次调用尝试推进链，恰命中阈值时返回要注入的提醒
    /// （上游 `observe`；先推进状态再看下游结果——拒绝的调用同样计数）。
    pub fn observe(&self, tool_name: &str, raw_arguments: &str) -> Option<ReminderText> {
        if !self.tracked(tool_name) {
            return None;
        }
        let canonical = canonicalize(raw_arguments);
        let key = serde_json::to_string(&(tool_name, canonical.as_str())).unwrap_or_else(|_| {
            format!("[{},{}]", serde_json::to_string(tool_name).unwrap_or_default(), canonical)
        });
        let mut chain = self.chain.lock().unwrap();
        let count = match chain.as_ref() {
            Some(c) if c.key == key => c.count + 1,
            _ => 1,
        };
        *chain = Some(Chain { key, count });
        if !self.config.thresholds.contains(&count) {
            return None;
        }
        let text = if count == self.config.thresholds[0] {
            GENTLE_REMINDER.to_string()
        } else {
            detailed_reminder(tool_name, count, &preview_arguments(&canonical, self.config.preview_chars))
        };
        Some(ReminderText { text, summary: format!("{tool_name} × {count}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gentle_reminder_is_verbatim() {
        assert_eq!(
            GENTLE_REMINDER,
            "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call."
        );
    }

    #[test]
    fn detailed_reminder_template_is_verbatim() {
        assert_eq!(
            detailed_reminder("bash", 5, "{}"),
            "Repeated tool call detected:\n- tool: bash\n- consecutive_calls: 5\n- arguments: {}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered."
        );
    }

    #[test]
    fn canonicalize_sorts_keys_deep() {
        assert_eq!(
            canonicalize(r#"{"b":1,"a":{"d":2,"c":3}}"#),
            canonicalize(r#"{"a":{"c":3,"d":2},"b":1}"#)
        );
        assert_eq!(canonicalize(r#"{"b":1,"a":2}"#), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn canonicalize_malformed_arguments_become_quoted_string() {
        assert_eq!(canonicalize("not json"), "\"not json\"");
        // 同一畸形串重放仍是同键
        assert_eq!(canonicalize("not json"), canonicalize("not json"));
    }

    #[test]
    fn wildcard_matches_are_anchored_with_literal_metachars() {
        assert!(wildcard_matches("web_*", "web_search"));
        assert!(wildcard_matches("*", "anything"));
        assert!(wildcard_matches("mcp_*_tool", "mcp_a_b_tool"));
        assert!(!wildcard_matches("web_*", "xweb_search"));
        // `*` → 任意序列（含路径分隔符；上游 .* 同语义）
        assert!(wildcard_matches("web_*", "web_search/x"));
        assert!(wildcard_matches("bash", "bash"));
        assert!(!wildcard_matches("bash", "bashx"));
        // 元字符按字面匹配（上游转义表语义）
        assert!(wildcard_matches("a.b", "a.b"));
        assert!(!wildcard_matches("a.b", "axb"));
    }

    #[test]
    fn preview_arguments_caps_with_marker() {
        assert_eq!(preview_arguments("{}", 500), "{}");
        let long = "x".repeat(600);
        let preview = preview_arguments(&long, 500);
        assert!(preview.starts_with(&"x".repeat(500)));
        assert!(preview.ends_with("… (+100 more chars)"));
    }

    #[test]
    fn validated_config_sorts_thresholds_and_fails_loud() {
        let ok = RepeatGuardConfig { thresholds: vec![8, 3], ..Default::default() }
            .validated()
            .unwrap();
        assert_eq!(ok.thresholds, vec![3, 8]);
        assert_eq!(
            RepeatGuardConfig { thresholds: vec![], ..Default::default() }.validated().unwrap_err(),
            "repeat-tool-reminder: `thresholds` must not be empty"
        );
        assert_eq!(
            RepeatGuardConfig { thresholds: vec![3, 1], ..Default::default() }.validated().unwrap_err(),
            "repeat-tool-reminder: invalid threshold 1 — every threshold must be an integer >= 2"
        );
        assert_eq!(
            RepeatGuardConfig { thresholds: vec![3, 3], ..Default::default() }.validated().unwrap_err(),
            "repeat-tool-reminder: `thresholds` must not contain duplicates"
        );
        assert_eq!(
            RepeatGuardConfig { preview_chars: 0, ..Default::default() }.validated().unwrap_err(),
            "repeat-tool-reminder: invalid argumentsPreviewChars 0 — must be an integer >= 1"
        );
    }

    #[test]
    fn chain_hits_gentle_then_detailed_then_silent() {
        let guard = RepeatChainGuard::with_defaults();
        for _ in 0..2 {
            assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        }
        // 第 3 次 = thresholds[0] → 温和
        let third = guard.observe("bash", r#"{"command":"ls"}"#).unwrap();
        assert_eq!(third.text, GENTLE_REMINDER);
        assert_eq!(third.summary, "bash × 3");
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        // 第 5 次 → 详述（参数全量在界内）
        let fifth = guard.observe("bash", r#"{"command":"ls"}"#).unwrap();
        assert_eq!(fifth.summary, "bash × 5");
        assert!(fifth.text.contains("- arguments: {\"command\":\"ls\"}"));
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        // 第 8 次 → 详述
        let eighth = guard.observe("bash", r#"{"command":"ls"}"#).unwrap();
        assert_eq!(eighth.summary, "bash × 8");
    }

    #[test]
    fn different_arguments_restart_the_chain() {
        let guard = RepeatChainGuard::with_defaults();
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        // 换参数 → 连击长度回到 1
        assert!(guard.observe("bash", r#"{"command":"pwd"}"#).is_none());
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        // 键序不同的同参数对象延续同一连击（规范化生效），第 3 次命中
        let hit = guard.observe("bash", r#"{"command":"ls"}"#).unwrap();
        assert_eq!(hit.summary, "bash × 3");
    }

    #[test]
    fn reset_clears_the_chain() {
        let guard = RepeatChainGuard::with_defaults();
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
        guard.reset();
        assert!(guard.observe("bash", r#"{"command":"ls"}"#).is_none());
    }

    #[test]
    fn excluded_tools_are_transparent() {
        let guard = RepeatChainGuard::new(RepeatGuardConfig {
            exclude: vec!["mcp_*".into()],
            ..Default::default()
        });
        for _ in 0..5 {
            assert!(guard.observe("mcp_x", "{}").is_none());
        }
        // 透明 = 既不计数也不重置：mcp 调用后 bash 连击延续
        assert!(guard.observe("bash", "{}").is_none());
        assert!(guard.observe("bash", "{}").is_none());
        let hit = guard.observe("bash", "{}").unwrap();
        assert_eq!(hit.summary, "bash × 3");
    }

    #[test]
    fn include_narrows_tracking() {
        let guard = RepeatChainGuard::new(RepeatGuardConfig {
            include: vec!["web_*".into()],
            ..Default::default()
        });
        assert!(guard.observe("bash", "{}").is_none());
        assert!(guard.observe("web_search", "{}").is_none());
        assert!(guard.observe("web_search", "{}").is_none());
        let hit = guard.observe("web_search", "{}").unwrap();
        assert_eq!(hit.summary, "web_search × 3");
    }
}
