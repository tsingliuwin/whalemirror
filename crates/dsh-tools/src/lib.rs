//! dsh-tools — the scoped tool registry and guarded execution pipeline.
//!
//! Mirrors [`packages/core/tools`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/tools):
//! a tool is a JSON-schema `parameters` + async `execute`; the registry assembles
//! model-facing `ToolSchema`s and runs the guarded execution pipeline.

use async_trait::async_trait;
use dsh_llm::{CallId, ContentBlock, Disposer, ToolSchema};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// One tool invocation ready to execute.
#[derive(Clone, Debug)]
pub struct ToolExecutionInput {
    pub call_id: CallId,
    pub name: String,
    /// Parsed JSON arguments; preserved as the raw string body when the model
    /// emitted invalid JSON (the reference keeps invalid JSON as text).
    pub arguments: Value,
}

impl ToolExecutionInput {
    /// Parse model arguments, mapping empty input to `{}` and invalid JSON to
    /// the raw string (mirrors the reference `parseArguments`).
    pub fn with_raw_arguments(call_id: CallId, name: String, raw: String) -> Self {
        let arguments = if raw.is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&raw).unwrap_or(Value::String(raw))
        };
        Self { call_id, name, arguments }
    }
}

/// 会话工作目录（web `session.header.cwd` 的共享句柄）：宿主在切换
/// 工作区/会话时更新，工具据此取执行目录与相对路径基准。None = 未设置，
/// 工具回退进程 cwd（与 web 的 `header.cwd ?? process.cwd()` 同语义）。
#[derive(Clone, Debug, Default)]
pub struct Workdir(Arc<std::sync::RwLock<Option<std::path::PathBuf>>>);

impl Workdir {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(path: impl Into<std::path::PathBuf>) -> Self {
        Self(Arc::new(std::sync::RwLock::new(Some(path.into()))))
    }

    pub fn set(&self, path: impl Into<std::path::PathBuf>) {
        *self.0.write().unwrap() = Some(path.into());
    }

    pub fn get(&self) -> Option<std::path::PathBuf> {
        self.0.read().unwrap().clone()
    }

    /// 相对路径按工作目录展开；绝对路径原样返回。
    pub fn resolve(&self, path: &std::path::Path) -> std::path::PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            match self.get() {
                Some(wd) => wd.join(path),
                None => path.to_path_buf(),
            }
        }
    }
}

/// The result of one tool invocation.
#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// When true, the turn concludes after this step (the model is not asked
    /// for another step).
    pub concludes_turn: bool,
    /// UI 元数据缝（上游工具 `output.presentationMeta` 投影等价）：随 tool/result
    /// 持久化、不进模型可见文本；形态由各工具自定（如 diff 卡的
    /// `{card:"diff", diffs:[{path, oldText, newText}]}`，FileDiff 照上游
    /// presentation.ts——oldText 为 null 表示新文件/无先前内容）。
    pub presentation: Option<serde_json::Value>,
}

impl ToolExecutionResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: false, concludes_turn: false, presentation: None }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: true, concludes_turn: false, presentation: None }
    }
}

/// A model-facing tool: declares a JSON-schema definition and executes calls.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's name, description, and JSON-schema `parameters`.
    fn definition(&self) -> ToolDefinition;

    /// Execute one invocation.
    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult;
}

/// The static part of a tool: name, description, JSON-schema parameters.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn to_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// 步骤级工具准入上下文（上游条件注册 `when: ({ agent, modelSelection })`
/// 的 rustdsh 等价子集：plan 折叠态 + 本步模型选择）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAdmission {
    pub plan_active: bool,
    pub plan_entered: bool,
    pub provider: String,
    pub model: String,
}

/// 工具激活条件：为 false 的步骤不声明、不派发该工具（上游 conditional
/// tool 语义；调用按未知工具失败）。
pub type ToolCondition = Arc<dyn Fn(&ToolAdmission) -> bool + Send + Sync>;

struct Registered {
    tool: Arc<dyn Tool>,
    condition: Option<ToolCondition>,
}

