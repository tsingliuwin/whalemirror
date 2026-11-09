//! 三个模型工具（上游 `tool-jobs` 的 `job_output` / `job_list` / `job_kill`）。
//!
//! 描述、参数与模型侧渲染逐字对齐上游；等待语义按上游：
//! - `wait: true` 阻塞到结算、超时，或被用户消息（steer）打断；
//! - 超时/被打断都返回当前状态、作业继续跑，**不取消生产侧**；
//! - 被 steer 打断时状态行补 `, user steered`；
//! - 正文以 `[status: …]` 收尾；空正文渲染 `(no new output)`。

use async_trait::async_trait;
use dsh_llm::ContentBlock;
use dsh_llm::SessionId;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

use crate::render::{public_job, render_list, render_model_delta, status_line};
use crate::{JobId, Jobs};

/// 等待配置（上游 `Config`：waitTimeoutMs / maxWaitTimeoutMs）。
#[derive(Clone, Copy, Debug)]
pub struct JobToolConfig {
    pub wait_timeout_ms: u64,
    pub max_wait_timeout_ms: u64,
}

impl Default for JobToolConfig {
    fn default() -> Self {
        Self { wait_timeout_ms: 30_000, max_wait_timeout_ms: 600_000 }
    }
}

/// 属主槽：宿主在切会话时更新，工具调用时读取（单进程宿主 = 当前会话）。
pub type OwnerCell = std::sync::Arc<std::sync::Mutex<Option<SessionId>>>;

/// 调用上下文：注册表 + 属主槽。
#[derive(Clone)]
pub struct JobCaller {
    jobs: Jobs,
    owner: OwnerCell,
}

impl JobCaller {
    pub fn new(jobs: Jobs, owner: OwnerCell) -> Self {
        Self { jobs, owner }
    }

    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// 当前属主（切会话即时生效）。
    pub fn owner(&self) -> Option<SessionId> {
        self.owner.lock().unwrap().clone()
    }
}

pub struct JobOutputTool {
    caller: JobCaller,
    config: JobToolConfig,
}

impl JobOutputTool {
    pub fn new(caller: JobCaller, config: JobToolConfig) -> Self {
        Self { caller, config }
    }
}

pub struct JobListTool {
    caller: JobCaller,
}

impl JobListTool {
    pub fn new(caller: JobCaller) -> Self {
        Self { caller }
    }
}

pub struct JobKillTool {
    caller: JobCaller,
}

impl JobKillTool {
    pub fn new(caller: JobCaller) -> Self {
        Self { caller }
    }
}

fn text_of(result: &ToolExecutionResult) -> String {
    result
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 上游 `validateJobId`：非空字符串。
fn parse_job_id(args: &serde_json::Value) -> Result<JobId, String> {
    match args.get("job_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => Ok(JobId(id.to_string())),
        _ => Err("invalid job_id: expected a non-empty string".into()),
    }
}

#[async_trait]
impl Tool for JobOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "job_output".into(),
            description: "Read a background job: output since the previous read for stream jobs, or the result of a finished final-output job.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "Job id returned by the tool that started the background work." },
                    "wait": { "type": "boolean", "description": "Block until the job finishes or the timeout expires; a timed-out wait leaves the job running. Defaults to false." },
                    "timeout_ms": { "type": "number", "description": "Max wait in milliseconds with wait: true. Defaults to and is capped by configuration." }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let id = match parse_job_id(&input.arguments) {
            Ok(id) => id,
            Err(text) => return ToolExecutionResult::error(text),
        };
        let wait = input.arguments.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut steered = false;
        if wait {
            let requested = input
                .arguments
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(self.config.wait_timeout_ms);
            let timeout = requested.min(self.config.max_wait_timeout_ms);
            match self
                .caller
                .jobs
                .wait(&id, std::time::Duration::from_millis(timeout), self.caller.owner().as_ref())
                .await
            {
                Ok(view) => {
                    // 超时：作业仍活着，按 interrupted 语义补状态行
                    if !view.status.is_terminal() {
                        steered = false;
                    }
                }
                Err(text) => return ToolExecutionResult::error(text),
            }
        }
        let read = match self.caller.jobs.read(&id, self.caller.owner().as_ref()) {
            Ok(read) => read,
            Err(text) => return ToolExecutionResult::error(text),
        };
        let delta = render_model_delta(&read.chunks, read.lossy, &read.job.spill_paths);
        let mut text = if delta.is_empty() { "(no new output)".to_string() } else { delta };
        if let Some(result) = read.result {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&result);
        }
        let mut job = public_job(&read.job);
        if steered && !read.job.status.is_terminal() {
            job.detail = Some(match job.detail {
                Some(detail) => format!("{detail}; user steered"),
                None => "user steered".to_string(),
            });
        }
        let separator = if text.ends_with('\n') { "" } else { "\n" };
        ToolExecutionResult {
            content: vec![ContentBlock::Text {
                text: format!("{text}{separator}{}", status_line(&job)),
            }],
            is_error: false,
            concludes_turn: false,
            presentation: Some(json!({ "card": "read", "job": job_snapshot_json(&job) })),
        }
    }
}

