//! dsh-shell — a shell-command tool.
//!
//! One `shell` tool that runs a command line (through `cmd /C` on Windows,
//! `sh -c` elsewhere) and returns combined stdout/stderr, with a foreground
//! timeout（对齐 `packages/shell/bash-local`：默认 120s，每调用 `timeout_ms`
//! 覆盖且上限 `max_timeout_ms`，超时杀进程并报错）。进程等待在 tokio 运行时
//! 内完成；`kill_on_drop` 保证超时/取消后不留孤儿进程。
//!
//! PTY 会话：参考实现同样推迟（bash-local 的 XXX 注记——持久 cwd 与 PTY
//! 会话留待工作流需要时再做），这里保持一致。
//!
//! 子进程一律经 [`dsh_subprocess`] 的统一缝拉起（环境清洗、可执行文件显式
//! 解析、输出有界收集、终止语义），本 crate 只保留**产品策略**：Windows 上
//! 优先 Git Bash，以及把退出状态渲染成 seam 约定的标记行。

use std::time::Duration;

use async_trait::async_trait;
use dsh_subprocess::{ManagedProcess, SpawnSpec, Stdio as SpawnStdio, spawn, spawn_piped};
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

/// 默认前台超时（web bash-local 默认）。
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// 每调用覆盖的上限。
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// 前台两条流各自的有界上限：保留尾部这么多字节，超出部分淘汰并如实告知。
///
/// 上游 subprocess seam 同样有界；此前用 `read_to_end` 无界收集，`yes` 之类
/// 命令能直接把内存吃满。
pub const FOREGROUND_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

pub struct ShellTool {
    default_timeout_ms: u64,
    max_timeout_ms: u64,
    /// 会话工作目录（web session.header.cwd；None = 进程 cwd）
    workdir: dsh_tools::Workdir,
    /// 后台作业面（上游 tool-bash 的 `run_in_background`）：有注册表才声明该参数
    jobs: Option<dsh_jobs::JobCaller>,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
            max_timeout_ms: MAX_TIMEOUT_MS,
            workdir: dsh_tools::Workdir::new(),
            jobs: None,
        }
    }
}

impl ShellTool {
    pub fn new(default_timeout_ms: u64, max_timeout_ms: u64) -> Self {
        Self {
            default_timeout_ms: default_timeout_ms.max(1),
            max_timeout_ms: max_timeout_ms.max(default_timeout_ms.max(1)),
            workdir: dsh_tools::Workdir::new(),
            jobs: None,
        }
    }

    /// 注入会话工作目录（命令在此目录下执行）。
    pub fn with_workdir(mut self, workdir: dsh_tools::Workdir) -> Self {
        self.workdir = workdir;
        self
    }

    /// 注入后台作业面（`run_in_background` 与 `job_*` 工具共用同一注册表）。
    pub fn with_jobs(mut self, caller: dsh_jobs::JobCaller) -> Self {
        self.jobs = Some(caller);
        self
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        let shell_line = match shell_kind() {
            // bash：模型惯用语法（管道/;/引号/$()）原样生效
            "bash" => "The command runs through bash (Git Bash on Windows), so bash syntax — \
                       pipes, `;`, `$()`, quotes — works naturally. The working directory is \
                       already the session workspace root: do not `cd` first.",
            // cmd：bash 语法会碎裂（; 不分段、引号剥离、% 变量展开）
            _ => "The command runs through `cmd /C` (NOT bash): use `&&` instead of `;`, avoid \
                   `$()` and `%` characters, and expect quotes around arguments with spaces to \
                   be stripped. The working directory is already the session workspace root: do \
                   not `cd` first, and use Windows-style paths.",
        };
        ToolDefinition {
            // 上游 tool-bash name 'bash'：模型按上游习惯直呼名（同 read/write
            // 名形对齐）；旧会话日志 name="shell" 的调用回放归类双名兼容
            name: "bash".into(),
            description: format!("Execute a bash command and return its combined stdout and stderr. {shell_line}"),
            parameters: if self.jobs.is_some() {
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command line to run." },
                        "timeout_ms": { "type": "integer", "description": format!("Foreground timeout in milliseconds (default {}, max {}).", self.default_timeout_ms, self.max_timeout_ms) },
                        "run_in_background": { "type": "boolean", "description": "Run in the background and return a job id immediately (collect with job_output, stop with job_kill). No timeout applies." }
                    },
                    "required": ["command"]
                })
            } else {
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command line to run." },
                        "timeout_ms": { "type": "integer", "description": format!("Foreground timeout in milliseconds (default {}, max {}).", self.default_timeout_ms, self.max_timeout_ms) }
                    },
                    "required": ["command"]
                })
            },
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let command = input.arguments.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if command.trim().is_empty() {
            return ToolExecutionResult::error("command must be a non-empty string");
        }
        // 后台分支（上游 tool-bash run_in_background）：注册成作业、立刻回 id
        if input.arguments.get("run_in_background").and_then(|v| v.as_bool()) == Some(true) {
            let Some(caller) = self.jobs.clone() else {
                return ToolExecutionResult::error(
                    "background jobs unavailable: load dsh-jobs and its job tools",
                );
            };
            return match start_background_job(&caller, &command, &self.workdir) {
                Ok(id) => {
                    let cwd = self
                        .workdir
                        .get()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ToolExecutionResult {
                        content: vec![dsh_llm::ContentBlock::Text {
                            // 上游渲染：`started background job <id>`
                            text: format!("started background job {id}"),
                        }],
                        is_error: false,
                        concludes_turn: false,
                        presentation: Some(json!({
                            "card": "generic",
                            "title": command,
                            "kind": "execute",
                            "cwd": cwd,
                            "jobId": id.0,
                        })),
                    }
                }
                Err(error) => ToolExecutionResult::error(format!("spawn failed: {error}")),
            };
        }
        let timeout_ms = input
            .arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_ms)
            .clamp(1, self.max_timeout_ms);
        run(&command, timeout_ms, &self.workdir).await
    }
}