/// The scoped tool registry.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Registered>>>,
    order: Arc<RwLock<Vec<String>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its name, returning its disposer.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<Disposer, dsh_llm::LlmError> {
        self.register_inner(tool, None)
    }

    /// 以激活条件注册（上游条件工具）：条件不满足的步骤不声明、不派发。
    pub fn register_when(
        &self,
        tool: Arc<dyn Tool>,
        condition: ToolCondition,
    ) -> Result<Disposer, dsh_llm::LlmError> {
        self.register_inner(tool, Some(condition))
    }

    fn register_inner(
        &self,
        tool: Arc<dyn Tool>,
        condition: Option<ToolCondition>,
    ) -> Result<Disposer, dsh_llm::LlmError> {
        let name = tool.definition().name.clone();
        {
            let map = self.tools.read().unwrap();
            if map.contains_key(&name) {
                return Err(dsh_llm::LlmError::new(
                    format!("tool \"{name}\" is already registered"),
                    "DUPLICATE_TOOL",
                ));
            }
        }
        self.tools
            .write()
            .unwrap()
            .insert(name.clone(), Registered { tool, condition });
        self.order.write().unwrap().push(name.clone());

        let tools = Arc::clone(&self.tools);
        let order = Arc::clone(&self.order);
        Ok(Box::new(move || {
            tools.write().unwrap().remove(&name);
            order.write().unwrap().retain(|n| n != &name);
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .unwrap()
            .get(name)
            .map(|registered| Arc::clone(&registered.tool))
    }

    /// Every registered tool in registration order.
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order
            .iter()
            .filter_map(|n| map.get(n).map(|r| Arc::clone(&r.tool)))
            .collect()
    }

    /// 本步准入的工具（注册序；无条件工具恒在，条件工具按上下文过滤）。
    pub fn list_admitted(&self, admission: &ToolAdmission) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order
            .iter()
            .filter_map(|n| {
                let registered = map.get(n)?;
                let admitted = registered
                    .condition
                    .as_ref()
                    .map(|c| c(admission))
                    .unwrap_or(true);
                admitted.then(|| Arc::clone(&registered.tool))
            })
            .collect()
    }

    /// 单个工具的准入判定（未注册 = false；条件缺省 = true）。
    pub fn admitted(&self, name: &str, admission: &ToolAdmission) -> bool {
        let map = self.tools.read().unwrap();
        let Some(registered) = map.get(name) else {
            return false;
        };
        registered
            .condition
            .as_ref()
            .map(|c| c(admission))
            .unwrap_or(true)
    }

    /// The model-facing schemas for every registered tool.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.list().iter().map(|t| t.definition().to_schema()).collect()
    }

    /// Run the guarded execute pipeline for one call.
    pub async fn execute(&self, input: &ToolExecutionInput) -> Option<ToolExecutionResult> {
        let tool = self.get(&input.name)?;
        Some(tool.execute(input).await)
    }
}
// ---- 行 diff（上游 DiffBlock 语义的纯函数面）----

/// 一行补丁的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    /// 上下文行（中性显示，不计入增删统计——上游 review 修正语义）。
    Context,
    /// 删除行（旧文件独有）。
    Del,
    /// 新增行（新文件独有）。
    Add,
}

/// 一行补丁：种类与文本（不含行尾换行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// 一个 hunk：一处改动及其两侧最多 3 行上下文；远距改动分成多个 hunk。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub lines: Vec<DiffLine>,
}

/// 增删统计（上下文行不计入——上游 diffTotals 同语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DiffTotals {
    pub added: u64,
    pub deleted: u64,
}

/// 旧文本按行切分（末尾换行为终止符——与上游一致：仅末尾换行有无不同不展示）。
fn split_lines(text: &str) -> Vec<&str> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    if trimmed.is_empty() { Vec::new() } else { trimmed.split('\n').collect() }
}

/// 行 LCS DP 表（O(n·m)；上游 note 接受大替换的同步计算成本）。
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<u32>> {
    let mut t = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    t
}

fn op_lines(kind: DiffLineKind, lines: &[&str], out: &mut Vec<DiffLine>) {
    out.extend(lines.iter().map(|l| DiffLine { kind, text: (*l).to_string() }));
}

