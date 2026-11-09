//! dsh-jobs — 后台作业运行时（上游 `packages/jobs/*` 的等价面）。
//!
//! 三个包合成一个 crate：
//! - `jobs`：类型与投影词汇（`JobId` / `JobStatus` / `JobChannel` /
//!   `JobChunk` / `JobView`）；
//! - `jobs-local`：进程内注册表 + 输出环（head 淘汰、绝对偏移、lossy）；
//! - `tool-jobs`：`job_output` / `job_list` / `job_kill` 三个模型工具与
//!   模型侧渲染（`[status: …]` 行、`[stderr]` 段、丢字节提示）。
//!
//! 语义要点（与上游逐条对齐，偏差见各处注释）：
//! - 偏移是**绝对 UTF-8 字节**，淘汰只推进 `earliest`，已分配的 `at` 永不移动；
//! - 读是**消费式**（模型游标推进）与**非消费式**（观察者 `read_at`）两种；
//! - 结算后流结束；终态读会把环收紧到「结算保留量」，但不低于模型未读字节；
//! - `result`（子代理答案、工作流结果）只随**结算后第一次读**交付一次；
//! - `kill` 只请求取消，作业在真正停下后才落 `killed`；`awaited` 表示结算
//!   释放了活着的等待者（完成通知据此去重）。

pub mod render;
pub mod tools;

pub use tools::{JobCaller, JobListTool, JobKillTool, JobOutputTool, JobToolConfig, OwnerCell};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dsh_llm::SessionId;
use serde::{Deserialize, Serialize};

/// 运行中的保留上限（上游 `retainBytes` 默认 262144）。
pub const DEFAULT_RETAIN_BYTES: usize = 262_144;
/// 结算后的保留上限（上游 `settledRetainBytes` 默认 16384）。
pub const DEFAULT_SETTLED_RETAIN_BYTES: usize = 16_384;

/// 作业 id（上游 `<kind>-<short-uuid>`）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub String);

impl JobId {
    /// 由 kind 与前 8 位 uuid 组成（上游 `JobId` 品牌的生成形态）。
    pub fn generate(kind: &str) -> Self {
        let uuid = uuid::Uuid::new_v4();
        let short: String = uuid.to_string().chars().take(8).collect();
        Self(format!("{kind}-{short}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 作业生命周期（上游 `JobStatus`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

impl JobStatus {
    /// wire 形（tool 输出 schema 的 enum 取值）。
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Killed => "killed",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }
}

/// 输出流标签（上游 `JobChannel`）：`log` 只给观察者，不进模型面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobChannel {
    Stdout,
    Stderr,
    Log,
}

impl JobChannel {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Log => "log",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            "log" => Some(Self::Log),
            _ => None,
        }
    }
}

/// 一个输出块：绝对偏移 + 文本 + 流标签 + 缺口标记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobChunk {
    /// 块首字节的绝对偏移（一旦分配永不移动）。
    pub at: usize,
    pub text: String,
    pub channel: Option<JobChannel>,
    /// 此块之前的字节已丢失（生产侧缺口或淘汰）。
    pub gap_before: bool,
}

/// 只读作业投影（上游 `JobView`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobView {
    pub id: JobId,
    pub kind: String,
    pub label: String,
    pub owner: Option<SessionId>,
    pub output_limit_bytes: Option<usize>,
    pub status: JobStatus,
    pub progress: Option<String>,
    pub detail: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    /// 环坐标：`total` = 下一个块的起始偏移，`earliest` = 最老保留字节。
    pub output_total: usize,
    pub output_earliest: usize,
    pub spill_paths: Vec<String>,
}

/// 结算结局（生产侧声明）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOutcome {
    pub status: JobStatus,
    pub detail: Option<String>,
    /// 非流式结果（子代理答复/工作流结果）：随结算后第一次读交付一次。
    pub result: Option<String>,
}

impl JobOutcome {
    pub fn completed() -> Self {
        Self { status: JobStatus::Completed, detail: None, result: None }
    }

