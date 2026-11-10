//! `lsp` 工具的实机联调：参数校验、无工作区拒绝、超时预算、结果渲染——全部走
//! 真实缝 + stdio provider + Python 假服务端（缺 python 自动跳过）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use dsh_llm::{CallId, ContentBlock};
use dsh_lsp::provider::{StdioProvider, StdioProviderConfig};
use dsh_lsp::tool::{LspTool, LspToolConfig};
use dsh_lsp::{LspRegistry, LspRegistration};
use dsh_tools::{Tool, ToolExecutionInput};
use serde_json::json;

fn python() -> Option<String> {
    for candidate in ["python", "python3"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn fixture(args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_lsp_server.py")
            .display()
            .to_string(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    argv
}

fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = format!(
        "dsh-lsp-tool-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
        label
    );
    let path = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    path
}

fn registry_with_fixture(
    label: &str,
    extra: &[&str],
) -> (Arc<LspRegistry>, Arc<StdioProvider>, PathBuf, LspRegistration) {
    let workspace = scratch(label);
    std::fs::write(workspace.join("a.ts"), "export const a = 1\n").unwrap();
    let mut extensions = HashMap::new();
    extensions.insert(".ts".to_string(), "typescript".to_string());
    let provider = Arc::new(StdioProvider::new(
        "fixture",
        extensions,
        StdioProviderConfig {
            executable: "python".to_string(),
            args: fixture(extra),
            env: Vec::new(),
            initialization_options: serde_json::Value::Null,
            configuration: json!({ "static": true }),
            max_message_bytes: 16_000_000,
            max_stderr_bytes: 1_000_000,
            max_document_bytes: 4_000_000,
            shutdown_timeout_ms: 500,
            kill_grace_ms: 500,
        },
    ));
    let registry = Arc::new(LspRegistry::new());
    // 注册句柄必须活到测试结束：drop 即注销（全有或全无的另一半）。
    let registration = registry.register_provider(provider.clone()).expect("register");
    (registry, provider, workspace, registration)
}

fn input(operation: &str, file_path: &str, line: f64, character: f64) -> ToolExecutionInput {
    ToolExecutionInput::with_raw_arguments(
        CallId("call-1".into()),
        "lsp".into(),
        json!({ "operation": operation, "file_path": file_path, "line": line, "character": character })
            .to_string(),
    )
}

fn text_of(result: &dsh_tools::ToolExecutionResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_round_trips_definition_and_hover() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let (registry, provider, workspace, _registration) = registry_with_fixture("round", &[]);
    let tool = LspTool::new(
        Arc::clone(&registry),
        LspToolConfig::default(),
    )
    .with_workspace_root(Some(workspace.display().to_string()));

    let result = tool
        .execute(&input("goToDefinition", "a.ts", 2.0, 3.0))
        .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let text = text_of(&result);
    assert!(text.contains("a.ts:2:3"), "{}", text);
    assert!(result.presentation.is_some(), "呈现卡要带 presentation");

    let hover = tool
        .execute(&input("hover", "a.ts", 2.0, 3.0))
        .await;
    assert!(!hover.is_error, "{}", text_of(&hover));
    let hover_text = text_of(&hover);
    assert!(hover_text.contains("**hover body**"), "{}", hover_text);

    provider.dispose_all().await.expect("dispose");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_arguments_report_upstream_messages() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let (registry, _provider, workspace, _registration) = registry_with_fixture("args", &[]);
    let tool = LspTool::new(Arc::clone(&registry), LspToolConfig::default())
        .with_workspace_root(Some(workspace.display().to_string()));

    let result = tool.execute(&input("nope", "a.ts", 2.0, 3.0)).await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("operation must be one of goToDefinition, findReferences, goToImplementation, hover"), "{}", text_of(&result));

    let result = tool.execute(&input("hover", "a.ts", 0.0, 3.0)).await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("line must be a positive integer (one-based)"), "{}", text_of(&result));

    let result = tool.execute(&input("hover", " ", 2.0, 3.0)).await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("file_path must be a non-empty string"), "{}", text_of(&result));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_workspace_is_rejected_without_fallback() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let (registry, _provider, _workspace, _registration) = registry_with_fixture("ws", &[]);
    // 无会话工作区：上游 LSP_WORKSPACE_REQUIRED（无回退）。
    let tool = LspTool::new(Arc::clone(&registry), LspToolConfig::default());
    let result = tool.execute(&input("hover", "a.ts", 2.0, 3.0)).await;
    assert!(result.is_error);
    assert_eq!(text_of(&result), "the lsp tool requires a session workspace cwd");
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_budget_aborts_the_query() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    // --hang-hover：hover 永不响应 → 预算 300ms 到点 → 取消 → 工具以中止失败。
    let (registry, provider, workspace, _registration) =
        registry_with_fixture("budget", &["--hang-hover"]);
    let tool = LspTool::new(
        Arc::clone(&registry),
        LspToolConfig { timeout_ms: 300, ..Default::default() },
    )
    .with_workspace_root(Some(workspace.display().to_string()));

    let started = std::time::Instant::now();
    let result = tool.execute(&input("hover", "a.ts", 2.0, 3.0)).await;
    assert!(result.is_error, "预算耗尽要以错误收场");
    assert!(text_of(&result).contains("LSP query aborted"), "{}", text_of(&result));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "预算要真的生效：{:?}",
        started.elapsed()
    );

    provider.dispose_all().await.expect("dispose");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_extension_is_a_tool_error() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let (registry, _provider, workspace, _registration) = registry_with_fixture("ext", &[]);
    let tool = LspTool::new(Arc::clone(&registry), LspToolConfig::default())
        .with_workspace_root(Some(workspace.display().to_string()));

    // .rs 没有被任何 provider 认领：LSP_UNAVAILABLE 以工具错误透出。
    std::fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
    let result = tool.execute(&input("hover", "main.rs", 1.0, 1.0)).await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("no LSP provider handles"), "{}", text_of(&result));

    let _ = std::fs::remove_dir_all(&workspace);
}