/// 生成行补丁：LCS 对齐后，把相邻的改动行聚为一个 hunk，两侧保留至多
/// [`CONTEXT_LINES`] 行上下文；相距超过 `2 * CONTEXT_LINES` 的改动分属不同 hunk。
pub fn line_patch(old: &str, new: &str) -> Vec<DiffHunk> {
    pub const CONTEXT_LINES: usize = 3;
    let a = split_lines(old);
    let b = split_lines(new);
    let t = lcs_table(&a, &b);
    // 走 LCS 得到全量操作序列（context/del/add）
    let mut ops: Vec<DiffLine> = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            op_lines(DiffLineKind::Context, &a[i..i + 1], &mut ops);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            op_lines(DiffLineKind::Del, &a[i..i + 1], &mut ops);
            i += 1;
        } else {
            op_lines(DiffLineKind::Add, &b[j..j + 1], &mut ops);
            j += 1;
        }
    }
    op_lines(DiffLineKind::Del, &a[i..], &mut ops);
    op_lines(DiffLineKind::Add, &b[j..], &mut ops);

    // 聚 hunk：改动为中心，两侧扩 CONTEXT_LINES 上下文；相邻改动区重叠则并 hunk
    let is_change = |l: &DiffLine| l.kind != DiffLineKind::Context;
    let n = ops.len();
    let mut marked = vec![false; n];
    for k in 0..n {
        if is_change(&ops[k]) {
            for m in k.saturating_sub(CONTEXT_LINES)..=(k + CONTEXT_LINES).min(n - 1) {
                marked[m] = true;
            }
        }
    }
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut k = 0usize;
    while k < n {
        if !marked[k] {
            k += 1;
            continue;
        }
        let start = k;
        while k < n && (marked[k] || (k + 1 < n && marked[k + 1] && is_change(&ops[k + 1])))
        {
            // 连续标记区吞并；紧邻下一行是改动且已标记也吞并
            k += 1;
        }
        hunks.push(DiffHunk { lines: ops[start..k].to_vec() });
    }
    hunks
}