    pub fn completed_with(detail: impl Into<String>) -> Self {
        Self { status: JobStatus::Completed, detail: Some(detail.into()), result: None }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self { status: JobStatus::Failed, detail: Some(detail.into()), result: None }
    }

    pub fn killed() -> Self {
        Self { status: JobStatus::Killed, detail: None, result: None }
    }

    pub fn with_result(mut self, result: impl Into<String>) -> Self {
        self.result = Some(result.into());
        self
    }
}

/// 结算成因（上游 `JobSettleCause`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobSettleCause {
    Producer,
    Kill,
    Teardown,
}

impl JobSettleCause {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Producer => "producer",
            Self::Kill => "kill",
            Self::Teardown => "teardown",
        }
    }
}

// ---- 输出环 ----

struct RingChunk {
    at: usize,
    text: String,
    bytes: usize,
    channel: Option<JobChannel>,
    gap_before: bool,
}

/// UTF-8 安全尾部截断（上游 `utf8Tail`：游标越过续字节，绝不切在码点中间）。
pub fn utf8_tail(text: &str, max_bytes: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return (text.to_string(), bytes.len());
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    // 从 start 起必然是码点起点：往回找边界
    let mut s = start;
    while s > 0 && (bytes[s] & 0xC0) == 0x80 {
        s -= 1;
    }
    (text[s..].to_string(), bytes.len() - s)
}

/// 一个作业的输出环（上游 `OutputRing`）：绝对偏移、头部淘汰、任意偏移非消费读。
#[derive(Default)]
pub struct OutputRing {
    chunks: Vec<RingChunk>,
    retained_bytes: usize,
    pub total: usize,
    pub earliest: usize,
}

impl OutputRing {
    /// 追加一块并按上限裁头；空块返回 false（不唤醒观察者）。
    pub fn append(&mut self, text: &str, channel: Option<JobChannel>, gap_before: bool, cap: usize) -> bool {
        if text.is_empty() {
            return false;
        }
        let bytes = text.len();
        self.chunks.push(RingChunk {
            at: self.total,
            text: text.to_string(),
            bytes,
            channel,
            gap_before,
        });
        self.total += bytes;
        self.retained_bytes += bytes;
        self.trim(cap);
        true
    }

    /// 裁到 `cap`：整块淘汰；单块超限时只留 UTF-8 安全尾部并打缺口标记。
    pub fn trim(&mut self, cap: usize) {
        while self.retained_bytes > cap && self.chunks.len() > 1 {
            let dropped = self.chunks.remove(0);
            self.retained_bytes -= dropped.bytes;
        }
        if self.chunks.len() == 1 && self.chunks[0].bytes > cap {
            let chunk = &mut self.chunks[0];
            let (tail, tail_bytes) = utf8_tail(&chunk.text, cap);
            chunk.at += chunk.bytes - tail_bytes;
            chunk.text = tail;
            chunk.bytes = tail_bytes;
            chunk.gap_before = true;
            self.retained_bytes = tail_bytes;
        }
        self.earliest = self.chunks.first().map(|c| c.at).unwrap_or(self.total);
    }

