//! 实机联调：用一个真实的 stdio 子进程（Python 假 MCP 服务端）验证
//! `spawn_stdio` → initialize → tools/list → tools/call → 工具桥全链。
//!
//! 该测试在缺少 python 的环境自动跳过（CI 友好）。

use std::sync::Arc;

use dsh_mcp::McpServer;

fn python() -> Option<String> {
    for candidate in ["python", "python3"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.zcode/mcp_echo_server.py")
        .canonicalize()
        .unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.zcode/mcp_echo_server.py"))
}

#[tokio::test(flavor = "multi_thread")]
async fn real_stdio_server_round_trip() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let script = fixture();
    if !script.exists() {
        eprintln!("SKIP: fixture {} missing", script.display());
        return;
    }

    let client = dsh_mcp::spawn_for_test(&python, &[script.display().to_string()])
        .expect("spawn fixture server");
    let client = Arc::new(client);
    let init = client
        .initialize("whalemirror", "0.1.0", dsh_mcp::DEFAULT_MAX_INSTRUCTION_BYTES, std::time::Duration::from_secs(20))
        .await
        .expect("initialize");
    assert_eq!(init.protocol_version, dsh_mcp::PROTOCOL_VERSION);
    assert_eq!(init.server_name, "echo-fixture");
    assert!(init.has_tools);
    assert!(init.instructions.as_deref().unwrap_or("").contains("echoes text"));

    let tools = client
        .list_tools("fixture", std::time::Duration::from_secs(20))
        .await
        .expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|t| t.public_name.as_str()).collect();
    assert_eq!(names, vec!["mcp__fixture__echo", "mcp__fixture__add"]);

    let echo = client
        .call_tool("echo", serde_json::json!({ "text": "hello mcp" }), std::time::Duration::from_secs(20))
        .await
        .expect("tools/call echo");
    assert!(!echo.is_error);
    assert_eq!(echo.text, "hello mcp");

    let add = client
        .call_tool("add", serde_json::json!({ "a": 40, "b": 2 }), std::time::Duration::from_secs(20))
        .await
        .expect("tools/call add");
    assert_eq!(add.text, "42");

    // 服务端 isError → 模型可见的失败
    let bad = client
        .call_tool("add", serde_json::json!({ "a": "x", "b": 1 }), std::time::Duration::from_secs(20))
        .await
        .expect("protocol ok");
    assert!(bad.is_error, "服务端报错要透出");
    assert!(bad.text.contains("requires integers"), "{}", bad.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_registers_callable_tools_over_stdio() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let script = fixture();
    if !script.exists() {
        eprintln!("SKIP: fixture {} missing", script.display());
        return;
    }
    let client = Arc::new(
        dsh_mcp::spawn_for_test(&python, &[script.display().to_string()]).expect("spawn"),
    );
    client
        .initialize("whalemirror", "0.1.0", dsh_mcp::DEFAULT_MAX_INSTRUCTION_BYTES, std::time::Duration::from_secs(20))
        .await
        .expect("initialize");
    let tools = client
        .list_tools("fixture", std::time::Duration::from_secs(20))
        .await
        .expect("list");
    let server = McpServer::new("fixture", Arc::clone(&client), tools, None);

    let registry = dsh_tools::ToolRegistry::new();
    let disposers = server.register(&registry).expect("register");
    assert_eq!(disposers.len(), 2);

    // 通过注册表执行：证明工具真的接线了
    let input = dsh_tools::ToolExecutionInput::with_raw_arguments(
        dsh_llm::CallId("call-1".into()),
        "mcp__fixture__echo".into(),
        r#"{"text":"via registry"}"#.into(),
    );
    let result = registry.execute(&input).await.expect("tool executed");
    assert!(!result.is_error);
    let text = result
        .content
        .iter()
        .find_map(|b| match b {
            dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert_eq!(text, "via registry");

    // 下线整代
    for disposer in disposers {
        disposer();
    }
    assert!(registry.get("mcp__fixture__echo").is_none(), "disposer 要摘干净");
}