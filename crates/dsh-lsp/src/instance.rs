//! 一个语言服务器实例：连接 + `initialize` 握手 + 瞬态 `didOpen`→请求→`didClose`
//! 生命周期 + 有界拆卸。
//!
//! 对齐 [`packages/lsp/lsp-stdio/src/instance.ts`]：一个实例拥有一个
//! （provider id, 规范化 workspace）的进程。查询经一条串行队列排队，取消无法叫停
//! 服务器时直接终止**整个实例**（而不是杀掉不相干的工作）；不同实例之间并行。
//! `dispose()` 拒绝排队中的工作、先试优雅 `shutdown`/`exit`、超时后升级强杀，并
//! 等进程真正退出——没有任何东西能活得比拆卸更久。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::watch;

use crate::cancel::CancelToken;
use crate::client::{ConnectionSpec, LspConnection, ServerRequestHandler};
use crate::host::HostSource;
use crate::translate::{
    negotiate_position_encoding, normalize_hover, normalize_locations, supports_operation,
    supports_transient_open,
};
use crate::types::{LspOperation, LspProviderQuery, LspQueryResult};
use crate::{LspError, LspErrorCode};

/// 一个实例在连接参数之外还需要的东西（上游 `InstanceSpec`）。
#[derive(Clone, Debug)]
pub struct InstanceSpec {
    /// 拉起与上限参数。
    pub connection: ConnectionSpec,
    /// `initialize` 的 `rootUri`（规范化 workspace 的 `file:` URI）。
    pub workspace_uri: String,
    /// 转发给服务器的静态 `initialize` 选项。
    pub initialization_options: Value,
    /// 对每一条 `workspace/configuration` 询问的静态应答。
    pub configuration: Value,
    /// 优雅 `shutdown`/`exit` 的预算，超时即升级强杀。
    pub shutdown_timeout_ms: u64,
}

/// 拆卸事务状态：是否已发布 + 结果通道（载荷为 `Some` 即完成）。
struct TeardownSlot {
    started: bool,
    result: Option<watch::Receiver<Option<Result<(), LspError>>>>,
}

/// 一个已初始化的服务器进程。不作为 provider 导出——provider 负责单飞与池化。
pub struct LspInstance {
    connection: Arc<LspConnection>,
    /// 串行队列：后一条查询等前一条的**实际工作**（与上游 promise 尾同语义）。
    queue: Arc<tokio::sync::Mutex<()>>,
    spec: InstanceSpec,
    capabilities: watch::Receiver<Option<Result<Value, LspError>>>,
    disposed: Arc<AtomicBool>,
    teardown: Arc<Mutex<TeardownSlot>>,
}

impl LspInstance {
    /// 拉起进程、开始 `initialize` 握手（后台任务，不阻塞构造）。
    ///
    /// 必须在 tokio 运行时内调用。
    pub fn spawn(spec: InstanceSpec) -> Result<Self, LspError> {
        // 实例自己应答服务端→客户端请求：configuration 用静态配置、生命周期簿记回
        // null、`workspace/applyEdit` 一律拒绝（本宿主从不应用编辑或执行命令）。
        let configuration = spec.configuration.clone();
        let on_request: ServerRequestHandler = Arc::new(move |method: &str, params: Value| {
            answer_server_request(method, params, &configuration)
        });

        let connection = Arc::new(LspConnection::spawn(spec.connection.clone(), on_request)?);

        // 握手作为后台任务：构造立即返回，查询在 ready 上等；失败的握手不会变成
        // 无人认领的 rejection（上游同款防御）。
        let (ready_tx, ready_rx) = watch::channel(None);
        let handshake = {
            let connection = Arc::clone(&connection);
            let workspace_uri = spec.workspace_uri.clone();
            let initialization_options = spec.initialization_options.clone();
            tokio::spawn(async move {
                let outcome = initialize(&connection, &workspace_uri, &initialization_options).await;
                let _ = ready_tx.send(Some(outcome));
            })
        };
        // 握手任务持有连接的 Arc；防止其句柄被静默丢弃后恐慌无从察觉，这里挂上。
        tokio::spawn(async move {
            let _ = handshake.await;
        });

        Ok(Self {
            connection,
            queue: Arc::new(tokio::sync::Mutex::new(())),
            spec,
            capabilities: ready_rx,
            disposed: Arc::new(AtomicBool::new(false)),
            teardown: Arc::new(Mutex::new(TeardownSlot { started: false, result: None })),
        })
    }

