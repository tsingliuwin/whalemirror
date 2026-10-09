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

/// 上游 `edit` 工具（tool-fs/edit.ts：字面替换原语）：old_string →
/// new_string，replace_all=false 时要求恰好一次匹配。行尾归一化匹配
/// （CRLF 文件按 LF 匹配、写回保留原行尾）；diff presentation 与 write
/// 同形（FileDiff oldText=编辑前全文/newText=编辑后全文，渲染侧算 hunk）。
/// 偏差：上游默认 fs-observation-policy 强制 read-before-edit，rustdsh
/// 仅提示词引导不做硬门（见 backlog）。
pub struct EditTool {
    policy: Arc<dyn FsPolicy>,
    workdir: dsh_tools::Workdir,
}

impl EditTool {
    pub fn new(policy: Arc<dyn FsPolicy>, workdir: dsh_tools::Workdir) -> Self {
        Self { policy, workdir }
    }
}

/// 行尾归一化（上游 normalizeLineEndings：CRLF → LF）。
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[derive(Debug)]
struct EditInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// 上游 parseEditArgs 校验（file_path/path 容错为 rustdsh 侧扩展）。
fn parse_edit_args(args: &serde_json::Value) -> Result<EditInput, String> {
    let path = args
        .get("file_path")
        .or_else(|| args.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if path.is_empty() {
        return Err("file_path must be a non-empty string".into());
    }
    let old_string = args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
    if old_string.is_empty() {
        return Err("old_string must be a non-empty string".into());
    }
    let new_string = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
    if old_string == new_string {
        return Err("old_string and new_string must differ".into());
    }
    let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(EditInput { path, old_string: old_string.into(), new_string: new_string.into(), replace_all })
}

/// 字面替换核心（上游 fsio.applyLiteralEdit）：归一化后计数，0 处报未
/// 找到、多处且未 replace_all 报歧义；返回编辑后内容与替换次数。
fn apply_literal_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    display_path: &str,
) -> Result<(String, usize), String> {
    let old_norm = normalize_line_endings(old_string);
    let new_norm = normalize_line_endings(new_string);
    let replacements = content.matches(&old_norm).count();
    if replacements == 0 {
        return Err(format!("old_string was not found in \"{display_path}\""));
    }
    if !replace_all && replacements > 1 {
        return Err(format!(
            "old_string matched {replacements} times in \"{display_path}\"; provide a more specific old_string or set replace_all to true"
        ));
    }
    let edited = if replace_all {
        content.replace(&old_norm, &new_norm)
    } else {
        content.replacen(&old_norm, &new_norm, 1)
    };
    Ok((edited, replacements))
}

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".into(),
            description: "Edit an existing UTF-8 text file by replacing literal text.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to edit, resolved by the filesystem backend. Provide `file_path` before `old_string` and `new_string` in the arguments." },
                    "old_string": { "type": "string", "description": "Literal text to replace." },
                    "new_string": { "type": "string", "description": "Literal replacement text. Use an empty string to delete the match." },
                    "replace_all": { "type": "boolean", "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once." }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let parsed = match parse_edit_args(&input.arguments) {
            Ok(v) => v,
            Err(e) => return ToolExecutionResult::error(e),
        };
        let path = self.workdir.resolve(Path::new(&parsed.path));
        if !self.policy.allow_write(&path) {
            return ToolExecutionResult::error(self.policy.deny_reason());
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => return ToolExecutionResult::error(format!("edit failed: {e}")),
        };
        // CRLF 文件按 LF 匹配与编辑，写回保留原行尾（上游 normalize/restore）
        let crlf = raw.contains("\r\n");
        let display = path.display().to_string();
        let content = if crlf { normalize_line_endings(&raw) } else { raw.clone() };
        let (edited, _replacements) =
            match apply_literal_edit(&content, &parsed.old_string, &parsed.new_string, parsed.replace_all, &display) {
                Ok(v) => v,
                Err(e) => return ToolExecutionResult::error(e),
            };
        let out = if crlf { edited.replace('\n', "\r\n") } else { edited };
        if let Err(e) = std::fs::write(&path, &out) {
            return ToolExecutionResult::error(format!("edit failed: {e}"));
        }
        let text = if parsed.replace_all {
            format!("The file {display} has been updated. All occurrences were successfully replaced.")
        } else {
            format!("The file {display} has been updated successfully.")
        };
        let presentation = serde_json::json!({
            "card": "diff",
            "diffs": [{
                "path": display,
                "oldText": content,
                "newText": normalize_line_endings(&out),
            }],
        });
        ToolExecutionResult {
            presentation: Some(presentation),
            ..ToolExecutionResult::text(text)
        }
    }
}

