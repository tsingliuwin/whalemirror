//! dsh-agent-loop — the default agent driver.
//!
//! Mirrors [`packages/core/agent-loop`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/agent-loop):
//! the `ReactLoopAgent` state machine that drives one session through turn and
//! step boundaries, deriving every model request from the session log.
//!
//! Milestone-2 wiring: live `agent/*` extension points now dispatch through a
//! shared [`EventBus`] — `agent/pre-step` (waterfall), `agent/request`
//! (waterfall), `agent/request-error` (waterfall, retry), `agent/turn-stopping`
//! (serial), and the `agent/status`, `agent/error`, `agent/inbox/*` emits.
//! Remaining simplifications: no backoff behind `agent/request-error` retry,
//! and sequential (rather than pooled-parallel) tool dispatch.

use dsh_agent::{
    AgentErrorOccurred, AgentErrorPayload, AgentInboxClaimed, AgentInboxDiscarded,
    AgentInboxInserted, AgentPreStep, AgentRequest, AgentRequestError, AgentRequestPayload,
    AgentStatusChanged, AgentTurnStopping, Inbox, InboxClaimedPayload,
    PreStepDecision, PreStepInput, RequestErrorAction, RequestErrorPayload, TurnStoppingPayload,
};
pub use dsh_agent::{AgentStatus, InboxTarget};
use dsh_cordis::EventBus;
use dsh_llm::{
    AbortSignal, BlockAssembler, CallId, ContentBlock, FinishReason, GenerateOptions, LlmCallConfig,
    LlmRuntime, Message, MessageId, MessageSource, Role, SessionId, StreamChunk,
};
use dsh_session::{EpochHeader, HeaderReason, Session, SessionEntry, SessionEvent, TurnEndReason};
use dsh_session_projection::{Disposer, ProjectionDefinition, SessionProjections};
use dsh_system_prompt::{PromptAssembly, SystemPrompt};
use dsh_tools::{ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use flume::{Receiver, Sender};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::Notify;

pub mod policy;
pub mod repeat_guard;
pub mod retry;
pub mod time_context;

/// 一步的边界事实（`step/start` 或 `step/end` 及其 seq）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepBoundary {
    pub kind: StepBoundaryKind,
    pub seq: u64,
}

/// 边界种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepBoundaryKind {
    Start,
    End,
}

/// 从单个 agent 会话日志折叠出的轮次/步边界事实（web
/// `TurnBoundaryProjection`，由 dsh-agent-loop 注册 `turnBoundary` 单元）。
///
/// 读方契约：key 由 agent-loop 注册，缺席即「无打开轮次 / 无边界」的能力
/// 缺席而非损坏状态；对缺席无安全回退的读方（如 step-open 判定）可以
/// 显式报错（上游同一注释）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBoundaryProjection {
    /// 打开轮次的 `turn/start` seq；轮次之间为 null。
    pub open_turn_start_seq: Option<u64>,
    /// 最近一次 `step/start` 的 seq；首步前为 null。
    pub last_step_start_seq: Option<u64>,
    /// 最近的步边界（start 或 end）及其 seq。
    pub last_step_boundary: Option<StepBoundary>,
    /// 最近 `turn/start` 的轮次号；首轮前为 0。
    pub last_turn: u64,
}

/// `turnBoundary` 投影定义（stateVersion 与上游一致为 2；注册放在全部
/// config 校验之后是上游注释的语义——被拒绝的构造不留下投影单元）。
pub fn turn_boundary_projection_definition() -> ProjectionDefinition<TurnBoundaryProjection> {
    ProjectionDefinition {
        key: "turnBoundary",
        state_version: 2,
        init: TurnBoundaryProjection::default,
        apply: |state, entry: &SessionEntry| {
            let mut next = state.clone();
            match &entry.event {
                SessionEvent::TurnStart { turn } => {
                    next.open_turn_start_seq = Some(entry.seq);
                    next.last_turn = *turn;
                }
                SessionEvent::TurnEnd { .. } => {
                    next.open_turn_start_seq = None;
                }
                SessionEvent::StepStart { .. } => {
                    next.last_step_start_seq = Some(entry.seq);
                    next.last_step_boundary =
                        Some(StepBoundary { kind: StepBoundaryKind::Start, seq: entry.seq });
                }
                SessionEvent::StepEnd { .. } => {
                    next.last_step_boundary =
                        Some(StepBoundary { kind: StepBoundaryKind::End, seq: entry.seq });
                }
                _ => {}
            }
            next
        },
        // turnBoundary 无客户端 wire 面（宿主内消费）。
        view: None,
    }
}

/// Static configuration for one agent.
#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    /// Base system instructions, rendered ahead of registered prompt sections.
    pub system_prompt: Option<String>,
    /// 上下文压缩配置（threshold_tokens = 0 禁用；默认 60k/6 条）。
    pub compaction: dsh_compaction::CompactionConfig,
    /// 会话工作目录（web session.header.cwd → 模型可见的运行时上下文 +
    /// 工具执行基准；宿主切换工作区时更新同一句柄）。
    pub workdir: dsh_tools::Workdir,
    /// 附件存储根（`DSH_HOME/attachments/v1`）：请求组装把 FileBlock 投影
    /// 为 handle 文本时解析只读保存路径；None = 无可用路径（handle 落
    /// 「无法访问」分支）。
    pub attachments_root: Option<std::path::PathBuf>,
}

/// A live event emitted to UI/observers as the loop progresses.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted { turn: u64 },
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCall { tool_call_id: CallId, name: String, arguments: String },
    ToolResult { tool_call_id: CallId, is_error: bool },
    AssistantMessage { message: Message, usage: Option<dsh_llm::TokenUsage> },
    TurnEnded { turn: u64, reason: TurnEndReason },
    /// 压缩事务完成：影子区前 `shadowed_messages` 条消息已由检查点替换。
    Compacted { shadowed_messages: usize },
    /// 非 user source 的耐久上下文消息已随批落盘（认领的注入通知、
    /// model-switch 公告）：UI 侧以 context 行呈现（上游非 user source
    /// 一律 context 呈现的语义）；user source 消息由 composer 自行入列。
    ContextMessage { message: Message },
    Error { message: String, code: String },
}

/// Convenience for a client-submitted text prompt.
pub fn user_message(text: impl Into<String>) -> Message {
    Message::user_text(text)
}

/// 上游 routeLabel：与对侧 provider 相同则只显示 model，否则 `provider/model`。
fn route_label(provider: &str, model: &str, other_provider: &str) -> String {
    if provider == other_provider { model.to_string() } else { format!("{provider}/{model}") }
}

/// v3 system prompt 面节点消息（上游 createSystemMessage(text, SOURCE)）：
/// role system、固定 plugin source；空 prompt 落空 content（dormant head）。
fn system_prompt_message(rendered: &str) -> Message {
    let content = if rendered.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::text(rendered)]
    };
    Message::new(
        Role::System,
        content,
        MessageSource::Plugin {
            plugin: "@deepseek-ai/dsh-system-prompt".into(),
            form: None,
            summary: None,
            sections: None,
        },
    )
}

/// 构造模型切换公告消息（上游 `modelSwitchNotice`）：user/plugin
/// `model-selection` 形态，notice form 携带摘要。
fn model_switch_notice_message(previous: (&str, &str), selected: (&str, &str)) -> Message {
    let from = route_label(previous.0, previous.1, selected.0);
    let to = route_label(selected.0, selected.1, previous.0);
    Message::user_plugin(
        vec![ContentBlock::text(format!(
            "[model changed: assistant turns above this point were generated by {from}; the session continues with {to}]",
        ))],
        "model-selection",
        Some("notice".into()),
        Some(dsh_llm::bound_context_summary(&format!("{from} → {to}"))),
        None,
    )
}

enum StepEnd {
    /// Tools owe another request: open another step.
    NeedsAnotherStep,
    /// The turn concluded with this reason.
    Concluded(TurnEndReason),
}