#[async_trait]
impl Tool for JobListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "job_list".into(),
            description:
                "List your background jobs (running and finished) with their ids, kinds, and statuses."
                    .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
        let jobs: Vec<_> = self
            .caller
            .jobs
            .list(self.caller.owner().as_ref())
            .iter()
            .map(public_job)
            .collect();
        ToolExecutionResult {
            content: vec![ContentBlock::Text { text: render_list(&jobs) }],
            is_error: false,
            concludes_turn: false,
            presentation: Some(json!({
                "card": "read",
                "jobs": jobs.iter().map(job_snapshot_json).collect::<Vec<_>>(),
            })),
        }
    }
}

#[async_trait]
impl Tool for JobKillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "job_kill".into(),
            description: "Request cancellation of a running background job.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "Job id returned by the tool that started the background work." },
                    "reason": { "type": "string", "description": "Optional short reason, recorded in the log and forwarded to the job." }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let id = match parse_job_id(&input.arguments) {
            Ok(id) => id,
            Err(text) => return ToolExecutionResult::error(text),
        };
        let reason = input.arguments.get("reason").and_then(|v| v.as_str());
        let outcome = match self.caller.jobs.kill(&id, self.caller.owner().as_ref(), reason) {
            Ok(outcome) => outcome,
            Err(text) => return ToolExecutionResult::error(text),
        };
        let Ok(view) = self.caller.jobs.get(&id, self.caller.owner().as_ref()) else {
            return ToolExecutionResult::error(format!("unknown job {id}"));
        };
        let job = public_job(&view);
        let text = format!("{outcome}\n{}", status_line(&job));
        ToolExecutionResult {
            content: vec![ContentBlock::Text { text }],
            is_error: false,
            concludes_turn: false,
            presentation: Some(json!({
                "card": "execute",
                "outcome": outcome,
                "job": job_snapshot_json(&job),
            })),
        }
    }
}

fn job_snapshot_json(job: &crate::render::PublicJob) -> serde_json::Value {
    json!({
        "id": job.id,
        "kind": job.kind,
        "label": job.label,
        "status": job.status.as_wire(),
        "detail": job.detail,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
    })
}