    /// 保留块中与 `[from, total)` 重叠的部分（非消费读）。
    pub fn read_from(&self, from: usize) -> (Vec<JobChunk>, bool) {
        let mut out = Vec::new();
        for chunk in &self.chunks {
            if chunk.at + chunk.bytes <= from {
                continue;
            }
            out.push(JobChunk {
                at: chunk.at,
                text: chunk.text.clone(),
                channel: chunk.channel,
                gap_before: chunk.gap_before,
            });
        }
        (out, from < self.earliest)
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

// ---- 注册表 ----

/// 生产侧控制面：注册表用它取消工作。
pub type CancelHook = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// 生产者声明（上游 `JobSpec` 的 rustdsh 子集）。
pub struct JobSpec {
    pub kind: String,
    pub label: String,
    pub owner: Option<SessionId>,
    pub tool_call_id: Option<String>,
    pub output_limit_bytes: Option<usize>,
    /// 拉起工作：拿到 id + 写入面后返回取消钩子。失败返回错误文本（不注册）。
    pub run: Box<dyn FnOnce(JobWriter) -> Result<CancelHook, String> + Send>,
}

/// 生产侧写入面（上游 `JobHandle`）：追加输出、更新进度行。
#[derive(Clone)]
pub struct JobWriter {
    id: JobId,
    jobs: Arc<RegistryInner>,
}

impl JobWriter {
    pub fn id(&self) -> &JobId {
        &self.id
    }

    /// 追加一块输出（`log` 通道不进模型面）。
    pub fn append(&self, text: &str, channel: Option<JobChannel>) {
        self.append_with_gap(text, channel, false);
    }

    /// 追加并标记「此块之前的字节已丢失」。
    pub fn append_with_gap(&self, text: &str, channel: Option<JobChannel>, gap_before: bool) {
        self.jobs.append(&self.id, text, channel, gap_before, Writer::Producer);
    }

    /// 替换实时进度行（结算时清空）。
    pub fn update_progress(&self, line: impl Into<String>) {
        self.jobs.update_progress(&self.id, line.into(), Writer::Producer);
    }

    /// 生产侧报结算（幂等：首个终态生效）。
    pub fn settle(&self, outcome: JobOutcome) {
        self.jobs.settle(&self.id, outcome, JobSettleCause::Producer);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Writer {
    Producer,
    /// 注册表自泵（output 拉源）；rustdsh 暂无拉源，保留分类以便对齐。
    #[allow(dead_code)]
    Pump,
}

/// 一次消费读的结果（上游 `JobRead`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRead {
    pub chunks: Vec<JobChunk>,
    pub lossy: bool,
    /// 生产侧结果：只随结算后第一次读交付一次。
    pub result: Option<String>,
    pub job: JobView,
}

/// 一次非消费读（上游 `JobOutputRead`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOutputRead {
    pub chunks: Vec<JobChunk>,
    pub next: usize,
    pub lossy: bool,
}

/// 作业事件（上游 `JobEvent`；观察者/UI 面用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobEvent {
    Registered(JobView),
    Progress(JobView),
    Stopping(JobView),
    Removed(JobId),
    /// 结算；`awaited` = 释放了活着的等待者（完成通知据此去重）。
    Settled { job: JobView, cause: JobSettleCause, awaited: bool },
    /// 输出推进：只有 id 与新总量（观察者自己按自己的游标读）。
    Output { id: JobId, owner: Option<SessionId>, total: usize },
}

pub type JobListener = Arc<dyn Fn(&JobEvent) + Send + Sync>;

/// 订阅筛选：某属主 + 无主作业，或全部。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobFilter {
    Owner(SessionId),
    All,
}

struct TrackedJob {
    view: JobView,
    ring: OutputRing,
    cancel: CancelHook,
    kill_reason: Option<String>,
    settle_cause: Option<JobSettleCause>,
    result: Option<String>,
    result_delivered: bool,
    model_cursor: usize,
    waiters: Vec<tokio::sync::oneshot::Sender<()>>,
    tool_call_id: Option<String>,
}

struct RegistryInner {
    retain_bytes: usize,
    settled_retain_bytes: usize,
    jobs: Mutex<HashMap<JobId, TrackedJob>>,
    listeners: Mutex<Vec<(u64, JobFilter, JobListener)>>,
    next_listener_id: std::sync::atomic::AtomicU64,
    /// 会话事件落盘缝（宿主注入：`job/registered` / `job/settled`）。
    session_sink: Mutex<Option<Arc<dyn Fn(&SessionEventRow) + Send + Sync>>>,
}

/// 落盘行（`job/registered` / `job/settled` 的宿主承载）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEventRow {
    pub kind: SessionEventKind,
    pub owner: SessionId,
    pub job: JobView,
    pub cause: Option<JobSettleCause>,
    pub producer_reported: bool,
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEventKind {
    Registered,
    Settled,
}

/// 进程内注册表（上游 `jobs-local` 的 rustrsh 等价）。
pub struct Jobs {
    inner: Arc<RegistryInner>,
    runtime: tokio::runtime::Handle,
}

