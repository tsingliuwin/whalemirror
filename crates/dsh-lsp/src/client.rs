//! LSP JSON-RPC 端点：一条 spawn 出来的 stdio 语言服务器上的分帧连接。
//!
//! 对齐 [`packages/lsp/lsp-stdio/src/connection.ts`]：本模块自己拥有 id 关联、出站
//! 请求/通知、**入站的服务端→客户端请求**（交给调用方的 [`ServerRequestHandler`]
//! 应答；本宿主从不应用编辑、也不执行命令），以及有界的 stderr 诊断尾部。分帧/解码
//! 失败是不可恢复的：整条连接判死、在途请求全部拒绝、受管进程范围一并终止，避免
//! 辅助进程比 leader 活得更久。平台机制（环境清洗、可执行文件解析、进程组/Job
//! Object）都在 [`dsh_subprocess`] 的缝里，本模块不重复实现。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStderr, ChildStdout};
use tokio::sync::{oneshot, watch};

use dsh_subprocess::{spawn_piped, SpawnSpec, Stdio};

use crate::framing::{encode_message, MessageDecoder};
use crate::{LspError, LspErrorCode};

/// 等 stderr 泵把已到达的字节收进尾部后再拼退出文案的时间。
///
/// 子进程退出时 stderr 的字节已经在管道里，正常情况下泵在同一轮调度里就能读完；
/// 这里给一个上限，避免为了「诊断文案里带上 stderr」把关闭判定永久挂住。
const STDERR_SETTLE: Duration = Duration::from_millis(50);

/// 拉起一台语言服务器并维持这条连接所需的全部参数。
#[derive(Clone, Debug)]
pub struct ConnectionSpec {
    /// 解析后的可执行文件路径（不经 shell）。
    pub command: String,
    /// 传给可执行文件的参数。
    pub args: Vec<String>,
    /// 子进程的工作目录（规范化后的工作区）。
    pub cwd: Option<PathBuf>,
    /// 显式环境覆盖（缝在清洗后合并）。
    pub env: Vec<(String, String)>,
    /// 接受的服务端单条消息上限（字节）。
    pub max_message_bytes: usize,
    /// 保留的 stderr 尾部上限（字节）。
    pub max_stderr_bytes: usize,
    /// 终止宽限：`terminate` 后等这么久再放弃。
    pub kill_grace_ms: u64,
}

impl ConnectionSpec {
    /// 以可执行文件与参数构造，其余取本宿主的默认上限。
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            cwd: None,
            env: Vec::new(),
            max_message_bytes: 16_000_000,
            max_stderr_bytes: 1_000_000,
            kill_grace_ms: 2_000,
        }
    }
}

/// 应答一次服务端→客户端请求；返回 `Err(message)` 时以 -32601 回错误响应。
pub type ServerRequestHandler =
    Arc<dyn Fn(&str, Value) -> Result<Value, String> + Send + Sync>;

/// 一条活着的 JSON-RPC 端点。
pub struct LspConnection {
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    process: Arc<tokio::sync::Mutex<dsh_subprocess::ManagedProcess>>,
    shared: Arc<Shared>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl LspConnection {
    /// 拉起子进程并开始泵 stdout/stderr。
    ///
    /// 必须在 tokio 运行时内调用（内部会起读取任务）。
    pub fn spawn(
        spec: ConnectionSpec,
        on_request: ServerRequestHandler,
    ) -> Result<Self, LspError> {
        if spec.max_message_bytes == 0 {
            return Err(LspError::new(
                LspErrorCode::InvalidProvider,
                "lsp-stdio: maxMessageBytes must be a positive integer",
            ));
        }
        if spec.max_stderr_bytes == 0 {
            return Err(LspError::new(
                LspErrorCode::InvalidProvider,
                "lsp-stdio: maxStderrBytes must be a positive integer",
            ));
        }

        let mut argv = vec![spec.command.clone()];
        argv.extend(spec.args.iter().cloned());
        let mut spawn_spec = SpawnSpec::new(argv);
        spawn_spec.cwd = spec.cwd.clone();
        spawn_spec.env = spec.env.clone();
        spawn_spec.grace_ms = spec.kill_grace_ms;
        // 三条流都由本模块自己处置：stdin/stdout 是分帧协议，stderr 是有界诊断尾部。
        spawn_spec.stdin = Stdio::Pipe;
        spawn_spec.stdout = Stdio::Pipe;
        spawn_spec.stderr = Stdio::Pipe;

        let (process, stdout, stderr, stdin) = spawn_piped(&spawn_spec)
            .map_err(|error| LspError::new(LspErrorCode::ServerFailed, error))?;
        let (Some(stdout), Some(stdin)) = (stdout, stdin) else {
            return Err(LspError::new(
                LspErrorCode::ServerFailed,
                "lsp-stdio: subprocess implementation dropped a piped protocol stream",
            ));
        };

        let (closed_tx, _) = watch::channel(false);
        let shared = Arc::new(Shared::new(spec.max_stderr_bytes, closed_tx));
        let stdin = Arc::new(tokio::sync::Mutex::new(Some(stdin)));
        let process = Arc::new(tokio::sync::Mutex::new(process));

        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(pump_stdout(
            stdout,
            Arc::clone(&shared),
            Arc::clone(&process),
            Arc::clone(&stdin),
            Arc::clone(&on_request),
            spec.max_message_bytes,
        )));
        if let Some(stderr) = stderr {
            tasks.push(tokio::spawn(pump_stderr(stderr, Arc::clone(&shared))));
        }

        Ok(Self { stdin, process, shared, tasks: Mutex::new(tasks) })
    }