    /// 同步活性检查：进程已关闭、实例已丢弃、或传输已失败。
    pub fn dead(&self) -> bool {
        self.disposed.load(Ordering::SeqCst) || self.connection.failed()
    }

    /// 查询错误是否来自本实例的传输（上游 `failedWith` 的 `code`+`message` 等价）。
    pub fn is_transport_failure(&self, error: &LspError) -> bool {
        self.connection.failed() && self.connection.reason().as_ref() == Some(error)
    }

    /// 底层连接（测试与诊断用；协议允许并发请求）。
    pub fn connection(&self) -> &Arc<LspConnection> {
        &self.connection
    }

    /// 串行化队列的句柄：provider 等待「这个实例安静下来」用。
    pub fn queue(&self) -> &Arc<tokio::sync::Mutex<()>> {
        &self.queue
    }

    /// 经串行队列跑一条查询。
    ///
    /// 等队列期间也观察取消：前一条查询挂死时，后来者的超时必须还能放弃等待
    /// （而不是永远排在共享队列后面）。
    pub async fn query(
        &self,
        request: &LspProviderQuery,
        source: &HostSource,
    ) -> Result<LspQueryResult, LspError> {
        let signal = request.request.signal.clone();
        let _guard = tokio::select! {
            guard = self.queue.lock() => guard,
            _ = wait_cancelled(&signal) => return Err(crate::cancel::abort_error()),
        };
        let outcome = self.run_query(request, source, &signal).await;
        // 传输失败把进程留在未知状态：先试拆卸（错误被吞掉、留给 provider 终报）再抛。
        if let Err(error) = &outcome {
            if self.is_transport_failure(error) {
                self.await_teardown_attempt().await;
            }
        }
        outcome
    }

