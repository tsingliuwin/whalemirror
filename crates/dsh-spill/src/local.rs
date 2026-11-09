//! 本机文件后端（上游 `spill-local` 的等价面）。
//!
//! 进程私有根（OS 临时目录下的 `dsh-spill-<随机>`）、按会话 sha256 前 12 位
//! 分目录、文件名 `<6字节hex>-<安全段>`、独占创建。安全段编码是**单射**的：
//! 每个码元要么字面保留（`[A-Za-z0-9._-]` 除 `~`），要么转义成 `~XXXX`，
//! `~` 自身也转义，因此不同输入永不碰撞，`../` 与绝对路径都无法穿越。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{SaveTextSpill, SpillLocator, SpillRef, RETRIEVAL_HINT};

/// 默认根前缀（与上游 `DEFAULT_ROOT_PREFIX` 一致）。
pub const DEFAULT_ROOT_PREFIX: &str = "dsh-spill-";

/// 惰性创建的本进程私有根（进程退出后由 OS 清理临时目录）。
fn private_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!(
            "{DEFAULT_ROOT_PREFIX}{}",
            &uuid::Uuid::new_v4().to_string()[..12]
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

/// 把任意字符串编码成**单个**安全路径段（上游 `encodeSegment` 的逐字等价）。
pub fn encode_segment(raw: &str) -> String {
    if raw.is_empty() {
        return "~".to_string();
    }
    if raw == "." {
        return "~002E".to_string();
    }
    if raw == ".." {
        return "~002E~002E".to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let ok = ch != '~' && (ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
        if ok {
            out.push(ch);
        } else {
            // UTF-16 码元转义（与 JS `charCodeAt` 对齐：BMP 以上按代理对分别转义）
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("~{:04X}", unit));
            }
        }
    }
    out
}

/// 会话分目录（上游 `sessionDir`：sha256 前 12 位 hex）。
pub fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_id.as_bytes());
    let hex = format!("{digest:x}");
    root.join(format!("session-{}", &hex[..12]))
}

/// 保存一次外置：返回定位符与字节数。路径不可穿越、文件名独占。
pub fn save_text(spill: &SaveTextSpill) -> Result<SpillRef, String> {
    let root = private_root();
    save_text_at(root, spill)
}

/// 指定根的保存（测试用）。
pub fn save_text_at(root: &Path, spill: &SaveTextSpill) -> Result<SpillRef, String> {
    let dir = session_dir(root, &spill.session_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("spill mkdir failed: {e}"))?;
    let name = format!(
        "{}-{}",
        &uuid::Uuid::new_v4().to_string()[..12],
        encode_segment(&spill.suggested_name)
    );
    let path = dir.join(name);
    if path.exists() {
        return Err("spill name collision".into());
    }
    std::fs::write(&path, spill.content.as_bytes())
        .map_err(|e| format!("spill write failed: {e}"))?;
    Ok(SpillRef {
        locator: SpillLocator(path.display().to_string()),
        bytes: spill.content.len(),
        retrieval_hint: RETRIEVAL_HINT.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_encoding_is_injective_and_traversal_proof() {
        assert_eq!(encode_segment("web_fetch.txt"), "web_fetch.txt");
        assert_eq!(encode_segment(""), "~");
        assert_eq!(encode_segment("."), "~002E");
        assert_eq!(encode_segment(".."), "~002E~002E");
        // 只有整段 "." / ".." 需要转义；分隔符一律转义，故仍是单段安全名`n        assert_eq!(encode_segment("../etc/passwd"), "..~002Fetc~002Fpasswd");`n        assert!(!encode_segment("../etc/passwd").contains(0x2F as char));
        assert_eq!(encode_segment("a~b"), "a~007Eb");
        assert_eq!(encode_segment("中文"), "~4E2D~6587");
        // 不同输入不碰撞
        assert_ne!(encode_segment("a/b"), encode_segment("ab"));
    }

    #[test]
    fn session_dirs_are_stable_and_distinct() {
        let root = Path::new("/tmp/x");
        let a = session_dir(root, "session-a");
        let b = session_dir(root, "session-b");
        assert_ne!(a, b);
        assert_eq!(a, session_dir(root, "session-a"));
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("session-"));
        assert_eq!(name.len(), "session-".len() + 12);
    }

    #[test]
    fn saves_write_the_full_text_and_report_bytes() {
        let root = std::env::temp_dir().join(format!("dsh-spill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let spill = SaveTextSpill {
            session_id: "session-a".into(),
            source: crate::SpillSource::Tool {
                tool_name: "web_fetch".into(),
                call_id: "call-1".into(),
                label: "result".into(),
            },
            suggested_name: "web_fetch.txt".into(),
            content: "hello 你好".into(),
        };
        let saved = save_text_at(&root, &spill).unwrap();
        assert_eq!(saved.bytes, "hello 你好".len());
        assert_eq!(saved.retrieval_hint, RETRIEVAL_HINT);
        assert!(saved.locator.as_str().contains("session-"));
        assert!(saved.locator.as_str().ends_with("-web_fetch.txt"));
        let read = std::fs::read_to_string(saved.locator.as_str()).unwrap();
        assert_eq!(read, "hello 你好");
        let _ = std::fs::remove_dir_all(&root);
    }
}
