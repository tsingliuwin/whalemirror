//! dsh-subprocess — 统一子进程缝（上游 `packages/subprocess/*` 的等价面）。
//!
//! 上游的动机与本仓一致：**所有**子进程（bash 命令、后台作业、MCP 服务端、
//! 将来的 LSP / PTY / ssh）都必须走同一个缝，否则环境清洗、输出上限、进程组
//! 终止这些语义会在每个调用点各写一遍并各自漂移——rustdsh 之前正是三份副本
//! （`dsh-shell` 前台、`dsh-shell` 后台泵、`dsh-mcp` stdio）。
//!
//! 本 crate 提供上游的**核心四项**：
//! 1. [`scrubbed_parent_env`]：每个子进程的起始环境——去掉凭据形状的名字与
//!    所有 `DSH_*`（大小写不敏感），保留 PATH/HOME/区域/代理；
//! 2. [`resolve_executable`]：显式解析可执行文件（`CreateProcess` 裸名会先搜
//!    System32，曾把 shell 劫持进 WSL——必须返回解析后的绝对路径）；
//! 3. [`spawn`]：完全显式的 spawn 请求（argv / cwd / 每流处置 / 宽限 / 环境
//!    覆盖），输出按**非消费式偏移**读，可带 spill 文件；
//! 4. [`ManagedProcess::terminate`] / [`ManagedProcess::wait_for_exit`]：
//!    统一终止与静默等待。
//!
//! 未做（登记 backlog）：PTY 终端会话、control pipe（fd 7）、Linux scope
//! owner、Windows Job Object 的进程树回收——见 [`ManagedProcess`] 的偏差说明。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `DSH_*` 环境名前缀（上游同名常量）。
pub const DSH_ENV_PREFIX: &str = "DSH_";

/// 凭据形状的环境名（上游 `SENSITIVE_ENV_PATTERN`：大小写不敏感）。
pub const SENSITIVE_ENV_PATTERN: &[&str] = &["KEY", "PASSWORD", "SECRET", "TOKEN"];

/// 环境名是否是凭据形状。
pub fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SENSITIVE_ENV_PATTERN.iter().any(|needle| upper.contains(needle))
}

/// 环境名是否属于 harness 自己的命名空间。
pub fn is_dsh_env_name(name: &str) -> bool {
    name.to_ascii_uppercase().starts_with(DSH_ENV_PREFIX)
}

/// 每个子进程的起始环境（上游 `scrubbedParentEnv`）：父进程环境减去凭据形状
/// 的名字与所有 `DSH_*`。`PATH` / `HOME` / 区域 / 代理照常保留，子 CLI 才能
/// 正常工作；harness 身份不隐式外泄（要故意转发就写进显式 `env`，它在清洗
/// 之后合并）。
pub fn scrubbed_parent_env() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, value) in std::env::vars() {
        if is_sensitive_env_name(&key) || is_dsh_env_name(&key) {
            continue;
        }
        out.push((key, value));
    }
    out
}

/// 在父环境上叠加显式覆盖（覆盖在清洗之后，故显式给的值一定生效）。
pub fn child_env(explicit: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = scrubbed_parent_env();
    for (key, value) in explicit {
        if let Some(slot) = env.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            slot.1 = value.clone();
        } else {
            env.push((key.clone(), value.clone()));
        }
    }
    env
}

/// 解析可执行文件的绝对路径。
///
/// Windows 上裸名会让 `CreateProcess` 先搜 System32——`bash.exe` 在那里是
/// WSL 启动器（曾把 shell 工具劫持进 WSL），所以必须自己按 PATH 顺序找并
/// 规范化。`program` 含路径分隔符时直接按路径处理。
pub fn resolve_executable(program: &str) -> Result<PathBuf, String> {
    if program.trim().is_empty() {
        return Err("executable must be a non-empty string".into());
    }
    let candidate = Path::new(program);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        if candidate.is_file() {
            return Ok(candidate.canonicalize().unwrap_or_else(|_| candidate.to_path_buf()));
        }
        return Err(format!("executable not found: {program}"));
    }
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        let mut list: Vec<String> = pathext
            .split(';')
            .filter(|e| !e.trim().is_empty())
            .map(|e| e.trim().to_ascii_lowercase())
            .collect();
        if !list.iter().any(|e| e == ".exe") {
            list.push(".exe".into());
        }
        list
    } else {
        vec![String::new()]
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for ext in &exts {
                let mut candidate = dir.join(program);
                if !ext.is_empty() {
                    candidate.set_extension(&ext[1..]);
                }
                if candidate.is_file() {
                    // 相对 PATH 项（如 "."）同样会让 CreateProcess 重走搜索序
                    return Ok(candidate.canonicalize().unwrap_or(candidate));
                }
            }
        }
    }
    Err(format!("executable not found on PATH: {program}"))
}

