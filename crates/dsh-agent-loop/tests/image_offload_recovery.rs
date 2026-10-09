//! 端到端验收：图片预算恢复循环（4c，上游 compaction-image-offload）——
//!
//! 适配器首请求以 IMAGE_OFFLOAD_REQUIRED 携带需卸载数失败 → 恢复执行器
//! 按请求序选最旧未卸载图片落 `image/offload` 耐久决定 → 本步免费重试 →
//! 重试请求中该图片投影为占位文本（无图片字节）→ 轮正常完成。
//! 无可卸载出现时按普通错误收束（不无限重试）。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmFailure, LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

/// 首请求 IMAGE_OFFLOAD_REQUIRED（offload=1），其后每请求成功。
#[derive(Default)]
struct OffloadingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for OffloadingAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let n = { let mut r = self.requests.lock().unwrap(); r.push(options); r.len() };
        if n == 1 {
            let mut failure = LlmFailure {
                message: "request images exceed the budget; 1 more oldest occurrence(s) must be offloaded."
                    .into(),
                code: "IMAGE_OFFLOAD_REQUIRED".into(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: Some(1),
            };
            failure.offload_images = Some(1);
            return Err(LlmError::from_failure(failure));
        }
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn image_block(id: &str) -> ContentBlock {
    ContentBlock::Image {
        offloaded: false,
        attachment: dsh_llm::ImageAttachmentRef {
            attachment_id: format!("sha256:{id}"),
            name: None,
            media_type: "image/png".into(),
            bytes: 16 * 1024 * 1024,
            width: 4,
            height: 4,
            original_dimensions: None,
        },
    }
}

#[tokio::test]
async fn image_offload_required_recovers_with_durable_event_and_retry() {
    let dir = std::env::temp_dir().join(format!("dsh-loop-offload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(OffloadingAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = Arc::new(ReactLoopAgent::new(
        SessionId::new("session-offload"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    ));
    let sink_agent = Arc::clone(&agent);
    let sink_recorder = Arc::clone(&recorder);
    agent.set_event_sink(move |event| {
        let id = sink_agent.session().lock().unwrap().id.clone();
        let _ = sink_recorder.append(&id, "/tmp/ws", &event);
    });
    agent.spawn();

    // 用户消息携带一张图
    let msg = dsh_llm::Message::user(vec![image_block("aa")]);
    agent.send(msg, dsh_agent::InboxTarget::NextTurn);
    agent.when_idle().await;

    // ① 两次请求：首请求失败 + 恢复重试
    let requests = adapter.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "failed request + free retry");
    // ② 重试请求：图片带 offloaded=true（占位投影发生在 DeepSeek 适配器
    // 序列化面——image_request.rs offloaded_images_project 测；本测的
    // mock 适配器见到的即是已标记的内存形）
    let retry = &requests[1];
    let marked = retry.messages.iter().any(|m| {
        m.content.iter().any(|b| matches!(b, ContentBlock::Image { offloaded: true, .. }))
    });
    assert!(marked, "retry request carries the offloaded mark");
    let unmarked = retry.messages.iter().any(|m| {
        m.content.iter().any(|b| matches!(b, ContentBlock::Image { offloaded: false, .. }))
    });
    assert!(!unmarked, "no unoffloaded occurrence remains");
    drop(requests);

    // ③ 耐久决定落盘：image/offload targets 指向用户消息事件的图片
    let (session, _) = recorder
        .load(&SessionId::new("session-offload"), Some("/tmp/ws"))
        .expect("session loads");
    let offload = session
        .entries()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::ImageOffload { targets } => Some(targets.clone()),
            _ => None,
        })
        .expect("image/offload event persisted");
    assert_eq!(offload.len(), 1);
    assert_eq!(offload[0].image_indexes, vec![0]);

    // ④ 派生应用：重载后图片标记 offloaded（重试请求同形）
    let derived = session.derive_messages();
    match &derived[0].content[0] {
        ContentBlock::Image { offloaded, .. } => assert!(*offloaded),
        other => panic!("expected marked image, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn offload_required_with_no_images_left_concludes_as_error() {
    let dir = std::env::temp_dir().join(format!("dsh-loop-offload-e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(NoImageAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = Arc::new(ReactLoopAgent::new(
        SessionId::new("session-offload-e"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    ));
    agent.spawn();

    // 纯文本消息 + 无可卸载的失败 → 不重试，按错误收束
    agent.followup("hello");
    agent.when_idle().await;

    let requests = adapter.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "no retry when nothing to offload");
    drop(requests);
    let session = agent.session();
    let session = session.lock().unwrap();
    assert!(
        session.entries().iter().any(|e| matches!(&e.event, SessionEvent::TurnEnd { reason: dsh_session::TurnEndReason::Error { .. }, .. })),
        "turn concludes with error"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 恒以 IMAGE_OFFLOAD_REQUIRED(1) 失败（无可恢复 → 上游 delegate 语义）。
#[derive(Default)]
struct NoImageAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for NoImageAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        self.requests.lock().unwrap().push(options);
        Err(LlmError::from_failure(LlmFailure {
            message: "budget exceeded".into(),
            code: "IMAGE_OFFLOAD_REQUIRED".into(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: Some(1),
        }))
    }
}
