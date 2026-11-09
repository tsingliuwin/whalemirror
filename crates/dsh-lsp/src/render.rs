//! Pure formatting and coordinate conversion for the `lsp` tool: one-based↔zero-based
//! UTF-16 cursor conversion, workspace-grouped location rendering with `file:`-URI
//! resolution, complete-result capping, and UI presentation.
//!
//! Mirrors [`packages/lsp/tool-lsp/src/render.ts`]. No I/O.

use crate::types::{LspHover, LspLocation, LspOperation, LspPosition};
use url::Url;

/// The four operations the tool exposes, as a runtime tuple for schema enum + validation.
pub const LSP_OPERATIONS: [LspOperation; 4] = [
    LspOperation::GoToDefinition,
    LspOperation::FindReferences,
    LspOperation::GoToImplementation,
    LspOperation::Hover,
];

/// Default cap on rendered locations before an omission marker is appended.
pub const DEFAULT_MAX_LOCATIONS: usize = 100;

/// Default cap on the complete rendered tool result, including truncation metadata.
pub const DEFAULT_MAX_RESULT_CHARS: usize = 16_000;

/// Validated `lsp` arguments after coordinate checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspToolInput {
    pub operation: LspOperation,
    pub file_path: String,
    /// Zero-based UTF-16 position converted from the one-based model coordinates.
    pub position: LspPosition,
}

/// Validate and convert model arguments: `operation` must be one of the four;
/// `line`/`character` are positive one-based integers converted to the seam's zero-based
/// position.
pub fn parse_lsp_args(
    operation: &str,
    file_path: &str,
    line: f64,
    character: f64,
) -> Result<LspToolInput, String> {
    let operation = LspOperation::parse(operation).ok_or_else(|| {
        format!("operation must be one of {}", operation_list())
    })?;
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".to_string());
    }
    let line = one_based(line, "line")?;
    let character = one_based(character, "character")?;
    Ok(LspToolInput {
        operation,
        file_path: file_path.to_string(),
        // The model counts from 1; the seam (and protocol) count from 0.
        position: LspPosition { line: line - 1, character: character - 1 },
    })
}

