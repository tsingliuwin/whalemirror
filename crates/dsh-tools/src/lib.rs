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

/// The scoped tool registry.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    order: Arc<RwLock<Vec<String>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its name, returning its disposer.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<Disposer, dsh_llm::LlmError> {
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
        self.tools.write().unwrap().insert(name.clone(), tool);
        self.order.write().unwrap().push(name.clone());

        let tools = Arc::clone(&self.tools);
        let order = Arc::clone(&self.order);
        Ok(Box::new(move || {
            tools.write().unwrap().remove(&name);
            order.write().unwrap().retain(|n| n != &name);
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().unwrap().get(name).cloned()
    }

    /// Every registered tool in registration order.
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order.iter().filter_map(|n| map.get(n).cloned()).collect()
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