/// The default `Agent` implementation: a turn/step driver over an inbox.
/// 简单唯一 id（时间纳秒 hex；compactionId 这类不透明标识够用）。
fn uuid_simple() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

pub struct ReactLoopAgent {
    options: Arc<RwLock<AgentOptions>>,
    /// 工具结果外置策略（上游 spill-policy；宿主装配，None = 不外置）
    /// 历史工具结果剪枝的内存面（上游 compaction/prune 的等价）：键 = 事件 seq，
    /// 值 = 剪枝后的消息；请求组装与压缩输入都读这一层（日志本体不改）。
    pruned: Arc<RwLock<std::collections::HashMap<u64, Message>>>,
    spill: Arc<RwLock<Option<Arc<dsh_spill::SpillPolicy>>>>,
    /// 时间上下文（上游 context/time-context）：配置 + 上次注入/读数时刻。
    time_context: Arc<RwLock<Option<time_context::TimeContextState>>>,
    /// 最近一条模型可见消息的时刻（step 1 的 elapsed 基线）。
    last_visible_ms: Arc<RwLock<Option<u64>>>,
    /// 检查点缝（上游 session-checkpoint-policy）
    checkpoint: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    /// 技能加载缝（上游 tool-skill 的 `/name` 显式调用）
    skill_loader: Arc<Mutex<Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>>>,
    session: Arc<Mutex<Session>>,
    inbox: Arc<Mutex<Inbox>>,
    llm: Arc<LlmRuntime>,
    tools: Arc<ToolRegistry>,
    prompt: Arc<SystemPrompt>,
    projections: Arc<SessionProjections>,
    events: EventBus,
    wake: Arc<Notify>,
    abort: Arc<Mutex<AbortSignal>>,
    status: Arc<Mutex<AgentStatus>>,
    ui_events: Mutex<Option<Sender<AgentEvent>>>,
    on_event: Mutex<Option<EventSink>>,
    /// `turnBoundary` 注册的撤销柄（注册随 agent 存活，上游 rode the fiber）。
    /// Mutex 使整体保持 Sync（Disposer 本身只是 Send）。
    _turn_boundary_registration: Mutex<Option<Disposer>>,
    /// v3 system prompt 面节点 head：(日志 seq, 当前文本)。None = 本会话
    /// 尚无 system/message 行（上游 SystemPromptProjection 的 head 跟踪）。
    system_head: Mutex<Option<(u64, String)>>,
    /// plan 模式激活态（上游 PlanModeController 的投影折叠：最后一条
    /// plan/mode 事件赢；无此事件 = 未激活）。pending 切换在轮终边界落盘。
    plan_mode: Arc<AtomicBool>,
    /// 本会话是否曾进入过 plan 模式（上游 plan 投影 stateVersion 4 的
    /// `entered`）：`exit_plan_mode` 从首次进入起保持声明，离开只改
    /// 指引——addition-only 路由不再因撤下声明丢前缀缓存。
    plan_entered: Mutex<bool>,
    /// 待落盘的 plan 模式切换（轮终边界提交，上游 onBoundary 同语义）。
    plan_pending: Mutex<Option<bool>>,
    /// prompt 重试收据（上游 `SessionCommandController.promptReceipts`）：
    /// 同一输入的重复投递按首次接纳去重——消息被认领或移除后仍算接纳，
    /// 日志是可重建该收据的恢复源。
    prompt_receipts: Mutex<PromptReceipts>,
    /// 会话绑定的 agent 预设 id（上游会话头 `agentPreset`；工具准入按它
    /// 门控，运行中会话保持其预设不变）。
    agent_preset: Mutex<String>,
    /// 运行时策略观察（web `sandbox:policy` / `approval:policy` 语境的值
    /// 源；`set_session` 折叠恢复，宿主运行时切换显式更新——见
    /// [`policy::PolicyContext`]）。
    policy: policy::PolicyContext,
    /// 连续重复调用提醒守卫（上游 `guard/repeat-tool-reminder`；部署组合
    /// 默认配置——阈值 [3,5,8]、预览 500、无 include/exclude）。
    repeat_guard: repeat_guard::RepeatChainGuard,
}

/// 一个会话的 prompt 接纳收据：已扫描到的日志长度 + 已接纳的消息 id。
#[derive(Default)]
struct PromptReceipts {
    /// 已扫描的 `Session::entries().len()`（增量扫描游标）。
    offset: usize,
    /// 已接纳输入的消息 id（落盘用户行，或尚在收件箱中的首次入列）。
    accepted: HashSet<MessageId>,
}

/// Side-channel observer for every appended session event.
type EventSink = Box<dyn Fn(SessionEvent) + Send + Sync>;

