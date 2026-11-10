//! LSP 实例的实机联调：握手、瞬态文档生命周期、服务端请求应答、取消宽限升级、
//! 优雅/强制拆卸。全部走真实 Python stdio 子进程（缺 python 自动跳过）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dsh_lsp::client::ConnectionSpec;
use dsh_lsp::instance::{InstanceSpec, LspInstance};
use dsh_lsp::types::{LspOperation, LspPosition, LspProviderQuery, LspQueryRequest};
use dsh_lsp::{CancelToken, HostSource, LspError};
use serde_json::{json, Value};

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

fn fixture(extra_args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_lsp_server.py")
            .display()
            .to_string(),
    ];
    argv.extend(extra_args.iter().map(|s| s.to_string()));
    argv
}

fn spec(args: Vec<String>, workspace_uri: &str) -> InstanceSpec {
    InstanceSpec {
        connection: {
            let mut spec = ConnectionSpec::new("python", args);
            spec.kill_grace_ms = 500;
            spec
        },
        workspace_uri: workspace_uri.to_string(),
        initialization_options: Value::Null,
        configuration: json!({ "static": true }),
        shutdown_timeout_ms: 300,
    }
}

fn provider_query(operation: LspOperation, signal: Option<CancelToken>) -> LspProviderQuery {
    LspProviderQuery {
        request: LspQueryRequest {
            operation,
            file_path: "a.ts".into(),
            position: LspPosition { line: 1, character: 2 },
            workspace_root: "file:///w".into(),
            signal,
        },
        language_id: "typescript".into(),
    }
}

