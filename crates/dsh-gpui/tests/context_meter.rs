//! 上下文占用表（#95）的端到端回归：占用率与构成分布都从真实会话 +
//! 请求信封算出，口径与 web token-meter 的固定密度启发式一致
//! （每 4 字符 1 token、每块 4、每条消息角色 4）。

use dsh_gpui::context_meter::{
    ContextBreakdown, context_occupancy, estimate_message_chars, estimate_system_prompt, estimate_tools_tokens,
};
use dsh_llm::{ContentBlock, Message, SessionId};
use dsh_session::{Session, SessionEvent};

fn priced_session() -> (u64, u64) {
    let mut session = Session::new(SessionId::new("session-meter"));
    session.append(SessionEvent::UserMessage(Message::user_text("hello there")));
    let mut reply = Message::user_text("hi, how can I help with this workspace today?");
    reply.role = dsh_llm::Role::Assistant;
    session.append(SessionEvent::AssistantMessage {
        turn: 1,
        step: 1,
        message: reply,
        interrupted: false,
        usage: None,
        time_ms: None,
    });
    let system_tokens = estimate_system_prompt(Some("You are a helpful coding agent."));
    let message_tokens: u64 = session
        .derive_messages()
        .iter()
        .map(|m| {
            let chars = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    _ => 0,
                })
                .sum();
            estimate_message_chars(chars, m.content.len())
        })
        .sum();
    (system_tokens, message_tokens)
}

#[test]
fn occupancy_is_built_from_prompt_and_transcript() {
    let (system_tokens, message_tokens) = priced_session();
    // 系统提示词「You are a helpful coding agent.」= 30 字符 → 8 + 4
    assert_eq!(system_tokens, 12);
    // 两条消息的正文分别 11 / 47 字符；每条含 1 块 → 各 +4+4
    let user = estimate_message_chars(11, 1);
    let assistant = estimate_message_chars(47, 1);
    assert_eq!(message_tokens, user + assistant);

    let tools_tokens = estimate_tools_tokens(Some(r#"[{"name":"shell"}]"#));
    let breakdown = ContextBreakdown { system_tokens, tools_tokens, message_tokens };
    let occupancy = context_occupancy(breakdown.total(), 128_000).expect("capacity known");

    assert_eq!(occupancy.context_window, 128_000);
    let expected = ((breakdown.total() as f64 / 128_000.0) * 100.0).round() as u32;
    assert_eq!(occupancy.percent, expected);
    assert!(breakdown.total() >= system_tokens + message_tokens);
}

#[test]
fn missing_route_capacity_hides_the_meter() {
    let (system_tokens, message_tokens) = priced_session();
    let breakdown = ContextBreakdown { system_tokens, tools_tokens: 0, message_tokens };
    // 路由没声明容量 → 不渲染（上游 contextOccupancy 返回 null）
    assert_eq!(context_occupancy(breakdown.total(), 0), None);
    // 空会话（无系统提示词、无消息）同样不渲染
    assert_eq!(context_occupancy(0, 128_000), None);
}

#[test]
fn bar_segments_track_the_composition_not_the_capacity() {
    let breakdown = ContextBreakdown {
        system_tokens: 2_000,
        tools_tokens: 6_000,
        message_tokens: 2_000,
    };
    let occupancy = context_occupancy(breakdown.total(), 100_000).expect("capacity known");
    assert_eq!(occupancy.percent, 10);
    let widths = breakdown.segment_widths(occupancy.percent);
    // 三段按 2:6:2 切分整条 10% 色条
    assert_eq!(widths.len(), 3);
    assert!((widths[0] - 2.0).abs() < 0.01);
    assert!((widths[1] - 6.0).abs() < 0.01);
    assert!((widths[2] - 2.0).abs() < 0.01);
    let sum: f32 = widths.iter().sum();
    assert!((sum - occupancy.percent as f32).abs() < 0.01);
}
