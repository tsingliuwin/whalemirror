//! dsh-fs — a local-filesystem tool.
//!
//! One `fs` tool exposing `read` / `write` / `list` / `exists` operations over
//! `std::fs`, behind an injected [`FsPolicy`]（web `fs-sandbox` containment +
//! `fs-observation-policy` 的合并缝）：
//!
//! - [`AllowAllPolicy`]：直通（默认，无沙箱）。
//! - [`WorkspaceContainment`]：写限定在给定工作区根之下（含 `~` 展开与
//!   规范化——目标不存在时回退到最近存在祖先再判包含，web containment
//!   的同一保守语义）；读/列/存在检查不受限。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use dsh_llm::ContentBlock;
use serde_json::json;

/// 文件系统策略 provider：读可见性 + 写包含。
pub trait FsPolicy: Send + Sync {
    /// `read` / `list` / `exists` 是否允许。
    fn allow_read(&self, path: &Path) -> bool {
        let _ = path;
        true
    }
    /// `write` 是否允许。
    fn allow_write(&self, path: &Path) -> bool {
        let _ = path;
        true
    }
    /// 拒绝时的错误前缀（策略语义说明）。
    fn deny_reason(&self) -> &'static str {
        "path denied by fs policy"
    }
}

/// 直通策略（默认）：全部允许，无沙箱。
pub struct AllowAllPolicy;

impl FsPolicy for AllowAllPolicy {}

/// 工作区包含策略：写限定在根集合之下。
pub struct WorkspaceContainment {
    roots: RwLock<Vec<PathBuf>>,
}

impl WorkspaceContainment {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots: RwLock::new(roots) }
    }

    /// 宿主切换工作区时更新根集合。
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.roots.write().unwrap() = roots;
    }

    /// 目标是否落在任一根之下（词法规范化；`~` 展开）。
    fn under_any(&self, path: &Path) -> bool {
        let roots = self.roots.read().unwrap();
        if roots.is_empty() {
            return false;
        }
        let expanded = expand_home(path);
        let canonical = canonicalize_best_effort(&expanded);
        roots.iter().any(|root| {
            let root_expanded = expand_home(root);
            let root_canonical = canonicalize_best_effort(&root_expanded);
            canonical == root_canonical || canonical.starts_with(&root_canonical)
        })
    }
}

impl FsPolicy for WorkspaceContainment {
    fn allow_write(&self, path: &Path) -> bool {
        self.under_any(path)
    }

    fn deny_reason(&self) -> &'static str {
        "write denied: path is outside the workspace sandbox"
    }
}

/// 权限预设的沙箱档位（web SandboxMode 三值；`permission/preset` 旋钮
/// 的执行承载——上游 confined call 在 bash 与 fs 两面生效，rustdsh 落
/// fs 工具强制 + shell 提示词叙述的等价，见 main.rs 偏差说明）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsMode {
    /// 只读：写一律拒绝。
    ReadOnly,
    /// 工作区内写（默认）。
    WorkspaceWrite,
    /// 全放行。
    DangerFullAccess,
}

