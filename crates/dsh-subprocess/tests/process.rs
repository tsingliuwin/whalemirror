//! 真实子进程的端到端：收集/透传/终止/执行文件解析。
//!
//! 用系统 shell 而非 python，避免测试依赖解释器。

use dsh_subprocess::{Collected, ManagedProcess, SpawnSpec, Stdio, resolve_executable, spawn};

/// 平台 shell（Windows 用 cmd，其它用 sh）。
fn shell(argv: &str) -> SpawnSpec {
    #[cfg(windows)]
    {
        SpawnSpec::command("cmd.exe", &["/C", argv])
    }
    #[cfg(not(windows))]
    {
        SpawnSpec::command("sh", &["-c", argv])
    }
}

async fn wait_until<F: Fn() -> bool>(predicate: F) -> bool {
    for _ in 0..200 {
        if predicate() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// 一条流一个进程：`;` 在 cmd 里不是分隔符、`1>&2` 又会重定向整条 echo，
/// 所以拆成两次 spawn 才是可移植的写法。断言仍验证「两条流不串」。
fn collect_both(argv: &str) -> SpawnSpec {
    let mut spec = shell(argv);
    spec.stdout = Stdio::Collect { max_bytes: 4096, spill: None };
    spec.stderr = Stdio::Collect { max_bytes: 4096, spill: None };
    spec
}

#[tokio::test(flavor = "multi_thread")]
async fn collects_stdout_and_stderr_separately() {
    let mut out_process = spawn(&collect_both("echo out-line")).expect("spawn stdout");
    assert_eq!(out_process.wait_for_exit().await.expect("wait"), Some(0));
    let stdout = out_process.stdout().expect("stdout sink");
    let stderr = out_process.stderr().expect("stderr sink");
    assert!(
        wait_until(|| stdout.text().contains("out-line")).await,
        "stdout: {:?}",
        stdout.text()
    );
    assert!(
        stderr.text().is_empty(),
        "stdout 进程不该有 stderr: {:?}",
        stderr.text()
    );

    let mut err_process = spawn(&collect_both("echo err-line 1>&2")).expect("spawn stderr");
    assert_eq!(err_process.wait_for_exit().await.expect("wait"), Some(0));
    let out2 = err_process.stdout().expect("stdout sink");
    let err2 = err_process.stderr().expect("stderr sink");
    assert!(
        wait_until(|| err2.text().contains("err-line")).await,
        "stderr: {:?}",
        err2.text()
    );
    assert!(
        !out2.text().contains("err-line"),
        "stderr 不该串入 stdout: {:?}",
        out2.text()
    );
    assert!(
        !stdout.text().contains("err-line"),
        "两次 spawn 的缓冲区不得共享: {:?}",
        stdout.text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nonzero_exit_is_reported_not_raised() {
    let spec = shell("exit 7");
    let mut process = spawn(&spec).expect("spawn");
    let code = process.wait_for_exit().await.expect("wait");
    assert_eq!(code, Some(7), "退出码是结果数据，不是 spawn 失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn output_is_bounded_but_reports_losiness() {
    // 上限 64 字节，命令产出远超
    let mut spec = shell("echo 0123456789012345678901234567890123456789012345678901234567890123456789");
    spec.stdout = Stdio::Collect { max_bytes: 16, spill: None };
    let mut process = spawn(&spec).expect("spawn");
    process.wait_for_exit().await.expect("wait");
    let sink: &Collected = process.stdout().expect("sink");
    let _ = wait_until(|| sink.text().len() >= 16).await;
    let text = sink.text();
    assert!(text.len() <= 16, "有界尾部: {text:?}");
    assert!(sink.lossy(), "头部被淘汰要如实报告");
    // 非消费式：重复读一致
    assert_eq!(sink.text(), text);
}

#[tokio::test(flavor = "multi_thread")]
async fn spill_file_keeps_the_complete_stream() {
    let dir = std::env::temp_dir().join(format!("dsh-subprocess-spill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let spill = dir.join("stdout.log");
    let mut spec = shell("echo keep-me-whole");
    spec.stdout = Stdio::Collect { max_bytes: 4, spill: Some(spill.clone()) };
    let mut process = spawn(&spec).expect("spawn");
    process.wait_for_exit().await.expect("wait");
    let sink = process.stdout().expect("sink");
    let _ = wait_until(|| sink.lossy()).await;
    assert!(sink.lossy(), "内存只留 4 字节");
    // spill 文件有完整流
    assert!(
        wait_until(|| std::fs::read_to_string(&spill)
            .map(|t| t.contains("keep-me-whole"))
            .unwrap_or(false))
        .await,
        "spill 应保存完整输出: {:?}",
        std::fs::read_to_string(&spill)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn terminate_stops_a_long_running_process() {
    let spec = shell("sleep 30");
    let mut process: ManagedProcess = spawn(&spec).expect("spawn");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    process.terminate().await.expect("terminate");
    // 终止后 wait_for_exit 立即返回（不再挂 30s）
    let start = std::time::Instant::now();
    let _ = process.wait_for_exit().await;
    assert!(start.elapsed() < std::time::Duration::from_secs(5), "终止要真的生效");
}

#[tokio::test(flavor = "multi_thread")]
async fn child_env_is_scrubbed_unless_explicitly_forwarded() {
    // 父进程设一个凭据形状的变量：默认不该出现在子进程环境里。
    // rust 2024：进程环境写入是 unsafe（可能与其它线程并发读冲突）；
    // 本测试独占这两个变量名，其它测试不读它们。
    unsafe {
        std::env::set_var("DSH_TEST_SECRET_TOKEN", "leak-me-not");
        std::env::set_var("DSH_TEST_PLAIN", "dsh-namespace");
    }
    #[cfg(windows)]
    let spec = shell("echo [%DSH_TEST_SECRET_TOKEN%][%DSH_TEST_PLAIN%]");
    #[cfg(not(windows))]
    let spec = shell("echo \"[$DSH_TEST_SECRET_TOKEN][$DSH_TEST_PLAIN]\"");
    let mut process = spawn(&spec).expect("spawn");
    process.wait_for_exit().await.expect("wait");
    let sink = process.stdout().expect("sink");
    let _ = wait_until(|| !sink.text().trim().is_empty()).await;
    let text = sink.text();
    assert!(!text.contains("leak-me-not"), "凭据形状的名字必须被清洗: {text}");
    assert!(!text.contains("dsh-namespace"), "DSH_* 也必须被清洗: {text}");

    // 显式转发则照常到达
    let mut spec = shell("echo [%DSH_TEST_SECRET_TOKEN%]");
    #[cfg(not(windows))]
    {
        spec = shell("echo \"[$DSH_TEST_SECRET_TOKEN]\"");
    }
    spec.env = vec![("DSH_TEST_SECRET_TOKEN".to_string(), "forwarded".to_string())];
    let mut process = spawn(&spec).expect("spawn");
    process.wait_for_exit().await.expect("wait");
    let sink = process.stdout().expect("sink");
    let _ = wait_until(|| sink.text().contains("forwarded")).await;
    assert!(sink.text().contains("forwarded"), "显式 env 在清洗之后合并: {:?}", sink.text());
    unsafe {
        std::env::remove_var("DSH_TEST_SECRET_TOKEN");
        std::env::remove_var("DSH_TEST_PLAIN");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_executable_finds_the_system_shell() {
    #[cfg(windows)]
    let found = resolve_executable("cmd.exe");
    #[cfg(not(windows))]
    let found = resolve_executable("sh");
    let path = found.expect("system shell resolves");
    assert!(path.is_absolute(), "必须返回绝对路径: {}", path.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_executable_fails_loudly() {
    let spec = SpawnSpec::command("dsh-definitely-missing-binary-xyz", &[]);
    let err = match spawn(&spec) {
        Ok(_) => panic!("missing executable must fail"),
        Err(error) => error,
    };
    assert!(err.contains("not found") || err.contains("spawn failed"), "{err}");
}