//! 诊断/演示：用 crate API 构造一个演示任务并写进真实存储
//! （与 schedule 工具同一条构造+落盘路径）。
//! 用法：cargo run -p dsh-schedule --example make_demo_task -- <cwd>
use dsh_schedule::{create_daily_schedule_record, decode_schedule_task, ScheduleTarget};
use serde_json::json;

fn main() {
    let cwd = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let now = dsh_schedule::format_instant(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    );
    let record = create_daily_schedule_record(
        "schedule-a6-demo-1",
        "这是 A6 自动化任务页验证用的演示任务：报一句当前时间即可。",
        &json!({ "time": "09:30:00", "time_zone": "Asia/Shanghai" }),
        dsh_schedule::instant_epoch_ms(&now).unwrap_or(0) as i64,
        "A6 页面验证任务",
    )
    .expect("daily record");
    let task = decode_schedule_task(&json!({
        "record": serde_json::to_value(&dsh_schedule::ScheduleRecord::Daily(record.clone())).unwrap(),
        "target": { "kind": "new_once", "create": { "cwd": cwd, "agentPreset": "standard" } },
        "involvedSessionIds": [],
        "status": "active",
        "createdAt": now,
        "updatedAt": now,
        "deliveryHistory": {
            "records": [],
            "earlierRecordsUnavailable": false,
            "earlierRecordsPruned": false
        },
    }))
    .expect("task");
    let home = std::env::var("HOME").unwrap();
    let dir = std::path::PathBuf::from(format!("{home}/.dsh/schedule/tasks"));
    std::fs::create_dir_all(&dir).unwrap();
    let store = dsh_schedule::ScheduleStore::new(&dir);
    store.put(&task).expect("put");
    println!(
        "written: {} (scheduled_at={})",
        task.record.id(),
        task.record.scheduled_at()
    );
}