impl FsMode {
    /// web 行形值解析（`sandbox/mode` data.mode）。
    pub fn from_web(value: &str) -> Option<Self> {
        match value {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }

    /// web 行形值。
    pub fn as_web(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// 三档可切策略：包装工作区包含（根集合仍由宿主按工作区喂），随
/// 权限预设 `set_mode` 切档；fs 工具经 `Arc<dyn FsPolicy>` 持有。
pub struct SwitchablePolicy {
    mode: RwLock<FsMode>,
    containment: WorkspaceContainment,
}

impl SwitchablePolicy {
    pub fn new(mode: FsMode, roots: Vec<PathBuf>) -> Self {
        Self { mode: RwLock::new(mode), containment: WorkspaceContainment::new(roots) }
    }

    /// 权限预设切换档位。
    pub fn set_mode(&self, mode: FsMode) {
        *self.mode.write().unwrap() = mode;
    }

    /// 当前档位。
    pub fn mode(&self) -> FsMode {
        *self.mode.read().unwrap()
    }

    /// 宿主切换工作区时更新根集合（仅 workspace-write 档消费）。
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        self.containment.set_roots(roots);
    }
}

impl FsPolicy for SwitchablePolicy {
    fn allow_write(&self, path: &Path) -> bool {
        match self.mode() {
            FsMode::ReadOnly => false,
            FsMode::WorkspaceWrite => self.containment.allow_write(path),
            FsMode::DangerFullAccess => true,
        }
    }

    fn deny_reason(&self) -> &'static str {
        match self.mode() {
            FsMode::ReadOnly => "write denied: the session is read-only (permission preset)",
            FsMode::WorkspaceWrite => self.containment.deny_reason(),
            FsMode::DangerFullAccess => "write denied",
        }
    }
}

/// 展开 `~` / `~/` 前缀。
fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{home}/{rest}"));
        }
    } else if text == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    path.to_path_buf()
}

/// 尽力规范化：目标不存在（写新文件）时回退到最近存在的祖先，再接回
/// 剩余段（web containment 的 ancestor-walk 保守等价）。
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut ancestor = path.to_path_buf();
    let mut suffix: Vec<PathBuf> = Vec::new();
    while let Some(parent) = ancestor.parent() {
        suffix.push(ancestor.file_name().map(PathBuf::from).unwrap_or_default());
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for seg in suffix.into_iter().rev() {
                out.push(seg);
            }
            return out;
        }
        ancestor = parent.to_path_buf();
    }
    path.to_path_buf()
}

/// `fs` 工具：所有操作经注入的策略检查后落到 `std::fs`。
/// 相对路径按会话工作目录展开（web session.header.cwd 语义）。
#[derive(Clone)]
pub struct FsTool {
    policy: Arc<dyn FsPolicy>,
    workdir: dsh_tools::Workdir,
}

impl FsTool {
    pub fn new(policy: Arc<dyn FsPolicy>) -> Self {
        Self { policy, workdir: dsh_tools::Workdir::new() }
    }

    /// 注入会话工作目录（相对路径的解析基准）。
    pub fn with_workdir(mut self, workdir: dsh_tools::Workdir) -> Self {
        self.workdir = workdir;
        self
    }
}

impl Default for FsTool {
    fn default() -> Self {
        Self::new(Arc::new(AllowAllPolicy))
    }
}

#[async_trait]
impl Tool for FsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs".into(),
            description: "Read, write, list, or check files on the local filesystem.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["read", "write", "list", "exists"] },
                    "path": { "type": "string", "description": "Filesystem path." },
                    "content": { "type": "string", "description": "Content to write (write op only)." }
                },
                "required": ["op", "path"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let op = input.arguments.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let path = input.arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let path = self.workdir.resolve(Path::new(path));
        let path = path.as_path();
        if path.as_os_str().is_empty() {
            return ToolExecutionResult::error("path must be a non-empty string");
        }
        match op {
            "read" | "list" | "exists" => {
                if !self.policy.allow_read(path) {
                    return ToolExecutionResult::error(self.policy.deny_reason());
                }
            }
            "write" => {
                if !self.policy.allow_write(path) {
                    return ToolExecutionResult::error(self.policy.deny_reason());
                }
            }
            _ => {}
        }
        match op {
            "read" => {
                // 目录在 Windows 上 read_to_string 报 ERROR_ACCESS_DENIED
                // （"拒绝访问 os error 5"），语义误导模型；显式指路到 list。
                if path.is_dir() {
                    return ToolExecutionResult::error(format!(
                        "read failed: {} is a directory; use op=list",
                        path.display()
                    ));
                }
                match std::fs::read_to_string(path) {
                    Ok(text) => ToolExecutionResult::text(text),
                    Err(e) => ToolExecutionResult::error(format!("read failed: {e}")),
                }
            }
            "write" => {
                let content = input.arguments.get("content").and_then(|v| v.as_str()).unwrap_or("");
                // before 捕获（上游 DiffResultView.FileDiff：oldText null = 新文件/无先前内容；
                // 非文本旧内容同样按 null——UI 元数据只服务文本 diff）
                let before = std::fs::read(path)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok());
                match std::fs::write(path, content) {
                    Ok(()) => {
                        let presentation = serde_json::json!({
                            "card": "diff",
                            "diffs": [{
                                "path": path.display().to_string(),
                                "oldText": before,
                                "newText": content,
                            }],
                        });
                        ToolExecutionResult {
                            presentation: Some(presentation),
                            ..ToolExecutionResult::text(format!("wrote {} bytes", content.len()))
                        }
                    }
                    Err(e) => ToolExecutionResult::error(format!("write failed: {e}")),
                }
            }
            "list" => match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut out = String::new();
                    for entry in entries.flatten() {
                        out.push_str(&entry.file_name().to_string_lossy());
                        out.push('\n');
                    }
                    ToolExecutionResult::text(out)
                }
                Err(e) => ToolExecutionResult::error(format!("list failed: {e}")),
            },
            "exists" => ToolExecutionResult::text(format!("{}", path.exists())),
            other => ToolExecutionResult::error(format!("unknown op \"{other}\"")),
        }
    }
}