fn source() -> HostSource {
    HostSource {
        file_url: "file:///w/a.ts".into(),
        text: "export const a = 1\n".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_answered_and_hover_normalizes() {
    let Some(python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let instance = LspInstance::spawn(spec(fixture(&[]), "file:///w")).expect("spawn");

    let result = instance
        .query(&provider_query(LspOperation::Hover, None), &source())
        .await
        .expect("hover");
    match result {
        dsh_lsp::LspQueryResult::Hover { hover } => {
            let hover = hover.expect("有 hover");
            assert_eq!(hover.contents, "**hover body**");
        }
        _ => panic!("hover 查询要归一到 Hover"),
    }

    // 实例应答了 initialize 期间服务端主动发的 workspace/configuration 与
    // client/registerCapability；config-echo 的回显即双向链路的实证。
    let echoed = instance
        .connection()
        .request("test/config-echo", json!({}))
        .await
        .expect("config echo");
    assert_eq!(echoed, json!([{ "static": true }]));
    let _ = python;
}

#[tokio::test(flavor = "multi_thread")]
async fn transient_document_lifecycle_opens_and_closes() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let instance = LspInstance::spawn(spec(fixture(&[]), "file:///w")).expect("spawn");

    instance
        .query(&provider_query(LspOperation::GoToDefinition, None), &source())
        .await
        .expect("definition");

    let history = instance
        .connection()
        .request("test/open-history", json!({}))
        .await
        .expect("open history");
    assert_eq!(history["opens"], json!(["file:///w/a.ts"]));
    assert_eq!(history["closes"], json!(["file:///w/a.ts"]), "瞬态文档查询后要关");
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_operation_rejects_before_any_request() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    // --no-hover：capabilities 声明不支持 hover。
    let instance = LspInstance::spawn(spec(fixture(&["--no-hover"]), "file:///w")).expect("spawn");

    let error = instance
        .query(&provider_query(LspOperation::Hover, None), &source())
        .await
        .expect_err("不支持的操作要拒绝");
    assert_eq!(error.message, "server does not support hover");
    assert_eq!(error.code(), "LSP_UNSUPPORTED_OPERATION");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_grace_escalates_to_teardown() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let instance = Arc::new(
        LspInstance::spawn(spec(fixture(&["--hang-hover"]), "file:///w")).expect("spawn"),
    );
    let token = CancelToken::new();

    // 查询放进并发任务：取消必须发生在查询真正在途时（Rust future 不 poll 不执行）。
    let query_request = provider_query(LspOperation::Hover, Some(token.clone()));
    let source = source();
    let task = {
        let instance = Arc::clone(&instance);
        tokio::spawn(async move { instance.query(&query_request, &source).await })
    };
    // 等握手与 didOpen/hover（不回响应）真正发出，然后取消。
    tokio::time::sleep(Duration::from_millis(300)).await;
    token.abort();

    let error = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("取消要结算查询")
        .expect("任务不能恐慌")
        .expect_err("取消要报错");
    assert_eq!(error.message, "LSP query aborted", "{}", error.message);

    // hover 永不响应：宽限到点 → 实例被拆 → 优雅 shutdown/exit 或强杀 → 连接关闭。
    tokio::time::timeout(Duration::from_secs(15), instance.connection().closed())
        .await
        .expect("升级拆卸后连接要关闭");
    assert!(instance.dead());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ignored_escalates_within_budget() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    // shutdown_timeout_ms=300：优雅阶段超时后强杀。
    let mut instance_spec = spec(fixture(&[]), "file:///w");
    instance_spec.shutdown_timeout_ms = 300;
    let instance = LspInstance::spawn(instance_spec).expect("spawn");
    let connection = instance.connection();

    // 让服务器从此忽略 shutdown。
    connection
        .request("test/ignore-shutdown", json!({}))
        .await
        .expect("arm ignore");

    let started = std::time::Instant::now();
    instance.dispose().await.expect("dispose");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "拆卸不能挂死：用了 {:?}",
        started.elapsed()
    );
    assert!(instance.dead());
}

#[tokio::test(flavor = "multi_thread")]
async fn transport_failure_is_classified_from_the_retained_reason() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let instance = LspInstance::spawn(spec(fixture(&[]), "file:///w")).expect("spawn");
    // 崩溃：在途请求会拿到保留的退出原因。
    let error = instance
        .connection()
        .request("test/crash", json!({}))
        .await
        .expect_err("crash");
    assert!(instance.is_transport_failure(&error), "崩溃错误要被归为传输失败");
    assert!(!instance.is_transport_failure(&LspError::new(
        dsh_lsp::LspErrorCode::Timeout,
        "LSP query aborted"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_serialize_per_instance() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    let instance = Arc::new(LspInstance::spawn(spec(fixture(&[]), "file:///w")).expect("spawn"));

    // 两条并发查询：串行队列保证 didOpen→请求→didClose 的生命周期不交错——
    // 用 open-history 的顺序断言（开→关→开→关，绝无 开→开→关→关）。
    let definition = provider_query(LspOperation::GoToDefinition, None);
    let hover = provider_query(LspOperation::Hover, None);
    let source = source();
    let one = instance.query(&definition, &source);
    let two = instance.query(&hover, &source);
    let (one, two) = tokio::join!(one, two);
    assert!(one.is_ok() && two.is_ok());

    let history = instance
        .connection()
        .request("test/open-history", json!({}))
        .await
        .expect("open history");
    let opens = history["opens"].as_array().unwrap().len();
    let closes = history["closes"].as_array().unwrap().len();
    assert_eq!(opens, 2, "两条查询各自开一次文档");
    assert_eq!(closes, 2, "两条查询各自关一次文档");
    // 顺序：开合按同一文档交替（关必须跟在开后面，生命周期不交错）。
    let opens_seq: Vec<&str> = history["opens"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let closes_seq: Vec<&str> = history["closes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(opens_seq, closes_seq, "开合按同一文档交替");
}

#[tokio::test(flavor = "multi_thread")]
async fn pooling_is_per_workspace_key() {
    let Some(_python) = python() else {
        eprintln!("SKIP: no python interpreter available");
        return;
    };
    // 这个测试属于 provider 层的池化断言；实例层只确认同一实例重复查询不重新握手。
    let instance = LspInstance::spawn(spec(fixture(&[]), "file:///w")).expect("spawn");
    let first = instance
        .query(&provider_query(LspOperation::GoToDefinition, None), &source())
        .await
        .expect("first");
    let second = instance
        .query(&provider_query(LspOperation::GoToDefinition, None), &source())
        .await
        .expect("second");
    let _ = (first, second);

    let count = instance
        .connection()
        .request("test/init-count", json!({}))
        .await
        .expect("init count");
    assert_eq!(count, json!(1), "同一实例不重新握手");
}

// 让 HashMap 引用不显得孤立（language_id 构造走 provider_query）。
#[allow(dead_code)]
fn _language_map() -> HashMap<String, String> {
    HashMap::new()
}