/// 一条输出流的处置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stdio {
    /// 不要这条流（等价于关闭）。
    Ignore,
    /// 交给父进程（透传诊断）。
    Inherit,
    /// 由调用方自己读（MCP 的 JSON-RPC 分帧、LSP 的 header 分帧）。
    Pipe,
    /// 收集：有界内存尾部 + 可选 spill 文件（完整流可恢复）。
    Collect { max_bytes: usize, spill: Option<PathBuf> },
}

/// 一次 spawn 请求（上游 `SubprocessSpawnSpec` 的子集）。
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    /// `argv[0]` 是程序，其余是参数。
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
    /// 显式环境覆盖（清洗之后合并）。
    pub env: Vec<(String, String)>,
    /// 终止宽限：`terminate` 后等这么久再强杀。
    pub grace_ms: u64,
}

impl SpawnSpec {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            cwd: None,
            stdin: Stdio::Ignore,
            stdout: Stdio::Collect { max_bytes: 64 * 1024, spill: None },
            stderr: Stdio::Collect { max_bytes: 64 * 1024, spill: None },
            env: Vec::new(),
            grace_ms: 5_000,
        }
    }

    /// 便捷：`program` + 参数。
    pub fn command(program: impl Into<String>, args: &[&str]) -> Self {
        let mut argv = vec![program.into()];
        argv.extend(args.iter().map(|a| a.to_string()));
        Self::new(argv)
    }
}

/// 有界输出缓冲：保留尾部 `max_bytes`，可同时写 spill 文件。
///
/// 读是**非消费式**的：后台读者与最终批量读共享同一条流而不互相偷字节
/// （上游同语义）。
#[derive(Clone, Default)]
pub struct Collected {
    inner: Arc<Mutex<CollectedInner>>,
}

#[derive(Default)]
struct CollectedInner {
    buf: Vec<u8>,
    /// 已被头部淘汰的字节数（绝对偏移的起点）。
    dropped: usize,
    cap: usize,
}

impl Collected {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CollectedInner { buf: Vec::new(), dropped: 0, cap })),
        }
    }

    fn push(&self, bytes: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        inner.buf.extend_from_slice(bytes);
        if inner.buf.len() > inner.cap {
            let excess = inner.buf.len() - inner.cap;
            inner.buf.drain(..excess);
            inner.dropped += excess;
        }
    }

    /// 追加（供收集任务用，公开以免 with_output 之外还要绕）。
    pub fn append(&self, bytes: &[u8]) {
        self.push(bytes);
    }

    /// 从绝对偏移 `from` 起读保留区（非消费式）。
    pub fn read_from(&self, from: usize) -> (Vec<u8>, usize, bool) {
        let inner = self.inner.lock().unwrap();
        let end = inner.dropped + inner.buf.len();
        let start = from.max(inner.dropped);
        let lossy = from < inner.dropped;
        let slice = if start >= end {
            Vec::new()
        } else {
            inner.buf[start - inner.dropped..].to_vec()
        };
        (slice, end, lossy)
    }

    /// 全部保留文本（UTF-8 lossy）。
    pub fn text(&self) -> String {
        let (bytes, _, _) = self.read_from(0);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// 全部保留字节（编码由调用方裁决：Windows 代码页兜底要拿原始字节）。
    pub fn bytes(&self) -> Vec<u8> {
        self.read_from(0).0
    }

    /// 是否有丢失（头部被淘汰）。
    pub fn lossy(&self) -> bool {
        self.inner.lock().unwrap().dropped > 0
    }
}

