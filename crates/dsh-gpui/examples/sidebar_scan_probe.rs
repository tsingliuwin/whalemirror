//! 诊断：直接跑侧栏内容扫描（与 UI 同一函数），打印命中行。
//! 用法：cargo run -p dsh-gpui --example sidebar_scan_probe -- <cwd> <query>
fn main() {
    let mut args = std::env::args().skip(1);
    let cwd = args.next().expect("cwd");
    let query = args.next().expect("query");
    let home = std::env::var("HOME").unwrap();
    let recorder = dsh_persist::SessionRecorder::new(
        std::path::PathBuf::from(format!("{home}/.dsh/sessions")),
    );
    let (hits, more) = dsh_gpui::session_query_host::sidebar_content_scan(&recorder, &cwd, &query);
    println!("hits={} more={}", hits.len(), more);
    for h in hits {
        println!(
            "- {} | {:?} | snippet: {}",
            h.session_id,
            h.title,
            h.snippet.chars().take(80).collect::<String>()
        );
    }
}
