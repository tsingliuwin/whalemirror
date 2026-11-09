//! 协议层端到端：用内存对（`tokio::io::duplex`）跑一个**假 MCP 服务端**，
//! 走真实的 JSON-RPC 帧（换行分隔），验证
//! initialize 协商 / tools/list 分页 / tools/call 结果映射 / 错误与超时。

use dsh_mcp::{McpClient, SUPPORTED_VERSIONS, render_call_content};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 假服务端脚本：读一行请求，按方法回一条响应（或按脚本注入错误/沉默）。
type ServerRead = tokio::io::ReadHalf<tokio::io::DuplexStream>;
type ServerWrite = tokio::io::WriteHalf<tokio::io::DuplexStream>;

struct FakeServer {
    reader: BufReader<ServerRead>,
    writer: ServerWrite,
}

impl FakeServer {
    async fn next_request(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.expect("read request");
        serde_json::from_str(line.trim()).expect("request is json")
    }

    async fn reply(&mut self, id: &Value, result: Value) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write response");
        self.writer.flush().await.expect("flush");
    }

    async fn reply_error(&mut self, id: &Value, code: i64, message: &str) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
        self.writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write response");
        self.writer.flush().await.expect("flush");
    }
}

fn linked() -> (McpClient, FakeServer) {
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let client = McpClient::new(client_read, client_write);
    let server = FakeServer {
        reader: BufReader::new(server_read),
        writer: server_write,
    };
    (client, server)
}

fn timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}

#[tokio::test]
async fn initialize_negotiates_and_reports_capabilities() {
    let (client, mut server) = linked();
    let handle = tokio::spawn(async move {
        let req = server.next_request().await;
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["protocolVersion"], dsh_mcp::PROTOCOL_VERSION);
        assert_eq!(req["params"]["clientInfo"]["name"], "whalemirror");
        let id = req["id"].clone();
        server
            .reply(
                &id,
                json!({
                    "protocolVersion": "2026-07-28",
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "fake", "version": "1.2.3" },
                    "instructions": "be nice",
                }),
            )
            .await;
        // 客户端随后发 initialized 通知（无 id）
        let note = server.next_request().await;
        assert_eq!(note["method"], "notifications/initialized");
        assert!(note.get("id").is_none(), "通知不带 id");
        drop(server);
    });

    let init = client
        .initialize("whalemirror", "0.1.0", dsh_mcp::DEFAULT_MAX_INSTRUCTION_BYTES, timeout())
        .await
        .expect("initialize succeeds");
    assert_eq!(init.protocol_version, "2026-07-28");
    assert_eq!(init.server_name, "fake");
    assert_eq!(init.server_version, "1.2.3");
    assert_eq!(init.instructions.as_deref(), Some("be nice"));
    assert!(init.has_tools && init.has_resources);
    handle.await.unwrap();
}

#[tokio::test]
async fn unsupported_version_and_oversized_instructions_fail_closed() {
    for (result, expected) in [
        (
            json!({ "protocolVersion": "1999-01-01", "capabilities": {}, "serverInfo": { "name": "x", "version": "1" } }),
            "unsupported MCP protocol version",
        ),
        (
            json!({
                "protocolVersion": SUPPORTED_VERSIONS[0],
                "capabilities": {},
                "serverInfo": { "name": "x", "version": "1" },
                "instructions": "y".repeat(100),
            }),
            "over the 16-byte limit",
        ),
    ] {
        let (client, mut server) = linked();
        let handle = tokio::spawn(async move {
            let req = server.next_request().await;
            server.reply(&req["id"].clone(), result).await;
        });
        let err = client
            .initialize("whalemirror", "0.1.0", 16, timeout())
            .await
            .expect_err("must fail closed");
        assert!(err.contains(expected), "{err}");
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn tools_list_follows_the_cursor_and_builds_public_names() {
    let (client, mut server) = linked();
    let handle = tokio::spawn(async move {
        let first = server.next_request().await;
        assert_eq!(first["method"], "tools/list");
        server
            .reply(
                &first["id"].clone(),
                json!({
                    "tools": [{ "name": "search", "description": "find", "inputSchema": { "type": "object" } }],
                    "nextCursor": "page-2",
                }),
            )
            .await;
        let second = server.next_request().await;
        assert_eq!(second["params"]["cursor"], "page-2");
        server
            .reply(&second["id"].clone(), json!({ "tools": [{ "name": "second-tool" }] }))
            .await;
    });

    let tools = client.list_tools("web", timeout()).await.expect("list ok");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].public_name, "mcp__web__search");
    assert_eq!(tools[1].public_name, "mcp__web__second-tool");
    handle.await.unwrap();
}