/// 上游 `read_image` 工具（tool-fs/read-image.ts）：读 PNG/JPEG/WebP/GIF
/// 文件并把图片本身交还模型——扩展名声明 + 魔数嗅探定媒体类型，字节
/// 入附件存储（内容寻址，先持久再返回），结果为 [envelope 文本, image
/// 块]（图片随 tool/result 落盘；请求序列化把嵌套图片拆为紧随的合成
/// user 消息——OpenAI wire 的 tool 角色不带 image_url part）。
/// 偏差：无 route 能力门（上游对非 vision 路由提前拒）；无字节/像素
/// 上限与规范化（上游 attachment service 面）；WEBP 仅 VP8X 画布可测。
pub struct ReadImageTool {
    policy: Arc<dyn FsPolicy>,
    workdir: dsh_tools::Workdir,
    store: Option<Arc<dsh_persist::AttachmentStore>>,
}

impl ReadImageTool {
    pub fn new(
        policy: Arc<dyn FsPolicy>,
        workdir: dsh_tools::Workdir,
        store: Option<Arc<dsh_persist::AttachmentStore>>,
    ) -> Self {
        Self { policy, workdir, store }
    }
}

/// 上游 IMAGE_EXTENSIONS：扩展名 → 声明媒体类型。
fn image_media_type_for_path(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// 魔数嗅探（上游 sniffImageMediaType）：PNG/JPEG/GIF/WEBP 签名。
fn sniff_image_media_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.starts_with(b"RIFF") && data.len() >= 12 && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

#[async_trait]
impl Tool for ReadImageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_image".into(),
            description: "Read a PNG/JPEG/WebP/GIF file and return the image itself. \
                Large images are downscaled automatically; do not install image libraries \
                or create thumbnails to inspect an image."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the image file, resolved by the filesystem backend." }
                },
                "required": ["file_path"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let raw_path = input
            .arguments
            .get("file_path")
            .or_else(|| input.arguments.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            return ToolExecutionResult::error("file_path must be a non-empty string");
        }
        let declared = image_media_type_for_path(raw_path);
        let has_extension = std::path::Path::new(raw_path).extension().is_some();
        if declared.is_none() && has_extension {
            let ext = std::path::Path::new(raw_path)
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            return ToolExecutionResult::error(format!(
                "cannot read \"{raw_path}\": the .{ext} extension does not declare a supported \
                 image format; read_image accepts PNG/JPEG/WebP/GIF files, including \
                 extension-less files in those formats"
            ));
        }
        let Some(store) = self.store.clone() else {
            return ToolExecutionResult::error(format!(
                "cannot read \"{raw_path}\" as an image: no attachment service is mounted"
            ));
        };
        let path = self.workdir.resolve(Path::new(raw_path));
        if !self.policy.allow_read(&path) {
            return ToolExecutionResult::error(self.policy.deny_reason());
        }
        let data = match std::fs::read(&path) {
            Ok(v) => v,
            Err(e) => return ToolExecutionResult::error(format!("read_image failed: {e}")),
        };
        let display = path.display().to_string();
        let media_type = match declared.or_else(|| sniff_image_media_type(&data)) {
            Some(t) => t,
            None => {
                return ToolExecutionResult::error(format!(
                    "cannot read \"{display}\": the file content is not a supported image \
                     format; read_image accepts PNG/JPEG/WebP/GIF files"
                ))
            }
        };
        let Some((width, height)) = dsh_persist::image_dimensions(&data) else {
            return ToolExecutionResult::error(format!(
                "cannot read \"{display}\": the {media_type} image dimensions could not be \
                 determined (basic VP8/VP8L WebP is unsupported); convert it to a standard \
                 WebP or a PNG/JPEG and retry"
            ));
        };
        let name = std::path::Path::new(raw_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| raw_path.to_string());
        // 先持久再返回：image 块引用的对象在 tool/result 落盘前必须已提交
        let reference = match store.save_file_verbatim(&data, Some(&name)) {
            Ok(r) => r,
            Err(e) => return ToolExecutionResult::error(format!("read_image failed: {e}")),
        };
        let attachment = dsh_llm::ImageAttachmentRef {
            attachment_id: reference.attachment_id,
            name: Some(reference.name),
            media_type: media_type.to_string(),
            bytes: reference.bytes,
            width,
            height,
            original_dimensions: None,
        };
        // 上游 imageReadContent 双块：模型可见 envelope + 图片本体
        let envelope = format!(
            "<path>{display}</path>\n<type>image</type>\n<content>\n{media_type} image, \
             {width}x{height} px, {bytes} bytes\n</content>",
            bytes = attachment.bytes,
        );
        let result = ToolExecutionResult {
            content: vec![
                ContentBlock::text(envelope),
                ContentBlock::Image { attachment, offloaded: false },
            ],
            is_error: false,
            concludes_turn: false,
            presentation: Some(json!({ "path": display })),
        };
        result
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

    #[tokio::test]
    async fn edit_replaces_single_match_with_diff_presentation() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma").unwrap();
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.clone());
        let edit = EditTool::new(Arc::new(AllowAllPolicy), wd);

        let r = edit
            .execute(&input(
                r#"{"file_path": "a.txt", "old_string": "beta", "new_string": "BETA"}"#,
            ))
            .await;
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nBETA\ngamma");
        let text = text_of(&r);
        assert!(text.contains("has been updated successfully"), "{text}");
        let diff = r.presentation.expect("diff presentation");
        assert_eq!(diff["card"], "diff");
        assert_eq!(diff["diffs"][0]["oldText"], "alpha\nbeta\ngamma");
        assert_eq!(diff["diffs"][0]["newText"], "alpha\nBETA\ngamma");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_error_paths_match_upstream_wording() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-edit-e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x\nx\ny").unwrap();
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.clone());
        let edit = EditTool::new(Arc::new(AllowAllPolicy), wd);

        // 未找到
        let r = edit
            .execute(&input(r#"{"file_path": "a.txt", "old_string": "zzz", "new_string": "q"}"#))
            .await;
        assert!(text_of(&r).contains("old_string was not found in"), "{}", text_of(&r));
        // 多处且未 replace_all
        let r = edit
            .execute(&input(r#"{"file_path": "a.txt", "old_string": "x", "new_string": "q"}"#))
            .await;
        let text = text_of(&r);
        assert!(
            text.contains("old_string matched 2 times") && text.contains("replace_all to true"),
            "{text}"
        );
        // replace_all=true：全部替换 + All occurrences 文案
        let r = edit
            .execute(&input(
                r#"{"file_path": "a.txt", "old_string": "x", "new_string": "q", "replace_all": true}"#,
            ))
            .await;
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "q\nq\ny");
        assert!(text_of(&r).contains("All occurrences were successfully replaced"));
        // old == new / 空 old_string
        let r = edit
            .execute(&input(r#"{"file_path": "a.txt", "old_string": "q", "new_string": "q"}"#))
            .await;
        assert!(text_of(&r).contains("must differ"));
        let r = edit
            .execute(&input(r#"{"file_path": "a.txt", "old_string": "", "new_string": "q"}"#))
            .await;
        assert!(text_of(&r).contains("old_string must be a non-empty string"));
        // path 容错
        let r = edit
            .execute(&input(r#"{"path": "a.txt", "old_string": "q", "new_string": "z", "replace_all": true}"#))
            .await;
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "z\nz\ny");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_normalizes_crlf_matching_and_restores_line_endings() {
        // CRLF 文件：old_string 用 LF 匹配；写回保留 CRLF
        let dir = std::env::temp_dir().join(format!("dsh-fs-edit-crlf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("w.txt"), "a\r\nb\r\nc").unwrap();
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.clone());
        let edit = EditTool::new(Arc::new(AllowAllPolicy), wd);

        let r = edit
            .execute(&input(r#"{"file_path": "w.txt", "old_string": "a\nb", "new_string": "A\nB"}"#))
            .await;
        assert!(text_of(&r).contains("has been updated successfully"), "{}", text_of(&r));
        let content = std::fs::read_to_string(dir.join("w.txt")).unwrap();
        assert_eq!(content, "A\r\nB\r\nc", "CRLF must be preserved");

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

#[cfg(test)]
mod read_image_tests {
    use super::*;
    use dsh_llm::ContentBlock;

    fn input(args: &str) -> ToolExecutionInput {
        ToolExecutionInput::with_raw_arguments(
            dsh_llm::CallId("c1".to_string()),
            "read_image".to_string(),
            args.to_string(),
        )
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    fn tool(dir: &std::path::Path) -> ReadImageTool {
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.to_path_buf());
        let store = dsh_persist::AttachmentStore::new(dir.join("attachments"));
        ReadImageTool::new(Arc::new(AllowAllPolicy), wd, Some(Arc::new(store)))
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

    #[tokio::test]
    async fn read_image_returns_envelope_and_image_block_persisted() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-ri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), png(320, 200)).unwrap();
        let t = tool(&dir);

        let r = t.execute(&input(r#"{"file_path": "shot.png"}"#)).await;
        assert!(!r.is_error);
        // 双块：envelope 文本 + 图片本体
        assert_eq!(r.content.len(), 2);
        let text = text_of(&r);
        assert!(text.contains("<path>"), "{text}");
        assert!(text.contains("image/png image, 320x200 px"), "{text}");
        assert!(text.contains(&format!("{} bytes", png(320, 200).len())), "{text}");
        match &r.content[1] {
            ContentBlock::Image { attachment, offloaded: false } => {
                assert_eq!(attachment.width, 320);
                assert_eq!(attachment.height, 200);
                assert_eq!(attachment.media_type, "image/png");
                assert_eq!(attachment.name.as_deref(), Some("shot.png"));
                // 字节已持久（内容寻址对象真实存在）
                let hex = attachment.attachment_id.trim_start_matches("sha256:");
                let obj = dir.join("attachments").join("file-objects").join(&hex[..2]).join(hex);
                assert!(obj.exists(), "object must be committed: {}", obj.display());
                assert_eq!(std::fs::read(&obj).unwrap(), png(320, 200));
            }
            other => panic!("expected image block, got {other:?}"),
        }
        // presentation：{path}（上游 presentationMeta）
        assert_eq!(r.presentation.as_ref().unwrap()["path"], dir.join("shot.png").display().to_string());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_image_error_paths_match_upstream_wording() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-ri-e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), png(1, 1)).unwrap();
        std::fs::write(dir.join("note.txt"), b"plain text").unwrap();
        let t = tool(&dir);

        // 非图片扩展名
        let r = t.execute(&input(r#"{"file_path": "note.txt"}"#)).await;
        let text = text_of(&r);
        assert!(
            text.contains("the .txt extension does not declare a supported image format"),
            "{text}"
        );
        // 扩展名无、内容非图片
        std::fs::write(dir.join("noext"), b"not an image").unwrap();
        let r = t.execute(&input(r#"{"file_path": "noext"}"#)).await;
        assert!(text_of(&r).contains("the file content is not a supported image format"), "{}", text_of(&r));
        // 空 file_path
        let r = t.execute(&input("{}")).await;
        assert!(text_of(&r).contains("file_path must be a non-empty string"));
        // 无附件存储
        let wd = dsh_tools::Workdir::new();
        wd.set(dir.to_path_buf());
        let bare = ReadImageTool::new(Arc::new(AllowAllPolicy), wd, None);
        let r = bare.execute(&input(r#"{"file_path": "a.png"}"#)).await;
        assert!(text_of(&r).contains("no attachment service is mounted"));
        // path 容错
        let r = t.execute(&input(r#"{"path": "a.png"}"#)).await;
        assert!(!r.is_error);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_image_sniffs_extensionless_png() {
        let dir = std::env::temp_dir().join(format!("dsh-fs-ri-s-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blob"), png(4, 4)).unwrap();
        let t = tool(&dir);
        let r = t.execute(&input(r#"{"file_path": "blob"}"#)).await;
        assert!(!r.is_error, "{}", text_of(&r));
        match &r.content[1] {
            ContentBlock::Image { attachment, .. } => assert_eq!(attachment.media_type, "image/png"),
            other => panic!("expected image block, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
