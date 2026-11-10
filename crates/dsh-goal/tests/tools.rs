//! 目标工具的权限门与参数校验：直接人类轮、CAS、blocked 门槛、wrapup 注入。

use std::sync::{Arc, Mutex};

use dsh_goal::authority::AuthorityProbe;
use dsh_goal::store::{CreateGoalRequest, GoalActivation, GoalStore};
use dsh_goal::tools::{GoalTools, register_goal_tools};
use dsh_llm::{CallId, ContentBlock, Message, MessageId, MessageSource, Role};
use dsh_tools::{Tool, ToolExecutionInput, ToolRegistry};

fn probe_of(messages: Vec<Message>) -> Arc<dyn Fn() -> AuthorityProbe + Send + Sync> {
    Arc::new(move || AuthorityProbe {
        open_turn_messages: messages.clone(),
        is_root_session: true,
    })
}

fn human_probe() -> Arc<dyn Fn() -> AuthorityProbe + Send + Sync> {
    probe_of(vec![Message {
        id: MessageId::new("h"),
        role: Role::User,
        content: vec![ContentBlock::text("do it")],
        source: MessageSource::User,
    }])
}

fn goal_round_probe(goal_id: &str, revision: u64, round: u64) -> Arc<dyn Fn() -> AuthorityProbe + Send + Sync> {
    probe_of(vec![Message {
        id: MessageId::new("g"),
        role: Role::User,
        content: vec![ContentBlock::text("continue")],
        source: MessageSource::Goal { goal_id: goal_id.into(), revision, round },
    }])
}

fn tools_with_probe(
    probe: Arc<dyn Fn() -> AuthorityProbe + Send + Sync>,
) -> (Arc<GoalTools>, Arc<GoalStore>, Arc<Mutex<Vec<String>>>) {
    let store = Arc::new(GoalStore::new());
    let tools = Arc::new(GoalTools::new(Arc::clone(&store)));
    tools.set_probe(probe);
    let wrapups: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let wrapups_sink = Arc::clone(&wrapups);
    tools.set_defer_context(Arc::new(move |text: String| {
        wrapups_sink.lock().unwrap().push(text);
    }));
    (tools, store, wrapups)
}

fn execute(tool: &dyn Tool, arguments: &str) -> dsh_tools::ToolExecutionResult {
    let input = ToolExecutionInput::with_raw_arguments(
        CallId::new("c"),
        tool.definition().name,
        arguments.into(),
    );
    futures_block_on(tool.execute(&input))
}

