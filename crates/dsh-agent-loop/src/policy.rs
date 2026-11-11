//! 运行时策略语境（上游 `sandbox:policy` / `approval:policy` prompt 语境与
//! 审批策略切换通知的 rustdsh 等价面）。
//!
//! 上游语境每次请求从投影读；rustdsh 用共享句柄同语义——`set_session` 折叠
//! 恢复，宿主运行时切换（权限预设）显式更新，下一次请求组装生效。

use std::path::Path;
use std::sync::{Arc, RwLock};

/// 无事件会话的旋钮回落预设（上游 catalog defaultPreset；宿主设置默认同名）。
pub const DEFAULT_PRESET: &str = "workspace-write";

/// `read-only` 档模型可见语句（上游 `renderPolicyContext`，逐字）。
const READ_ONLY_SENTENCE: &str = "Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.";

/// `danger-full-access` 档模型可见语句（上游 `renderPolicyContext`，逐字）。
const DANGER_FULL_ACCESS_SENTENCE: &str = "Current DSH file policy: danger-full-access. The DSH file sandbox does not restrict file modifications by available operations.";

/// `never` 策略模型可见语句（上游 `NEVER_SENTENCE`，逐字）。
const NEVER_SENTENCE: &str = "Approval prompts are disabled in this session: actions that require approval are rejected automatically — do not request sandbox escalation (do not set `sandbox_permissions`).";

/// `ask` 策略模型可见语句（上游 `ASK_SENTENCE`，逐字）。
const ASK_SENTENCE: &str = "Approval prompts are enabled in this session: operations that require approval may ask through the configured answerers; without an available answerer, the request fails closed.";

/// 内置权限预设的 (sandbox/mode, approval/policy) 列（上游部署目录三行；
/// 未知 key 回落 workspace-write——宿主 UI 表 `PERMISSION_PRESETS` 的
/// 旋钮列与此同源）。
pub fn preset_knobs(preset: &str) -> (&'static str, &'static str) {
    match preset {
        "read-only" => ("read-only", "ask"),
        "danger-full-access" => ("danger-full-access", "never"),
        // workspace-write 与未知 key 同路（上游目录默认行）
        _ => ("workspace-write", "ask"),
    }
}

/// 策略切换通知里的策略名（上游 `POLICY_NOTICE_NAMES`，逐字；未知原样透传）。
fn policy_notice_name(policy: &str) -> &str {
    match policy {
        "ask" => "ask the user for approval",
        "never" => "never ask the user for approval",
        other => other,
    }
}

/// 审批策略切换通知正文（上游 `ApprovalService.setPolicy` inject 模板，逐字）。
pub fn policy_change_notice_text(previous: &str, policy: &str) -> String {
    format!(
        "The user changed the approval policy from \"{}\" to \"{}\".",
        policy_notice_name(previous),
        policy_notice_name(policy),
    )
}

/// workspace-write 的附加目录句（上游 extra：空表同 `undefined` = 无 extra；
/// 目录字面量走 JSON 字符串转义——上游 `JSON.stringify(additional)`）。
fn additional_roots_sentence(additional: &[String]) -> String {
    if additional.is_empty() {
        return String::new();
    }
    let items: Vec<String> = additional
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_else(|_| format!("\"{r}\"")))
        .collect();
    format!(" Additional writable directories: [{}].", items.join(","))
}

/// JSON 字符串字面量（上游 `JSON.stringify` 的标量形；反斜杠等按 JSON 转义）。
fn json_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

/// 沙箱档模型可见语句（上游 `renderPolicyContext` 逐字；未知档 = None 不硬造）。
/// 工作区根缺省回落进程 cwd（上游 configured root 回退同语义）。
pub fn render_sandbox_policy(
    mode: &str,
    workspace_root: Option<&Path>,
    additional_roots: &[String],
) -> Option<String> {
    match mode {
        "read-only" => Some(READ_ONLY_SENTENCE.into()),
        "workspace-write" => {
            let root = workspace_root
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            Some(format!(
                "Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: {}.{extra} Some platform temporary areas may also be writable.",
                json_scalar(&root),
                extra = additional_roots_sentence(additional_roots),
            ))
        }
        "danger-full-access" => Some(DANGER_FULL_ACCESS_SENTENCE.into()),
        _ => None,
    }
}

/// 审批策略模型可见语句（上游 `approval:policy` 语境逐字；未知 = None）。
pub fn render_approval_policy(policy: &str) -> Option<String> {
    match policy {
        "ask" => Some(ASK_SENTENCE.into()),
        "never" => Some(NEVER_SENTENCE.into()),
        _ => None,
    }
}

/// 运行时策略观察句柄：宿主更新，请求组装读（上游语境读投影的同语义；
/// Arc 内厅——克隆共享同一观察面）。
#[derive(Clone, Debug, Default)]
pub struct PolicyContext {
    sandbox_mode: Arc<RwLock<Option<String>>>,
    additional_roots: Arc<RwLock<Vec<String>>>,
    approval_policy: Arc<RwLock<Option<String>>>,
}

