//! dsh-subagent — 子 agent 工具（对齐 `packages/subagent` 的进程内分支）。
//!
//! 参考实现把子 agent 生命周期拆成 spawn/driver/control/report 多个包并支持
//! 后台运行；本实现取其前台默认路径的语义：`subagent` 工具以全新会话
//! （in-process fork）驱动一个子 ReactLoopAgent 跑完任务，等它结束并把
//! 末条助手消息作为工具结果返回。深度护栏（默认 2 层）防止子 agent 递归
//! 生子失控；超时（默认 300s）到点取消并报错。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::event::EventBus;
use dsh_llm::types::ContentBlock;
use dsh_llm::LlmRuntime;
use dsh_session::{Session, SessionEvent};
use dsh_session_projection::SessionProjections;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};

/// `subagent` 工具配置。
pub struct SubagentTool {
    llm: Arc<LlmRuntime>,
    tools: Arc<dsh_tools::ToolRegistry>,
    prompt: Arc<SystemPrompt>,
    /// 子 agent 路由（默认继承父路由；宿主在切换模型时经
    /// [`Self::set_route`] 同步更新）。
    route: Arc<std::sync::RwLock<(String, String)>>,
    /// 会话工作目录（子 agent 与父共享同一句柄 → 自动继承）
    workdir: dsh_tools::Workdir,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
    /// 全局活着的子 agent 深度（跨嵌套共享）。
    depth: Arc<AtomicU32>,
    /// 允许的最大嵌套层数。
    max_depth: u32,
    /// 子 agent 会话投影注册表（默认独立；宿主可共享自己的注册表）。
    projections: Arc<SessionProjections>,
    /// 单次子任务等待上限。
    timeout: Duration,
    /// 子会话耐久化缝（宿主注入：cwd 由闭包内部解析）：注入后子会话
    /// 事件（含标题）全部落盘，侧栏按普通会话回看（上游子会话即完整
    /// 耐久会话）。
    child_sink: std::sync::Mutex<Option<std::sync::Arc<dyn Fn(&dsh_llm::types::SessionId, &SessionEvent) + Send + Sync>>>,
    /// 父 agent 弱引用：子任务建立时向父日志落 `subagent/catalog` 发现
    /// 事实（上游 establishCatalogChild；Weak 防工具↔agent 构造环）。
    parent: std::sync::Mutex<std::sync::Weak<ReactLoopAgent>>,
}

impl SubagentTool {
    pub fn new(
        llm: Arc<LlmRuntime>,
        tools: Arc<dsh_tools::ToolRegistry>,
        prompt: Arc<SystemPrompt>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            llm,
            tools,
            prompt,
            route: Arc::new(std::sync::RwLock::new((provider.into(), model.into()))),
            workdir: dsh_tools::Workdir::new(),
            max_tokens: None,
            system_prompt: None,
            depth: Arc::new(AtomicU32::new(0)),
            max_depth: 2,
            projections: Arc::new(SessionProjections::default()),
            timeout: Duration::from_secs(300),
            child_sink: std::sync::Mutex::new(None),
            parent: std::sync::Mutex::new(std::sync::Weak::new()),
        })
    }

    /// 注入子会话耐久化缝（宿主以 recorder+cwd 构造闭包）。
    pub fn set_child_sink(
        &self,
        sink: std::sync::Arc<dyn Fn(&dsh_llm::types::SessionId, &SessionEvent) + Send + Sync>,
    ) {
        *self.child_sink.lock().unwrap() = Some(sink);
    }

    /// 接父 agent（构造完成后调用；子任务建立时向父日志落目录事实）。
    pub fn set_parent_link(&self, parent: std::sync::Arc<ReactLoopAgent>) {
        *self.parent.lock().unwrap() = std::sync::Arc::downgrade(&parent);
    }

    /// 共享宿主的投影注册表（子 agent 的 turnBoundary 折进同一注册表）。
    pub fn with_projections(mut self: Arc<Self>, projections: Arc<SessionProjections>) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.projections = projections;
        }
        self
    }

    /// 覆盖子 agent 的基础 system prompt 与 token 上限。
    pub fn with_system_prompt(mut self: Arc<Self>, system_prompt: Option<String>) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.system_prompt = system_prompt;
        }
        self
    }

    pub fn with_max_tokens(mut self: Arc<Self>, max_tokens: Option<u32>) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.max_tokens = max_tokens;
        }
        self
    }

    pub fn with_max_depth(mut self: Arc<Self>, max_depth: u32) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.max_depth = max_depth;
        }
        self
    }

    pub fn with_timeout(mut self: Arc<Self>, timeout: Duration) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.timeout = timeout;
        }
        self
    }

    /// 宿主路由切换时同步子 agent 路由。
    pub fn set_route(&self, provider: impl Into<String>, model: impl Into<String>) {
        *self.route.write().unwrap() = (provider.into(), model.into());
    }

    /// 注入会话工作目录句柄（Workdir 是共享句柄：宿主更新同一对象，
    /// 子 agent 自动跟随）。
    pub fn with_workdir(mut self: Arc<Self>, workdir: dsh_tools::Workdir) -> Arc<Self> {
        if let Some(tool) = Arc::get_mut(&mut self) {
            tool.workdir = workdir;
        }
        self
    }
}