/// 上游 `read` 工具名形别名（tool-fs/read.ts：name 'read'，参数
/// file_path/offset/limit）：执行侧改写参数委托 fs op=read 原面（沙箱/
/// workdir 同路）。提示词 section 以「read 工具」名引用——缺注册时模型
/// 直呼名会吃 `no tool "read"`（miaocr 会话实测回归）。
pub struct ReadTool {
    fs: FsTool,
}

impl ReadTool {
    pub fn new(fs: FsTool) -> Self {
        Self { fs }
    }
}

/// 上游 read 的行窗（offset 1 起 / limit 行数）；缺省全文件。
fn apply_read_window(text: &str, offset: Option<u64>, limit: Option<u64>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_string();
    }
    let start = offset.unwrap_or(1).saturating_sub(1) as usize;
    let take = limit.unwrap_or(u64::MAX).min(usize::MAX as u64) as usize;
    text.lines().skip(start).take(take).collect::<Vec<_>>().join("
")
}

#[async_trait]
impl Tool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: "Read a UTF-8 text file and return its content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to read, resolved by the filesystem backend. Provide `file_path` before other arguments." },
                    "offset": { "type": "number", "description": "1-based first line to return. Defaults to 1." },
                    "limit": { "type": "number", "description": "Maximum number of lines to return." }
                },
                "required": ["file_path"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        // 上游参数名为准；path 容错覆盖 fs op 形的习惯偏差
        let args = &input.arguments;
        let path = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str());
        let path = match path {
            Some(p) if !p.trim().is_empty() => p,
            _ => return ToolExecutionResult::error("file_path must be a non-empty string"),
        };
        let forwarded = ToolExecutionInput {
            call_id: input.call_id.clone(),
            name: "fs".into(),
            arguments: json!({"op": "read", "path": path}),
        };
        let result = self.fs.execute(&forwarded).await;
        // 行窗在成功文本上后置应用（委托面返回整文件）
        let offset = args.get("offset").and_then(|v| v.as_u64());
        let limit = args.get("limit").and_then(|v| v.as_u64());
        if offset.is_none() && limit.is_none() {
            return result;
        }
        if let Some(ContentBlock::Text { text }) = result.content.first() {
            let windowed = apply_read_window(text, offset, limit);
            return ToolExecutionResult::text(windowed);
        }
        result
    }
}

/// 上游 `write` 工具名形别名（tool-fs/write.ts：name 'write'，参数
/// file_path/content）：委托 fs op=write 原面（diff presentation 同路）。
pub struct WriteTool {
    fs: FsTool,
}

