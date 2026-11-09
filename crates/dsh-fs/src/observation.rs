//! 文件观察策略（上游 `fs/fs-observation-policy` 的等价面）。
//!
//! 守护「改之前必须先读过」：
//! - `edit` 要求目标有**先前读到的 present 观察**——没读过报
//!   `cannot modify "<path>": file has not been read — read the file, then retry`；
//!   观察为 absent（读过但当时不存在）报 `cannot edit …: not found`；
//! - `write` 允许**新建**，但拒绝覆盖未读过的既有文件（同一句文案）；
//! - 读之后文件又被外部改动 → 版本不符，报「file has changed since it was read」。
//!
//! 上游部署把这一行 `disabled: true`（发货组合默认关闭），rustdsh 保持同样的
//! 默认：能力在、开关默认关，打开方式见 [`ObservationPolicy::from_env`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 一次观察：文件当时存在（带版本）或确认不存在。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Present { mtime_ns: u128, size: u64 },
    Absent,
}

/// 版本形（写/编辑前的 CAS 依据）。
type Version = (u128, u64);

fn version_of(path: &Path) -> Option<Version> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((mtime_ns, meta.len()))
}

/// 观察记录：按「属主（会话）+ 目标路径」记状态。
///
/// 属主用共享槽（宿主在切会话时写），工具调用时读——与 jobs 的 OwnerCell
/// 同一思路，但这里只需要一个字符串键。
#[derive(Clone, Default)]
pub struct ObservationStore {
    owner: Arc<Mutex<Option<String>>>,
    observed: Arc<Mutex<HashMap<(String, PathBuf), Observation>>>,
}

impl ObservationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置属主（切会话时调用）。
    pub fn set_owner(&self, owner: Option<String>) {
        *self.owner.lock().unwrap() = owner;
    }

    fn owner_key(&self) -> Option<String> {
        self.owner.lock().unwrap().clone()
    }

    /// 记录一次 present/absent 观察。
    pub fn observe(&self, path: &Path, observation: Observation) {
        let Some(owner) = self.owner_key() else { return };
        self.observed
            .lock()
            .unwrap()
            .insert((owner, path.to_path_buf()), observation);
    }

    /// 读取观察。
    pub fn get(&self, path: &Path) -> Option<Observation> {
        let owner = self.owner_key()?;
        self.observed.lock().unwrap().get(&(owner, path.to_path_buf())).cloned()
    }
}

/// 策略本身：默认关闭（对齐上游发货组合），可用 `DSH_FS_OBSERVATION_POLICY=1`
/// 打开。
#[derive(Clone)]
pub struct ObservationPolicy {
    store: ObservationStore,
    enabled: bool,
}

impl ObservationPolicy {
    pub fn new(store: ObservationStore) -> Self {
        Self { store, enabled: false }
    }

    /// 按环境变量构造：`DSH_FS_OBSERVATION_POLICY` 为 `1`/`true` 时启用。
    pub fn from_env(store: ObservationStore) -> Self {
        let enabled = std::env::var("DSH_FS_OBSERVATION_POLICY")
            .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "True"))
            .unwrap_or(false);
        Self { store, enabled }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 设置属主会话（切会话时由宿主调用）——观察按会话隔离。
    pub fn set_owner(&self, owner: Option<String>) {
        self.store.set_owner(owner);
    }

    /// `read` 之后的观察登记：成功读到 → present（带版本）；不存在 → absent。
    pub fn after_read(&self, path: &Path, read_ok: bool) {
        if !self.enabled {
            return;
        }
        match version_of(path) {
            Some((mtime_ns, size)) if read_ok => {
                self.store.observe(path, Observation::Present { mtime_ns, size })
            }
            _ => self.store.observe(path, Observation::Absent),
        }
    }

    /// `write` 的准入门：新建允许，覆盖要求先前读过且版本未变。
    pub fn check_write(&self, path: &Path) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let display = path.display().to_string();
        let before = self.store.get(path);
        let current = version_of(path);
        match (before, current) {
            // 没读过、但目标也不存在：允许新建
            (None, None) => Ok(()),
            // 没读过却要覆盖既有文件
            (None, Some(_)) => Err(unread(&display)),
            // 读过且当时不存在：允许创建，但被并发创建者抢先就要拦
            (Some(Observation::Absent), None) => Ok(()),
            (Some(Observation::Absent), Some(_)) => Err(unread(&display)),
            // 读过（present）：版本必须未变
            (Some(Observation::Present { mtime_ns, size }), Some(now)) => {
                if now == (mtime_ns, size) {
                    Ok(())
                } else {
                    Err(format!(
                        "cannot modify \"{display}\": file has changed since it was read — read the file again, then retry"
                    ))
                }
            }
            // 读过（present）但文件被删了
            (Some(Observation::Present { .. }), None) => Err(format!(
                "cannot modify \"{display}\": file has changed since it was read — read the file again, then retry"
            )),
        }
    }

    /// `edit` 的准入门：必须有 present 观察且版本未变。
    pub fn check_edit(&self, path: &Path) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let display = path.display().to_string();
        match (self.store.get(path), version_of(path)) {
            (None, _) => Err(unread(&display)),
            (Some(Observation::Absent), _) => {
                Err(format!("cannot edit \"{display}\": not found"))
            }
            (Some(Observation::Present { mtime_ns, size }), Some(now)) if now == (mtime_ns, size) => Ok(()),
            (Some(Observation::Present { .. }), _) => Err(format!(
                "cannot edit \"{display}\": file has changed since it was read — read the file again, then retry"
            )),
        }
    }
}