#[tokio::test]
async fn tools_call_maps_content_and_errors() {
    let (client, mut server) = linked();
    let handle = tokio::spawn(async move {
        // 成功调用
        let req = server.next_request().await;
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "search"); // 原始名上线
        assert_eq!(req["params"]["arguments"]["q"], "rust");
        server
            .reply(
                &req["id"].clone(),
                json!({
                    "content": [
                        { "type": "text", "text": "hit one" },
                        { "type": "resource_link", "name": "doc", "uri": "file:///doc.md" }
                    ]
                }),
            )
            .await;
        // 服务端报错（isError 而非 JSON-RPC error）
        let req = server.next_request().await;
        server
            .reply(&req["id"].clone(), json!({ "isError": true, "content": [{ "type": "text", "text": "boom" }] }))
            .await;
        // JSON-RPC 层错误
        let req = server.next_request().await;
        server.reply_error(&req["id"].clone(), -32601, "method not found").await;
    });

    let ok = client
        .call_tool("search", json!({ "q": "rust" }), timeout())
        .await
        .expect("call ok");
    assert!(!ok.is_error);
    assert_eq!(ok.text, "hit one\ndoc: file:///doc.md");

    let failed = client.call_tool("search", json!({}), timeout()).await.expect("protocol ok");
    assert!(failed.is_error, "isError 要透出");
    assert_eq!(failed.text, "boom");

    let rpc = client.call_tool("search", json!({}), timeout()).await.expect_err("rpc error");
    assert!(rpc.contains("MCP error -32601: method not found"), "{rpc}");
    handle.await.unwrap();
}

#[tokio::test]
async fn a_silent_server_times_out_instead_of_hanging() {
    let (client, _server) = linked(); // 服务端永不应答
    let start = std::time::Instant::now();
    let err = client
        .request("tools/list", json!({}), std::time::Duration::from_millis(120))
        .await
        .expect_err("must time out");
    assert!(err.contains("timed out after 120ms"), "{err}");
    assert!(start.elapsed() < std::time::Duration::from_secs(3), "不能挂死");
}

#[tokio::test]
async fn a_dropped_server_fails_pending_requests_immediately() {
    let (client, server) = linked();
    drop(server); // 对端关闭
    let start = std::time::Instant::now();
    let err = client
        .request("tools/list", json!({}), std::time::Duration::from_secs(30))
        .await
        .expect_err("must fail");
    // 立即失败（写端 broken pipe 或读端 EOF 后的 pending 清空），不能等到 30s 超时
    assert!(
        err.contains("closed") || err.contains("write failed"),
        "{err}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(3), "必须快速失败: {err}");
    assert!(client.is_closed(), "写失败后连接标记为已关闭");
}

#[tokio::test]
async fn non_json_lines_and_unknown_ids_are_ignored() {
    let (client, mut server) = linked();
    let handle = tokio::spawn(async move {
        // 先灌噪声：非 JSON、无 id 的通知、别人 id 的响应
        server.writer.write_all(b"not json\n").await.unwrap();
        server
            .writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"note\"}\n")
            .await
            .unwrap();
        server
            .writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9999,\"result\":{}}\n")
            .await
            .unwrap();
        server.writer.flush().await.unwrap();
        // 再正常应答
        let req = server.next_request().await;
        server.reply(&req["id"].clone(), json!({ "tools": [] })).await;
    });
    let tools = client.list_tools("web", timeout()).await.expect("噪声不影响配对");
    assert!(tools.is_empty());
    handle.await.unwrap();
}

#[test]
fn call_content_is_deterministic_for_mixed_blocks() {
    let content: Vec<Value> = vec![
        json!({ "type": "text", "text": "a" }),
        json!({ "type": "image", "mimeType": "image/png" }),
        json!({ "type": "text", "text": "b" }),
    ];
    let once = render_call_content(&content);
    assert_eq!(once, render_call_content(&content));
    assert!(once.starts_with('a') && once.ends_with('b'), "{once}");
}