impl PolicyContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// 更新沙箱档与附加可写目录（web `sandbox/mode` 投影 +
    /// `additionalWritableRoots`；rustdsh 部署组合无附加目录，恒空表）。
    pub fn set_sandbox(&self, mode: &str, additional_roots: Vec<String>) {
        *self.sandbox_mode.write().unwrap() = Some(mode.to_string());
        *self.additional_roots.write().unwrap() = additional_roots;
    }

    /// 更新审批策略（web `approval/policy` 投影：ask | never）。
    pub fn set_approval_policy(&self, policy: &str) {
        *self.approval_policy.write().unwrap() = Some(policy.to_string());
    }

    /// 沙箱档语句（未接线/未知档 = None，不硬造）。
    pub fn sandbox_sentence(&self, workspace_root: Option<&Path>) -> Option<String> {
        let mode = self.sandbox_mode.read().unwrap().clone()?;
        let roots = self.additional_roots.read().unwrap().clone();
        render_sandbox_policy(&mode, workspace_root, &roots)
    }

    /// 审批策略语句（未接线 = None）。
    pub fn approval_sentence(&self) -> Option<String> {
        render_approval_policy(self.approval_policy.read().unwrap().as_deref()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_sentence_is_verbatim() {
        assert_eq!(
            render_sandbox_policy("read-only", None, &[]).unwrap(),
            "Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns."
        );
    }

    #[test]
    fn danger_full_access_sentence_is_verbatim() {
        assert_eq!(
            render_sandbox_policy("danger-full-access", None, &[]).unwrap(),
            "Current DSH file policy: danger-full-access. The DSH file sandbox does not restrict file modifications by available operations."
        );
    }

    #[test]
    fn workspace_write_sentence_uses_json_root_and_omits_empty_extra() {
        let sentence = render_sandbox_policy(
            "workspace-write",
            Some(Path::new("E:\\rustproject\\rustdsh")),
            &[],
        )
        .unwrap();
        assert_eq!(
            sentence,
            "Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: \"E:\\\\rustproject\\\\rustdsh\". Some platform temporary areas may also be writable."
        );
    }

    #[test]
    fn workspace_write_additional_roots_render_json_array() {
        let sentence = render_sandbox_policy(
            "workspace-write",
            Some(Path::new("/ws")),
            &["/tmp/a".into(), "/tmp/b\"x".into()],
        )
        .unwrap();
        assert!(sentence.contains(". Additional writable directories: [\"/tmp/a\",\"/tmp/b\\\"x\"]. Some platform temporary areas may also be writable."));
    }

    #[test]
    fn unknown_sandbox_mode_yields_none() {
        assert_eq!(render_sandbox_policy("banana", None, &[]), None);
    }

    #[test]
    fn approval_sentences_are_verbatim() {
        assert_eq!(
            render_approval_policy("ask").unwrap(),
            "Approval prompts are enabled in this session: operations that require approval may ask through the configured answerers; without an available answerer, the request fails closed."
        );
        assert_eq!(
            render_approval_policy("never").unwrap(),
            "Approval prompts are disabled in this session: actions that require approval are rejected automatically — do not request sandbox escalation (do not set `sandbox_permissions`)."
        );
        assert_eq!(render_approval_policy("banana"), None);
    }

    #[test]
    fn preset_knobs_match_upstream_catalog() {
        assert_eq!(preset_knobs("read-only"), ("read-only", "ask"));
        assert_eq!(preset_knobs("workspace-write"), ("workspace-write", "ask"));
        assert_eq!(preset_knobs("danger-full-access"), ("danger-full-access", "never"));
        assert_eq!(preset_knobs("banana"), ("workspace-write", "ask"));
        assert_eq!(preset_knobs(DEFAULT_PRESET), ("workspace-write", "ask"));
    }

    #[test]
    fn policy_change_notice_uses_notice_names() {
        assert_eq!(
            policy_change_notice_text("ask", "never"),
            "The user changed the approval policy from \"ask the user for approval\" to \"never ask the user for approval\"."
        );
        assert_eq!(
            policy_change_notice_text("never", "ask"),
            "The user changed the approval policy from \"never ask the user for approval\" to \"ask the user for approval\"."
        );
    }

    #[test]
    fn policy_context_updates_share_across_clones() {
        let ctx = PolicyContext::new();
        let clone = ctx.clone();
        ctx.set_sandbox("read-only", Vec::new());
        ctx.set_approval_policy("never");
        assert_eq!(
            clone.sandbox_sentence(None).as_deref(),
            Some(READ_ONLY_SENTENCE)
        );
        assert_eq!(
            clone.approval_sentence().as_deref(),
            Some(NEVER_SENTENCE)
        );
        // 未接线 = None
        let fresh = PolicyContext::new();
        assert_eq!(fresh.sandbox_sentence(None), None);
        assert_eq!(fresh.approval_sentence(), None);
    }
}