impl Clone for Jobs {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner), runtime: self.runtime.clone() }
    }
}

impl Jobs {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                retain_bytes: DEFAULT_RETAIN_BYTES,
                settled_retain_bytes: DEFAULT_SETTLED_RETAIN_BYTES,
                jobs: Mutex::new(HashMap::new()),
                listeners: Mutex::new(Vec::new()),
                next_listener_id: std::sync::atomic::AtomicU64::new(0),
                session_sink: Mutex::new(None),
            }),
            runtime,
        }
    }

    /// 自定义环保留量（测试与部署调参用；上游是同名配置项）。
    pub fn with_retention(runtime: tokio::runtime::Handle, retain: usize, settled_retain: usize) -> Self {
        let mut jobs = Self::new(runtime);
        // 构造期一次性设定：注册后不再变化（比内部可变写更简单）
        let inner = Arc::get_mut(&mut jobs.inner).expect("fresh registry is uniquely owned");
        inner.retain_bytes = retain;
        inner.settled_retain_bytes = settled_retain;
        jobs
    }

    /// 注入会话事件落盘缝。
    pub fn set_session_sink(&self, sink: Arc<dyn Fn(&SessionEventRow) + Send + Sync>) {
        *self.inner.session_sink.lock().unwrap() = Some(sink);
    }

    /// 注册一个作业；`run` 内失败则不应发生（注册表已提交），错误由生产侧自行报。
    pub fn start(&self, spec: JobSpec) -> Result<JobId, String> {
        let id = JobId::generate(&spec.kind);
        let now = now_ms();
        let owner = spec.owner.clone();
        let label = spec.label.clone();
        let kind = spec.kind.clone();
        let tool_call_id = spec.tool_call_id.clone();
        let view = JobView {
            id: id.clone(),
            kind,
            label,
            owner: owner.clone(),
            output_limit_bytes: spec.output_limit_bytes,
            status: JobStatus::Running,
            progress: None,
            detail: None,
            started_at: now,
            finished_at: None,
            output_total: 0,
            output_earliest: 0,
            spill_paths: Vec::new(),
        };
        let writer = JobWriter { id: id.clone(), jobs: Arc::clone(&self.inner) };
        // 先把记录插进去再跑 `run`：上游语义是「starter 调用内 staging 的写入
        // 被保留、随注册提交一起可见」——先插入才收得到这些块。
        {
            let mut jobs = self.inner.jobs.lock().unwrap();
            jobs.insert(
                id.clone(),
                TrackedJob {
                    view: view.clone(),
                    ring: OutputRing::default(),
                    cancel: Arc::new(|_reason: Option<String>| {}),
                    kill_reason: None,
                    settle_cause: None,
                    result: None,
                    result_delivered: false,
                    model_cursor: 0,
                    waiters: Vec::new(),
                    tool_call_id: tool_call_id.clone(),
                },
            );
        }
        let cancel = match (spec.run)(writer) {
            Ok(cancel) => cancel,
            Err(error) => {
                // `run` 失败：不注册（上游「throw leaves nothing registered」）
                self.inner.jobs.lock().unwrap().remove(&id);
                return Err(error);
            }
        };
        {
            let mut jobs = self.inner.jobs.lock().unwrap();
            if let Some(job) = jobs.get_mut(&id) {
                job.cancel = cancel;
            }
        }
        self.inner.record(&view, SessionEventKind::Registered, None, false);
        self.inner.emit(JobEvent::Registered(view.clone()), owner.as_ref());
        Ok(id)
    }

    /// 列表（上游 `list(caller)`：属主自己的 + 无主作业）。
    pub fn list(&self, caller: Option<&SessionId>) -> Vec<JobView> {
        let jobs = self.inner.jobs.lock().unwrap();
        let mut out: Vec<JobView> = jobs
            .values()
            .filter(|j| match (&j.view.owner, caller) {
                (None, _) => true,
                (Some(owner), Some(c)) => owner == c,
                (Some(_), None) => false,
            })
            .map(|j| Self::project(j))
            .collect();
        out.sort_by_key(|v| v.started_at);
        out
    }

    /// 单个投影（含访问围栏）。
    pub fn get(&self, id: &JobId, caller: Option<&SessionId>) -> Result<JobView, String> {
        let jobs = self.inner.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| format!("unknown job {id}"))?;
        Self::assert_access(job, caller, id)?;
        Ok(Self::project(job))
    }

    /// 消费读：从模型游标起交付增量，推进游标；终态时收紧环并交付一次 `result`。
    pub fn read(&self, id: &JobId, caller: Option<&SessionId>) -> Result<JobRead, String> {
        let mut jobs = self.inner.jobs.lock().unwrap();
        let job = jobs.get_mut(id).ok_or_else(|| format!("unknown job {id}"))?;
        Self::assert_access_ref(job, caller, id)?;
        let (chunks, lossy) = job.ring.read_from(job.model_cursor);
        job.model_cursor = job.ring.total;
        let result = if job.view.status.is_terminal() && !job.result_delivered {
            job.result_delivered = true;
            job.result.clone()
        } else {
            None
        };
        if job.view.status.is_terminal() {
            let settled = self.inner.settled_retain_bytes;
            let floor = job.ring.total.saturating_sub(job.model_cursor);
            job.ring.trim(settled.max(floor));
        }
        let view = Self::project(job);
        Ok(JobRead { chunks, lossy, result, job: view })
    }

    /// 非消费读（观察者面）。
    pub fn read_at(&self, id: &JobId, from: usize, caller: Option<&SessionId>) -> Result<JobOutputRead, String> {
        let jobs = self.inner.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| format!("unknown job {id}"))?;
        Self::assert_access(job, caller, id)?;
        let (chunks, lossy) = job.ring.read_from(from);
        Ok(JobOutputRead { chunks, next: job.ring.total, lossy })
    }

    /// 请求取消；终态作业返回 `already-finished`。
    pub fn kill(&self, id: &JobId, caller: Option<&SessionId>, reason: Option<&str>) -> Result<&'static str, String> {
        // 取消钩子必须在**锁外**调用：生产侧可能在 cancel 内同步结算
        // （`writer.settle`），持锁调用会自锁。
        let (view, cancel) = {
            let mut jobs = self.inner.jobs.lock().unwrap();
            let job = jobs.get_mut(id).ok_or_else(|| format!("unknown job {id}"))?;
            Self::assert_access_ref(job, caller, id)?;
            if job.view.status.is_terminal() {
                return Ok("already-finished");
            }
            if let Some(reason) = reason {
                job.kill_reason = Some(reason.to_string());
            }
            job.settle_cause = Some(JobSettleCause::Kill);
            job.view.status = JobStatus::Stopping;
            (Self::project(job), Arc::clone(&job.cancel))
        };
        (cancel)(reason.map(str::to_string));
        self.inner.emit(JobEvent::Stopping(view.clone()), view.owner.as_ref());
        Ok("requested")
    }

    /// 等结算或超时；超时返回当前投影（作业仍活着）。
    pub async fn wait(&self, id: &JobId, timeout: std::time::Duration, caller: Option<&SessionId>) -> Result<JobView, String> {
        let rx = {
            let mut jobs = self.inner.jobs.lock().unwrap();
            let job = jobs.get_mut(id).ok_or_else(|| format!("unknown job {id}"))?;
            Self::assert_access_ref(job, caller, id)?;
            if job.view.status.is_terminal() {
                return Ok(Self::project(job));
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            job.waiters.push(tx);
            rx
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(_) | Err(_) => self.get(id, caller),
        }
    }

    /// 移除已结算作业（上游 `remove`）。
    pub fn remove(&self, id: &JobId, caller: Option<&SessionId>) -> Result<(), String> {
        let view = {
            let mut jobs = self.inner.jobs.lock().unwrap();
            let job = jobs.get(id).ok_or_else(|| format!("unknown job {id}"))?;
            Self::assert_access(job, caller, id)?;
            if !job.view.status.is_terminal() {
                return Err(format!(
                    "job {id} is still {}; kill it and wait for settlement before removing it",
                    job.view.status.as_wire()
                ));
            }
            let view = Self::project(job);
            jobs.remove(id);
            view
        };
        self.inner.emit(JobEvent::Removed(view.id.clone()), view.owner.as_ref());
        Ok(())
    }

    /// 属主销毁：取消并强制结算（teardown）。
    pub fn cancel_owner(&self, owner: &SessionId) {
        let ids: Vec<JobId> = {
            let jobs = self.inner.jobs.lock().unwrap();
            jobs.values()
                .filter(|j| j.view.owner.as_ref() == Some(owner) && !j.view.status.is_terminal())
                .map(|j| j.view.id.clone())
                .collect()
        };
        for id in ids {
            let cancel = {
                let mut jobs = self.inner.jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(&id) else { continue };
                job.settle_cause = Some(JobSettleCause::Teardown);
                job.cancel.clone()
            };
            cancel(Some("owner disposed".to_string()));
            self.inner.settle(&id, JobOutcome::killed(), JobSettleCause::Teardown);
        }
    }

    /// 订阅（返回 disposer）。
    pub fn subscribe(&self, filter: JobFilter, listener: JobListener) -> impl Fn() + Send + Sync {
        let token = self.inner.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.listeners.lock().unwrap().push((token, filter, listener));
        let inner = Arc::clone(&self.inner);
        move || {
            let mut listeners = inner.listeners.lock().unwrap();
            listeners.retain(|(id, _, _)| *id != token);
        }
    }

    fn project(job: &TrackedJob) -> JobView {
        let mut view = job.view.clone();
        view.output_total = job.ring.total;
        view.output_earliest = job.ring.earliest;
        view
    }

    fn assert_access(job: &TrackedJob, caller: Option<&SessionId>, id: &JobId) -> Result<(), String> {
        match (&job.view.owner, caller) {
            (None, _) => Ok(()),
            (Some(owner), Some(c)) if owner == c => Ok(()),
            _ => Err(format!("job {id} belongs to another session")),
        }
    }

    fn assert_access_ref(job: &TrackedJob, caller: Option<&SessionId>, id: &JobId) -> Result<(), String> {
        Self::assert_access(job, caller, id)
    }
}