impl ReactLoopAgent {
    pub fn new(
        id: SessionId,
        options: AgentOptions,
        llm: Arc<LlmRuntime>,
        tools: Arc<ToolRegistry>,
        prompt: Arc<SystemPrompt>,
        projections: Arc<SessionProjections>,
        events: EventBus,
    ) -> Arc<Self> {
        // 注册柄由 agent 持有：注册随 agent 存活，disposer 的强引用反过来
        // 保证注册表不会先于 agent 消失。
        let turn_boundary_registration = projections.register(turn_boundary_projection_definition());
        Arc::new(Self {
            options: Arc::new(RwLock::new(options)),
        spill: Arc::new(RwLock::new(None)),
        pruned: Arc::new(RwLock::new(std::collections::HashMap::new())),
        time_context: Arc::new(RwLock::new(None)),
        last_visible_ms: Arc::new(RwLock::new(None)),
        checkpoint: Arc::new(Mutex::new(None)),
        skill_loader: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(Session::new(id))),
            inbox: Arc::new(Mutex::new(Inbox::new())),
            llm,
            tools,
            prompt,
            projections,
            events,
            wake: Arc::new(Notify::new()),
            abort: Arc::new(Mutex::new(AbortSignal::new())),
            status: Arc::new(Mutex::new(AgentStatus::Idle)),
            ui_events: Mutex::new(None),
            on_event: Mutex::new(None),
            _turn_boundary_registration: Mutex::new(Some(turn_boundary_registration)),
            system_head: Mutex::new(None),
            plan_mode: Arc::new(AtomicBool::new(false)),
            plan_entered: Mutex::new(false),
            plan_pending: Mutex::new(None),
            prompt_receipts: Mutex::new(PromptReceipts::default()),
            agent_preset: Mutex::new(String::new()),
            policy: policy::PolicyContext::new(),
            repeat_guard: repeat_guard::RepeatChainGuard::with_defaults(),
        })
    }

    /// 会话绑定的 agent 预设（上游会话头 `agentPreset`；空 = 部署默认
    /// standard）。宿主在装载会话时设置，工具准入按它门控。
    pub fn set_agent_preset(&self, preset: impl Into<String>) {
        *self.agent_preset.lock().unwrap() = preset.into();
    }

    pub fn agent_preset(&self) -> String {
        self.agent_preset.lock().unwrap().clone()
    }

    /// 运行时策略观察句柄（宿主运行时切换用：权限预设换挡时更新沙箱档/
    /// 审批策略，下一次请求组装生效；会话恢复走 `set_session` 折叠）。
    pub fn policy(&self) -> &policy::PolicyContext {
        &self.policy
    }

    /// 会话工作目录句柄（共享——宿主切工作区/建树进入都经 `set` 更新，
    /// 工具面与下一次请求组装的 sandbox 语境同步跟随）。
    pub fn workdir(&self) -> dsh_tools::Workdir {
        self.options.read().unwrap().workdir.clone()
    }

    /// 运行期热更新模型路由（设置面板保存时调用）。
    pub fn set_provider_and_model(&self, provider: impl Into<String>, model: impl Into<String>) {
        let mut o = self.options.write().unwrap();
        o.provider = provider.into();
        o.model = model.into();
    }

    /// 运行期热更新基础人设（会话切换换预设时调用——minimal 的 persona
    /// 与 standard 不同；下一次请求组装即生效）。
    pub fn set_system_prompt(&self, text: impl Into<String>) {
        self.options.write().unwrap().system_prompt = Some(text.into());
    }

    pub fn session(&self) -> Arc<Mutex<Session>> {
        Arc::clone(&self.session)
    }

    /// Replace the agent's session (session switching). The inbox is cleared.
    /// The v3 system head is rebuilt from the log (last `system/message` row).
    pub fn set_session(&self, session: Session) {
        self.projections.forget_session(&self.session.lock().unwrap());
        let head = session.entries().iter().rev().find_map(|e| match &e.event {
            SessionEvent::SystemMessage { message, .. } => {
                let text = message
                    .content
                    .first()
                    .and_then(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((e.seq, text))
            }
            _ => None,
        });
        let plan_active = session.entries().iter().rev().find_map(|e| match &e.event {
            SessionEvent::PlanMode { active, .. } => Some(*active),
            _ => None,
        });
        let plan_entered = session.entries().iter().any(|e| {
            matches!(&e.event, SessionEvent::PlanMode { active: true, .. })
        });
        self.plan_mode.store(plan_active.unwrap_or(false), Ordering::Relaxed);
        *self.plan_entered.lock().unwrap() = plan_entered;
        // 权限旋钮折叠 → 运行时语境（上游 `sandbox:policy` / `approval:policy`
        // 语境每请求读投影；set_session 即折叠恢复）。三类事件各取最后值，
        // 缺省回落预设映射（上游 effective = projected ?? 部署默认；rustdsh
        // 部署默认 = 目录默认预设 workspace-write 的旋钮）。
        {
            let mut preset: Option<String> = None;
            let mut approval: Option<String> = None;
            let mut sandbox: Option<String> = None;
            for e in session.entries() {
                match &e.event {
                    SessionEvent::PermissionPreset { preset: p } => preset = Some(p.clone()),
                    SessionEvent::ApprovalPolicy { policy: p } => approval = Some(p.clone()),
                    SessionEvent::SandboxModeSwitch { mode } => sandbox = Some(mode.clone()),
                    _ => {}
                }
            }
            let (default_sandbox, default_approval) = policy::preset_knobs(
                preset.as_deref().unwrap_or(policy::DEFAULT_PRESET),
            );
            self.policy
                .set_sandbox(&sandbox.unwrap_or_else(|| default_sandbox.into()), Vec::new());
            self.policy
                .set_approval_policy(&approval.unwrap_or_else(|| default_approval.into()));
        }
        // v3 replace 语义要求换端点恰为当前 head：切换会话必须把内存 head
        // 重定基到日志末条 system/message 行——否则 None 分支会对已有 head
        // 重复 append（旧行残留），跨会话残留旧 seq 则写出悬空 replace 端点。
        *self.system_head.lock().unwrap() = head;
        self.plan_pending.lock().unwrap().take();
        *self.session.lock().unwrap() = session;
        self.inbox.lock().unwrap().clear();
        // 收据按「确切的会话」缓存（上游 WeakMap<Session, …>）：换会话必须
        // 丢弃旧收据并从新日志重建，否则旧会话的消息 id 会误挡新会话输入。
        *self.prompt_receipts.lock().unwrap() = PromptReceipts::default();
    }

    /// Append one event to the session log and fan it out to the sink
    /// (persistence). Public seam for log-writing plugins (upstream
    /// `agent.session.append(...)`): retry events ride this too.
    pub fn append_session_event(&self, event: SessionEvent) {
        self.append_event(event);
    }

    /// 审批策略切换通知（上游 `ApprovalService.setPolicy` 的 inject 面）：
    /// user/plugin `user-approval` 裸源耐久消息（V4 wire `{kind:
    /// 'user-approval'}`），落盘并即时入内存面——运行中轮从下一步、空闲
    /// 会话从下一轮可见。policy 事件本身由宿主切换路径落（上游同分工）。
    pub fn append_policy_change_notice(&self, previous: &str, policy: &str) {
        let message = Message::new(
            Role::User,
            vec![ContentBlock::text(policy::policy_change_notice_text(previous, policy))],
            MessageSource::Plugin {
                plugin: "user-approval".into(),
                form: None,
                summary: None,
                sections: None,
            },
        );
        self.append_event(SessionEvent::UserMessage(message.clone()));
        self.emit_ui(AgentEvent::ContextMessage { message });
    }

    /// plan 模式切换（上游 /plan 命令与 exit_plan_mode 审阅共用的提交路径）：
    /// 落 `plan/mode` 事件（轮终边界语义的立即形态），目标与当前一致时静默；
    /// 模式切换时注入 narration（plugin source plan-mode notice）。
    pub fn set_plan_mode(&self, active: bool) {
        let current = {
            let s = self.session.lock().unwrap();
            s.entries()
                .iter()
                .rev()
                .find_map(|e| match &e.event {
                    SessionEvent::PlanMode { active, .. } => Some(*active),
                    _ => None,
                })
                .unwrap_or(false)
        };
        if current == active {
            return;
        }
        let narrate = true;
        self.append_event(SessionEvent::PlanMode { active });
        self.plan_mode.store(active, Ordering::Relaxed);
        if active {
            *self.plan_entered.lock().unwrap() = true;
        }
        // narration（上游 narration：最后 header 见证与目标不同才注入）
        let text = if active {
            "The user switched this session to plan mode."
        } else {
            "The user switched this session back to the default mode."
        };
        if narrate {
            let msg = dsh_llm::Message::new(
                dsh_llm::Role::User,
                vec![dsh_llm::ContentBlock::text(text)],
                dsh_llm::MessageSource::Plugin {
                    plugin: "plan-mode".into(),
                    form: Some("notice".into()),
                    summary: Some(text.to_string()),
                    sections: None,
                },
            );
            // 上游 agent.inject：通知进收件箱（不唤醒），下一次认领（空闲期
            // 为下一轮 step 1）随批落盘——直写会让 notice 游离在轮间、被记
            // 到上一轮名下（上游日志形不符）。
            self.inject(msg);
        }
    }

    /// plan 模式是否激活（内存折叠值）。
    pub fn plan_mode_active(&self) -> bool {
        self.plan_mode.load(Ordering::Relaxed)
    }

    /// plan 激活态的共享句柄（exit_plan_mode 工具用：离开 plan 模式后的
    /// 调用要在执行侧以「unavailable outside plan mode」失败）。
    pub fn plan_active_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.plan_mode)
    }

    /// 声明面准入（上游条件工具注册的核心语义）：`exit_plan_mode`
    /// 从首次进入 plan 模式起保持声明（`entered`），激活期内自然也在；
    /// 其余工具无条件准入。未准入的调用在派发侧按未知工具拒绝。
    fn tool_admitted(&self, name: &str) -> bool {
        self.tools.admitted(name, &self.admission_context())
    }

    /// 本步的工具准入上下文（plan 折叠态 + 当前路由）。
    fn admission_context(&self) -> dsh_tools::ToolAdmission {
        let (provider, model) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone())
        };
        dsh_tools::ToolAdmission {
            plan_active: self.plan_mode_active(),
            plan_entered: *self.plan_entered.lock().unwrap(),
            provider,
            model,
            preset: self.agent_preset(),
        }
    }

    /// The projection registry this agent folds boundary facts into.
    pub fn projections(&self) -> Arc<SessionProjections> {
        Arc::clone(&self.projections)
    }

    /// Register a side-channel observer for every appended session event
    /// (e.g. JSONL persistence). Called with the session lock released.
    /// 装配工具结果外置策略（上游 spill-policy 的 maxInlineTokens）。
    pub fn set_spill_policy(&self, policy: Option<Arc<dsh_spill::SpillPolicy>>) {
        *self.spill.write().unwrap() = policy;
    }

    /// 装配时间上下文（上游 context/time-context）：每步进入前按节流注入
    /// 一条带时区说明的时间读数。`None` = 不注入。
    pub fn set_time_context(&self, config: Option<time_context::TimeContextConfig>) {
        *self.time_context.write().unwrap() = config.map(|config| time_context::TimeContextState {
            config,
            last_injection_ms: None,
            last_reading_ms: None,
        });
    }

    /// 装配技能加载缝（上游 `tool-skill` 的显式调用面）：`/name` 手势按名取
    /// 已渲染好的 `<skill_content>` 正文；返回 None = 未知或不可由人调用。
    pub fn set_skill_loader(
        &self,
        loader: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    ) {
        *self.skill_loader.lock().unwrap() = Some(loader);
    }

    /// 扫描本步用户消息里的 `/name` 手势，把命中技能的正文追加到消息批尾。
    fn append_skill_invocations(&self, messages: &mut Vec<Message>) {
        let loader = { self.skill_loader.lock().unwrap().clone() };
        let Some(loader) = loader else { return };
        let mut names: Vec<String> = Vec::new();
        for message in messages.iter() {
            // 只有直接用户输入算手势（外部文本无法伪造这个动作）
            if !matches!(message.source, dsh_llm::MessageSource::User) {
                continue;
            }
            for block in &message.content {
                if let ContentBlock::Text { text } = block {
                    for name in dsh_skill::invoked_skill_names(text) {
                        if !names.iter().any(|n| *n == name) {
                            names.push(name);
                        }
                    }
                }
            }
        }
        for name in names {
            if let Some(content) = loader(&name) {
                messages.push(dsh_skill::invocation_message(&name, content));
            }
        }
    }

    /// 装配检查点缝（上游 `session-checkpoint-policy`）：请求派生与顶层
    /// 工具派发之前把已提交的事件刷到稳定存储。`None` = 不检查点。
    pub fn set_checkpoint_sink(&self, sink: Arc<dyn Fn() + Send + Sync>) {
        *self.checkpoint.lock().unwrap() = Some(sink);
    }

    /// 执行一次检查点（失败静默：与上游 fail-closed 不同，rustdsh 的落盘
    /// 失败已在写路径可见，这里不再阻断轮次）。
    fn checkpoint_now(&self) {
        let sink = { self.checkpoint.lock().unwrap().clone() };
        if let Some(sink) = sink {
            sink();
        }
    }

    /// 本步进入时的时间上下文注入（上游 pre-step 的 `time-context` 面）。
    ///
    /// 读数三行 + `snapshot` source 落成持久化的 user-role 上下文消息；
    /// 节流窗口内的步跳过。基线：step 1 = 最近一条模型可见消息的时刻，
    /// 之后 = 上次读数（缺失报 `unavailable`）。
    fn maybe_inject_time_context(&self, turn: u64, step: u64) {
        let mut state = match self.time_context.write().unwrap().clone() {
            Some(state) => state,
            None => return,
        };
        let now = now_ms();
        if !time_context::injection_due(now, state.last_injection_ms, &state.config) {
            return;
        }
        let previous = if step == 1 { *self.last_visible_ms.read().unwrap() } else { state.last_reading_ms };
        let offset_seconds = local_utc_offset_seconds();
        let zone = match &state.config.time_zone {
            Some(zone) => zone.clone(),
            None => time_context::local_zone_label(offset_seconds),
        };
        let reading = time_context::TimeReading {
            timestamp: time_context::format_timestamp(now as i64 / 1000, offset_seconds, &zone),
            zone,
        };
        let text = time_context::render_reading(turn, step, &reading, previous, now);
        let message = Message::new(
            Role::User,
            vec![ContentBlock::text(text.clone())],
            MessageSource::Plugin {
                plugin: "time-context".into(),
                form: Some("snapshot".into()),
                summary: Some(text.lines().next().unwrap_or("").to_string()),
                sections: Some(vec![dsh_llm::ContextSnapshotSection { name: "time-context".into(), text }]),
            },
        );
        state.last_injection_ms = Some(now);
        state.last_reading_ms = Some(now);
        *self.time_context.write().unwrap() = Some(state);
        self.append_event(SessionEvent::UserMessage(message.clone()));
        self.emit_ui(AgentEvent::ContextMessage { message });
    }

    pub fn set_event_sink(&self, sink: impl Fn(SessionEvent) + Send + Sync + 'static) {
        *self.on_event.lock().unwrap() = Some(Box::new(sink));
    }

    /// Append one event to the session log and fan it out to the sink.
    /// Returns the log seq the event took (0 基致密).
    fn append_event(&self, event: SessionEvent) -> u64 {
        let seq = {
            let mut s = self.session.lock().unwrap();
            s.append(event.clone())
        };
        if let Some(sink) = self.on_event.lock().unwrap().as_ref() {
            sink(event);
        }
        seq
    }

    pub fn status(&self) -> AgentStatus {
        *self.status.lock().unwrap()
    }

    /// Subscribe to live UI events, returning the receiver half.
    pub fn subscribe(&self) -> Receiver<AgentEvent> {
        let (tx, rx) = flume::unbounded();
        *self.ui_events.lock().unwrap() = Some(tx);
        rx
    }

    fn emit_ui(&self, event: AgentEvent) {
        if let Some(tx) = self.ui_events.lock().unwrap().as_ref() {
            let _ = tx.send(event);
        }
    }

    fn set_status(&self, status: AgentStatus) {
        let changed = {
            let mut cur = self.status.lock().unwrap();
            if *cur == status {
                false
            } else {
                *cur = status;
                true
            }
        };
        if changed {
            self.events.emit::<AgentStatusChanged>(status);
        }
    }

    /// 把同一输入首次接纳之前的日志行补进收据（游标之后的 `user/message`）。
    ///
    /// 只锁 session 内部，调用方持有 `prompt_receipts` 锁——收据锁始终是
    /// 最外层，避免与驱动侧「先收件箱后日志」的加锁序形成环。
    fn scan_prompt_receipts(&self, receipts: &mut PromptReceipts) {
        let session = self.session.lock().unwrap();
        let entries = session.entries();
        if receipts.offset > entries.len() {
            // 防御：日志不该缩短；真发生了就整段重扫（换会话路径已显式重置）
            receipts.offset = 0;
            receipts.accepted.clear();
        }
        for entry in &entries[receipts.offset..] {
            if let SessionEvent::UserMessage(message) = &entry.event {
                receipts.accepted.insert(message.id.clone());
            }
        }
        receipts.offset = entries.len();
    }

    /// Queue a message and wake the driver.
    ///
    /// 同一 `MessageId` 的重复投递只接纳一次（上游 8706daa679：prompt 重试
    /// 在输入被接纳后按请求身份去重，含已被认领或已从队列移除的消息）。
    /// 返回是否本次新接纳；去重时既不重复入列、不重复发 `agent/inbox/inserted`
    /// 也不重复唤醒驱动。
    pub fn send(&self, message: Message, target: InboxTarget) -> bool {
        let mut receipts = self.prompt_receipts.lock().unwrap();
        self.scan_prompt_receipts(&mut receipts);
        if receipts.accepted.contains(&message.id) {
            return false;
        }
        {
            // 收据锁内完成「查重 + 入列」，并发重试只有一条能进来。
            let mut inbox = self.inbox.lock().unwrap();
            if inbox.contains(&message.id.0) {
                receipts.accepted.insert(message.id.clone());
                return false;
            }
            inbox.append(target, message.clone());
            // 入列即接纳（rustdsh 的首次持久插入点；日志行在认领时落盘，
            // 收据先记，认领/移除都不会撤销接纳）。
            receipts.accepted.insert(message.id.clone());
        }
        drop(receipts);
        self.events.emit::<AgentInboxInserted>(message);
        self.wake.notify_one();
        true
    }

    /// Queue a user prompt as the next turn.
    pub fn followup(&self, text: impl Into<String>) {
        self.send(user_message(text), InboxTarget::NextTurn);
    }

    /// Queue input for the next step boundary.
    pub fn steer(&self, text: impl Into<String>) {
        self.send(user_message(text), InboxTarget::NextStep);
    }

    /// 排队待下一轮的快照（上游 inbox next-turn 投影的读面——QueueDock
    /// 呈现与测试断言用）。
    pub fn queued_next_turn(&self) -> Vec<Message> {
        self.inbox.lock().unwrap().next_turn().to_vec()
    }

    /// 插话提升一条排队消息入当前轮（上游 queue.steer 消费语义：
    /// next-turn → next-step，本轮下个步界消费）。未命中 false。
    pub fn promote_queued(&self, message_id: &str) -> bool {
        let mut inbox = self.inbox.lock().unwrap();
        let hit = inbox.promote(message_id);
        drop(inbox);
        if hit {
            // 空闲驱动不消费 next-step 之外的提升语义：唤醒兜底
            self.wake.notify_one();
        }
        hit
    }

    /// 全部提升（上游空稿加速手势 steerQueue）。返回提升条数。
    pub fn promote_all_queued(&self) -> usize {
        let mut inbox = self.inbox.lock().unwrap();
        let n = inbox.promote_all();
        drop(inbox);
        if n > 0 {
            self.wake.notify_one();
        }
        n
    }

    /// 编辑排队消息（上游 queue.edit——仅纯文本行，内容整体替换）。
    pub fn edit_queued(&self, message_id: &str, new_text: &str) -> bool {
        self.inbox.lock().unwrap().edit(message_id, new_text)
    }

    /// 删除排队消息（上游 queue.remove）。
    pub fn remove_queued(&self, message_id: &str) -> bool {
        self.inbox.lock().unwrap().remove(message_id)
    }

    /// Inject context without waking the driver.
    pub fn inject(&self, message: Message) {
        self.inbox.lock().unwrap().append(InboxTarget::NextStep, message.clone());
        self.events.emit::<AgentInboxInserted>(message);
    }

    /// Cancel in-flight work and clear pending input.
    pub fn cancel(&self) {
        let discarded = {
            let mut inbox = self.inbox.lock().unwrap();
            let mut all = inbox.next_turn().to_vec();
            all.extend_from_slice(inbox.next_step());
            inbox.clear();
            all
        };
        for m in discarded {
            self.events.emit::<AgentInboxDiscarded>(m);
        }
        self.abort.lock().unwrap().abort();
    }

    /// Spawn the driver task on the ambient tokio runtime.
    pub fn spawn(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move { this.drive().await });
    }

    /// Wait until the driver is idle with no pending inbox work.
    pub async fn when_idle(&self) {
        while self.status() == AgentStatus::Idle && self.inbox.lock().unwrap().has_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        while self.status() != AgentStatus::Idle {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    async fn drive(&self) {
        loop {
            self.wake.notified().await;
            *self.abort.lock().unwrap() = AbortSignal::new();
            self.set_status(AgentStatus::Running);
            while self.inbox.lock().unwrap().has_pending() && !self.abort.lock().unwrap().aborted() {
                self.run_turn().await;
            }
            self.set_status(AgentStatus::Idle);
        }
    }

    /// 当前请求信封（系统提示词 + 准入工具声明）。占用表等只读界面用它
    /// 给「下一次请求」定价，与驱动发请求时组装的同一函数。
    pub fn prompt_assembly(&self) -> PromptAssembly {
        self.assemble_prompt()
    }

    fn assemble_prompt(&self) -> PromptAssembly {
        let mut parts: Vec<String> = Vec::new();
        let base = {
            let o = self.options.read().unwrap();
            o.system_prompt.clone().unwrap_or_default()
        };
        if !base.is_empty() {
            parts.push(base);
        }
        let sections = self.prompt.render();
        if !sections.is_empty() {
            parts.push(sections);
        }
        // 运行时语境：sandbox:policy + approval:policy（上游语境逐字；工作区
        // 根 = 会话工作目录，档位随宿主切换在下一次请求组装生效——set_session
        // 折叠恢复，权限预设切换走 policy() 显式更新）
        {
            let o = self.options.read().unwrap();
            if let Some(sentence) = self.policy.sandbox_sentence(o.workdir.get().as_deref()) {
                parts.push(sentence);
            }
            if let Some(sentence) = self.policy.approval_sentence() {
                parts.push(sentence);
            }
        }
        PromptAssembly {
            system: parts.join("\n\n"),
            // 按步准入（上游条件工具注册）：未准入的工具既不出现在声明
            // 里，也不出现在指引/派发面（调用按未知工具失败）
            tools: self
                .tools
                .list_admitted(&self.admission_context())
                .iter()
                .map(|t| t.definition().to_schema())
                .collect(),
        }
    }

    /// 轮次起点的上下文压缩：估算 token 超阈值时，选影子区（工具配对安全
    /// 边界）、以当前路由原样重放前缀 + 压缩指令发起辅助调用
    /// （`AuxiliaryPurpose::Compaction`），摘要落 `SessionEvent::Compaction`
    /// （derive_messages 侧生效）。失败静默跳过。
    async fn maybe_compact(&self, assembly: &PromptAssembly) {
        let config = { self.options.read().unwrap().compaction };
        if config.threshold_tokens == 0 {
            return;
        }
        let pairs = {
            let s = self.session.lock().unwrap();
            s.derive_messages_with_seqs()
        };
        if pairs.len() <= config.keep_recent {
            return;
        }
        let msgs: Vec<Message> = pairs.iter().map(|(_, m)| m.clone()).collect();
        if dsh_compaction::estimate_tokens(&msgs) <= config.threshold_tokens {
            return;
        }
        // 触发确认后才剪枝（上游顺序）：把超阈值的历史工具结果换成
        // 头 + 标记 + 尾，写进内存面；压缩输入随之变小。
        let pruned_any = self.prune_tool_results(&pairs);
        let pairs = if pruned_any {
            let overlay = self.pruned.read().unwrap().clone();
            pairs
                .into_iter()
                .map(|(seq, m)| (seq, overlay.get(&seq).cloned().unwrap_or(m)))
                .collect::<Vec<_>>()
        } else {
            pairs
        };
        self.compact_pairs(assembly, &pairs).await;
    }

    /// 剪枝内存面：逐条 `tool/result` 按码点预算换掉过长的中段。
    /// 返回是否发生了替换（上游 `pruneSession` 的模型可见等价）。
    fn prune_tool_results(&self, pairs: &[(u64, Message)]) -> bool {
        let config = dsh_spill::prune::PruneConfig::default();
        let mut changed = false;
        let mut overlay = self.pruned.write().unwrap();
        for (seq, message) in pairs {
            let has_result = message
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if !has_result {
                continue;
            }
            let mut next = message.clone();
            for block in next.content.iter_mut() {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if let Ok(Some(pruned)) = dsh_spill::prune::prune_content(content, config) {
                        *content = pruned;
                        changed = true;
                    }
                }
            }
            if changed {
                overlay.insert(*seq, next);
            }
        }
        changed
    }

    /// 影子区压缩核心（自动阈值路径与手动 /compact 共用）：边界选择 →
    /// 辅助摘要 → 落 `SessionEvent::Compaction` + UI 通知；失败静默。
    async fn compact_pairs(&self, assembly: &PromptAssembly, pairs: &[(u64, Message)]) {
        let config = { self.options.read().unwrap().compaction };
        let msgs: Vec<Message> = pairs.iter().map(|(_, m)| m.clone()).collect();
        let boundary = dsh_compaction::select_boundary(&msgs, config.keep_recent);
        if boundary == 0 {
            return;
        }
        let before_seq = pairs[boundary - 1].0;
        let shadowed: Vec<Message> = pairs[..boundary].iter().map(|(_, m): &(u64, Message)| m.clone()).collect();
        let (provider, model) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone())
        };
        let signal = self.abort.lock().unwrap().clone();
        match dsh_compaction::summarize_with_llm(
            &self.llm,
            &provider,
            &model,
            Some(assembly.system.clone()),
            Some(assembly.tools.clone()),
            &shadowed,
            signal,
        )
        .await
        {
            Ok(summary) => {
                // v2 富形：影子区边界/清单/token 估算/路由（上游
                // compaction/summary 必填七件套）
                self.append_event(SessionEvent::compaction(
                    before_seq,
                    summary,
                    format!("cp-{}", uuid_simple()),
                    pairs.first().map(|(s, _)| *s).unwrap_or(0),
                    pairs[..boundary].iter().map(|(s, _)| *s).collect(),
                    dsh_compaction::estimate_tokens(&shadowed) as u64,
                    provider.clone(),
                    model.clone(),
                ));
                self.emit_ui(AgentEvent::Compacted { shadowed_messages: boundary });
            }
            Err(_) => {}
        }
    }

    /// 手动压缩（上游 /compact 指令位）：无视 token 阈值立即压缩影子区。
    /// 仅空闲态生效（发起后与新轮并发的小窗口与 steer 同语义，接受）；
    /// 会话过小（≤ keep_recent 条消息）或摘要失败静默跳过。
    pub fn compact_now(self: &Arc<Self>) {
        if self.status() != AgentStatus::Idle {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let config = { this.options.read().unwrap().compaction };
            let pairs = {
                let s = this.session.lock().unwrap();
                s.derive_messages_with_seqs()
            };
            if pairs.len() <= config.keep_recent {
                return;
            }
            let assembly = this.assemble_prompt();
            this.compact_pairs(&assembly, &pairs).await;
        });
    }

    async fn run_turn(&self) {
        // 上游 agent.ts：lastTurn 从 turnBoundary 投影读取（key 恒在——
        // 本 crate 注册了它），缺席回退 0 只是类型层面的兜底。
        let turn = {
            let s = self.session.lock().unwrap();
            self.projections
                .state_of(&s, "turnBoundary")
                .and_then(|v| v.get("lastTurn").and_then(|t| t.as_u64()))
                .unwrap_or(0)
                + 1
        };
        self.append_event(SessionEvent::TurnStart { turn });
        self.emit_ui(AgentEvent::TurnStarted { turn });

        // 上下文压缩：轮次起点检查估算 token，超阈值则摘要影子区
        // （失败尽力而为——本轮按未压缩继续）
        let assembly = self.assemble_prompt();
        self.maybe_compact(&assembly).await;

        let mut target = InboxTarget::NextTurn;
        let mut turn_ends: Option<TurnEndReason> = None;
        let mut step = 0u64;

        loop {
            let signal = self.abort.lock().unwrap().clone();
            if signal.aborted() {
                turn_ends = Some(TurnEndReason::Aborted { cancel_cause: dsh_session::CancelCause::Legacy });
                break;
            }

            step += 1;
            let claimed = { self.inbox.lock().unwrap().claim(target, turn) };
            for m in &claimed {
                self.events.emit::<AgentInboxClaimed>(InboxClaimedPayload { message: m.clone(), turn });
            }

            // agent/pre-step waterfall: reject or rewrite the claimed batch.
            let input = PreStepInput { messages: claimed, turn, step, signal: signal.clone() };
            let decision = self
                .events
                .waterfall::<AgentPreStep>(input, |input| {
                    Box::pin(async move { PreStepDecision::Enter { messages: input.messages } })
                })
                .await;

            let messages = match decision {
                PreStepDecision::Reject => {
                    turn_ends = Some(TurnEndReason::Blocked);
                    break;
                }
                PreStepDecision::Enter { messages } => messages,
            };

            // repeat-tool-reminder 重置钩（上游 agent/pre-step 监听同位）：
            // 本步输入含 user 源消息 → 连击链清零（用户插话改变语境，跨
            // 插话的重复不是循环）
            if messages.iter().any(|m| matches!(m.source, MessageSource::User)) {
                self.repeat_guard.reset();
            }

            if step > 1 && turn_ends.is_some() && messages.is_empty() {
                break;
            }
            if step == 1 && messages.is_empty() {
                turn_ends = Some(TurnEndReason::Completed);
                break;
            }

            let assembly = self.assemble_prompt();
            // 上游 agent.ts 落盘序：step/start → system commits → user 批。
            self.append_event(SessionEvent::StepStart { turn, step, time_ms: None });
            // 时间上下文（上游 context/time-context 的 entering pre-step 面）
            self.maybe_inject_time_context(turn, step);
            // v3 system prompt 面节点：rustdsh 的 provider 路线走请求 system
            // 参数（非 in-history），按上游 SystemPromptProjection 归一化语义
            // ——无 head 则 append（空 prompt 也保留 head），变化则精确替换
            // head（上游验证要求 replace 端点恰为当前 head）。
            {
                let rendered = assembly.system.clone();
                let mut head = self.system_head.lock().unwrap();
                match head.as_ref() {
                    None => {
                        let seq = self.append_event(SessionEvent::SystemMessage {
                            turn,
                            step,
                            message: system_prompt_message(&rendered),
                            replace: None,
                        });
                        *head = Some((seq, rendered));
                    }
                    Some((seq, text)) if *text != rendered => {
                        let seq = *seq;
                        let new_seq = self.append_event(SessionEvent::SystemMessage {
                            turn,
                            step,
                            message: system_prompt_message(&rendered),
                            replace: Some(seq),
                        });
                        *head = Some((new_seq, rendered));
                    }
                    _ => {}
                }
            }
            // 模型切换公告（上游 model-selection.ts pre-step 通知）：本步有新增
            // 消息且本轮路由与上一持久化 request/header 不同时，在消息批尾部追加
            // 一条公告；首次请求与 effort-only 变化不公告。公告随消息落盘并自然
            // 进入由日志派生的请求历史。
            let mut messages = messages;
            {
                let selected = {
                    let o = self.options.read().unwrap();
                    (o.provider.clone(), o.model.clone())
                };
                let previous = {
                    let s = self.session.lock().unwrap();
                    s.request_header().map(|h| (h.config.provider.clone(), h.config.model.clone()))
                };
                if let Some(prev) = previous
                    && (prev.0 != selected.0 || prev.1 != selected.1)
                {
                    messages.push(model_switch_notice_message(
                        (&prev.0, &prev.1),
                        (&selected.0, &selected.1),
                    ));
                }
            }
            // 用户显式技能调用（上游 `skill-invocation`）：用户消息里整词形如
            // `/name` 且该技能存在、可被人调用时，把 `<skill_content>` 正文作为
            // 本步的注入上下文追加在**最后**——背景材料在前，模型该照做的在
            // 最后、离答案最近。未知名与 user-invocable=false 保持普通文本。
            self.append_skill_invocations(&mut messages);
            *self.last_visible_ms.write().unwrap() = Some(now_ms());
            for m in &messages {
                self.append_event(SessionEvent::UserMessage(m.clone()));
                // 非 user source（认领的注入通知、model-switch 公告）随批
                // 落盘的同时实时入列 UI（context 行）；重载回放走日志同形。
                if !matches!(m.source, dsh_llm::MessageSource::User) {
                    self.emit_ui(AgentEvent::ContextMessage { message: m.clone() });
                }
            }

            let step_end = self.run_step(turn, step, &assembly, &signal).await;

            self.append_event(SessionEvent::StepEnd { turn, step });

            if let StepEnd::Concluded(reason) = step_end
                && !matches!(turn_ends, Some(TurnEndReason::MaxTokens)) {
                    turn_ends = Some(reason);
                }

            if turn_ends.is_some() && !self.inbox.lock().unwrap().has_pending_next_step() {
                self.events
                    .serial::<AgentTurnStopping>(TurnStoppingPayload { turn, signal: signal.clone() })
                    .await;
                break;
            }
            target = InboxTarget::NextStep;
        }

        let reason = turn_ends.unwrap_or(TurnEndReason::Completed);
        self.append_event(SessionEvent::TurnEnd { turn, reason: reason.clone() });
        self.emit_ui(AgentEvent::TurnEnded { turn, reason });
    }

    /// 图片卸载选择器（上游 offloadOldestImages）：按会话日志序（=请求序）
    /// 遍历输入消息事件，深度优先数图片出现（含嵌套 tool-result 与已
    /// 卸载），选最旧的未卸载出现共 `count` 个，落一条 image/offload 耐久
    /// 决定（targets 非空才落）。返回是否有可卸载。
    fn offload_oldest_images(&self, mut count: usize) -> bool {
        if count == 0 {
            return false;
        }
        let mut targets: Vec<dsh_session::ImageOffloadTarget> = Vec::new();
        {
            let session = self.session.lock().unwrap();
            for entry in session.entries() {
                if count == 0 {
                    break;
                }
                // 输入消息节点（上游：user/message 与 tool/result；assistant
                // 排除——模型输出不卸载）
                let message = match &entry.event {
                    SessionEvent::UserMessage(m) => m,
                    SessionEvent::ToolResult { message, .. } => message,
                    _ => continue,
                };
                let mut indexes: Vec<usize> = Vec::new();
                let mut image_index = 0usize;
                // 深度优先遍历（含嵌套 tool-result；已卸载的也计数但不选中）
                fn visit(blocks: &[dsh_llm::ContentBlock], index: &mut usize, count: &mut usize, picked: &mut Vec<usize>) {
                    for b in blocks {
                        if *count == 0 {
                            return;
                        }
                        match b {
                            dsh_llm::ContentBlock::Image { offloaded, .. } => {
                                if !*offloaded {
                                    picked.push(*index);
                                    *count -= 1;
                                }
                                *index += 1;
                            }
                            dsh_llm::ContentBlock::ToolResult { content, .. } => {
                                visit(content, index, count, picked);
                            }
                            _ => {}
                        }
                    }
                }
                visit(&message.content, &mut image_index, &mut count, &mut indexes);
                if !indexes.is_empty() {
                    targets.push(dsh_session::ImageOffloadTarget { seq: entry.seq, image_indexes: indexes });
                }
            }
        }
        if targets.is_empty() {
            return false;
        }
        self.append_event(SessionEvent::ImageOffload { targets });
        true
    }

    async fn run_step(
        &self,
        turn: u64,
        step: u64,
        assembly: &PromptAssembly,
        signal: &AbortSignal,
    ) -> StepEnd {
        let (provider, model) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone())
        };

        loop {
            let options = self.build_request(turn, step, assembly, signal).await;
            let mut stream = match self.llm.stream(options).await {
                Ok(s) => s,
                Err(e) => {
                    // 图片预算恢复（4c，上游 compaction-image-offload 执行器）：
                    // IMAGE_OFFLOAD_REQUIRED 携带需卸载数——按请求序选最旧
                    // 未卸载出现落 image/offload 耐久决定，本步免费重试；
                    // 无可卸载时按普通错误收束
                    if e.code == "IMAGE_OFFLOAD_REQUIRED"
                        && let Some(count) = e.failure.offload_images
                        && count > 0
                        && self.offload_oldest_images(count)
                    {
                        continue;
                    }
                    self.report_error(turn, step, &e.message, &e.code);
                    return StepEnd::Concluded(TurnEndReason::Error { failure: *e.failure });
                }
            };

            let mut assembler = BlockAssembler::new();
            while let Some(chunk) = stream.next().await {
                if signal.aborted() {
                    let content = assembler.interrupted_blocks();
                    if !content.is_empty() {
                        let message = Message::assistant(content, &provider, &model);
                        self.append_event(SessionEvent::AssistantMessage {
                            turn,
                            step,
                            message: message.clone(),
                            interrupted: true,
                            usage: assembler.usage().cloned(),
                            time_ms: None,
                        });
                    }
                    return StepEnd::Concluded(TurnEndReason::Aborted { cancel_cause: dsh_session::CancelCause::Legacy });
                }

                self.append_event(SessionEvent::AssistantChunk {
                    turn,
                    step,
                    chunk: chunk.clone(),
                });
                match &chunk {
                    StreamChunk::TextDelta { text, .. } => {
                        self.emit_ui(AgentEvent::TextDelta { text: text.clone() });
                    }
                    StreamChunk::ReasoningDelta { text, .. } => {
                        self.emit_ui(AgentEvent::ReasoningDelta { text: text.clone() });
                    }
                    _ => {}
                }
                assembler.push(chunk);
            }

            let finish = assembler.finish();
            if let FinishReason::Error { failure } | FinishReason::Aborted { failure } = &finish {
                // agent/request-error waterfall: a listener may elect to retry.
                let action = self
                    .events
                    .waterfall::<AgentRequestError>(
                        RequestErrorPayload {
                            turn,
                            step,
                            provider: provider.clone(),
                            failure: failure.clone(),
                            retry_policy: self.llm.provider_retry_policy(&provider),
                            signal: signal.clone(),
                        },
                        |_| Box::pin(async move { None }),
                    )
                    .await;
                if matches!(action, Some(RequestErrorAction::Retry)) {
                    continue;
                }
                self.report_error(turn, step, &failure.message, &failure.code);
                return StepEnd::Concluded(TurnEndReason::Error { failure: failure.clone() });
            }

            let blocks = match assembler.blocks() {
                Ok(b) => b,
                Err(e) => {
                    self.report_error(turn, step, &e.message, &e.code);
                    return StepEnd::Concluded(TurnEndReason::Error { failure: *e.failure });
                }
            };

            let usage = assembler.usage().cloned();
            let message = Message::assistant(blocks, &provider, &model);
            self.append_event(SessionEvent::AssistantMessage {
                turn,
                step,
                message: message.clone(),
                interrupted: false,
                usage: usage.clone(),
                time_ms: None,
            });
            self.emit_ui(AgentEvent::AssistantMessage { message: message.clone(), usage });

            if matches!(finish, FinishReason::MaxTokens) {
                return StepEnd::Concluded(TurnEndReason::MaxTokens);
            }

            let tool_calls: Vec<(CallId, String, String)> = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { id, name, arguments } => {
                        Some((id.clone(), name.clone(), arguments.clone()))
                    }
                    _ => None,
                })
                .collect();

            if tool_calls.is_empty() {
                return StepEnd::Concluded(TurnEndReason::Completed);
            }

            let concluded = self.execute_tool_calls(turn, step, &tool_calls, signal).await;
            return if concluded {
                StepEnd::Concluded(TurnEndReason::Completed)
            } else {
                StepEnd::NeedsAnotherStep
            };
        }
    }

    fn report_error(&self, turn: u64, step: u64, message: &str, code: &str) {
        self.events.emit::<AgentErrorOccurred>(AgentErrorPayload {
            turn,
            step,
            message: message.to_string(),
            code: code.to_string(),
        });
        self.emit_ui(AgentEvent::Error { message: message.to_string(), code: code.to_string() });
    }

    async fn execute_tool_calls(
        &self,
        turn: u64,
        step: u64,
        tool_calls: &[(CallId, String, String)],
        _signal: &AbortSignal,
    ) -> bool {
        // 顶层工具派发前检查点（上游：已记录的调用先变耐久，再执行外部副作用）
        self.checkpoint_now();
        let mut concluded = false;
        for (id, name, arguments) in tool_calls {
            self.emit_ui(AgentEvent::ToolCall {
                tool_call_id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            });
            self.append_event(SessionEvent::ToolCall {
                turn,
                step,
                call_id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                // live 无回传承载（写侧 envelope time 为权威；重载时读侧
                // 塞回）——session_stats_fold 的 toolMs 配对用
                time_ms: None,
            });

            let input =
                ToolExecutionInput::with_raw_arguments(id.clone(), name.clone(), arguments.clone());
            // 未准入 = 未知工具（上游 UNKNOWN_TOOL）：声明面缺席的工具
            // 即使模型硬调用也不执行
            let result: ToolExecutionResult = if !self.tool_admitted(name) {
                ToolExecutionResult::error(format!("unknown tool \"{name}\""))
            } else {
                match self.tools.execute(&input).await {
                    Some(r) => r,
                    None => ToolExecutionResult::error(format!("no tool \"{name}\"")),
                }
            };
            concluded |= result.concludes_turn;

            // 工具结果外置（上游 spill-policy 的 post-execute 面）：超预算的
            // 文本/图片结果存到 spill 后端、模型只看到头尾预览 + 定位符；
            // 存储失败/通知放不下时保留原文（策略永不吞结果）。
            let mut result = result;
            if let Some(policy) = self.spill.read().unwrap().clone() {
                let session_id = self.session.lock().unwrap().id.as_str().to_string();
                policy.apply(&session_id, name, id.as_str(), &mut result);
            }

            let msg = Message::tool_result(id.clone(), result.content.clone(), result.is_error);
            // 工具的 UI 元数据缝（上游 output.presentationMeta 投影等价）随
            // tool/result 落盘，历史回放的 diff 卡等从这里恢复
            self.append_event(SessionEvent::ToolResult {
                turn,
                step,
                message: msg,
                time_ms: None,
                presentation: result.presentation.clone(),
            });
            self.emit_ui(AgentEvent::ToolResult {
                tool_call_id: id.clone(),
                is_error: result.is_error,
            });

            // repeat-tool-reminder（上游 tools/post-execute 面，先计数后看
            // 结果——拒绝/未知的调用同样计数）：恰命中阈值注入提醒
            // （user/plugin repeat-tool-reminder notice 耐久消息，下一步可见）
            if let Some(reminder) = self.repeat_guard.observe(name, arguments) {
                let message = Message::new(
                    Role::User,
                    vec![ContentBlock::text(reminder.text)],
                    MessageSource::Plugin {
                        plugin: "repeat-tool-reminder".into(),
                        form: Some("notice".into()),
                        summary: Some(reminder.summary),
                        sections: None,
                    },
                );
                self.append_event(SessionEvent::UserMessage(message.clone()));
                self.emit_ui(AgentEvent::ContextMessage { message });
            }
        }
        concluded
    }

    async fn build_request(
        &self,
        turn: u64,
        step: u64,
        assembly: &PromptAssembly,
        signal: &AbortSignal,
    ) -> GenerateOptions {
        let (provider, model, max_tokens) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone(), o.max_tokens)
        };
        let mut seed = LlmCallConfig::new(&provider, &model);
        seed.max_tokens = max_tokens;

        // agent/request waterfall: replace or amend the proposed config.
        let config = {
            let seed = seed.clone();
            self.events
                .waterfall::<AgentRequest>(
                    AgentRequestPayload { turn, step, signal: signal.clone() },
                    move |_| Box::pin(async move { seed }),
                )
                .await
        };

        let system = if assembly.system.is_empty() { None } else { Some(assembly.system.clone()) };
        let tools = if assembly.tools.is_empty() { None } else { Some(assembly.tools.clone()) };

        let (boundary, session_id) = {
            let s = self.session.lock().unwrap();
            (s.derive_messages(), s.id.clone())
        };
        // 剪枝内存面叠加（上游 compaction/prune 的模型可见等价：日志保留
        // 原文，请求看到的是剪枝后的结果）——按 seq 对齐后替换。
        let boundary = {
            let overlay = self.pruned.read().unwrap();
            if overlay.is_empty() {
                boundary
            } else {
                let seqs = {
                    let s = self.session.lock().unwrap();
                    s.derive_messages_with_seqs()
                        .into_iter()
                        .map(|(seq, _)| seq)
                        .collect::<Vec<_>>()
                };
                boundary
                    .into_iter()
                    .zip(seqs)
                    .map(|(message, seq)| overlay.get(&seq).cloned().unwrap_or(message))
                    .collect()
            }
        };
        // 请求组装：FileBlock → 确定性 handle 文本（上游
        // projectFilesToText 在所有 provider 路由前无条件执行——provider
        // 永远收不到文件字节，模型按需用现有文件工具读存储副本）
        let boundary = {
            let root = self.options.read().unwrap().attachments_root.clone();
            match root {
                Some(root) => {
                    let store = dsh_persist::AttachmentStore::new(root);
                    dsh_llm::project_files_to_text(boundary, &|ref_: &dsh_llm::FileAttachmentRef| -> Option<String> {
                        store
                            .file_host_path(ref_)
                            .map(|p| p.to_string_lossy().into_owned())
                    })
                }
                None => dsh_llm::project_files_to_text(boundary, &|_| None),
            }
        };

        // v3：system prompt 经 system/message 行持久化，request/header 不再
        // 携带 system 字段（上游 canonicalizeTransformedEvent 语义）；模型
        // 请求本身仍走 options.system。
        let header = EpochHeader {
            config: config.clone(),
            system: None,
            tools: tools.clone(),
        };
        let (changed, reason) = {
            let s = self.session.lock().unwrap();
            match s.request_header() {
                None => (true, HeaderReason::Initial),
                Some(h) => (*h != header, HeaderReason::Change),
            }
        };
        if changed {
            self.append_event(SessionEvent::RequestHeader { header, reason });
        }

        let mut options = GenerateOptions::new(provider, model, boundary);
        options.system = system;
        options.tools = tools;
        options.max_tokens = config.max_tokens;
        options.temperature = config.temperature;
        options.signal = signal.clone();
        options.session_id = Some(session_id);
        options
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{MessageSource, Role};

    #[test]
    fn route_label_shows_model_only_for_same_provider() {
        assert_eq!(route_label("p", "m1", "p"), "m1");
        assert_eq!(route_label("p1", "m", "p2"), "p1/m");
    }

    /// 上游 modelSwitchNotice 模板与 source 形（48cc1cf1d6）。
    #[test]
    fn model_switch_notice_matches_upstream_template() {
        let m = model_switch_notice_message(
            ("deepseek-official", "deepseek-v4-flash"),
            ("deepseek-official", "deepseek-v4-pro"),
        );
        assert_eq!(m.role, Role::User);
        match &m.source {
            MessageSource::Plugin { plugin, form, summary, sections } => {
                assert_eq!(plugin, "model-selection");
                assert_eq!(form.as_deref(), Some("notice"));
                assert_eq!(summary.as_deref(), Some("deepseek-v4-flash → deepseek-v4-pro"));
                assert!(sections.is_none());
            }
            other => panic!("plugin source expected, got {other:?}"),
        }
        match &m.content[0] {
            ContentBlock::Text { text } => assert_eq!(
                text,
                "[model changed: assistant turns above this point were generated by deepseek-v4-flash; the session continues with deepseek-v4-pro]",
            ),
            other => panic!("text block expected, got {other:?}"),
        }
    }
}

