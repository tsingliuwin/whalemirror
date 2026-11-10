//! 通用 stdio provider：`settings.yaml servers` 表里一个条目对应一个 provider。
//!
//! 对齐 [`packages/lsp/lsp-stdio/src/index.ts`] 的 `LocalLspProvider`：每个 provider
//! 对每个规范化 workspace 惰性起**一台**服务器进程，查询经瞬态文档走过去；被选中的
//! 传输在下次只读查询前/中失败时，干净替换一次并透明重试。源文件读取在**队列内、
//! 起进程之前**——排到的查询看到的是「轮到它时的当前字节」。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::cancel::CancelToken;
use crate::host::{canonicalize_workspace, read_source, HostSource, HostWorkspace};
use crate::instance::{InstanceSpec, LspInstance};
use crate::types::{LspProvider, LspProviderQuery, LspQueryResult};
use crate::{LspError, LspErrorCode};

/// 一台已解析的本地语言服务器配置（上游 `LspLocalServerConfig` 的必需字段）。
#[derive(Clone, Debug)]
pub struct StdioProviderConfig {
    /// 解析后的可执行文件（注册前已 resolve，绝不经 shell）。
    pub executable: String,
    /// 传给可执行文件的参数。
    pub args: Vec<String>,
    /// 显式环境覆盖（清洗后合并）。
    pub env: Vec<(String, String)>,
    /// 子进程工作目录之外的初始化参数见 [`InstanceSpec`]。
    pub initialization_options: Value,
    /// `workspace/configuration` 的静态应答。
    pub configuration: Value,
    /// 单条消息上限。
    pub max_message_bytes: usize,
    /// stderr 尾部上限。
    pub max_stderr_bytes: usize,
    /// 单份源文件上限。
    pub max_document_bytes: usize,
    /// 优雅 shutdown/exit 预算。
    pub shutdown_timeout_ms: u64,
    /// 取消宽限与 SIGTERM→SIGKILL 宽限。
    pub kill_grace_ms: u64,
}

/// 池化的通用 provider：每个规范化 workspace 一台实例，按需创建。
pub struct StdioProvider {
    id: String,
    extension_to_language: HashMap<String, String>,
    config: StdioProviderConfig,
    instances: Mutex<HashMap<String, Arc<LspInstance>>>,
    /// 每个工作区一条完整「读源→起实例→查询」的串行尾巴。
    queues: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    lifetime: CancelToken,
    disposed: AtomicBool,
}

impl StdioProvider {
    /// 用一份已解析的配置构造（`executable` 必须已经 resolve 过）。
    pub fn new(
        id: impl Into<String>,
        extension_to_language: HashMap<String, String>,
        config: StdioProviderConfig,
    ) -> Self {
        Self {
            id: id.into(),
            extension_to_language,
            config,
            instances: Mutex::new(HashMap::new()),
            queues: Mutex::new(HashMap::new()),
            lifetime: CancelToken::new(),
            disposed: AtomicBool::new(false),
        }
    }