impl RegistryInner {
    fn append(&self, id: &JobId, text: &str, channel: Option<JobChannel>, gap_before: bool, writer: Writer) {
        let owner = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(id) else { return };
            if job.view.status.is_terminal() {
                // 结算后生产侧写入：记录并丢弃（上游同语义）
                return;
            }
            if !job.ring.append(text, channel, gap_before, self.retain_bytes) {
                return;
            }
            job.view.owner.clone()
        };
        let _ = writer;
        self.emit(
            JobEvent::Output {
                id: id.clone(),
                owner: owner.clone(),
                total: {
                    let jobs = self.jobs.lock().unwrap();
                    jobs.get(id).map(|j| j.ring.total).unwrap_or(0)
                },
            },
            owner.as_ref(),
        );
    }

    fn update_progress(&self, id: &JobId, line: String, writer: Writer) {
        let view = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(id) else { return };
            if job.view.status.is_terminal() {
                return;
            }
            job.view.progress = Some(line);
            Jobs::project(job)
        };
        let _ = writer;
        let owner = view.owner.clone();
        self.emit(JobEvent::Progress(view), owner.as_ref());
    }

    fn settle(&self, id: &JobId, outcome: JobOutcome, cause: JobSettleCause) {
        // 首胜：初始终态不再改（teardown 强制失败对迟到生产结算稳定）
        let (view, waiters, awaited) = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(id) else { return };
            if job.view.status.is_terminal() {
                return;
            }
            let cause = job.settle_cause.unwrap_or(cause);
            job.view.status = outcome.status;
            // killed 结算带 kill 原因（生产事实在前）
            match (outcome.status, &job.kill_reason) {
                (JobStatus::Killed, Some(reason)) => {
                    job.view.detail = Some(match outcome.detail {
                        Some(detail) => format!("{detail}; {reason}"),
                        None => reason.clone(),
                    });
                }
                _ => {
                    if outcome.detail.is_some() {
                        job.view.detail = outcome.detail;
                    }
                }
            }
            job.view.progress = None;
            job.result = outcome.result;
            job.view.finished_at = Some(now_ms());
            let settled_cap = self.settled_retain_bytes;
            let floor = job.ring.total.saturating_sub(job.model_cursor);
            job.ring.trim(settled_cap.max(floor));

            let waiters: Vec<_> = job.waiters.drain(..).collect();
            let awaited = !waiters.is_empty();
            job.settle_cause = Some(cause);
            (Jobs::project(job), waiters, awaited)
        };
        // 先释放等待者，再广播结算（上游序）
        for waiter in waiters {
            let _ = waiter.send(());
        }
        self.record(&view, SessionEventKind::Settled, Some(cause), true);
        let owner = view.owner.clone();
        self.emit(JobEvent::Settled { job: view.clone(), cause, awaited }, owner.as_ref());
        self.emit(
            JobEvent::Output { id: view.id.clone(), owner: owner.clone(), total: view.output_total },
            owner.as_ref(),
        );
    }

    fn record(&self, view: &JobView, kind: SessionEventKind, cause: Option<JobSettleCause>, producer_reported: bool) {
        let Some(owner) = view.owner.clone() else { return };
        let sink = self.session_sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            let tool_call_id = {
                let jobs = self.jobs.lock().unwrap();
                jobs.get(&view.id).and_then(|j| j.tool_call_id.clone())
            };
            sink(&SessionEventRow {
                kind,
                owner,
                job: view.clone(),
                cause,
                producer_reported,
                tool_call_id,
            });
        }
    }

    fn emit(&self, event: JobEvent, owner: Option<&SessionId>) {
        let listeners: Vec<(u64, JobFilter, JobListener)> = {
            let listeners = self.listeners.lock().unwrap();
            listeners.clone()
        };
        for (_id, filter, listener) in listeners {
            let matches = match (&filter, owner) {
                (JobFilter::All, _) => true,
                (JobFilter::Owner(f), Some(o)) => f == o,
                // 属主筛选也收无主作业（那一会话能看见的集合）
                (JobFilter::Owner(_), None) => true,
            };
            if matches {
                listener(&event);
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs() -> Jobs {
        Jobs::new(tokio::runtime::Handle::current())
    }

    #[test]
    fn utf8_tail_never_splits_a_codepoint() {
        // 上游语义：起点越过续字节后取尾（可能短于预算）——`你好世界` 12B、预算 6B → `世界`
        let (cjk_tail, cjk_bytes) = utf8_tail("你好世界", 6);
        assert_eq!(cjk_bytes, 6);
        assert_eq!(cjk_tail, "世界");
        assert_eq!(cjk_tail.len(), cjk_bytes);
        let (tail, bytes) = utf8_tail("abc", 10);
        assert_eq!((tail.as_str(), bytes), ("abc", 3));
    }

    #[test]
    fn ring_offsets_stay_absolute_across_head_eviction() {
        let mut ring = OutputRing::default();
        ring.append("aaaa", None, false, 8);
        ring.append("bbbb", None, false, 8);
        assert_eq!(ring.total, 8);
        assert_eq!(ring.earliest, 0);
        ring.append("cccc", None, false, 8);
        // 淘汰头部一块：偏移不移动，earliest 前移
        assert_eq!(ring.total, 12);
        assert_eq!(ring.earliest, 4);
        let (chunks, lossy) = ring.read_from(0);
        assert!(lossy, "游标落在保留区之前");
        assert_eq!(chunks[0].at, 4);
    }

    #[test]
    fn oversized_single_chunk_keeps_utf8_safe_tail_with_gap() {
        let mut ring = OutputRing::default();
        ring.append("你好世界", None, false, 6);
        assert_eq!(ring.total, 12);
        assert_eq!(ring.earliest, 6);
        let (chunks, _) = ring.read_from(0);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "世界");
        assert!(chunks[0].gap_before);
    }
}
