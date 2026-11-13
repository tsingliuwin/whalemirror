//! 诊断：计时 `SessionRecorder::load` 的完整链路（多帧 zstd 解压 → 逐行
//! JSON 解析 → SessionEvent 构建 → Session fold），定位「切会话卡顿」的
//! 持久层成本占比。
//! 用法：cargo run -p dsh-persist --example load_bench -- <id> <cwd>
use dsh_persist::SessionRecorder;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("id");
    let cwd = args.next().expect("cwd");
    let home = std::env::var("HOME").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap());
    let rec = SessionRecorder::new(format!("{home}/.dsh/sessions").into());

    // 冷藏一次（把文件页从系统缓存里挤出再读，模拟冷启动；失败忽略）
    let _ = std::fs::read_dir(format!("{home}/.dsh/sessions"));

    let t0 = Instant::now();
    let (session, _) = rec
        .load(&dsh_llm::SessionId(id.clone()), Some(cwd.as_str()))
        .expect("load");
    let t1 = Instant::now();
    let entries = session.entries();
    let text_bytes: usize = entries
        .iter()
        .map(|e| format!("{:?}", e.event).len())
        .sum();
    println!(
        "load={:.2?}  events={}  debug体量={:.1}MB",
        t1 - t0,
        entries.len(),
        text_bytes as f64 / 1e6
    );
    // 二次 load：热页缓存下的重复切换成本（UI 每次切换都重新 load）
    let t2 = Instant::now();
    let (session2, _) = rec.load(&dsh_llm::SessionId(id), Some(cwd.as_str())).expect("reload");
    let t3 = Instant::now();
    println!("reload(热)={:.2?}  events={}", t3 - t2, session2.entries().len());
}