/// 结束原因（上游 SubagentResult['stopReason'] 的 rustdsh 对应面）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementStop {
    Completed,
    Aborted,
    MaxTokens,
    Error,
}

/// 结束摘要首行（上游 settlementSummary 逐字——`Background subagent {id}`
/// 五分支文案；rustdsh 无 continuable 拒绝面，走 completed 不可续形）。
pub fn settlement_summary_line(child_id: &str, stop: SettlementStop) -> String {
    let subject = format!("Background subagent {child_id}");
    match stop {
        // rustdsh 子会话为一次性（无 follow-up 通道）——不可续形
        SettlementStop::Completed => format!("{subject} finished. It cannot receive follow-up messages."),
        SettlementStop::Aborted => format!("{subject} was stopped before it finished."),
        SettlementStop::MaxTokens => format!("{subject} ran out of room before it finished."),
        SettlementStop::Error => format!("{subject} failed before it finished."),
    }
}

/// settlement 通知文本（上游 createSettlementMessage 的 content 面：
/// 摘要行 + closing text 两形 + 无 structured/diagnostic 携带面）。
pub fn settlement_notice_text(child_id: &str, stop: SettlementStop, closing: Option<&str>) -> String {
    let summary = settlement_summary_line(child_id, stop);
    match closing.map(str::trim).filter(|t| !t.is_empty()) {
        None => format!("{summary}
It left no closing message."),
        Some(text) => format!("{summary}
Its closing message:
{text}"),
    }
}

/// 从子会话日志取末条带文本的助手消息（子任务的最终答复）。
fn final_assistant_text(session: &Session) -> Option<String> {
    session
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            SessionEvent::AssistantMessage { message, .. } => {
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.trim().is_empty() { None } else { Some(text) }
            }
            _ => None,
        })
}

