//! 宿主侧文件访问：上游 `lsp-stdio/src/host.ts` 的等价物。
//!
//! 上游经 `ctx.fs` 拿「规范化工作区 + 源文本」，因为语言服务器可能跑在另一台机器/
//! 另一个 PID 命名空间；rustdsh 是进程内单机，于是直接用 `std::fs` 做同样三件事：
//! 1) 把工作区根规范化成绝对路径与 `file:` URI；2) 把源文件解析到工作区内并做包含
//! 校验；3) 按字节上限读全文（超限即报错，不做静默截断——截断会让语言服务器看到
//! 一份不存在的文档）。

use std::path::{Path, PathBuf};

use url::Url;

/// 一个规范化后的工作区：进程可用的绝对路径 + 发给语言服务器的 `file:` URI。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostWorkspace {
    /// 规范化绝对路径（可作子进程 cwd）。
    pub canonical_path: PathBuf,
    /// 规范化的 `file:` URI（`initialize` 的 `rootUri`）。
    pub file_url: String,
}

/// 一份校验过的源文件：发给语言服务器的 URI + 当前完整文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSource {
    /// 按执行平台语法的 `file:` URI。
    pub file_url: String,
    /// 当前完整 UTF-8 文本。
    pub text: String,
}

/// 解析并校验一个工作区根。
pub fn canonicalize_workspace(workspace_root: &str) -> Result<HostWorkspace, String> {
    let requested = Path::new(workspace_root);
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                format!("workspace root \"{workspace_root}\" cannot be resolved: {error}")
            })?
            .join(requested)
    };
    let canonical_path = absolute
        .canonicalize()
        .map_err(|error| format!("workspace root \"{workspace_root}\" cannot be resolved: {error}"))?;
    let metadata = std::fs::metadata(&canonical_path).map_err(|error| {
        format!("workspace root \"{workspace_root}\" cannot be resolved: {error}")
    })?;
    if !metadata.is_dir() {
        return Err(format!("workspace root \"{workspace_root}\" is not a directory"));
    }
    let file_url = file_url(&canonical_path)?;
    Ok(HostWorkspace { canonical_path, file_url })
}

/// 解析、包含校验并读取一份有字节上限的查询源。
///
/// `file_path` 可以是绝对路径，也可以是相对 `workspace` 的路径。
pub fn read_source(
    file_path: &str,
    workspace: &HostWorkspace,
    max_document_bytes: usize,
) -> Result<HostSource, String> {
    let requested = Path::new(file_path);
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.canonical_path.join(requested)
    };
    let canonical = absolute
        .canonicalize()
        .map_err(|error| format!("source \"{file_path}\" cannot be resolved: {error}"))?;
    // 组件级前缀判定：`/w` 不会把 `/w2/x` 误判成内部（与上游 fs.contains 同语义）。
    if !canonical.starts_with(&workspace.canonical_path) {
        return Err(format!("source \"{file_path}\" resolves outside the workspace"));
    }
    let bytes = std::fs::read(&canonical)
        .map_err(|error| format!("source \"{file_path}\" could not be read: {error}"))?;
    if bytes.len() > max_document_bytes {
        return Err(format!(
            "source \"{file_path}\" exceeds the {max_document_bytes}-byte limit; reading stopped after {} bytes",
            bytes.len()
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("source \"{file_path}\" could not be read: {error}"))?;
    Ok(HostSource { file_url: file_url(&canonical)?, text })
}

/// 把一个绝对路径表达成 `file:` URI。
///
/// Windows 的 `canonicalize` 会给出 `\\?\C:\...` 的 verbatim 前缀，那个前缀不属于
/// URI 语法（还会被百分号编码成 `%5C%5C%3F%5C`），所以先剥掉再转换。
pub fn file_url(path: &Path) -> Result<String, String> {
    let plain = strip_verbatim(path);
    Url::from_file_path(&plain)
        .map(|url| url.to_string())
        .map_err(|_| format!("path \"{}\" cannot be expressed as a file URI", plain.display()))
}

/// 去掉 Windows verbatim 前缀（`\\?\`，含 UNC 形态 `\\?\UNC\`）。
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path.to_path_buf()
}