/// 未读的统一文案（上游 `cannot modify "<path>": file has not been read — read the file, then retry`）。
fn unread(display: &str) -> String {
    format!("cannot modify \"{display}\": file has not been read — read the file, then retry")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(owner: &str) -> (ObservationPolicy, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dsh-fs-obs-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ObservationStore::new();
        store.set_owner(Some(owner.to_string()));
        let mut policy = ObservationPolicy::new(store);
        policy.enabled = true;
        (policy, dir)
    }

    fn uuid_like() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    #[test]
    fn disabled_policy_allows_everything() {
        let store = ObservationStore::new();
        store.set_owner(Some("s1".into()));
        let policy = ObservationPolicy::new(store); // enabled = false
        let dir = std::env::temp_dir().join("dsh-fs-obs-off");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(policy.check_write(&file).is_ok());
        assert!(policy.check_edit(&file).is_ok());
    }

    #[test]
    fn unread_overwrite_and_edit_are_rejected_with_upstream_text() {
        let (policy, dir) = policy("s1");
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let err = policy.check_write(&file).unwrap_err();
        assert_eq!(
            err,
            format!(
                "cannot modify \"{}\": file has not been read — read the file, then retry",
                file.display()
            )
        );
        let edit_err = policy.check_edit(&file).unwrap_err();
        assert_eq!(edit_err, err, "edit 与 write 的未读文案相同");

        // 读完即放行
        policy.after_read(&file, true);
        assert!(policy.check_write(&file).is_ok());
        assert!(policy.check_edit(&file).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creating_new_files_needs_no_prior_read() {
        let (policy, dir) = policy("s1");
        let fresh = dir.join("new.txt");
        assert!(policy.check_write(&fresh).is_ok(), "新建允许");
        // 读过但当时不存在 → 也允许创建
        policy.after_read(&fresh, false);
        assert!(policy.check_write(&fresh).is_ok());
        // 被别人抢先创建 → 拦下
        std::fs::write(&fresh, "raced").unwrap();
        assert!(policy.check_write(&fresh).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_after_read_but_externally_changed_is_stale() {
        let (policy, dir) = policy("s1");
        let file = dir.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        policy.after_read(&file, true);
        assert!(policy.check_edit(&file).is_ok());
        // 外部改动：内容与大小都变
        std::fs::write(&file, "v2-longer").unwrap();
        let err = policy.check_edit(&file).unwrap_err();
        assert!(err.contains("file has changed since it was read"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_a_file_observed_absent_reports_not_found() {
        let (policy, dir) = policy("s1");
        let file = dir.join("gone.txt");
        policy.after_read(&file, false); // 读失败 = 确认不存在
        let err = policy.check_edit(&file).unwrap_err();
        assert_eq!(err, format!("cannot edit \"{}\": not found", file.display()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn observations_are_per_owner() {
        let store = ObservationStore::new();
        store.set_owner(Some("s1".into()));
        let policy = ObservationPolicy { store: store.clone(), enabled: true };
        let dir = std::env::temp_dir().join("dsh-fs-obs-owner");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        policy.after_read(&file, true);
        assert!(policy.check_edit(&file).is_ok());
        // 换会话：观察不继承，必须重读
        store.set_owner(Some("s2".into()));
        assert!(policy.check_edit(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
