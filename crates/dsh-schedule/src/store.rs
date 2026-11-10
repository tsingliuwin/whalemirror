//! `~/.dsh/schedule/` 落盘（A1.4 批②④）。
//!
//! 上游 `storage-json` per-record 布局的本地等价：`schedule` 域 version 2
//! （兼容读 version 1），表 `tasks` 每条记录一份带版本戳的文档
//! `tasks/<id>.json`——`{"version": 2, "record": <任务>}`，pretty 两空格
//! 缩进 + 尾换行。per-record 契约：畸形或版本不被接受的文档是 FOREIGN，
//! 读作不存在（一份坏文件不拖垮整个域）；版本戳合法但任务值不合 schema
//! 则是数据损坏，报错不静默丢。写入走临时文件 + rename 的原子替换。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{decode_schedule_task, ScheduleLogError, ScheduleTask};

/// 当前域版本（上游 `scheduleDomain.version`）。
pub const SCHEDULE_DOMAIN_VERSION: u64 = 2;

/// 兼容读取的旧版本（上游 `compatibleVersions`）。
pub const SCHEDULE_COMPATIBLE_VERSIONS: [u64; 1] = [1];

/// 存储失败闭集。
#[derive(Debug)]
pub enum ScheduleStoreError {
    /// 文件系统失败。
    Io(io::Error),
    /// 版本戳合法但任务值不合 schema（数据损坏）。
    Corrupt {
        /// 损坏的任务 id（文件名去后缀）。
        id: String,
        /// 耐久解码诊断。
        error: ScheduleLogError,
    },
    /// 任务 id 不可作文件名（空、带首尾空白、含路径分隔符或 `.`/`..`）。
    InvalidId(String),
}

impl std::fmt::Display for ScheduleStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Corrupt { id, error } => write!(f, "schedule task {id} is corrupt: {error}"),
            Self::InvalidId(id) => write!(f, "schedule id {id:?} cannot name a stored record"),
        }
    }
}

impl std::error::Error for ScheduleStoreError {}

impl From<io::Error> for ScheduleStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// 一次全量列举的结果：合法任务 + 损坏文档清单（不让一份坏文件遮蔽
/// 其余任务）。
#[derive(Debug, Default)]
pub struct ScheduleListing {
    /// 解码成功的任务（按文件名升序）。
    pub tasks: Vec<ScheduleTask>,
    /// 版本戳合法但值损坏的文档（id + 诊断）。
    pub corrupt: Vec<(String, ScheduleLogError)>,
}

/// 一份 per-record 文档的线形（version 先行，同上游序列化序）。
#[derive(Serialize)]
struct RecordDocument<'a> {
    version: u64,
    record: &'a ScheduleTask,
}

/// 宿主级任务的耐久存储（一个进程一个实例；不跨进程加锁——宿主单例）。
#[derive(Clone, Debug)]
pub struct ScheduleStore {
    tasks_dir: PathBuf,
}