/// 当前 epoch 毫秒。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// 本地 UTC 偏移（秒）。Windows 下按当前时刻取 bias：DST 差异由系统时区
/// 规则给出，rustdsh 不自行维护时区库。
fn local_utc_offset_seconds() -> i32 {
    #[cfg(windows)]
    {
        use std::os::raw::c_int;
        // GetTimeZoneInformation 的 TIME_ZONE_INFORMATION：bias 在前，标准/夏令时
        // 两段各 8 个 WORD 后跟名称。这里只需要 bias 与夏令时 bias。
        #[repr(C)]
        #[derive(Default)]
        struct TzInfo {
            bias: c_int,
            standard_name: [u16; 32],
            standard_date: [u16; 8],
            standard_bias: c_int,
            daylight_name: [u16; 32],
            daylight_date: [u16; 8],
            daylight_bias: c_int,
        }
        unsafe extern "system" {
            fn GetTimeZoneInformation(info: *mut TzInfo) -> u32;
        }
        let mut info = TzInfo::default();
        let result = unsafe { GetTimeZoneInformation(&mut info) };
        // 2 = TIME_ZONE_ID_DAYLIGHT
        let extra = if result == 2 { info.daylight_bias } else { 0 };
        // bias 是「本地→UTC」的分钟差，取反才是 UTC→本地
        -(info.bias + extra) * 60
    }
    #[cfg(not(windows))]
    {
        // 无 libc 依赖：按进程时区的 GMT 偏移自零点起算不可得，回落到 0（UTC）。
        // 模型仍能拿到 UTC 时间戳与 Etc/GMT 标签，语义明确不误导。
        0
    }
}