/// The comma-joined operation names, for the validation message and schema enum.
pub fn operation_list() -> String {
    LSP_OPERATIONS
        .iter()
        .map(|operation| operation.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validate a one-based coordinate is a positive integer.
fn one_based(value: f64, name: &str) -> Result<u32, String> {
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 {
        return Err(format!("{name} must be a positive integer (one-based)"));
    }
    Ok(value as u32)
}

/// Render a locations result grouped by file, converting each zero-based location back
/// to a one-based `path:line:character` entry.
///
/// A `file:` URI inside the workspace becomes a workspace-relative path; outside it, a
/// URI-derived absolute path; a non-`file:` URI is kept verbatim. Applies `max_locations`
/// and appends an omission marker when it truncates by count, then applies the complete
/// result cap.
pub fn format_locations(
    locations: &[LspLocation],
    workspace_uri: &str,
    max_locations: usize,
    max_result_chars: usize,
) -> String {
    if locations.is_empty() {
        return bound_result("No results.", max_result_chars, "locations");
    }
    let shown = &locations[..locations.len().min(max_locations)];
    let omitted = locations.len() - shown.len();
    // Insertion-ordered grouping: entries stay adjacent per path, paths in first-seen order.
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for location in shown {
        let path = render_uri(&location.uri, workspace_uri);
        let line = location.range.start.line + 1;
        let character = location.range.start.character + 1;
        let entry = format!("{path}:{line}:{character}");
        match grouped.iter_mut().find(|(known, _)| *known == path) {
            Some((_, entries)) => entries.push(entry),
            None => grouped.push((path, vec![entry])),
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (_, entries) in grouped {
        lines.extend(entries);
    }
    if omitted > 0 {
        let plural = if omitted == 1 { "" } else { "s" };
        lines.push(format!(
            "… {omitted} more location{plural} omitted (limit {max_locations})."
        ));
    }
    bound_result(&lines.join("\n"), max_result_chars, "locations")
}

/// Render a hover result, applying `max_result_chars` last and keeping its marker within
/// the cap.
pub fn format_hover(hover: Option<&LspHover>, max_result_chars: usize) -> String {
    let text = match hover {
        None => "No hover information.",
        Some(hover) => hover.contents.as_str(),
    };
    bound_result(text, max_result_chars, "hover")
}

/// Bound a complete rendered result, including the truncation notice itself.
fn bound_result(text: &str, max_chars: usize, label: &str) -> String {
    let length = text.chars().count();
    if length <= max_chars {
        return text.to_string();
    }
    let notice = format!("\n… {label} truncated (limit {max_chars} characters).");
    let notice_length = notice.chars().count();
    if notice_length >= max_chars {
        return notice.chars().take(max_chars).collect();
    }
    let keep = max_chars - notice_length;
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(&notice);
    out
}

/// Resolve a location URI without applying the harness host's path rules.
///
/// A valid `file:` URI becomes workspace-relative when it is under the provider's
/// canonical workspace URI, or a URI-derived absolute path otherwise; malformed and
/// non-`file:` URIs remain verbatim.
pub fn render_uri(uri: &str, workspace_uri: &str) -> String {
    if !uri.starts_with("file:") {
        return uri.to_string();
    }
    let (Ok(target), Ok(workspace)) = (Url::parse(uri), Url::parse(workspace_uri)) else {
        return uri.to_string();
    };
    if workspace.scheme() != "file" {
        return uri.to_string();
    }
    // A `file:` URI does not carry its world's OS, so a leading `/X:` segment is read as
    // a Windows drive. A POSIX workspace literally rooted at `/c:/...` would mis-render
    // (display only; edits and reads use the exact URI).
    let windows_world = !workspace.host_str().unwrap_or("").is_empty() || has_drive(&workspace);
    let target_windows_world =
        windows_world && (!target.host_str().unwrap_or("").is_empty() || has_drive(&target));
    let (Some(workspace_path), Some(target_path)) = (
        file_path(&workspace, windows_world),
        file_path(&target, target_windows_world),
    ) else {
        return uri.to_string();
    };
    if windows_world != target_windows_world {
        return target_path;
    }
    let separator = if windows_world { '\\' } else { '/' };
    let relative = relative_path(&workspace_path, &target_path, windows_world);
    let outside = relative == ".."
        || relative.starts_with(&format!("..{separator}"))
        || is_absolute_path(&relative, windows_world);
    let rendered = if relative.is_empty() {
        ".".to_string()
    } else if outside {
        target_path
    } else {
        relative
    };
    if windows_world {
        rendered.replace('\\', "/")
    } else {
        rendered
    }
}

/// Whether a `file:` URL's path starts with a Windows drive segment (`/X:` or `/X%3A`).
fn has_drive(url: &Url) -> bool {
    let path = url.path().as_bytes();
    path.len() >= 3 && path[0] == b'/' && path[1].is_ascii_alphabetic() && path[2] == b':'
}

/// Decode a file URL for its execution world while containing malformed URL failures.
fn file_path(url: &Url, windows: bool) -> Option<String> {
    let raw = url.path();
    // Node's `fileURLToPath` rejects encoded path separators.
    if raw.contains("%2f") || raw.contains("%2F") || raw.contains("%5c") || raw.contains("%5C") {
        return None;
    }
    let path = percent_decode(raw)?;
    if path.contains('\0') {
        return None;
    }
    let host = url.host_str().unwrap_or("");
    if !host.is_empty() {
        // An authority is a UNC share; POSIX `file:` URLs have no meaningful host here.
        if !windows {
            return None;
        }
        return Some(format!("\\\\{host}{}", path.replace('/', "\\")));
    }
    if windows {
        let bytes = path.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
            return Some(path[1..].replace('/', "\\"));
        }
        return Some(path.replace('/', "\\"));
    }
    Some(path)
}

/// Percent-decode a UTF-8 path, rejecting invalid escapes and non-UTF-8 results.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether a rendered relative path is in fact absolute.
fn is_absolute_path(path: &str, windows: bool) -> bool {
    if windows {
        let bytes = path.as_bytes();
        return path.starts_with('\\')
            || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
    }
    path.starts_with('/')
}

/// `path.relative` equivalent over the two decoded paths.
fn relative_path(from: &str, to: &str, windows: bool) -> String {
    let separator = if windows { '\\' } else { '/' };
    let from_parts = split_path(from, windows);
    let to_parts = split_path(to, windows);
    // A different Windows drive can never be expressed relatively.
    if windows && drive(from) != drive(to) {
        return to.to_string();
    }
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        parts.push("..".to_string());
    }
    for part in &to_parts[common..] {
        parts.push(part.clone());
    }
    parts.join(&separator.to_string())
}

/// Split a path into comparable components, dropping the root marker and (on Windows)
/// the drive prefix.
fn split_path(path: &str, windows: bool) -> Vec<String> {
    let mut rest = path;
    if windows {
        if let Some(prefix) = drive(path) {
            rest = &path[prefix.len()..];
        }
    }
    rest.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| part.to_string())
        .collect()
}

/// The `X:` drive prefix of a Windows path, if any.
fn drive(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(path[..2].to_lowercase());
    }
    None
}

/// UI presentation for a pending `lsp` call.
///
/// Uses a generic search card; the title carries the operation and one-based cursor, and
/// `locations` focuses the queried line. The shared location shape has no character, so
/// the title preserves the column.
pub fn present_lsp_call(operation: &str, file_path: &str, line: f64, character: f64) -> serde_json::Value {
    serde_json::json!({
        "card": "generic",
        "kind": "search",
        "title": format!("LSP {operation} {file_path}:{}:{}", line as i64, character as i64),
        "locations": [{ "path": file_path, "line": line as i64 }],
    })
}