/// 后台命令的注册表接线：拉起子进程、把 stdout/stderr 逐块泵进输出环、
/// 退出时按上游 `processOutcome` 落终态（`exit code: N` / `signal: X`）。
fn start_background_job(
    caller: &dsh_jobs::JobCaller,
    command: &str,
    workdir: &dsh_tools::Workdir,
) -> Result<dsh_jobs::JobId, String> {
    use std::sync::{Arc, Mutex};

    let cwd = workdir.get().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let label = command.to_string();
    let owner = caller.owner();
    let command = command.to_string();
    let spawned: Arc<Mutex<Option<dsh_jobs::JobId>>> = Arc::new(Mutex::new(None));
    let spawned_out = Arc::clone(&spawned);
    let jobs = caller.jobs().clone();

    let id = jobs
        .start(dsh_jobs::JobSpec {
            kind: "bash".into(),
            label,
            owner,
            tool_call_id: None,
            output_limit_bytes: None,
            run: Box::new(move |writer: dsh_jobs::JobWriter| {
                let mut spec = shell_spec(&command);
                spec.cwd = Some(cwd.clone());
                spec.stdin = SpawnStdio::Ignore;
                // Pipe 而非 Collect：后台作业要把输出**增量**泵进 JobWriter
                // （拉取式流可见性），批量收集会让 job_output 长时间空白。
                let (managed, stdout, stderr, _stdin) = spawn_piped(&spec)?;
                let managed: Arc<Mutex<Option<ManagedProcess>>> =
                    Arc::new(Mutex::new(Some(managed)));
                let cancel_managed = Arc::clone(&managed);
                let cancel: dsh_jobs::CancelHook = Arc::new(move |_reason: Option<String>| {
                    // 同步钩子只投递终止信号；回收在泵任务的 wait 侧完成
                    if let Ok(mut slot) = cancel_managed.lock() {
                        if let Some(process) = slot.as_mut() {
                            let _ = process.start_kill();
                        }
                    }
                });
                // 泵 + 等退出都在后台任务里跑（registry 已先提交记录）
                tokio::spawn(async move {
                    /// 把一条流逐块泵进输出环（后台作业的流式可见性全靠它）。
                    async fn pump_into<R>(
                        stream: Option<R>,
                        writer: &dsh_jobs::JobWriter,
                        channel: dsh_jobs::JobChannel,
                    ) where
                        R: tokio::io::AsyncRead + Unpin,
                    {
                        use tokio::io::AsyncReadExt;
                        let Some(mut stream) = stream else { return };
                        let mut piece = vec![0u8; 4096];
                        loop {
                            match stream.read(&mut piece).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => writer.append(&decode_output(&piece[..n]), Some(channel)),
                            }
                        }
                    }
                    let run_out = pump_into(stdout, &writer, dsh_jobs::JobChannel::Stdout);
                    let run_err = pump_into(stderr, &writer, dsh_jobs::JobChannel::Stderr);
                    tokio::join!(run_out, run_err);
                    // 先把受管进程从共享槽里取出来（不在 await 点持锁）
                    let taken = { managed.lock().unwrap().take() };
                    let (code, signal) = match taken {
                        Some(mut process) => {
                            let code = process.wait_for_exit().await.ok().flatten();
                            (code, process.signal.clone())
                        }
                        None => (None, None),
                    };
                    // 上游 killed 语义：信号终止 / 拿不到退出状态都算 killed
                    let outcome = match (code, signal) {
                        (_, Some(sig)) => dsh_jobs::JobOutcome {
                            status: dsh_jobs::JobStatus::Killed,
                            detail: Some(format!("signal: {sig}")),
                            result: None,
                        },
                        (Some(code), None) => {
                            dsh_jobs::JobOutcome::completed_with(format!("exit code: {code}"))
                        }
                        (None, None) => dsh_jobs::JobOutcome {
                            status: dsh_jobs::JobStatus::Killed,
                            detail: Some("killed before exit".into()),
                            result: None,
                        },
                    };
                    writer.settle(outcome);
                });
                let _ = &spawned_out;
                Ok(cancel)
            }),
        })
        .map_err(|e| e)?;
    *spawned.lock().unwrap() = Some(id.clone());
    Ok(id)
}