#[async_trait]
impl Tool for SubagentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "subagent".into(),
            description: "Delegate a self-contained task to a fresh subagent that runs in the background of this \
conversation with its own context window and the same tools. It cannot see this conversation; write the \
prompt as a complete brief. This call waits for the subagent and returns its final report."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "A short (3-5 word) description of the delegated task, for display." },
                    "prompt": { "type": "string", "description": "The complete task brief for the subagent: goal, relevant context, and the expected shape of the result." }
                },
                "required": ["description", "prompt"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let prompt = input
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if prompt.trim().is_empty() {
            return ToolExecutionResult::error("prompt must be a non-empty string");
        }
        if self.depth.load(Ordering::SeqCst) >= self.max_depth {
            return ToolExecutionResult::error(format!(
                "subagent depth limit reached ({}); run the task inline instead",
                self.max_depth
            ));
        }

        let (provider, model) = self.route.read().unwrap().clone();
        let child_id = dsh_llm::types::SessionId::new(uuid::Uuid::new_v4().to_string());
        let description = input
            .arguments
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("task")
            .to_string();
        let options = AgentOptions {
            provider,
            model,
            max_tokens: self.max_tokens,
            system_prompt: self.system_prompt.clone(),
            compaction: Default::default(),
            workdir: self.workdir.clone(),
            // 子代理不上传文件（上游同）：附件根不传——委派提示里由主
            // agent 的 handle 文本携带保存路径，执行环境可见即可读
            attachments_root: None,
        };
        let child = ReactLoopAgent::new(
            child_id.clone(),
            options,
            Arc::clone(&self.llm),
            Arc::clone(&self.tools),
            Arc::clone(&self.prompt),
            Arc::clone(&self.projections),
            EventBus::new(),
        );
        let _events = child.subscribe(); // 子事件隔离：留接收端防背压，不转发
        // 子会话耐久化 + 父目录（上游 establishCatalogChild 语义：子建立
        // 即向父落 one-shot 发现事实；标题=description 供侧栏识别）
        if let Some(sink) = self.child_sink.lock().unwrap().as_ref() {
            let sink_for_child = Arc::clone(sink);
            let sink_id = child_id.clone();
            child.set_event_sink(move |event| sink_for_child(&sink_id, &event));
            sink(&child_id, &SessionEvent::SessionTitle { title: description.clone() });
            if let Some(parent) = self.parent.lock().unwrap().upgrade() {
                let created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                parent.append_session_event(SessionEvent::SubagentCatalog {
                    child_id: child_id.as_str().to_string(),
                    child_created_at: created_at,
                    mode: "one-shot".into(),
                    label: Some(description.clone()),
                });
            }
        }
        child.spawn();
        self.depth.fetch_add(1, Ordering::SeqCst);
        child.followup(prompt);
        let waited = tokio::time::timeout(self.timeout, child.when_idle()).await;
        self.depth.fetch_sub(1, Ordering::SeqCst);
        let text = final_assistant_text(&child.session().lock().unwrap());
        // settlement 通知（上游 notifySettlement）：子任务结束（完成/超时/
        // 无消息）都向父日志落 subagent-settled user/message——三路径复用
        //（超时=aborted 形；无消息仍 completed+It left no closing message.
        // 形——上游 error 形仅在结果本身失败时走，此处 final report 工具
        // 结果已各自携带失败语义，通知只记结束事实）
        let stop = if waited.is_err() { SettlementStop::Aborted } else { SettlementStop::Completed };
        if waited.is_err() {
            child.cancel();
        }
        let cid = child_id.as_str();
        {
            let notice = settlement_notice_text(cid, stop, text.as_deref());
            let notice_msg = dsh_llm::Message::new(
                dsh_llm::Role::User,
                vec![dsh_llm::ContentBlock::text(notice)],
                dsh_llm::MessageSource::Context {
                    context_kind: "subagent-settled".into(),
                    plugin: None,
                    form: Some("notice".into()),
                    summary: Some(settlement_summary_line(cid, stop)),
                    changes_paths: Vec::new(),
                    reference_labels: Vec::new(),
                    name: None,
                },
            );
            if let Some(parent) = self.parent.lock().unwrap().upgrade() {
                parent.append_session_event(dsh_session::SessionEvent::UserMessage(
                    notice_msg,
                ));
            }
        }
        match (waited, text) {
            (Err(_), _) => ToolExecutionResult::error(format!(
                "subagent timed out after {}s; its work was cancelled",
                self.timeout.as_secs()
            )),
            (Ok(()), Some(text)) => ToolExecutionResult::text(format!(
                "Subagent ({description}) final report:\n\n{text}"
            )),
            (Ok(()), None) => ToolExecutionResult::error("subagent produced no final message"),
        }
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;

    #[test]
    fn summary_lines_match_upstream_wording() {
        assert_eq!(
            settlement_summary_line("session-c1", SettlementStop::Completed),
            "Background subagent session-c1 finished. It cannot receive follow-up messages."
        );
        assert_eq!(
            settlement_summary_line("session-c1", SettlementStop::Aborted),
            "Background subagent session-c1 was stopped before it finished."
        );
        assert_eq!(
            settlement_summary_line("session-c1", SettlementStop::MaxTokens),
            "Background subagent session-c1 ran out of room before it finished."
        );
        assert_eq!(
            settlement_summary_line("session-c1", SettlementStop::Error),
            "Background subagent session-c1 failed before it finished."
        );
    }

    #[test]
    fn notice_text_has_two_closing_forms() {
        // 有 closing 文本：摘要 + Its closing message: + 文本
        let with = settlement_notice_text("session-c1", SettlementStop::Completed, Some("report body"));
        assert!(with.contains("finished. It cannot receive follow-up messages."), "{with}");
        assert!(with.contains("Its closing message:\nreport body"), "{with}");
        // 无 closing：It left no closing message.
        let without = settlement_notice_text("session-c1", SettlementStop::Completed, None);
        assert!(without.contains("It left no closing message."), "{without}");
        // 空白 closing 同无消息形态
        let blank = settlement_notice_text("session-c1", SettlementStop::Completed, Some("   "));
        assert!(blank.contains("It left no closing message."), "{blank}");
    }
}