    /// 保留的 stderr 尾部（诊断用）。
    pub fn stderr_tail(&self) -> String {
        self.shared.stderr_tail()
    }

    /// 传输是否已经失败（即使子进程的关闭事件还没到）。
    pub fn failed(&self) -> bool {
        self.shared.failed()
    }

    /// 保留的致命原因（没有失败时为 `None`）。
    pub fn reason(&self) -> Option<LspError> {
        self.shared.reason()
    }

    /// 等连接关闭（失败或子进程退出）。
    pub async fn closed(&self) {
        let mut rx = self.shared.closed.subscribe();
        if *rx.borrow() {
            return;
        }
        // 发送方只在关闭时写入一次；错误意味着它已经 drop，同样视为关闭。
        let _ = rx.changed().await;
    }

    /// 发一条请求并等结果。
    ///
    /// 关闭后发出的请求立即失败（子进程关闭后不会再有任何响应），写入失败的请求会把
    /// 整条连接判死并拒绝所有在途请求。
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        if let Some(reason) = self.shared.reason() {
            return Err(reason);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        if let Err(error) = self
            .write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await
        {
            return Err(error);
        }
        match rx.await {
            Ok(result) => result,
            // 发送端被丢弃只发生在关闭路径上，此时原因已经记录。
            Err(_) => Err(self.shared.reason().unwrap_or_else(|| {
                LspError::new(LspErrorCode::ServerFailed, "language server exited")
            })),
        }
    }

    /// 发一条通知（无 id、无响应）。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// 对一条在途请求发 `$/cancelRequest`（尽力而为：写失败只判死连接，不回报）。
    pub async fn cancel(&self, request_id: u64) {
        let _ = self
            .write(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": { "id": request_id },
            }))
            .await;
    }

    /// 下一条 `request()` 会用的 id，便于调用方先备好取消。
    pub fn peek_next_id(&self) -> u64 {
        self.shared.next_id.load(Ordering::Relaxed)
    }

    /// 终止受管进程范围（幂等）。
    pub async fn terminate(&self) {
        let mut process = self.process.lock().await;
        let _ = process.terminate().await;
    }

    /// 等受管进程范围清空；返回是否在宽限内退出。
    pub async fn wait_for_exit(&self) -> bool {
        let mut process = self.process.lock().await;
        process.wait_for_exit().await.is_ok()
    }

    /// 写一条分帧消息；写失败立即把整条连接判死。
    async fn write(&self, message: Value) -> Result<(), LspError> {
        if let Some(reason) = self.shared.reason() {
            return Err(reason);
        }
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(self.shared.reason().unwrap_or_else(|| {
                LspError::new(LspErrorCode::ServerFailed, "language server stdin is closed")
            }));
        };
        let framed = encode_message(&message);
        if let Err(error) = stdin.write_all(&framed).await {
            let failure = LspError::new(
                LspErrorCode::ServerFailed,
                format!("lsp write failed: {error}"),
            );
            drop(guard);
            self.shared.fail(failure.clone());
            return Err(failure);
        }
        if let Err(error) = stdin.flush().await {
            let failure = LspError::new(
                LspErrorCode::ServerFailed,
                format!("lsp write failed: {error}"),
            );
            drop(guard);
            self.shared.fail(failure.clone());
            return Err(failure);
        }
        Ok(())
    }
}

impl std::fmt::Debug for LspConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspConnection")
            .field("next_id", &self.shared.next_id.load(Ordering::Relaxed))
            .field("failed", &self.shared.failed())
            .finish()
    }
}