/// 构造一条命令的 spawn 请求（前台 `run` 与后台作业共用同一份 shell 选择）。
///
/// Windows 上优先 Git Bash，找不到才退 `cmd /C`；Unix 一律 `sh -c`。
fn shell_spec(command: &str) -> SpawnSpec {
    #[cfg(windows)]
    let (program, flag) = match shell_program() {
        Some(bash) => (bash.to_string_lossy().into_owned(), "-c"),
        None => ("cmd".to_string(), "/C"),
    };
    #[cfg(not(windows))]
    let (program, flag) = ("sh".to_string(), "-c");
    SpawnSpec::command(program, &[flag, command])
}

/// Windows 上优先 Git Bash：模型惯用 bash 语法（`;`、`$()`、引号、
/// `--format="%ci"`），经 `cmd /C` 全部碎裂（cmd 剥引号、`%` 变量展开、
/// `;` 不分段——真实会话 8 个错误 6 条重试链皆源于此）。对齐参考实现
/// bash-local 的 bash 语义；找不到 bash 再退 `cmd /C`。
#[cfg(windows)]
fn shell_program() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static BASH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    BASH.get_or_init(|| {
        // PATH 上的 bash（Git Bash 终端环境）。必须返回解析后的完整路径：
        // CreateProcess 对裸名先搜 System32，而 System32\bash.exe 是 WSL
        // 启动器——曾因此把 shell 工具劫持进 WSL（无 cargo/rustup、/c
        // 路径不存在、每条命令带 wsl.exe UTF-16 代理警告噪声）
        if let Ok(path_var) = std::env::var("PATH") {
            let found = std::env::split_paths(&path_var)
                .map(|dir| dir.join("bash.exe"))
                .find(|p| p.is_file());
            if let Some(bash) = found {
                // 相对 PATH 项（如 "."）同样会让 CreateProcess 重新走搜索
                // 序——规范化成绝对路径堵死回退
                return Some(bash.canonicalize().unwrap_or(bash));
            }
        }
        // GUI 进程的 PATH 常只有 Git\cmd；按安装位次找包装器 bash.exe
        // （Git\bin\bash.exe 会先把 usr/bin 前置进 PATH，grep/sed 可用）
        let mut candidates = Vec::new();
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                candidates.push(std::path::PathBuf::from(base).join("Git").join("bin").join("bash.exe"));
            }
        }
        candidates.into_iter().find(|p| p.is_file())
    })
    .clone()
}

#[cfg(not(windows))]
fn shell_program() -> Option<std::path::PathBuf> {
    None
}

/// 本进程实际的 shell 风味（"bash" | "cmd"）：供提示词 section 与测试对齐。
pub fn shell_kind() -> &'static str {
    #[cfg(windows)]
    {
        if shell_program().is_some() {
            "bash"
        } else {
            "cmd"
        }
    }
    #[cfg(not(windows))]
    {
        "bash"
    }
}