    /// 一条查询的完整生命周期（已在队列内）。
    async fn run_query(
        &self,
        request: &LspProviderQuery,
        source: &HostSource,
        signal: &Option<CancelToken>,
    ) -> Result<LspQueryResult, LspError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(LspError::new(
                LspErrorCode::Disposed,
                "LSP instance was disposed",
            ));
        }
        check_cancelled(signal)?;

        // 等握手；失败的握手（编码协商失败、结构不对、进程没退出就死了）要拆掉
        // 实例，别让一个永久拒绝的 ready 毒化这个 workspace 的后续查询。
        let capabilities = match self.ready().await {
            Ok(capabilities) => capabilities,
            Err(error) => {
                if !self.dead() {
                    self.await_teardown_attempt().await;
                }
                return Err(error);
            }
        };
        let operation = request.operation();
        if !supports_operation(&capabilities, operation) {
            return Err(LspError::new(
                LspErrorCode::UnsupportedOperation,
                format!("server does not support {}", operation.as_str()),
            ));
        }
        if !supports_transient_open(capabilities.get("textDocumentSync")) {
            return Err(LspError::new(
                LspErrorCode::UnsupportedOperation,
                "server does not support the transient textDocument/didOpen this host requires",
            ));
        }

        let uri = &source.file_url;
        // didOpen：写失败或等待中被取消都让文档状态不可知——拆掉实例让池驱逐它。
        let opened: Result<(), LspError> = tokio::select! {
            sent = self.connection.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": request.language_id,
                        "version": 1,
                        "text": source.text,
                    },
                }),
            ) => match sent {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.await_teardown_attempt().await;
                    Err(error)
                }
            },
            _ = wait_cancelled(signal) => {
                self.await_teardown_attempt().await;
                Err(crate::cancel::abort_error())
            }
        };
        // didOpen 失败/取消在 select 分支里已处理拆卸并返回错误。
        let outcome = match opened {
            Ok(()) => self
                .send_request(operation, uri, request.position(), signal)
                .await,
            Err(error) => Err(error),
        };
        // finally：已开且实例还活着就补 didClose；关不掉不覆盖主结果，但要拆实例。
        if !self.dead() {
            if let Err(close_failure) = self
                .connection
                .notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }))
                .await
            {
                let _ = close_failure;
                self.await_teardown_attempt().await;
            }
        }
        match outcome {
            Ok(payload) => self.normalize(operation, payload),
            Err(error) => Err(error),
        }
    }

    /// 发语义请求；带取消时竞速并给服务器一个有界宽限去兑现 `$/cancelRequest`。
    async fn send_request(
        &self,
        operation: LspOperation,
        uri: &str,
        position: crate::types::LspPosition,
        signal: &Option<CancelToken>,
    ) -> Result<Value, LspError> {
        let mut params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
        });
        // findReferences 恒包含声明：调用方拿不到开关，影响分析永远不漏定义点。
        if operation == LspOperation::FindReferences {
            params["context"] = json!({ "includeDeclaration": true });
        }
        let method = operation.method();
        let request_id = self.connection.peek_next_id();
        // 请求任务持连接 Arc：取消赢得竞速后任务继续，响应仍会结算在途表
        // （与上游「send promise 不被丢弃」同构）。
        let mut send = tokio::spawn({
            let connection = Arc::clone(&self.connection);
            let method = method.to_string();
            let params = params.clone();
            async move { connection.request(&method, params).await }
        });

        let response = match signal {
            None => match send.await {
                Ok(result) => result,
                Err(_) => Err(LspError::new(
                    LspErrorCode::ServerFailed,
                    "language server request task failed",
                )),
            },
            Some(signal) => {
                let cancelled = tokio::select! {
                    result = &mut send => Some(result),
                    _ = signal.wait() => None,
                };
                match cancelled {
                    Some(result) => match result {
                        Ok(result) => result,
                        Err(_) => Err(LspError::new(
                            LspErrorCode::ServerFailed,
                            "language server request task failed",
                        )),
                    },
                    None => {
                        // 已取消：尽力发 $/cancelRequest，然后有界地等它结算。
                        self.connection.cancel(request_id).await;
                        let grace = Duration::from_millis(self.spec.connection.kill_grace_ms);
                        let settled = tokio::select! {
                            _ = &mut send => true,
                            _ = tokio::time::sleep(grace) => false,
                        };
                        if !settled {
                            // 请求还活着：终止实例（拆卸会等进程关闭），别让它活得比查询久。
                            self.await_teardown_attempt().await;
                        }
                        Err(crate::cancel::abort_error())
                    }
                }
            }
        };
        response
    }

    /// 归一结果（上游 `normalize`）。
    fn normalize(&self, operation: LspOperation, payload: Value) -> Result<LspQueryResult, LspError> {
        if operation == LspOperation::Hover {
            let hover = normalize_hover(Some(&payload))?;
            return Ok(LspQueryResult::Hover { hover });
        }
        // resolved_workspace_uri 由本实例的工作区 URI 提供：渲染时用它做相对化，
        // 而不是在渲染层重解析 spec.cwd（执行平台可能与调用方不同）。
        let locations = normalize_locations(Some(&payload))?;
        Ok(LspQueryResult::Locations {
            locations,
            resolved_workspace_uri: self.spec.workspace_uri.clone(),
        })
    }

    /// 等握手结果（多条查询共享同一次握手）。
    async fn ready(&self) -> Result<Value, LspError> {
        let mut rx = self.capabilities.clone();
        loop {
            if let Some(outcome) = rx.borrow_and_update().clone() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                // 发送端只在写结果后 drop；此路径是防御性的。
                return Err(LspError::new(
                    LspErrorCode::ServerFailed,
                    "language server exited before initialize completed",
                ));
            }
        }
    }

    /// 拒绝后续工作并拆卸：优雅 shutdown/exit → 超时升级强杀，等进程真正退出。
    pub async fn dispose(&self) -> Result<(), LspError> {
        let rx = self.start_teardown();
        Self::await_teardown(rx).await
    }

    /// 等拆卸完成但把失败留给 provider 级的最终上报（上游 `awaitTeardownAttempt`）。
    pub async fn await_teardown_attempt(&self) {
        let rx = self.start_teardown();
        let _ = Self::await_teardown(rx).await;
    }

    /// 只发布一次拆卸；返回结果通道让所有等待者等同一个静默边界。
    fn start_teardown(&self) -> watch::Receiver<Option<Result<(), LspError>>> {
        self.disposed.store(true, Ordering::SeqCst);
        let mut slot = self.teardown.lock().unwrap();
        if !slot.started {
            slot.started = true;
            let (tx, rx) = watch::channel(None);
            slot.result = Some(rx);
            let connection = Arc::clone(&self.connection);
            let shutdown_timeout = Duration::from_millis(self.spec.shutdown_timeout_ms);
            // 拆卸跑在自己的任务里：调用方提前放弃等待也不中断拆卸本身。
            tokio::spawn(async move {
                let outcome = run_teardown(connection, shutdown_timeout).await;
                let _ = tx.send(Some(outcome));
            });
        }
        slot.result
            .clone()
            .expect("发布过拆卸就一定有结果通道")
    }

    /// 在结果通道上等拆卸完成；发送端异常消失是防御性错误。
    async fn await_teardown(
        mut rx: watch::Receiver<Option<Result<(), LspError>>>,
    ) -> Result<(), LspError> {
        loop {
            if let Some(outcome) = rx.borrow_and_update().clone() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                return Err(LspError::new(
                    LspErrorCode::ServerFailed,
                    "language server teardown did not finish",
                ));
            }
        }
    }
}

