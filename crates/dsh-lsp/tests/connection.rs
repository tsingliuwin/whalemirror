//! LSP 连接（JSON-RPC 端点）的实机联调：用 Python 假服务端跑真实 stdio 子进程。
//!
//! 缺 python 时自动跳过（与 `dsh-mcp` 的 stdio 端到端测试同一约定）。被验证的是
//! `client.rs` 的四个方向：出站请求/通知、入站响应、入站服务端→客户端请求、
//! 以及进程/分帧失败时的关闭文案与在途请求结算。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dsh_lsp::{ConnectionSpec, LspConnection, ServerRequestHandler};
use serde_json::{json, Value};

/// 依次尝试 `python` / `python3`，都没有就跳过。
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

/// 假服务端脚本（随 crate 提交，缺文件时同样跳过）。
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_lsp_server.py")
}

/// 默认应答者：只受理 `workspace/configuration`，其余走 -32601。
fn handler() -> ServerRequestHandler {
    Arc::new(|method: &str, _params: Value| match method {
        "workspace/configuration" => Ok(json!([{ "focused": true }])),
        other => Err(format!("unhandled server request: {other}")),
    })
}

/// 起一条指向假服务端的连接。
fn connect(python: &str) -> LspConnection {
    let mut spec = ConnectionSpec::new(python, vec![fixture().display().to_string()]);
    spec.kill_grace_ms = 1_000;
    LspConnection::spawn(spec, handler()).expect("spawn fake language server")
}

/// 初始化并等协商完成（同时触发服务端主动发的 `workspace/configuration`）。
async fn initialize(connection: &LspConnection) -> Value {
    let result = connection
        .request(
            "initialize",
            json!({ "processId": null, "rootUri": null, "capabilities": {} }),
        )
        .await
        .expect("initialize");
    connection.notify("initialized", json!({})).await.expect("initialized");
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_and_server_request_round_trip() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);

    let init = initialize(&connection).await;
    assert_eq!(init["capabilities"]["hoverProvider"], json!(true));
    assert_eq!(init["capabilities"]["positionEncoding"], json!("utf-16"));

    let definition = connection
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": "file:///w/a.ts" },
                "position": { "line": 1, "character": 2 },
            }),
        )
        .await
        .expect("definition");
    assert_eq!(definition[0]["uri"], json!("file:///w/a.ts"));
    assert_eq!(definition[0]["range"]["start"]["character"], json!(2));

    let hover = connection
        .request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": "file:///w/a.ts" },
                "position": { "line": 1, "character": 3 },
            }),
        )
        .await
        .expect("hover");
    assert_eq!(hover["contents"]["kind"], json!("markdown"));
    assert_eq!(hover["contents"]["value"], json!("**hover body**"));

    // 服务端主动发的请求真的被应答、并且应答回到了服务端：这条回显就是双向链路的实证。
    let echoed = connection.request("test/config-echo", json!({})).await.expect("config echo");
    assert_eq!(echoed, json!([{ "focused": true }]));
}

#[tokio::test(flavor = "multi_thread")]
async fn error_response_becomes_lsp_error() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);
    initialize(&connection).await;

    let error = connection
        .request("test/unknown", json!({}))
        .await
        .expect_err("服务端错误响应要转成 LspError");
    assert_eq!(error.message, "unknown method test/unknown");
    assert!(!connection.failed(), "单条错误响应不是致命关闭");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_frame_fails_the_connection() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);
    initialize(&connection).await;

    // 服务端写了一段「合法分帧、非法 JSON」的字节：解码不可恢复，在途请求必须被拒绝。
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        connection.request("test/garbage", json!({})),
    )
    .await
    .expect("解码失败要立即结算在途请求")
    .expect_err("分帧被破坏要报错");
    assert!(error.message.contains("not valid JSON"), "{}", error.message);
    assert!(connection.failed());
}

#[tokio::test(flavor = "multi_thread")]
async fn process_exit_rejects_pending_request_with_stderr_tail() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);
    initialize(&connection).await;

    let error = tokio::time::timeout(
        Duration::from_secs(10),
        connection.request("test/crash", json!({})),
    )
    .await
    .expect("子进程退出要结算在途请求")
    .expect_err("崩溃要报错");
    assert!(error.message.contains("language server exited"), "{}", error.message);
    assert!(
        error.message.contains("boom: fake server exploded"),
        "退出文案要带 stderr 尾部：{}",
        error.message
    );
    assert!(connection.stderr_tail().contains("boom"));
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_after_close_fail_immediately() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);
    initialize(&connection).await;

    connection.request("shutdown", json!({})).await.expect("shutdown");
    connection.notify("exit", json!({})).await.expect("exit");
    tokio::time::timeout(Duration::from_secs(10), connection.closed())
        .await
        .expect("exit 后连接要关闭");

    // 关闭后发出的请求不能挂在半开连接上等一个永不来的响应。
    let error = connection
        .request("textDocument/hover", json!({}))
        .await
        .expect_err("关闭后请求要立即失败");
    assert_eq!(error.message, "language server exited");
}

#[tokio::test(flavor = "multi_thread")]
async fn ids_increment_and_cancel_is_best_effort() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let connection = connect(&python);
    let first = connection.peek_next_id();
    initialize(&connection).await;
    let second = connection.peek_next_id();
    assert_eq!(second, first + 1, "每条请求占一个 id");

    // 对一条已完成/不存在的 id 取消：只发 $/cancelRequest，不产生错误也不判死连接。
    connection.cancel(first).await;
    connection.cancel(9_999).await;
    assert!(!connection.failed());
    connection.request("textDocument/hover", json!({})).await.expect("取消后连接仍可用");
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_failure_reports_server_failed() {
    let spec = ConnectionSpec::new("dsh-lsp-no-such-binary-xyz", Vec::new());
    let error = LspConnection::spawn(spec, handler()).expect_err("不存在的可执行文件要报错");
    assert!(error.message.contains("executable not found"), "{}", error.message);
    assert_eq!(error.code(), "LSP_SERVER_FAILED");
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_caps_are_rejected() {
    let mut spec = ConnectionSpec::new("python", Vec::new());
    spec.max_stderr_bytes = 0;
    let error = LspConnection::spawn(spec, handler()).expect_err("stderr 上限必须为正");
    assert!(error.message.contains("maxStderrBytes"), "{}", error.message);

    let mut spec = ConnectionSpec::new("python", Vec::new());
    spec.max_message_bytes = 0;
    let error = LspConnection::spawn(spec, handler()).expect_err("消息上限必须为正");
    assert!(error.message.contains("maxMessageBytes"), "{}", error.message);
}