    /// 丢弃全部实例并拒绝后续查询。
    ///
    /// 返回首个拆卸失败（多个失败时只报第一个——错误本身已带定位信息）。
    pub async fn dispose_all(&self) -> Result<(), LspError> {
        self.disposed.store(true, Ordering::SeqCst);
        // 用实例级的中止原因标记 lifetime；等待中的宿主 I/O 与队列由此解锁。
        self.lifetime.abort();

        let live: Vec<Arc<LspInstance>> = {
            let mut instances = self.instances.lock().unwrap();
            instances.drain().map(|(_, instance)| instance).collect()
        };
        let queues: Vec<Arc<tokio::sync::Mutex<()>>> = {
            let mut queues = self.queues.lock().unwrap();
            queues.drain().map(|(_, queue)| queue).collect()
        };

        // 先拆卸（杀进程，在途查询随传输失败而结束），再等每个队列安静下来。
        let mut first_failure: Option<LspError> = None;
        for instance in &live {
            if let Err(error) = instance.dispose().await {
                first_failure.get_or_insert(error);
            }
        }
        for queue in &queues {
            let _ = queue.lock().await;
        }
        match first_failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 调用方信号与 provider lifetime 的合取（上游 `querySignal` 的 `AbortSignal.any`）。
    fn fused_is_cancelled(&self, signal: &Option<CancelToken>) -> bool {
        self.lifetime.cancelled()
            || signal.as_ref().is_some_and(CancelToken::cancelled)
    }

    /// 取或同步发布这个工作区的实例（上游 `instanceFor`）。
    fn instance_for(
        &self,
        key: &str,
        workspace: &HostWorkspace,
    ) -> Result<Arc<LspInstance>, LspError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(LspError::new(
                LspErrorCode::Disposed,
                "lsp-stdio provider is disposed",
            ));
        }
        let mut instances = self.instances.lock().unwrap();
        if let Some(existing) = instances.get(key) {
            return Ok(Arc::clone(existing));
        }
        let spec = InstanceSpec {
            connection: crate::client::ConnectionSpec {
                command: self.config.executable.clone(),
                args: self.config.args.clone(),
                cwd: Some(workspace.canonical_path.clone()),
                env: self.config.env.clone(),
                max_message_bytes: self.config.max_message_bytes,
                max_stderr_bytes: self.config.max_stderr_bytes,
                kill_grace_ms: self.config.kill_grace_ms,
            },
            workspace_uri: workspace.file_url.clone(),
            initialization_options: self.config.initialization_options.clone(),
            configuration: self.config.configuration.clone(),
            shutdown_timeout_ms: self.config.shutdown_timeout_ms,
        };
        let created = Arc::new(LspInstance::spawn(spec)?);
        instances.insert(key.to_string(), Arc::clone(&created));
        Ok(created)
    }

    /// 槽位还装着这个实例时才清除（上游 `evictIfCurrent`）。
    fn evict_if_current(&self, key: &str, instance: &Arc<LspInstance>) {
        let mut instances = self.instances.lock().unwrap();
        if let Some(current) = instances.get(key) {
            if Arc::ptr_eq(current, instance) {
                instances.remove(key);
            }
        }
    }

    /// 一条查询的完整生命周期：读源 → 取/建实例 → 查询（死实例透明换新重试一次）。
    async fn run_query(
        &self,
        request: &LspProviderQuery,
        key: &str,
        workspace: &HostWorkspace,
    ) -> Result<LspQueryResult, LspError> {
        // 队列内、起进程之前读源：无效源不会留下一个空转进程。
        let source: HostSource = read_source(
            request.file_path(),
            workspace,
            self.config.max_document_bytes,
        )
        .map_err(|message| {
            LspError::new(LspErrorCode::DocumentUnavailable, message)
        })?;

        let mut instance = self.instance_for(key, workspace)?;
        let mut can_retry_transport = true;
        loop {
            let outcome = instance.query(request, &source).await;
            // 死实例永不可复用（哪怕它最终的静默观察失败了）。
            if instance.dead() {
                let _ = instance.dispose().await;
                self.evict_if_current(key, &instance);
            }
            match outcome {
                Ok(result) => return Ok(result),
                Err(error) => {
                    // 只读查询：被选中的子进程死在空闲或下次写入时，干净换一次再试。
                    if !can_retry_transport || !instance.is_transport_failure(&error) {
                        return Err(error);
                    }
                    can_retry_transport = false;
                    if self.disposed.load(Ordering::SeqCst) {
                        return Err(LspError::new(
                            LspErrorCode::Disposed,
                            "lsp-stdio provider is disposed",
                        ));
                    }
                    instance = self.instance_for(key, workspace)?;
                }
            }
        }
    }
}

#[async_trait]
impl LspProvider for StdioProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn extension_to_language(&self) -> &HashMap<String, String> {
        &self.extension_to_language
    }

    async fn query(&self, request: LspProviderQuery) -> Result<LspQueryResult, LspError> {
        // 已中止的信号在 provider I/O 之前生效：被取消的请求绝不拉起服务器。
        if self.disposed.load(Ordering::SeqCst) {
            return Err(LspError::new(
                LspErrorCode::Disposed,
                "lsp-stdio provider is disposed",
            ));
        }
        if self.fused_is_cancelled(&request.request.signal) {
            return Err(crate::cancel::abort_error());
        }

        let workspace = canonicalize_workspace(&request.request.workspace_root).map_err(|message| {
            LspError::new(LspErrorCode::DocumentUnavailable, message)
        })?;
        let key = workspace.canonical_path.display().to_string();

        // 每个工作区一条串行尾巴：后来者等前一条的**实际工作**。
        let queue = {
            let mut queues = self.queues.lock().unwrap();
            Arc::clone(queues.entry(key.clone()).or_default())
        };
        let signal = request.request.signal.clone();
        let _guard = tokio::select! {
            guard = queue.lock() => guard,
            _ = wait_cancelled(&request.request.signal) => {
                return Err(crate::cancel::abort_error());
            }
        };
        let _ = signal; // 取消只用于早退；队列锁在 select 分支里已经拿到/放弃。
        if self.disposed.load(Ordering::SeqCst) {
            return Err(LspError::new(
                LspErrorCode::Disposed,
                "lsp-stdio provider is disposed",
            ));
        }
        self.run_query(&request, &key, &workspace).await
    }
}

/// 等待一个可选的取消信号（`None` 永不完成）。
async fn wait_cancelled(signal: &Option<CancelToken>) {
    match signal {
        Some(signal) => signal.wait().await,
        None => std::future::pending().await,
    }
}