async fn run(command: &str, timeout_ms: u64, workdir: &dsh_tools::Workdir) -> ToolExecutionResult {
    let mut spec = shell_spec(command);
    spec.cwd = Some(workdir.get().unwrap_or_else(|| std::env::current_dir().unwrap_or_default()));
    spec.stdout = SpawnStdio::Collect { max_bytes: FOREGROUND_OUTPUT_LIMIT, spill: None };
    spec.stderr = SpawnStdio::Collect { max_bytes: FOREGROUND_OUTPUT_LIMIT, spill: None };
    let mut process = match spawn(&spec) {
        Ok(process) => process,
        Err(error) => return ToolExecutionResult::error(error),
    };
    // 整体受前台超时约束：超时杀进程（kill_on_drop 兜底孤儿）
    let waited = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        process.wait_for_exit(),
    )
    .await;
    match waited {
        Ok(Ok(exit_code)) => {
            let out = process.stdout().map(|s| s.bytes()).unwrap_or_default();
            let err = process.stderr().map(|s| s.bytes()).unwrap_or_default();
            let lossy = process.stdout().is_some_and(|s| s.lossy())
                || process.stderr().is_some_and(|s| s.lossy());
            let mut text = decode_output(&out);
            if !err.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&decode_output(&err));
            }
            if lossy {
                // 有界收集必须自曝：模型看不出「尾部」就无从判断自己少读了什么
                text.push_str(&format!(
                    "\n[output truncated: 仅保留尾部 {FOREGROUND_OUTPUT_LIMIT} 字节]"
                ));
            }
            append_exit_marker(&mut text, exit_code, process.signal.as_deref());
            // 上游 seam 语义：命令失败也是 completed call（isError=false——
            // 退出状态是结果数据，模型读标记自诊断）；调用级失败（spawn
            // 失败/超时）才走 error，且不带标记（超时等事实留在正文）。
            ToolExecutionResult::text(text)
        }
        Ok(Err(error)) => ToolExecutionResult::error(error),
        Err(_) => {
            process.kill().await;
            ToolExecutionResult::error(format!("command timed out after {timeout_ms}ms"))
        }
    }
}

/// 退出标记（上游 shell seam 共享约定）：渲染结果以机器可读一行收尾——
/// `[exit code: N]`、无退出码时 `[exit code: null]`、信号终止时
/// `[killed by signal: X]`——模型因此总能知道命令如何结束；呈现侧
/// parseExitStatus 剥除标记还原结构化状态（终端卡把退出状态画成自己的
/// pill，标记留在输出里就会双重渲染）。
fn append_exit_marker(text: &mut String, exit_code: Option<i32>, signal: Option<&str>) {
    if let Some(signal) = signal {
        text.push_str(&format!("\n[killed by signal: {signal}]"));
        return;
    }
    match exit_code {
        Some(code) => text.push_str(&format!("\n[exit code: {code}]")),
        None => text.push_str("\n[exit code: null]"),
    }
}

/// 输出解码：UTF-8 优先（rust 工具链/现代程序的原生编码）；字节不是合法
/// UTF-8 时按系统代码页兜底——Windows 上 `cmd /C` 的输出（含其错误信息）
/// 用 OEM/控制台代码页编码（中文系统为 GBK/cp936），直接 lossy 解会成乱码，
/// 模型读不懂就无法自诊断。
fn decode_output(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => text,
        Err(_) => decode_system_codepage(bytes),
    }
}

/// 当前 Windows 代码页：有控制台用控制台输出 CP，否则回退 OEM CP。
#[cfg(windows)]
fn system_codepage() -> u16 {
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn GetOEMCP() -> u32;
    }
    let cp = unsafe { GetConsoleOutputCP() };
    let cp = if cp == 0 { unsafe { GetOEMCP() } } else { cp };
    u16::try_from(cp).unwrap_or(0)
}

#[cfg(windows)]
fn decode_system_codepage(bytes: &[u8]) -> String {
    let enc = codepage::to_encoding(system_codepage()).unwrap_or(encoding_rs::UTF_8);
    enc.decode(bytes).0.into_owned()
}

