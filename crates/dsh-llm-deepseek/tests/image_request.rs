//! vision 请求切片：user 消息含 Image 块 → wire content 变 parts 数组
//! （文本句柄 + image_url data URL，上游 serialize.ts base64 路径同形）；
//! 反查失败降级占位文本；纯文本消息保持字符串 content。
use std::io::{Read, Write};

use dsh_llm::{ContentBlock, GenerateOptions, ImageAttachmentRef, LlmAdapter, Message};
use dsh_llm_deepseek::DeepSeekAdapter;

/// 本地 mock 端点：循环读完整请求（headers/body 可能分属多个 TCP 段）存文件，
/// 回一条最小 SSE。
fn spawn_mock(dir: &std::path::Path) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = sock.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let cl = text
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= i + 4 + cl {
                        break;
                    }
                }
            }
            let _ = std::fs::write(dir.join("request.json"), &buf);
            let sse = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n\
                       data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
                       data: [DONE]\n\n";
            let _ = sock.write_all(sse.as_bytes());
            let _ = sock.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn image_ref() -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: "sha256:0011223344556677889900aabbccddeeff00112233445566778899aabbccddee"
            .into(),
        name: Some("shot.png".into()),
        media_type: "image/png".into(),
        bytes: 4,
        width: 32,
        height: 32,
        original_dimensions: None,
    }
}

/// 从保存的 HTTP 报文里剥出 JSON body（跳过 \r\n\r\n 头体分隔）。
fn parse_request(dir: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(dir.join("request.json")).unwrap();
    let body_start = raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    serde_json::from_str(&raw[body_start..]).unwrap()
}

#[tokio::test]
async fn user_image_serializes_as_image_url_parts() {
    let dir = std::env::temp_dir().join(format!("dsh-img-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let base = spawn_mock(&dir);

    let png = vec![0x89, b'P', b'N', b'G'];
    let adapter = DeepSeekAdapter::with_base_url("test-key", base)
        .with_image_fetcher(std::sync::Arc::new(move |_| Some(png.clone())));

    let msg = Message::user(vec![
        ContentBlock::text("看这张图"),
        ContentBlock::Image {
            attachment: image_ref(),
        },
    ]);
    let options = GenerateOptions::new("deepseek", "deepseek-flash", vec![msg]);
    let _ = adapter.stream(options).await;

    let body = parse_request(&dir);
    let content = &body["messages"][0]["content"];
    assert!(content.is_array(), "image message must use parts array");
    let parts = content.as_array().unwrap();
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "看这张图");
    let handle = parts[1]["text"].as_str().unwrap();
    assert!(handle.contains("request preview 32x32px"), "{handle}");
    assert!(handle.contains("may be resized or re-encoded"), "{handle}");
    let img = &parts[2];
    assert_eq!(img["type"], "image_url");
    let url = img["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
    use base64::Engine as _;
    let b64 = url.split("base64,").nth(1).unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(b64).unwrap(),
        vec![0x89, b'P', b'N', b'G']
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn image_fetch_failure_degrades_to_placeholder() {
    let dir = std::env::temp_dir().join(format!("dsh-img-miss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let base = spawn_mock(&dir);

    let adapter = DeepSeekAdapter::with_base_url("test-key", base)
        .with_image_fetcher(std::sync::Arc::new(|_| None));

    let msg = Message::user(vec![ContentBlock::Image {
        attachment: image_ref(),
    }]);
    let options = GenerateOptions::new("deepseek", "deepseek-flash", vec![msg]);
    let _ = adapter.stream(options).await;

    let body = parse_request(&dir);
    let parts = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "handle + placeholder, no image_url part");
    assert!(parts[1]["text"]
        .as_str()
        .unwrap()
        .starts_with("[image sha256:0011")
        && parts[1]["text"].as_str().unwrap().ends_with(" unavailable]"));
    assert!(parts.iter().all(|p| p["type"] != "image_url"));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn plain_text_keeps_string_content() {
    let dir = std::env::temp_dir().join(format!("dsh-img-plain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let base = spawn_mock(&dir);
    let adapter = DeepSeekAdapter::with_base_url("test-key", base);

    let options = GenerateOptions::new(
        "deepseek",
        "deepseek-flash",
        vec![Message::user_text("hi")],
    );
    let _ = adapter.stream(options).await;

    let body = parse_request(&dir);
    assert_eq!(body["messages"][0]["content"], "hi");
    std::fs::remove_dir_all(&dir).ok();
}
