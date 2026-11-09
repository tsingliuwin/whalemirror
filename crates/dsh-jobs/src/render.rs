//! 模型侧渲染（上游 `tool-jobs/src/render.ts` 的等价面）：状态行、
//! 增量正文（stdout + 单个 `[stderr]` 段）、丢字节提示、公开投影。

use crate::{JobChannel, JobChunk, JobStatus, JobView};

/// 模型安全的作业快照（上游 `PublicJobSnapshot`：去掉属主与环偏移）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicJob {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub status: JobStatus,
    /// 运行中是实时进度行，结算后是终态原因。
    pub detail: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
}

/// 状态旁的限定语：运行时取进度行，结算后取终态原因。
pub fn job_detail(job: &JobView) -> Option<String> {
    job.progress.clone().or_else(|| job.detail.clone())
}

/// 去掉属主/偏移/上限，得到模型安全投影。
pub fn public_job(job: &JobView) -> PublicJob {
    PublicJob {
        id: job.id.0.clone(),
        kind: job.kind.clone(),
        label: job.label.clone(),
        status: job.status,
        detail: job_detail(job),
        started_at: job.started_at,
        finished_at: job.finished_at,
    }
}

/// `[status: …]` 行（上游 `statusLine`）。
pub fn status_line(job: &PublicJob) -> String {
    match &job.detail {
        Some(detail) => format!("[status: {}, {detail}]", job.status.as_wire()),
        None => format!("[status: {}]", job.status.as_wire()),
    }
}

/// 消费读的正文（上游 `renderModelDelta`）：stdout 与无标签块在前，所有
/// stderr 收进一个 `[stderr]` 段；`log` 块只给观察者、不进模型面。丢字节
/// 时追加丢字节提示（列出 spill 文件，没有则 `(unavailable)`）。
pub fn render_model_delta(chunks: &[JobChunk], lossy: bool, spill_paths: &[String]) -> String {
    let visible: Vec<&JobChunk> =
        chunks.iter().filter(|c| c.channel != Some(JobChannel::Log)).collect();
    let out: String = visible
        .iter()
        .filter(|c| c.channel != Some(JobChannel::Stderr))
        .map(|c| c.text.as_str())
        .collect();
    let err: String = visible
        .iter()
        .filter(|c| c.channel == Some(JobChannel::Stderr))
        .map(|c| c.text.as_str())
        .collect();
    let separator = if !out.is_empty() && !out.ends_with('\n') { "\n" } else { "" };
    let body = if err.is_empty() {
        out.clone()
    } else {
        format!("{out}{separator}[stderr]\n{err}")
    };
    let gap_in_chunks = visible.iter().any(|c| c.gap_before);
    if !lossy && !gap_in_chunks {
        return body;
    }
    let paths = if spill_paths.is_empty() {
        "(unavailable)".to_string()
    } else {
        spill_paths.join(", ")
    };
    let notice = format!("[some output was dropped from memory; full output: {paths}]");
    let sep = if !body.is_empty() && !body.ends_with('\n') { "\n" } else { "" };
    format!("{body}{sep}{notice}")
}

/// `job_list` 的一行（上游：`<id> [<kind>] <status> — <label>`）。
pub fn list_line(job: &PublicJob) -> String {
    format!("{} [{}] {} — {}", job.id, job.kind, job.status.as_wire(), job.label)
}

/// `job_list` 的正文（空表给 `(no background jobs)`）。
pub fn render_list(jobs: &[PublicJob]) -> String {
    if jobs.is_empty() {
        "(no background jobs)".to_string()
    } else {
        jobs.iter().map(list_line).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobId;

    fn job(status: JobStatus, progress: Option<&str>, detail: Option<&str>) -> JobView {
        JobView {
            id: JobId("bash-1234abcd".into()),
            kind: "bash".into(),
            label: "sleep 5".into(),
            owner: Some(dsh_llm::SessionId::new("s1")),
            output_limit_bytes: None,
            status,
            progress: progress.map(str::to_string),
            detail: detail.map(str::to_string),
            started_at: 1,
            finished_at: None,
            output_total: 0,
            output_earliest: 0,
            spill_paths: Vec::new(),
        }
    }

    fn chunk(at: usize, text: &str, channel: Option<JobChannel>) -> JobChunk {
        JobChunk { at, text: text.into(), channel, gap_before: false }
    }

    #[test]
    fn status_line_prefers_progress_then_detail() {
        let running = public_job(&job(JobStatus::Running, Some("3/10"), Some("ignored")));
        assert_eq!(status_line(&running), "[status: running, 3/10]");
        let settled = public_job(&job(JobStatus::Completed, None, Some("exit code: 0")));
        assert_eq!(status_line(&settled), "[status: completed, exit code: 0]");
        let bare = public_job(&job(JobStatus::Running, None, None));
        assert_eq!(status_line(&bare), "[status: running]");
    }

    #[test]
    fn public_projection_drops_owner_and_offsets() {
        let p = public_job(&job(JobStatus::Killed, None, Some("user stopped it")));
        assert_eq!(p.id, "bash-1234abcd");
        assert_eq!(p.kind, "bash");
        assert_eq!(p.detail.as_deref(), Some("user stopped it"));
    }

    #[test]
    fn delta_puts_stderr_in_one_marked_section_and_skips_log() {
        let chunks = vec![
            chunk(0, "out-1\n", None),
            chunk(6, "noise\n", Some(JobChannel::Log)),
            chunk(12, "err-1\n", Some(JobChannel::Stderr)),
            chunk(18, "out-2\n", None),
        ];
        assert_eq!(render_model_delta(&chunks, false, &[]), "out-1\nout-2\n[stderr]\nerr-1\n");
        // 只有 stderr 时正文即 `[stderr]` 段
        let only_err = vec![chunk(0, "boom\n", Some(JobChannel::Stderr))];
        assert_eq!(render_model_delta(&only_err, false, &[]), "[stderr]\nboom\n");
        // log 块不进模型面
        let only_log = vec![chunk(0, "phase 1\n", Some(JobChannel::Log))];
        assert_eq!(render_model_delta(&only_log, false, &[]), "");
    }

    #[test]
    fn lossy_reads_name_the_spill_files() {
        let chunks = vec![chunk(0, "tail\n", None)];
        assert_eq!(
            render_model_delta(&chunks, true, &["C:/tmp/spill.txt".to_string()]),
            "tail\n[some output was dropped from memory; full output: C:/tmp/spill.txt]"
        );
        assert_eq!(
            render_model_delta(&chunks, true, &[]),
            "tail\n[some output was dropped from memory; full output: (unavailable)]"
        );
    }

    #[test]
    fn list_line_matches_upstream_shape() {
        let p = public_job(&job(JobStatus::Running, Some("50%"), None));
        assert_eq!(list_line(&p), "bash-1234abcd [bash] running — sleep 5");
        assert_eq!(render_list(&[]), "(no background jobs)");
    }
}