/// 一个受管子进程。
pub struct ManagedProcess {
    /// `done` 的退出事实。
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    stdout: Option<Collected>,
    stderr: Option<Collected>,
    child: Option<tokio::process::Child>,
    grace_ms: u64,
    /// 收集任务：`wait_for_exit` 要等它们排空，读到的才是完整输出。
    pumps: Vec<tokio::task::JoinHandle<()>>,
}

/// 排空收集任务的等待上限。
///
/// 子进程退出即关闭管道写端，排空应该是微秒级；但若命令派生的**孙**进程
/// 仍持有写端，读端就永远等不到 EOF——这里设上限，宁可停在部分输出也不
/// 让 `wait_for_exit` 永久挂住。
const DRAIN_MS: u64 = 2_000;

impl ManagedProcess {
    /// 收集到的 stdout / stderr。
    pub fn stdout(&self) -> Option<&Collected> {
        self.stdout.as_ref()
    }

    pub fn stderr(&self) -> Option<&Collected> {
        self.stderr.as_ref()
    }

    /// 排空收集任务（有上限，不因孙进程持有写端而挂死）。
    async fn drain_pumps(&mut self) {
        let grace = std::time::Duration::from_millis(DRAIN_MS);
        for pump in self.pumps.drain(..) {
            let _ = tokio::time::timeout(grace, pump).await;
        }
    }

    /// 等退出（并收尾输出）。返回退出码（信号终止时为 None）。
    pub async fn wait_for_exit(&mut self) -> Result<Option<i32>, String> {
        let Some(mut child) = self.child.take() else {
            return Ok(self.exit_code);
        };
        let status = child.wait().await.map_err(|e| format!("wait failed: {e}"))?;
        self.exit_code = status.code();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.signal() {
                self.signal = Some(sig.to_string());
            }
        }
        self.drain_pumps().await;
        Ok(self.exit_code)
    }

    /// 终止：先 SIGKILL（tokio 的 `kill`），宽限后仍未退出即放弃等待。
    ///
    /// 偏差：上游按「受管进程范围」回收**后代**（Linux scope owner / Windows
    /// Job Object）；rustdsh 只保证直接子进程——派生子进程的命令可能留下孤儿
    /// （`kill_on_drop` 兜住句柄，但孙进程要靠命令自己收尾）。登记 backlog。
    pub async fn terminate(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else { return Ok(()) };
        let _ = child.start_kill();
        let grace = std::time::Duration::from_millis(self.grace_ms.max(1));
        let outcome = match tokio::time::timeout(grace, child.wait()).await {
            Ok(Ok(status)) => {
                self.exit_code = status.code();
                Ok(())
            }
            // 宽限到点仍未退出：不阻塞调用方（进程仍可能活着）
            Ok(Err(error)) => Err(format!("wait after terminate failed: {error}")),
            Err(_) => Err(format!(
                "process did not exit within {}ms of terminate",
                self.grace_ms
            )),
        };
        self.drain_pumps().await;
        outcome
    }

    /// 立即强杀（不等宽限）。
    pub async fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.drain_pumps().await;
    }

    /// 同步发起终止：只投递信号，不等回收。
    ///
    /// 取消钩子是同步上下文（`dsh_jobs::CancelHook` 不能在钩子里 await），
    /// 所以终止必须分两步——这里置信号，等待退出仍在调用方的异步侧完成。
    pub fn start_kill(&mut self) -> Result<(), String> {
        match self.child.as_mut() {
            Some(child) => child.start_kill().map_err(|e| format!("kill failed: {e}")),
            None => Ok(()),
        }
    }
}