impl ScheduleStore {
    /// 以域根目录（宿主传入，通常 `~/.dsh/schedule`）创建存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { tasks_dir: root.into().join("tasks") }
    }

    /// 任务文档目录。
    pub fn tasks_dir(&self) -> &Path {
        &self.tasks_dir
    }

    /// 校验 id 可作文件名（上游 key 直接成文件名，宿主生成的 id 恒合法；
    /// 这里防御畸形输入越出域目录）。
    fn record_path(&self, id: &str) -> Result<PathBuf, ScheduleStoreError> {
        let invalid = id.is_empty()
            || id.trim() != id
            || id == "."
            || id == ".."
            || id.contains('/')
            || id.contains('\\')
            || id.contains('\0');
        if invalid {
            return Err(ScheduleStoreError::InvalidId(id.to_string()));
        }
        Ok(self.tasks_dir.join(format!("{id}.json")))
    }

    /// 解析一份 per-record 文档；FOREIGN（畸形或版本不被接受）读作 None
    /// （上游 `parseRecord`）。
    fn parse_record(text: &str) -> Option<serde_json::Value> {
        let document: serde_json::Value = serde_json::from_str(text).ok()?;
        let object = document.as_object()?;
        let stamped = object.get("version")?.as_u64()?;
        if stamped != SCHEDULE_DOMAIN_VERSION && !SCHEDULE_COMPATIBLE_VERSIONS.contains(&stamped) {
            return None;
        }
        object.get("record").cloned()
    }

    /// 读一个任务；文件不存在或 FOREIGN 为 None，值损坏报错。
    pub fn get(&self, id: &str) -> Result<Option<ScheduleTask>, ScheduleStoreError> {
        let path = self.record_path(id)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(record) = Self::parse_record(&text) else {
            return Ok(None);
        };
        decode_schedule_task(&record)
            .map(Some)
            .map_err(|error| ScheduleStoreError::Corrupt { id: id.to_string(), error })
    }

    /// 写一个任务（当前形状、version 2；原子替换）。
    pub fn put(&self, task: &ScheduleTask) -> Result<(), ScheduleStoreError> {
        let id = task.record.id();
        let path = self.record_path(id)?;
        fs::create_dir_all(&self.tasks_dir)?;
        let document = RecordDocument { version: SCHEDULE_DOMAIN_VERSION, record: task };
        let mut text = serde_json::to_string_pretty(&document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        text.push('\n');
        let temporary = self.tasks_dir.join(format!("{id}.json.tmp"));
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &path)?;
        Ok(())
    }

    /// 删除一个任务；返回是否确有文件被删。
    pub fn remove(&self, id: &str) -> Result<bool, ScheduleStoreError> {
        let path = self.record_path(id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// 列举全部任务；FOREIGN 文档跳过，值损坏文档进 `corrupt` 清单。
    pub fn list(&self) -> Result<ScheduleListing, ScheduleStoreError> {
        let mut listing = ScheduleListing::default();
        let entries = match fs::read_dir(&self.tasks_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(listing),
            Err(error) => return Err(error.into()),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json") {
                names.push(id.to_string());
            }
        }
        names.sort();
        for id in names {
            let path = self.tasks_dir.join(format!("{id}.json"));
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let Some(record) = Self::parse_record(&text) else {
                continue;
            };
            match decode_schedule_task(&record) {
                Ok(task) => listing.tasks.push(task),
                Err(error) => listing.corrupt.push((id, error)),
            }
        }
        Ok(listing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_schedule_record, CreateSpec, DeliveryHistory, ScheduleTarget, ScheduleTaskStatus,
    };
    use serde_json::json;

    /// 2026-01-10T08:00:00.000Z（周六）。
    const NOW: i64 = 1_768_032_000_000;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dsh-schedule-{tag}-{}-{nanos}", std::process::id()))
    }

    fn task(id: &str) -> ScheduleTask {
        let record =
            create_schedule_record(id, "t", "p", &json!({"kind": "after", "after_seconds": 90}), NOW)
                .unwrap();
        ScheduleTask {
            record,
            target: ScheduleTarget::Session { session_id: "session-1".into() },
            involved_session_ids: vec!["session-1".into()],
            status: ScheduleTaskStatus::Active,
            created_at: Some("2026-01-10T08:00:00.000Z".into()),
            updated_at: None,
            last_delivery: None,
            delivery_history: DeliveryHistory {
                records: Vec::new(),
                earlier_records_unavailable: false,
                earlier_records_pruned: false,
            },
            pending_delivery: None,
        }
    }

    #[test]
    fn put_get_list_remove_round_trip() {
        let root = temp_root("roundtrip");
        let store = ScheduleStore::new(&root);
        assert!(store.get("t1").unwrap().is_none());
        assert!(store.list().unwrap().tasks.is_empty());
        store.put(&task("t1")).unwrap();
        store.put(&task("t2")).unwrap();
        assert_eq!(store.get("t1").unwrap().unwrap().record.id(), "t1");
        let listing = store.list().unwrap();
        assert_eq!(listing.tasks.len(), 2);
        assert!(listing.corrupt.is_empty());
        // 文档形：version 先行、尾换行。
        let text = fs::read_to_string(store.tasks_dir().join("t1.json")).unwrap();
        assert!(text.starts_with("{\n  \"version\": 2,\n  \"record\": {"));
        assert!(text.ends_with("}\n"));
        assert!(store.remove("t1").unwrap());
        assert!(!store.remove("t1").unwrap());
        assert_eq!(store.list().unwrap().tasks.len(), 1);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn foreign_documents_read_as_absent() {
        let root = temp_root("foreign");
        let store = ScheduleStore::new(&root);
        fs::create_dir_all(store.tasks_dir()).unwrap();
        // 畸形 JSON / 版本不被接受 → FOREIGN。
        fs::write(store.tasks_dir().join("bad.json"), "not json").unwrap();
        fs::write(store.tasks_dir().join("stale.json"), "{\"version\": 9, \"record\": {}}\n").unwrap();
        assert!(store.get("bad").unwrap().is_none());
        assert!(store.get("stale").unwrap().is_none());
        // 版本戳合法但值损坏 → 进 corrupt 清单，其余任务照常列举。
        fs::write(
            store.tasks_dir().join("broken.json"),
            "{\"version\": 2, \"record\": {\"status\": \"active\"}}\n",
        )
        .unwrap();
        store.put(&task("ok")).unwrap();
        let listing = store.list().unwrap();
        assert_eq!(listing.tasks.len(), 1);
        assert_eq!(listing.corrupt.len(), 1);
        assert_eq!(listing.corrupt[0].0, "broken");
        assert!(matches!(
            store.get("broken"),
            Err(ScheduleStoreError::Corrupt { .. })
        ));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn invalid_ids_cannot_escape_the_domain() {
        let store = ScheduleStore::new(temp_root("ids"));
        for id in ["", " padded", "..", "a/b", "a\\b"] {
            assert!(matches!(store.get(id), Err(ScheduleStoreError::InvalidId(_))));
        }
        let _ = CreateSpec::default();
    }
}