impl Drop for LspConnection {
    fn drop(&mut self) {
        // 读取任务持有 Shared/process 的 Arc；连接先走就把它们停掉，进程句柄随
        // ManagedProcess 的 `kill_on_drop` 收尾。
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// 连接内共享的关闭事实、在途表与 stderr 尾部。
struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, LspError>>>>,
    reason: Mutex<Option<LspError>>,
    stderr: Mutex<Vec<u8>>,
    max_stderr_bytes: usize,
    next_id: AtomicU64,
    closed: watch::Sender<bool>,
}

impl Shared {
    fn new(max_stderr_bytes: usize, closed: watch::Sender<bool>) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            reason: Mutex::new(None),
            stderr: Mutex::new(Vec::new()),
            max_stderr_bytes,
            next_id: AtomicU64::new(1),
            closed,
        }
    }

    fn failed(&self) -> bool {
        self.reason.lock().unwrap().is_some()
    }

    fn reason(&self) -> Option<LspError> {
        self.reason.lock().unwrap().clone()
    }

    /// 记录第一次失败原因、拒绝所有在途请求、广播关闭。
    fn fail(&self, error: LspError) {
        {
            let mut reason = self.reason.lock().unwrap();
            if reason.is_none() {
                *reason = Some(error.clone());
            }
        }
        let waiting: Vec<_> = self.pending.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        for tx in waiting {
            let _ = tx.send(Err(error.clone()));
        }
        let _ = self.closed.send(true);
    }

    /// 追加 stderr 并只保留尾部 `max_stderr_bytes`。
    fn append_stderr(&self, bytes: &[u8]) {
        let mut tail = self.stderr.lock().unwrap();
        tail.extend_from_slice(bytes);
        if tail.len() > self.max_stderr_bytes {
            let excess = tail.len() - self.max_stderr_bytes;
            tail.drain(..excess);
        }
    }

    fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    /// 退出文案：stderr 有内容时附上尾部（与上游 `exitMessage` 逐字一致）。
    fn exit_message(&self) -> String {
        let tail = self.stderr_tail();
        let trimmed = tail.trim();
        if trimmed.is_empty() {
            "language server exited".to_string()
        } else {
            format!("language server exited; stderr: {trimmed}")
        }
    }

    /// 用一条响应结算一条在途请求。
    fn settle(&self, id: u64, result: Result<Value, LspError>) {
        let pending = self.pending.lock().unwrap().remove(&id);
        if let Some(tx) = pending {
            let _ = tx.send(result);
        }
    }
}

/// stdout 泵：解码、派发，遇到分帧/JSON 失败即判死并终止受管范围。
async fn pump_stdout(
    mut stdout: ChildStdout,
    shared: Arc<Shared>,
    process: Arc<tokio::sync::Mutex<dsh_subprocess::ManagedProcess>>,
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    on_request: ServerRequestHandler,
    max_message_bytes: usize,
) {
    let mut decoder = MessageDecoder::new(max_message_bytes);
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => match decoder.push(&buffer[..read]) {
                Ok(messages) => {
                    for message in messages {
                        dispatch(message, &shared, &stdin, &on_request);
                    }
                }
                Err(error) => {
                    // 流位置已被破坏，不可恢复：判死并终止进程，避免辅助进程残留。
                    shared.fail(LspError::new(LspErrorCode::MalformedResponse, error));
                    if let Ok(mut process) = process.try_lock() {
                        let _ = process.start_kill();
                    }
                    return;
                }
            },
            Err(_) => break,
        }
    }
    // 子进程退出（或关掉了 stdout）：给 stderr 泵一点时间，然后按退出文案判死。
    tokio::time::sleep(STDERR_SETTLE).await;
    shared.fail(LspError::new(LspErrorCode::ServerFailed, shared.exit_message()));
}

/// stderr 泵：只保留有界尾部。
async fn pump_stderr(mut stderr: ChildStderr, shared: Arc<Shared>) {
    let mut buffer = vec![0u8; 8 * 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => shared.append_stderr(&buffer[..read]),
            Err(_) => break,
        }
    }
}

/// 派发一条入站消息：服务端请求要应答，服务端通知忽略，响应结算在途请求。
fn dispatch(
    message: Value,
    shared: &Arc<Shared>,
    stdin: &Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    on_request: &ServerRequestHandler,
) {
    let Value::Object(frame) = message else { return };
    match (frame.get("method"), frame.get("id")) {
        // 服务端 → 客户端请求：异步应答，不阻塞后续帧的读取。
        (Some(Value::String(method)), Some(id)) if id.is_number() || id.is_string() => {
            let method = method.clone();
            let id = id.clone();
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let handler = Arc::clone(on_request);
            let stdin = Arc::clone(stdin);
            tokio::spawn(async move {
                let response = match handler(&method, params) {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err(message) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": message },
                    }),
                };
                let mut guard = stdin.lock().await;
                if let Some(stdin) = guard.as_mut() {
                    let _ = stdin.write_all(&encode_message(&response)).await;
                    let _ = stdin.flush().await;
                }
            });
        }
        // 服务端通知（诊断、日志等）：本宿主忽略。
        (Some(_), _) => {}
        // 响应：结算对应请求。
        (None, Some(Value::Number(id))) => {
            let Some(id) = id.as_u64() else { return };
            if let Some(error) = frame.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("LSP error response")
                    .to_string();
                shared.settle(id, Err(LspError::new(LspErrorCode::ServerFailed, message)));
                return;
            }
            let result = frame.get("result").cloned().unwrap_or(Value::Null);
            shared.settle(id, Ok(result));
        }
        _ => {}
    }
}