#[cfg(not(windows))]
fn decode_system_codepage(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::types::CallId;

    fn input(raw: &str) -> ToolExecutionInput {
        ToolExecutionInput::with_raw_arguments(CallId("t".into()), "bash".into(), raw.into())
    }

    #[test]
    fn tool_name_is_upstream_bash() {
        // 上游名形对齐锁：tool-bash name 'bash'（曾为 'shell'——模型直呼
        // bash 会吃 no tool）
        assert_eq!(ShellTool::default().definition().name, "bash");
    }

    #[tokio::test]
    async fn runs_and_returns_output() {
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "echo hello"}"#)).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("hello"), "{text}");
        // 退出标记（seam 约定）：成功也带码 0
        assert!(text.ends_with("\n[exit code: 0]"), "{text}");
    }

    #[tokio::test]
    async fn failing_exit_stays_completed_with_marker() {
        // 上游语义：非零退出是 completed call（isError=false，退出状态是
        // 结果数据），标记 `[exit code: N]` 收尾供呈现侧剥出胶囊
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "exit 3"}"#)).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(!is_error, "{result:?}");
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.ends_with("\n[exit code: 3]"), "{text}");
    }

    #[tokio::test]
    async fn times_out_and_kills() {
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "sleep 5", "timeout_ms": 100}"#)).await;
        let ToolExecutionResult { content, is_error, .. } = &result;
        assert!(is_error);
        let text = content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("timed out after 100ms"), "{text}");
    }

    #[tokio::test]
    async fn runs_in_injected_workdir() {
        let dir = std::env::temp_dir().join(format!("dsh-shell-wd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = ShellTool::default().with_workdir(dsh_tools::Workdir::with_value(dir.clone()));
        // 命令按风味选：bash → pwd；cmd /C → 裸 `cd` 打印当前目录
        let raw = if shell_kind() == "bash" {
            r#"{"command": "pwd"}"#
        } else {
            r#"{"command": "cd"}"#
        };
        let result = tool.execute(&input(raw)).await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains(&format!("dsh-shell-wd-{}", std::process::id())), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn clamps_timeout_override_to_max() {
        let tool = ShellTool::new(1000, 200);
        // 覆盖 5000 被 clamp 到 200：sleep 5 应在 200ms 预算内超时
        let result = tool.execute(&input(r#"{"command": "sleep 5", "timeout_ms": 5000}"#)).await;
        assert!(result.is_error);
    }

    #[test]
    fn utf8_output_passes_through() {
        assert_eq!(decode_output("héllo ✔".as_bytes()), "héllo ✔");
    }

    #[tokio::test]
    async fn bash_semantics_work_when_bash_is_flavor() {
        // 真实会话的 8 个错误全是 bash 语法经 cmd /C 碎裂（; 不分段、引号
        // 剥离、% 变量展开）；bash 风味下必须原样生效。
        if shell_kind() != "bash" {
            return;
        }
        let tool = ShellTool::default();
        // `;` 分隔 + 引号内空格 + % 字符，三件套同时验证
        let result = tool
            .execute(&input(
                r#"{"command": "echo one; printf '%s %s\\n' 'a %b' 'c'; echo three"}"#,
            ))
            .await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("one") && text.contains("a %b c") && text.contains("three"), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn shell_kind_matches_available_program() {
        // 味道判定与实际 spawn 程序一致：bash 存在则为 bash，否则 cmd
        let has_bash = shell_program().is_some();
        assert_eq!(shell_kind() == "bash", has_bash);
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_output_decodes_via_system_codepage() {
        // 「系统找不到指定的路径」的 GBK（cp936）字节；仅在 OEM CP 为 936
        // 的系统上断言内容，其他代码页只要求不 panic（回退 UTF-8/其他编码）。
        let gbk = [0xcf, 0xb5, 0xcd, 0xb3, 0xd5, 0xd2, 0xb2, 0xbb, 0xb5, 0xbd, 0xd6, 0xb8, 0xb6, 0xa8, 0xb5, 0xc4, 0xc2, 0xb7, 0xbe, 0xb6];
        let decoded = decode_output(&gbk);
        if system_codepage() == 936 {
            assert_eq!(decoded, "系统找不到指定的路径");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_non_utf8_stderr_is_readable() {
        // 端到端：cmd 的中文 echo 经管道回来，字节编码随父链 console 输出代码页
        // 漂移（936→GBK 字节、65001→UTF-8 字节、偶发尾字节截断），子进程编码
        // 不可控——端到端只断言执行可达且解码非空；「GBK 字节经系统代码页解码
        // 可读」的强语义由 non_utf8_output_decodes_via_system_codepage
        // 纯字节单测固定覆盖。
        let tool = ShellTool::default();
        let result = tool.execute(&input(r#"{"command": "echo 系统找不到指定的路径"}"#)).await;
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(!text.trim().is_empty(), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn shell_program_is_never_a_bare_name() {
        // 裸名 "bash" 会让 CreateProcess 先搜 System32，命中 WSL 启动器
        // （曾把 shell 工具劫持进 WSL：无 cargo、/c 路径不存在、每条命令带
        // wsl.exe UTF-16 代理警告）。锁死不变量：解析结果必须是绝对路径。
        if let Some(bash) = shell_program() {
            assert!(
                bash.is_absolute() && bash.file_name().is_some_and(|f| f == "bash.exe"),
                "resolved bash must be an absolute path, got {bash:?}"
            );
        }
    }
}
