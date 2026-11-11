//! 诊断：run() 的 Pipe 收集。
fn main() {
    let spec = dsh_subprocess::SpawnSpec {
        argv: vec!["git".into(), "--version".into()],
        cwd: None,
        stdin: dsh_subprocess::Stdio::Ignore,
        stdout: dsh_subprocess::Stdio::Pipe,
        stderr: dsh_subprocess::Stdio::Pipe,
        env: vec![],
        grace_ms: 3000,
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let out = rt.block_on(dsh_subprocess::run(&spec));
    match out {
        Ok(c) => println!(
            "exit={:?} stdout_len={} stdout={:?} stderr={:?}",
            c.exit_code,
            c.stdout.len(),
            String::from_utf8_lossy(&c.stdout),
            String::from_utf8_lossy(&c.stderr)
        ),
        Err(e) => println!("ERR {e}"),
    }
}