/// 一次跑完的批式结果（前台命令用）。
pub struct Completed {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// 拉起 → 等退出 → 交出两条流：`Collect` 处置的便捷封装。
///
/// 只适用于 `Ignore` / `Inherit` / `Collect`；要自己读写分帧（MCP / LSP）用
/// [`spawn_piped`]。
/// run() 的 Pipe 兜底收集上限（1 MiB——批式面的输出量级）。
const RUN_PIPE_CAP_BYTES: usize = 1024 * 1024;

pub async fn run(spec: &SpawnSpec) -> Result<Completed, String> {
    // Pipe 在批式收口下等价有界 Collect（1 MiB）——此前 Pipe 的句柄随
    // ManagedProcess 被 take 后无人读，stdout/stderr 恒空（git 序列实测
    // 抓到：rev-parse 输出为空 → 后续步 cwd 空 → CreateProcess 123）
    let mut spec = spec.clone();
    if spec.stdout == Stdio::Pipe {
        spec.stdout = Stdio::Collect { max_bytes: RUN_PIPE_CAP_BYTES, spill: None };
    }
    if spec.stderr == Stdio::Pipe {
        spec.stderr = Stdio::Collect { max_bytes: RUN_PIPE_CAP_BYTES, spill: None };
    }
    let mut process = spawn(&spec)?;
    let exit_code = process.wait_for_exit().await?;
    let signal = process.signal.clone();
    let stdout = process.stdout().map(Collected::bytes).unwrap_or_default();
    let stderr = process.stderr().map(Collected::bytes).unwrap_or_default();
    Ok(Completed { exit_code, signal, stdout, stderr })
}

/// 拉起一个受管子进程。
///
/// `Pipe` 处置的流由调用方通过 [`spawn_piped`] 取走读写端；本函数适用于
/// `Ignore` / `Inherit` / `Collect` 三种。
pub fn spawn(spec: &SpawnSpec) -> Result<ManagedProcess, String> {
    let stdout_sink = match &spec.stdout {
        Stdio::Collect { max_bytes, .. } => Some(Collected::new(*max_bytes)),
        _ => None,
    };
    let stderr_sink = match &spec.stderr {
        Stdio::Collect { max_bytes, .. } => Some(Collected::new(*max_bytes)),
        _ => None,
    };
    let mut child = spawn_child(spec)?;

    // 收集任务：把 pipe 内容泵进 Collected（spill 文件同源写一份）
    let mut pumps: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    if let (Some(sink), Some(pipe)) = (stdout_sink.clone(), child.stdout.take()) {
        let spill = match &spec.stdout {
            Stdio::Collect { spill, .. } => spill.clone(),
            _ => None,
        };
        pumps.push(tokio::spawn(async move {
            pump(pipe, sink, spill).await;
        }));
    }
    if let (Some(sink), Some(pipe)) = (stderr_sink.clone(), child.stderr.take()) {
        let spill = match &spec.stderr {
            Stdio::Collect { spill, .. } => spill.clone(),
            _ => None,
        };
        pumps.push(tokio::spawn(async move {
            pump(pipe, sink, spill).await;
        }));
    }

    Ok(ManagedProcess {
        exit_code: None,
        signal: None,
        stdout: stdout_sink,
        stderr: stderr_sink,
        child: Some(child),
        grace_ms: spec.grace_ms,
        pumps,
    })
}

/// 需要自己读写分帧的 spawn：返回 (进程, stdout 读端, stderr 读端, stdin 写端)。
///
/// `Ignore` / `Inherit` 的流**尊重调用方**（对应返回位为 None）——MCP 服务端
/// 常常往 stderr 打日志，硬塞一个随后被丢弃的管道会让它 EPIPE。
pub fn spawn_piped(
    spec: &SpawnSpec,
) -> Result<
    (
        ManagedProcess,
        Option<tokio::process::ChildStdout>,
        Option<tokio::process::ChildStderr>,
        Option<tokio::process::ChildStdin>,
    ),
    String,
> {
    let mut piped = spec.clone();
    for slot in [&mut piped.stdin, &mut piped.stdout, &mut piped.stderr] {
        // Collect 在分帧场景没有意义（收集任务归 spawn 管），一律归一到 Pipe
        if !matches!(*slot, Stdio::Ignore | Stdio::Inherit) {
            *slot = Stdio::Pipe;
        }
    }
    let mut child = spawn_child(&piped)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    Ok((
        ManagedProcess {
            exit_code: None,
            signal: None,
            stdout: None,
            stderr: None,
            child: Some(child),
            grace_ms: piped.grace_ms,
            pumps: Vec::new(),
        },
        stdout,
        stderr,
        stdin,
    ))
}

/// 真正落地到 `CreateProcess`：解析程序、清洗环境、按每流处置挂管道。
fn spawn_child(spec: &SpawnSpec) -> Result<tokio::process::Child, String> {
    let program = spec
        .argv
        .first()
        .ok_or_else(|| "spawn requires at least argv[0]".to_string())?;
    let resolved = resolve_executable(program)?;
    let mut cmd = tokio::process::Command::new(&resolved);
    cmd.args(&spec.argv[1..]);
    cmd.env_clear();
    for (key, value) in child_env(&spec.env) {
        cmd.env(key, value);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.kill_on_drop(true);
    cmd.stdin(disposition(&spec.stdin));
    cmd.stdout(disposition(&spec.stdout));
    cmd.stderr(disposition(&spec.stderr));
    #[cfg(windows)]
    {
        // GUI 宿主无控制台可继承：不设此标志每个命令都会弹黑窗并抢前台
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("spawn failed: {e}"))
}

fn disposition(stdio: &Stdio) -> std::process::Stdio {
    use std::process::Stdio as PStdio;
    match stdio {
        Stdio::Ignore => PStdio::null(),
        Stdio::Inherit => PStdio::inherit(),
        // Pipe 在这里也走 piped，取走端由调用方决定
        Stdio::Pipe => PStdio::piped(),
        // Collect：父进程读管道（收集任务接管）
        Stdio::Collect { .. } => PStdio::piped(),
    }
}

async fn pump<R>(mut reader: R, sink: Collected, spill: Option<PathBuf>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut file = spill.and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                sink.push(&buf[..n]);
                if let Some(file) = file.as_mut() {
                    let _ = file.write_all(&buf[..n]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_and_dsh_names_are_scrubbed() {
        assert!(is_sensitive_env_name("DEEPSEEK_API_KEY"));
        assert!(is_sensitive_env_name("github_token"));
        assert!(is_sensitive_env_name("DB_PASSWORD"));
        assert!(is_sensitive_env_name("MY_SECRET"));
        assert!(!is_sensitive_env_name("PATH"));
        assert!(!is_sensitive_env_name("HOME"));

        assert!(is_dsh_env_name("DSH_HOME"));
        assert!(is_dsh_env_name("dsh_sessions_dir"), "Windows 环境名大小写不敏感");
        assert!(!is_dsh_env_name("PATH"));

        let env = scrubbed_parent_env();
        assert!(env.iter().all(|(k, _)| !is_sensitive_env_name(k) && !is_dsh_env_name(k)));
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")), "PATH 必须保留");
    }

    #[test]
    fn explicit_env_overrides_after_scrub() {
        let env = child_env(&[("MY_TOKEN".to_string(), "kept-on-purpose".to_string())]);
        assert!(env.iter().any(|(k, v)| k == "MY_TOKEN" && v == "kept-on-purpose"));
        // 覆盖已有名字（大小写不敏感）只留一条
        let env = child_env(&[("path".to_string(), "override".to_string())]);
        let path_entries: Vec<&String> = env
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .map(|(_, v)| v)
            .collect();
        assert_eq!(path_entries, vec!["override"], "不能留下两条 PATH");
    }

    #[test]
    fn collected_buffer_keeps_a_bounded_tail_with_absolute_offsets() {
        let sink = Collected::new(8);
        sink.append(b"aaaa");
        let (bytes, end, lossy) = sink.read_from(0);
        assert_eq!((bytes.as_slice(), end, lossy), (b"aaaa".as_slice(), 4, false));
        sink.append(b"bbbbbb");
        // 尾部保留 8 字节，头部 2 字节淘汰，偏移是绝对的
        let (bytes, end, lossy) = sink.read_from(0);
        assert_eq!(bytes, b"aabbbbbb");
        assert_eq!(end, 10);
        assert!(lossy);
        // 非消费式：再读一次结果一致
        assert_eq!(sink.read_from(0).0, b"aabbbbbb");
        assert_eq!(sink.read_from(4).0, b"bbbbbb");
        assert!(sink.lossy());
    }

    #[test]
    fn resolve_executable_rejects_empty_and_unknown() {
        assert!(resolve_executable("").is_err());
        assert!(resolve_executable("definitely-not-a-real-executable-xyz").is_err());
    }
}