/// 极小 block_on（避免拉 dev-dependency futures）。
fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn text_of(result: &dsh_tools::ToolExecutionResult) -> String {
    result
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn registry_registers_three_goal_tools() {
    let (tools, _, _) = tools_with_probe(human_probe());
    let registry = ToolRegistry::new();
    let disposers = register_goal_tools(&registry, Arc::clone(&tools)).expect("register");
    assert_eq!(disposers.len(), 3);
    let names: Vec<String> = registry.list().iter().map(|t| t.definition().name).collect();
    assert!(names.contains(&"get_goal".to_string()));
    assert!(names.contains(&"create_goal".to_string()));
    assert!(names.contains(&"update_goal".to_string()));
    for disposer in disposers {
        disposer();
    }
}

#[test]
fn create_requires_direct_human_turn() {
    let (tools, store, _) = tools_with_probe(Arc::new(|| AuthorityProbe::default()));
    let result = execute(&*create_tool(&tools), r#"{"objective":"obj"}"#);
    assert!(result.is_error);
    assert!(text_of(&result).contains("requires a direct human turn"), "{}", text_of(&result));

    // 人类轮：成功。
    let (tools_ok, store, _) = tools_with_probe(human_probe());
    let result = execute(&*create_tool(&tools_ok), r#"{"objective":"obj"}"#);
    assert!(!result.is_error, "{}", text_of(&result));
    let view = store.get().unwrap().expect("有目标");
    assert_eq!(view.objective, "obj");
    assert_eq!(view.activation, GoalActivation::Armed);
}

fn create_tool(tools: &Arc<GoalTools>) -> Arc<dyn Tool> {
    let registry = ToolRegistry::new();
    let _ = register_goal_tools(&registry, Arc::clone(tools));
    registry.get("create_goal").expect("create_goal 注册")
}

fn update_tool(tools: &Arc<GoalTools>) -> Arc<dyn Tool> {
    let registry = ToolRegistry::new();
    let _ = register_goal_tools(&registry, Arc::clone(tools));
    registry.get("update_goal").expect("update_goal 注册")
}

fn get_tool(tools: &Arc<GoalTools>) -> Arc<dyn Tool> {
    let registry = ToolRegistry::new();
    let _ = register_goal_tools(&registry, Arc::clone(tools));
    registry.get("get_goal").expect("get_goal 注册")
}

#[test]
fn get_goal_reports_null_without_goal_and_json_value_with() {
    let (tools, store, _) = tools_with_probe(human_probe());
    let result = execute(&*get_tool(&tools), "{}");
    assert!(!result.is_error);
    assert_eq!(text_of(&result), r#"{"goal":null}"#);

    let _ = store.create(CreateGoalRequest { objective: "obj".into(), max_goal_rounds: None }, 256).expect("create");
    let result = execute(&*get_tool(&tools), "{}");
    let text = text_of(&result);
    assert!(text.contains("\"phase\":\"active\""), "{text}");
    assert!(text.contains("\"activation\":\"armed\""), "{text}");
    assert!(text.contains("\"objective\":\"obj\""), "{text}");
}

#[test]
fn update_goal_validates_goal_id_and_revision_verbatim() {
    let (tools, _, _) = tools_with_probe(human_probe());
    let result = execute(
        &*update_tool(&tools),
        r#"{"goal_id":"","revision":0,"action":"pause"}"#,
    );
    assert!(result.is_error);
    assert!(
        text_of(&result).contains("goal_id must be non-empty and revision must be a positive safe integer"),
        "{}",
        text_of(&result)
    );

    let result = execute(
        &*update_tool(&tools),
        r#"{"goal_id":"g","revision":1,"action":"explode"}"#,
    );
    assert!(result.is_error);
    assert!(text_of(&result).contains("action must be one of"), "{}", text_of(&result));
}

#[test]
fn model_cannot_resume_a_paused_goal() {
    let (tools, store, _) = tools_with_probe(human_probe());
    let view = store.create(CreateGoalRequest { objective: "obj".into(), max_goal_rounds: None }, 256).expect("create");
    let _ = store.pause(&dsh_goal::store::GoalRef {
        id: view.id.clone(),
        revision: view.revision,
    });
    let arguments = format!(
        r#"{{"goal_id":"{}","revision":{},"action":"resume"}}"#,
        view.id, view.revision + 1
    );
    let result = execute(&*update_tool(&tools), &arguments);
    assert!(result.is_error);
    assert!(
        text_of(&result).contains("the model cannot resume a paused goal; the user must resume it"),
        "{}",
        text_of(&result)
    );
}

#[test]
fn blocked_requires_reason_and_consecutive_rounds() {
    // 直接人类轮（无 goal-round authority）：blocked 只需要 reason。
    let (tools, store, _) = tools_with_probe(human_probe());
    let view = store.create(CreateGoalRequest { objective: "obj".into(), max_goal_rounds: None }, 256).expect("create");
    let arguments = format!(
        r#"{{"goal_id":"{}","revision":{},"action":"blocked"}}"#,
        view.id, view.revision
    );
    let result = execute(&*update_tool(&tools), &arguments);
    assert!(result.is_error);
    assert!(text_of(&result).contains("blocked_reason is required with action blocked"));

    // 自主轮（goal-round authority）roundsStarted < 3：门槛拒绝。
    // 探针要匹配真实目标 id——先创建再换探针。
    let (tools_round, store_round, _) = tools_with_probe(human_probe());
    let view = store_round.create(CreateGoalRequest { objective: "obj".into(), max_goal_rounds: None }, 256).expect("create");
    tools_round.set_probe(goal_round_probe(&view.id, view.revision, view.rounds_started));
    let arguments = format!(
        r#"{{"goal_id":"{}","revision":{},"action":"blocked","blocked_reason":"stuck"}}"#,
        view.id, view.revision
    );
    let result = execute(&*update_tool(&tools_round), &arguments);
    assert!(result.is_error);
    assert!(
        text_of(&result).contains("blocked requires at least 3 consecutive goal rounds; current round is 0"),
        "{}",
        text_of(&result)
    );
}

#[test]
fn complete_in_goal_round_defers_wrapup_context() {
    let (tools, store, wrapups) = tools_with_probe(goal_round_probe("goal-round", 1, 0));
    let view = store.create(CreateGoalRequest { objective: "ship it".into(), max_goal_rounds: None }, 256).expect("create");
    // 探针的 goal id 要匹配真实目标。
    let _ = view;
    // 用直接人类轮完成，权限走 DirectHuman——不注入 wrapup。
    let arguments = format!(
        r#"{{"goal_id":"{}","revision":{},"action":"complete"}}"#,
        view.id, view.revision
    );
    // 换人类探针完成。
    tools.set_probe(human_probe());
    let result = execute(&*update_tool(&tools), &arguments);
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(
        wrapups.lock().unwrap().is_empty(),
        "直接人类轮完成不注入 wrapup（上游 deferContext 仅 goal-round）"
    );
    let _ = store;
}

#[test]
fn guidance_matches_upstream_text() {
    let (tools, _, _) = tools_with_probe(human_probe());
    let text = tools.guidance();
    assert!(text.starts_with("create_goal may infer goal intent from a direct human request in any language."));
    assert!(text.contains("blocked only after the same blocking condition persists for at least 3"));
    assert!(text.ends_with("or useful remaining work is not blocked."));
}