/// 增删统计：非上下文行计数（上游 diffTotals 同语义）。
pub fn diff_totals(old: &str, new: &str) -> DiffTotals {
    let a = split_lines(old);
    let b = split_lines(new);
    let t = lcs_table(&a, &b);
    let lcs = t[0][0] as u64;
    DiffTotals {
        deleted: a.len() as u64 - lcs,
        added: b.len() as u64 - lcs,
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    #[test]
    fn insertion_deletion_replacement_totals() {
        assert_eq!(diff_totals("", "a
b"), DiffTotals { added: 2, deleted: 0 });
        assert_eq!(diff_totals("a
b", ""), DiffTotals { added: 0, deleted: 2 });
        assert_eq!(diff_totals("a
b", "a
c"), DiffTotals { added: 1, deleted: 1 });
        assert_eq!(diff_totals("a
b", "a
b"), DiffTotals::default());
        // 仅末尾换行有无不同不展示（上游 content-line 规则）
        assert_eq!(diff_totals("a
", "a"), DiffTotals::default());
    }

    #[test]
    fn hunks_carry_context_and_split_at_distance() {
        // 上下文 3 行：改动两侧保留 ≤3 行中性上下文
        let old = (1..=10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("
");
        let new = old.replacen("l5", "L5", 1);
        let hunks = line_patch(&old, &new);
        assert_eq!(hunks.len(), 1, "close change: single hunk");
        let (ctx, del, add) = hunks[0]
            .lines
            .iter()
            .fold((0usize, 0usize, 0usize), |(c, d, a), l| match l.kind {
                DiffLineKind::Context => (c + 1, d, a),
                DiffLineKind::Del => (c, d + 1, a),
                DiffLineKind::Add => (c, d, a + 1),
            });
        assert_eq!((ctx, del, add), (6, 1, 1), "3 context on each side + del/add pair");
        // 相距 >6 行的两处改动 → 两个 hunk
        let old2 = (1..=20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("
");
        let new2 = old2.replacen("l2", "L2", 1).replacen("l18", "L18", 1);
        let hunks2 = line_patch(&old2, &new2);
        assert_eq!(hunks2.len(), 2, "distant changes split");
        // 上下文行不计入统计
        assert_eq!(diff_totals(&old, &new), DiffTotals { added: 1, deleted: 1 });
    }

    #[test]
    fn new_file_whole_content_is_added() {
        let hunks = line_patch("", "x
y");
        assert!(hunks[0].lines.iter().all(|l| l.kind == DiffLineKind::Add));
    }
}


/// 审阅决定（上游 exit_plan_mode 输出 {approved}）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanReviewDecision {
    /// 同意执行（离开 plan 模式，下一步开始执行）。
    Approve,
    /// 要求修改（保持 plan 模式；feedback 原文回给模型——空串=无反馈）。
    RequestChanges { feedback: String },
    /// 用户自行回复（dismissed：结束本轮不打扰，{approved:false}；
    /// 上游 ASK_CANCELLED → concludeTurn 同语义）。
    Dismissed,
}

impl PlanReviewDecision {
    /// approved 输出值。
    pub fn approved(&self) -> bool {
        matches!(self, PlanReviewDecision::Approve)
    }
}

/// 审阅回调（宿主注入：弹审阅面板等用户决定；阻塞至有答案）。上游
/// `userQuestions.ask` 的单进程等价——交互面归宿主。
pub type PlanReviewHook = std::sync::Arc<
    dyn Fn(&str, &dsh_llm::CallId) -> PlanReviewDecision + Send + Sync,
>;

/// `exit_plan_mode`（上游 plan-mode 包同名工具的审阅流实装）：提交校验
/// 后经审阅回调请求用户决定——Approve 输出 {approved:true}（离开 plan
/// 模式）；RequestChanges 以错误结果回模型（文案同上游 keep planning
/// 反馈路径）；Dismissed 结束本轮（concludes_turn，不打扰模型等待）。
pub struct ExitPlanModeTool {
    review: RwLock<Option<PlanReviewHook>>,
    /// plan 激活态共享句柄（宿主在 agent 构造后接线）：离开 plan 模式后
    /// 工具仍保持声明（前缀缓存），但调用以「unavailable outside plan
    /// mode」失败（上游渐进披露语义）。None = 无校验（数据面测试缺省）。
    plan_active: RwLock<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
}

impl ExitPlanModeTool {
    pub fn new() -> Self {
        Self { review: RwLock::new(None), plan_active: RwLock::new(None) }
    }

    /// 注入审阅回调（宿主 UI；无回调时自动同意——数据面测试缺省）。
    pub fn set_review_hook(&self, hook: PlanReviewHook) {
        *self.review.write().unwrap() = Some(hook);
    }

    /// 注入 plan 激活态句柄（宿主在 agent 构造后调用）。
    pub fn set_plan_active(&self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        *self.plan_active.write().unwrap() = Some(flag);
    }
}

impl Default for ExitPlanModeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exit_plan_mode".to_string(),
            description: "Present the completed plan for user review. The complete plan, as markdown, starting with a # heading that names it.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The complete plan, as markdown, starting with a # heading that names it."
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        // 渐进披露：声明保留（前缀缓存友好），但 plan 模式未激活（已
        // 离开）时调用失败——上游「unavailable outside plan mode」
        if let Some(flag) = self.plan_active.read().unwrap().as_ref()
            && !flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return ToolExecutionResult::error("exit_plan_mode is unavailable outside plan mode");
        }
        let plan = input
            .arguments
            .get("plan")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());
        let Some(plan) = plan else {
            return ToolExecutionResult::error("exit_plan_mode requires a plan argument");
        };
        // 上游标题校验：非空 markdown 且以 # 标题开头
        if !plan.trim_start().starts_with('#') || plan.trim().len() < 3 {
            return ToolExecutionResult::error(
                "exit_plan_mode requires a non-empty markdown plan starting with a # heading",
            );
        }
        // 审阅流（上游 userQuestions.ask）：回调在场等用户决定；
        // 无回调（数据面/测试）自动同意
        let decision = match self.review.read().unwrap().clone() {
            Some(hook) => hook(&plan, &input.call_id),
            None => PlanReviewDecision::Approve,
        };
        match decision {
            PlanReviewDecision::Approve => ToolExecutionResult::text(
                "Plan approved — plan mode exited; carry out the plan starting with your next step.",
            ),
            PlanReviewDecision::RequestChanges { feedback } if feedback.is_empty() => {
                ToolExecutionResult::error(
                    "The user chose to keep planning; revise the plan and present it again.",
                )
            }
            PlanReviewDecision::RequestChanges { feedback } => ToolExecutionResult::error(format!(
                "The user chose to keep planning; their feedback: {feedback}"
            )),
            // 上游 dismissed：结束本轮不打扰（concludeTurn）
            PlanReviewDecision::Dismissed => ToolExecutionResult {
                concludes_turn: true,
                ..ToolExecutionResult::text(
                    "The user dismissed the plan review to reply in their own words; plan mode remains active.",
                )
            },
        }
    }
}


#[cfg(test)]
mod exit_plan_tests {
    use super::*;

    fn run(plan: &str) -> ToolExecutionResult {
        run_with(plan, PlanReviewDecision::Approve)
    }

    fn run_with(plan: &str, decision: PlanReviewDecision) -> ToolExecutionResult {
        let tool = ExitPlanModeTool::new();
        tool.set_review_hook(std::sync::Arc::new(move |_plan: &str, _call: &CallId| {
            decision.clone()
        }));
        let input = ToolExecutionInput::with_raw_arguments(
            CallId("c1".to_string()),
            "exit_plan_mode".to_string(),
            serde_json::json!({"plan": plan}).to_string(),
        );
        futures::executor::block_on(tool.execute(&input))
    }

    fn text_of(r: &ToolExecutionResult) -> String {
        r.content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn rejects_plan_without_hash_heading() {
        let r = run("just some notes");
        assert!(r.is_error);
        assert!(text_of(&r).contains("# heading"));
    }

    #[test]
    fn accepts_hash_headed_plan() {
        let r = run("# My Plan

step one");
        assert!(!r.is_error);
        assert!(text_of(&r).contains("Plan approved"));
    }

    #[test]
    fn approve_returns_carried_out_text() {
        let r = run_with("# Plan
step", PlanReviewDecision::Approve);
        assert!(!r.is_error);
        assert!(!r.concludes_turn);
        assert!(text_of(&r).contains("Plan approved"));
    }

    #[test]
    fn request_changes_returns_error_with_feedback() {
        let r = run_with(
            "# Plan
step",
            PlanReviewDecision::RequestChanges { feedback: "make it smaller".into() },
        );
        assert!(r.is_error);
        assert!(!r.concludes_turn);
        let text = text_of(&r);
        assert!(text.contains("keep planning"), "{text}");
        assert!(text.contains("make it smaller"), "{text}");
        let r2 = run_with(
            "# Plan
step",
            PlanReviewDecision::RequestChanges { feedback: String::new() },
        );
        let text2 = text_of(&r2);
        assert!(text2.contains("revise the plan and present it again"), "{text2}");
        assert!(!text2.contains("their feedback"), "{text2}");
    }

    #[test]
    fn dismissed_concludes_turn_without_error() {
        let r = run_with("# Plan
step", PlanReviewDecision::Dismissed);
        assert!(!r.is_error);
        assert!(r.concludes_turn, "dismissed ends the turn (upstream concludeTurn)");
        let text = text_of(&r);
        assert!(text.contains("dismissed the plan review"), "{text}");
        assert!(text.contains("plan mode remains active"), "{text}");
    }
}

#[cfg(test)]
mod registry_admission_tests {
    use super::*;

    struct NamedTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: "test".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
            ToolExecutionResult::text("ok")
        }
    }

    fn names(tools: Vec<Arc<dyn Tool>>) -> Vec<String> {
        tools.iter().map(|t| t.definition().name.clone()).collect()
    }

    #[test]
    fn unconditional_tools_are_always_admitted() {
        let registry = ToolRegistry::new();
        let _a = registry.register(Arc::new(NamedTool("always"))).unwrap();
        let ctx = ToolAdmission::default();
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["always"]);
        assert!(registry.admitted("always", &ctx));
        assert!(registry.schemas().iter().any(|s| s.name == "always"));
    }

    #[test]
    fn conditional_tools_follow_the_admission_context() {
        let registry = ToolRegistry::new();
        let _plain = registry.register(Arc::new(NamedTool("plain"))).unwrap();
        let _gated = registry
            .register_when(
                Arc::new(NamedTool("gated")),
                Arc::new(|ctx: &ToolAdmission| ctx.plan_active),
            )
            .unwrap();
        let _model_gated = registry
            .register_when(
                Arc::new(NamedTool("model-gated")),
                Arc::new(|ctx: &ToolAdmission| ctx.model == "pro"),
            )
            .unwrap();

        // 默认上下文：条件工具缺席
        let ctx = ToolAdmission::default();
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain"]);
        assert!(!registry.admitted("gated", &ctx));
        assert!(!registry.admitted("model-gated", &ctx));
        // 未注册的名字不是准入
        assert!(!registry.admitted("missing", &ctx));

        // plan 激活：plan 条件工具入场（entered 单独不满足 active 条件）
        let ctx = ToolAdmission { plan_active: true, ..Default::default() };
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain", "gated"]);
        let ctx = ToolAdmission { plan_entered: true, ..Default::default() };
        assert!(!registry.admitted("gated", &ctx), "context 只携带事实，条件自行判断");

        // 路由条件：模型名来自上下文
        let ctx = ToolAdmission { model: "pro".into(), provider: "p".into(), ..Default::default() };
        assert!(registry.admitted("model-gated", &ctx));
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain", "model-gated"]);
    }

    #[test]
    fn disposer_removes_conditional_registrations_too() {
        let registry = ToolRegistry::new();
        let disposer = registry
            .register_when(
                Arc::new(NamedTool("temp")),
                Arc::new(|ctx: &ToolAdmission| ctx.plan_active),
            )
            .unwrap();
        let ctx = ToolAdmission { plan_active: true, ..Default::default() };
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["temp"]);
        disposer();
        assert!(registry.list_admitted(&ctx).is_empty());
        assert!(registry.get("temp").is_none());
    }
}