/// `initialize` 握手（上游 `initialize`）。
async fn initialize(
    connection: &Arc<LspConnection>,
    workspace_uri: &str,
    initialization_options: &Value,
) -> Result<Value, LspError> {
    let result = connection
        .request(
            "initialize",
            json!({
                // 子进程 provider 可能跑在另一个 PID 命名空间/机器上；宿主 PID 会让
                // 服务器监视一个不相关的进程。
                "processId": Value::Null,
                "rootUri": workspace_uri,
                "workspaceFolders": [{ "uri": workspace_uri, "name": "workspace" }],
                "capabilities": client_capabilities(),
                "initializationOptions": initialization_options,
            }),
        )
        .await?;
    let capabilities = result
        .get("capabilities")
        .cloned()
        .unwrap_or(Value::Null);
    // 缺省编码按 utf-16；其它取值是本宿主不支持的协议错误。
    negotiate_position_encoding(
        capabilities.get("positionEncoding").and_then(Value::as_str),
    )?;
    connection.notify("initialized", json!({})).await?;
    Ok(capabilities)
}

/// 拆卸事务：优雅阶段有界，强杀阶段无界（上游 `tearDown`）。
async fn run_teardown(
    connection: Arc<LspConnection>,
    shutdown_timeout: Duration,
) -> Result<(), LspError> {
    let graceful = async {
        connection.request("shutdown", Value::Null).await?;
        connection.notify("exit", Value::Null).await?;
        connection.closed().await;
        Ok::<(), LspError>(())
    };
    // 优雅失败或超时都吞掉：下面的受管范围回收才是权威收尾。
    let _ = tokio::time::timeout(shutdown_timeout, graceful).await;
    connection.terminate().await;
    connection.closed().await;
    connection.wait_for_exit().await;
    Ok(())
}

/// 应答服务端→客户端请求（上游 `answerServerRequest`）。
fn answer_server_request(method: &str, params: Value, configuration: &Value) -> Result<Value, String> {
    match method {
        "workspace/configuration" => {
            // 每一条询问项都用同一个静态配置值应答。
            let items = params
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(Value::Array(vec![configuration.clone(); items.len()]))
        }
        // 生命周期簿记：回空结果即可（本宿主不注册任何动态能力）。
        "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => {
            Ok(Value::Null)
        }
        "workspace/applyEdit" => {
            Err("workspace/applyEdit is not permitted by this host".to_string())
        }
        other => Err(format!("unsupported server request: {other}")),
    }
}

/// `initialize` 声明的客户端能力（上游 `CLIENT_CAPABILITIES` 逐字段）。
fn client_capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        "workspace": { "workspaceFolders": true, "configuration": true },
        "textDocument": {
            "synchronization": { "dynamicRegistration": false },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": true },
            "implementation": { "linkSupport": true },
            "references": {},
        },
    })
}

/// 等待一个可选的取消信号（`None` 永不完成）。
async fn wait_cancelled(signal: &Option<CancelToken>) {
    match signal {
        Some(signal) => signal.wait().await,
        None => std::future::pending().await,
    }
}

/// 已取消时抛上游同款中止错误。
fn check_cancelled(signal: &Option<CancelToken>) -> Result<(), LspError> {
    match signal {
        Some(signal) => signal.check(),
        None => Ok(()),
    }
}