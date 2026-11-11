//! 诊断：按 (id, cwd) 直载一个会话，打印含指定子串的行摘要。
//! 用法：cargo run -p dsh-persist --example session_probe -- <id> <cwd> <substr>
use dsh_persist::SessionRecorder;
fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("id");
    let cwd = args.next().expect("cwd");
    let substr = args.next().unwrap_or_default();
    let home = std::env::var("HOME").unwrap();
    let rec = SessionRecorder::new(format!("{home}/.dsh/sessions").into());
    let (session, _) = rec
        .load(&dsh_llm::SessionId(id), Some(cwd.as_str()))
        .expect("load");
    for e in session.entries() {
        let text = format!("{:?}", e.event);
        if text.contains(&substr) {
            let head: String = text.chars().take(240).collect();
            println!("[seq {}] {}", e.seq, head);
        }
    }
}