/// 便捷：从原始文本取正文（测试用）。
pub fn result_text(result: &ToolExecutionResult) -> String {
    text_of(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> JobCaller {
        JobCaller::new(
            Jobs::new(tokio::runtime::Handle::current()),
            std::sync::Arc::new(std::sync::Mutex::new(Some(SessionId::new("s1")))),
        )
    }

    fn input(name: &str, raw: &str) -> ToolExecutionInput {
        ToolExecutionInput::with_raw_arguments(
            dsh_llm::CallId("c1".into()),
            name.to_string(),
            raw.to_string(),
        )
    }

    #[tokio::test]
    async fn list_is_empty_before_any_job() {
        let tool = JobListTool::new(caller());
        let r = tool.execute(&input("job_list", "{}")).await;
        assert_eq!(result_text(&r), "(no background jobs)");
        assert!(!r.is_error);
    }

    #[tokio::test]
    async fn unknown_ids_are_tool_errors() {
        let out = JobOutputTool::new(caller(), JobToolConfig::default());
        let r = out.execute(&input("job_output", r#"{"job_id":"bash-nope"}"#)).await;
        assert!(r.is_error);
        assert!(result_text(&r).contains("unknown job bash-nope"));

        let kill = JobKillTool::new(caller());
        let r = kill.execute(&input("job_kill", r#"{"job_id":"bash-nope"}"#)).await;
        assert!(r.is_error);

        let r = out.execute(&input("job_output", "{}")).await;
        assert!(result_text(&r).contains("invalid job_id"));
    }

    #[tokio::test]
    async fn reads_render_delta_then_status_line() {
        let c = caller();
        let jobs = c.jobs().clone();
        let owner = c.owner();
        let id = jobs
            .start(crate::JobSpec {
                kind: "bash".into(),
                label: "echo hi".into(),
                owner: owner.clone(),
                tool_call_id: None,
                output_limit_bytes: None,
                run: Box::new(|writer| {
                    writer.append("hi\n", None);
                    writer.settle(crate::JobOutcome::completed_with("exit code: 0"));
                    Ok(std::sync::Arc::new(|_reason: Option<String>| {}))
                }),
            })
            .unwrap();
        let tool = JobOutputTool::new(c, JobToolConfig::default());
        let r = tool.execute(&input("job_output", &format!(r#"{{"job_id":"{id}"}}"#))).await;
        assert_eq!(result_text(&r), "hi\n[status: completed, exit code: 0]");

        // 第二次读：无新正文
        let r = tool.execute(&input("job_output", &format!(r#"{{"job_id":"{id}"}}"#))).await;
        assert_eq!(result_text(&r), "(no new output)\n[status: completed, exit code: 0]");
    }

    #[tokio::test]
    async fn kill_reports_outcome_and_snapshot() {
        let c = caller();
        let jobs = c.jobs().clone();
        let owner = c.owner();
        let id = jobs
            .start(crate::JobSpec {
                kind: "bash".into(),
                label: "sleep 5".into(),
                owner: owner.clone(),
                tool_call_id: None,
                output_limit_bytes: None,
                run: Box::new(|writer| {
                    let w = writer.clone();
                    let cancel = std::sync::Arc::new(move |_reason: Option<String>| {
                        w.settle(crate::JobOutcome::killed());
                    });
                    Ok(cancel)
                }),
            })
            .unwrap();
        let tool = JobKillTool::new(c.clone());
        let r = tool
            .execute(&input("job_kill", &format!(r#"{{"job_id":"{id}","reason":"no longer needed"}}"#)))
            .await;
        assert_eq!(result_text(&r), "requested\n[status: killed, no longer needed]");

        // 再杀一次：已结束
        let r = tool.execute(&input("job_kill", &format!(r#"{{"job_id":"{id}"}}"#))).await;
        assert!(result_text(&r).starts_with("already-finished"));

    }
}

/// 完成通知正文（上游 `background job <id> (<kind>: <label>) finished [status: …]`）。
///
/// 可达性判据同上游：完整通知（含未读输出与 result）不超过 `inline_bytes`
/// 时内联，否则只给「去读它」的指引；标签取首行且限 120 字符。
pub fn completion_notice(job: &crate::JobView, output: &str, result: Option<&str>, inline_bytes: usize) -> String {
    let label: String = job.label.lines().next().unwrap_or("").chars().take(120).collect();
    let status = crate::render::status_line(&crate::render::public_job(job));
    let head = format!(
        "background job {} ({}: {}) finished {status}. ",
        job.id, job.kind, label
    );
    let mut body = String::new();
    if !output.is_empty() {
        body.push_str("Output:\n");
        body.push_str(output);
        if !output.ends_with('\n') {
            body.push('\n');
        }
    }
    if let Some(result) = result {
        body.push_str(result);
    }
    if body.is_empty() {
        return format!("{head}No new output.");
    }
    let complete = format!("{head}{body}");
    if complete.len() <= inline_bytes {
        return complete;
    }
    format!("{head}Read its output with job_output.")
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use crate::{JobId, JobStatus, JobView};

    fn view(status: JobStatus) -> JobView {
        JobView {
            id: JobId("bash-1a2b3c4d".into()),
            kind: "bash".into(),
            label: "cargo test\nsecond line".into(),
            owner: Some(dsh_llm::SessionId::new("s1")),
            output_limit_bytes: None,
            status,
            progress: None,
            detail: Some("exit code: 0".into()),
            started_at: 1,
            finished_at: Some(2),
            output_total: 10,
            output_earliest: 0,
            spill_paths: Vec::new(),
        }
    }

    #[test]
    fn notice_inlines_short_output_and_trims_the_label() {
        let text = completion_notice(&view(JobStatus::Completed), "ok\n", None, 2048);
        assert_eq!(
            text,
            "background job bash-1a2b3c4d (bash: cargo test) finished [status: completed, exit code: 0]. Output:\nok\n"
        );
    }

    #[test]
    fn empty_output_says_no_new_output_and_result_rides_along() {
        let text = completion_notice(&view(JobStatus::Completed), "", None, 2048);
        assert!(text.ends_with("No new output."), "{text}");
        let with_result = completion_notice(&view(JobStatus::Completed), "lines\n", Some("answer"), 2048);
        assert!(with_result.contains("answer"), "{with_result}");
    }

    #[test]
    fn oversized_output_degrades_to_the_read_instruction() {
        let big = "x".repeat(3000);
        let text = completion_notice(&view(JobStatus::Completed), &big, None, 2048);
        assert!(text.ends_with("Read its output with job_output."), "{text}");
        assert!(!text.contains(&"x".repeat(10)), "输出正文未内联: {text}");
    }
}