impl WriteTool {
    pub fn new(fs: FsTool) -> Self {
        Self { fs }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: "Create or fully replace a UTF-8 text file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to write, resolved by the filesystem backend. Provide `file_path` before `content` in the arguments." },
                    "content": { "type": "string", "description": "Full UTF-8 text content to write." }
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let args = &input.arguments;
        let path = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str());
        let path = match path {
            Some(p) if !p.trim().is_empty() => p,
            _ => return ToolExecutionResult::error("file_path must be a non-empty string"),
        };
        let content = args.get("content").and_then(|v| v.as_str());
        let content = match content {
            Some(c) => c,
            None => return ToolExecutionResult::error("content is required for write"),
        };
        let forwarded = ToolExecutionInput {
            call_id: input.call_id.clone(),
            name: "fs".into(),
            arguments: json!({"op": "write", "path": path, "content": content}),
        };
        self.fs.execute(&forwarded).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_roots(roots: &[&str]) -> WorkspaceContainment {
        WorkspaceContainment::new(roots.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn write_inside_root_allowed_outside_denied() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let policy = policy_with_roots(&[dir.to_str().unwrap()]);
        assert!(policy.allow_write(&dir.join("sub/new.txt")));
        assert!(!policy.allow_write(Path::new("/etc/hosts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nonexistent_target_uses_ancestor_containment() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-test2-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let policy = policy_with_roots(&[dir.join("a").to_str().unwrap()]);
        // 目标 b/c/d.txt 尚不存在：回退到存在的祖先 b 判包含
        assert!(policy.allow_write(&dir.join("a/b/c/d.txt")));
        assert!(!policy.allow_write(&dir.join("outside/x.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_roots_deny_all_writes() {
        let policy = WorkspaceContainment::new(Vec::new());
        assert!(!policy.allow_write(Path::new("/tmp/x")));
    }

    #[tokio::test]
    async fn read_on_directory_points_to_list() {
        // 目录读取曾误报「拒绝访问 os error 5」；应显式提示改用 op=list
        let tool = FsTool::default();
        let dir = std::env::temp_dir().join(format!("dsh-fs-dir-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = tool
            .execute(&ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("t".into()),
                "fs".into(),
                format!(r#"{{"op": "read", "path": "{}"}}"#, dir.to_string_lossy().replace('\\', "\\\\")).into(),
            ))
            .await;
        assert!(result.is_error);
        let text = result
            .content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(text.contains("is a directory") && text.contains("op=list"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn switchable_policy_three_modes() {
        let tmp = std::env::temp_dir().join(format!("dsh-fs-mode-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let inside = tmp.join("in.txt");
        let outside = std::env::temp_dir().join(format!("dsh-fs-mode-out-{}.txt", std::process::id()));
        let policy = SwitchablePolicy::new(FsMode::WorkspaceWrite, vec![tmp.clone()]);
        // workspace-write：内可写、外拒
        assert!(policy.allow_write(&inside));
        assert!(!policy.allow_write(&outside));
        // read-only：内外全拒
        policy.set_mode(FsMode::ReadOnly);
        assert!(!policy.allow_write(&inside));
        assert!(!policy.allow_write(&outside));
        // danger-full-access：外也可写
        policy.set_mode(FsMode::DangerFullAccess);
        assert!(policy.allow_write(&outside));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn fs_mode_web_roundtrip() {
        assert_eq!(FsMode::from_web("read-only"), Some(FsMode::ReadOnly));
        assert_eq!(FsMode::from_web("workspace-write"), Some(FsMode::WorkspaceWrite));
        assert_eq!(
            FsMode::from_web("danger-full-access"),
            Some(FsMode::DangerFullAccess)
        );
        assert_eq!(FsMode::from_web("nope"), None);
        assert_eq!(FsMode::WorkspaceWrite.as_web(), "workspace-write");
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use dsh_tools::{Tool, ToolExecutionInput};

    #[tokio::test]
    async fn read_alias_accepts_file_path_and_path_with_window() {
        // 上游名形别名：file_path 为准、path 容错；offset/limit 行窗
        let dir = std::env::temp_dir().join(format!("dsh-fs-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "l1
l2
l3
l4").unwrap();
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.clone());
        let fs = FsTool::new(Arc::new(AllowAllPolicy)).with_workdir(wd.clone());
        let read = ReadTool::new(fs.clone());

        let r = read.execute(&input(r#"{"file_path": "a.txt"}"#)).await;
        assert_eq!(text_of(&r), "l1
l2
l3
l4");
        // path 容错（fs op 形习惯偏差）
        let r = read.execute(&input(r#"{"path": "a.txt"}"#)).await;
        assert_eq!(text_of(&r), "l1
l2
l3
l4");
        // 行窗：offset 2 起、limit 2 行
        let r = read
            .execute(&input(r#"{"file_path": "a.txt", "offset": 2, "limit": 2}"#))
            .await;
        assert_eq!(text_of(&r), "l2
l3");
        // 缺 file_path
        let r = read.execute(&input("{}")).await;
        assert!(text_of(&r).contains("file_path must be a non-empty string"));
        // 委托原面：目录 read 指路 op=list
        let r = read.execute(&input(r#"{"file_path": "."}"#)).await;
        assert!(text_of(&r).contains("use op=list"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_alias_writes_and_keeps_diff_presentation() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-alias-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.clone());
        let fs = FsTool::new(Arc::new(AllowAllPolicy)).with_workdir(wd);
        let write = WriteTool::new(fs);

        let r = write
            .execute(&input(r#"{"file_path": "new.txt", "content": "hello"}"#))
            .await;
        assert!(text_of(&r).contains("wrote 5 bytes"));
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "hello");
        // diff presentation 委托面原样保留（oldText=null = 新文件）
        let diff = r.presentation.expect("diff presentation");
        assert_eq!(diff["card"], "diff");
        assert_eq!(diff["diffs"][0]["oldText"], serde_json::Value::Null);
        // 追加写：oldText 为先前内容
        let r = write
            .execute(&input(r#"{"file_path": "new.txt", "content": "hello2"}"#))
            .await;
        assert_eq!(r.presentation.unwrap()["diffs"][0]["oldText"], "hello");
        // 缺 content
        let r = write.execute(&input(r#"{"file_path": "x.txt"}"#)).await;
        assert!(text_of(&r).contains("content is required"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn text_of(r: &ToolExecutionResult) -> String {
        r.content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn input(args: &str) -> ToolExecutionInput {
        ToolExecutionInput::with_raw_arguments(
            dsh_llm::CallId("c1".to_string()),
            "fs".to_string(),
            args.to_string(),
        )
    }

    /// write 的 presentation 缝：FileDiff（oldText=先前内容/新文件 null），
    /// 模型可见文本保持 "wrote N bytes"。
    #[tokio::test]
    async fn write_carries_file_diff_presentation() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-pres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = FsTool::new(std::sync::Arc::new(AllowAllPolicy));
        let path = dir.join("a.txt");
        let write_args = |content: &str| {
            serde_json::json!({"op": "write", "path": path.display().to_string(), "content": content})
                .to_string()
        };

        // 新文件：oldText = null
        let r = tool.execute(&input(&write_args("hi"))).await;
        assert!(!r.is_error);
        let meta = r.presentation.clone().unwrap();
        assert_eq!(meta["card"], "diff");
        assert_eq!(meta["diffs"][0]["oldText"], serde_json::Value::Null);
        assert_eq!(meta["diffs"][0]["newText"], "hi");
        assert!(
            matches!(&r.content[0], dsh_llm::ContentBlock::Text { text } if text.starts_with("wrote "))
        );

        // 覆盖：oldText = 先前内容
        let r = tool.execute(&input(&write_args("bye"))).await;
        let meta = r.presentation.unwrap();
        assert_eq!(meta["diffs"][0]["oldText"], "hi");
        assert_eq!(meta["diffs"][0]["newText"], "bye");
        std::fs::remove_dir_all(&dir).ok();
    }
}
