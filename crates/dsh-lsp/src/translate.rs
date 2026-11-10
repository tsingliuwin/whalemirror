//! 服务端能力判定与结果归一：上游 `lsp-stdio/src/translate.ts` 的纯函数面。
//!
//! 这里没有任何 I/O 或进程状态——每个函数都是纯变换，接收未经信任的 wire 数据
//! （[`Value`]）并产出缝的闭集类型，或给出逐字对齐的 [`LspError`]。

use serde_json::Value;

use crate::types::{LspHover, LspLocation, LspOperation, LspPosition, LspRange};
use crate::{LspError, LspErrorCode};

/// 该操作对应的 `textDocument/*` 方法名。
pub fn request_method(operation: LspOperation) -> &'static str {
    operation.method()
}

/// 服务器是否声明支持该操作。
///
/// 与上游 `supportsCapability` 同语义：只有**缺省**与布尔 `false` 判为不支持；
/// `true`、对象、以及 `null`（`typeof null === 'object'`）都算支持。
pub fn supports_operation(capabilities: &Value, operation: LspOperation) -> bool {
    let key = match operation {
        LspOperation::GoToDefinition => "definitionProvider",
        LspOperation::FindReferences => "referencesProvider",
        LspOperation::GoToImplementation => "implementationProvider",
        LspOperation::Hover => "hoverProvider",
    };
    supports_capability(capabilities.get(key))
}

/// `provider` 槽位的判定：缺省与 `false` 不支持，`true`、对象与 `null` 都支持。
fn supports_capability(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::Bool(present)) => *present,
        Some(_) => true,
    }
}

/// 服务器的 `textDocumentSync` 是否允许本宿主依赖的瞬态 `didOpen`/`didClose`。
///
/// 旧的枚举形态里 `Full`(1)/`Incremental`(2) 隐含开闭支持；选项形态必须显式
/// `openClose: true`——协议把缺省的 `openClose` 当 `false`。
pub fn supports_transient_open(sync: Option<&Value>) -> bool {
    match sync {
        None => false,
        Some(Value::Number(kind)) => matches!(kind.as_u64(), Some(1) | Some(2)),
        Some(Value::Object(options)) => {
            options.get("openClose").and_then(Value::as_bool).unwrap_or(false)
        }
        Some(_) => false,
    }
}

/// 归一位置编码：缺省即 `utf-16`；其它取值是协议错误，本宿主不支持。
pub fn negotiate_position_encoding(encoding: Option<&str>) -> Result<(), LspError> {
    match encoding {
        None | Some("utf-16") => Ok(()),
        Some(other) => Err(LspError::new(
            LspErrorCode::MalformedResponse,
            format!(
                "server negotiated unsupported position encoding \"{other}\"; this host requires utf-16"
            ),
        )),
    }
}

/// 归一导航结果（`Location`、`Location[]`、`LocationLink[]` 或 `null`）。
///
/// `Location` 直接映射；`LocationLink` 取 `targetUri` + `targetSelectionRange`。
/// `null` 是「没有结果」，`None`（响应里根本没有 `result` 字段）是协议错误。
pub fn normalize_locations(payload: Option<&Value>) -> Result<Vec<LspLocation>, LspError> {
    let payload = match payload {
        None => return Err(malformed("LSP navigation result was missing")),
        Some(Value::Null) => return Ok(Vec::new()),
        Some(value) => value,
    };
    let elements: Vec<&Value> = match payload {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut locations = Vec::with_capacity(elements.len());
    for element in elements {
        let Value::Object(record) = element else {
            return Err(malformed("LSP navigation result contained a non-object entry"));
        };
        if let Some((uri, range)) = as_location_link(record) {
            locations.push(LspLocation { uri, range });
            continue;
        }
        if let Some((uri, range)) = as_location(record) {
            locations.push(LspLocation { uri, range });
            continue;
        }
        return Err(malformed(
            "LSP navigation result contained neither a Location nor a LocationLink",
        ));
    }
    Ok(locations)
}

/// `LocationLink`：`targetUri` + `targetSelectionRange`。
fn as_location_link(record: &serde_json::Map<String, Value>) -> Option<(String, LspRange)> {
    let uri = record.get("targetUri")?.as_str()?.to_string();
    let range = to_range(record.get("targetSelectionRange")?)?;
    Some((uri, range))
}

/// `Location`：`uri` + `range`。
fn as_location(record: &serde_json::Map<String, Value>) -> Option<(String, LspRange)> {
    let uri = record.get("uri")?.as_str()?.to_string();
    let range = to_range(record.get("range")?)?;
    Some((uri, range))
}

/// 把 wire range 换成缝的 [`LspRange`]；结构不合法时返回 `None`。
fn to_range(value: &Value) -> Option<LspRange> {
    let range = value.as_object()?;
    Some(LspRange {
        start: to_position(range.get("start")?)?,
        end: to_position(range.get("end")?)?,
    })
}

/// 把 wire position 换成缝的 [`LspPosition`]；坐标必须是非负整数。
fn to_position(value: &Value) -> Option<LspPosition> {
    let position = value.as_object()?;
    Some(LspPosition {
        line: coordinate(position.get("line")?)?,
        character: coordinate(position.get("character")?)?,
    })
}

/// 协议坐标：非负整数。
fn coordinate(value: &Value) -> Option<u32> {
    let number = value.as_i64()?;
    if number < 0 {
        return None;
    }
    u32::try_from(number).ok()
}

/// 归一 `Hover`（或 `null`）。
///
/// `MarkupContent` 取 `value`；字符串形态的 `MarkedString` 原样；带语言的
/// `MarkedString` 变成围栏代码块；数组用空行连接。空内容视为「无 hover」——
/// 注意这个短路发生在 range 校验之前（与上游一致）。
pub fn normalize_hover(payload: Option<&Value>) -> Result<Option<LspHover>, LspError> {
    let payload = match payload {
        None => return Err(malformed("LSP hover result was missing")),
        Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let Value::Object(hover) = payload else {
        return Err(malformed("LSP hover result was not an object"));
    };
    let contents = render_hover_contents(hover.get("contents"))?;
    if contents.is_empty() {
        return Ok(None);
    }
    let Some(range) = hover.get("range") else {
        return Ok(Some(LspHover { contents, range: None }));
    };
    let Some(range) = to_range(range) else {
        return Err(malformed("LSP hover result contained a malformed range"));
    };
    Ok(Some(LspHover { contents, range: Some(range) }))
}

/// 把 `Hover.contents` 的三种编码渲染成一个字符串（输入是未信任的 wire 数据）。
fn render_hover_contents(contents: Option<&Value>) -> Result<String, LspError> {
    let Some(contents) = contents else {
        return Err(malformed("LSP hover result had no contents"));
    };
    match contents {
        Value::Null => Err(malformed("LSP hover result had no contents")),
        Value::String(text) => Ok(text.clone()),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                parts.push(render_marked_string(item)?);
            }
            Ok(parts.join("\n\n"))
        }
        Value::Object(record) => {
            if let Some(kind) = record.get("kind").and_then(Value::as_str) {
                if kind == "markdown" || kind == "plaintext" {
                    let Some(value) = record.get("value").and_then(Value::as_str) else {
                        return Err(malformed("LSP hover MarkupContent value was not a string"));
                    };
                    return Ok(value.to_string());
                }
            }
            if record.get("language").and_then(Value::as_str).is_some()
                && record.get("value").and_then(Value::as_str).is_some()
            {
                return render_marked_string(contents);
            }
            Err(malformed(
                "LSP hover contents were not MarkupContent, MarkedString, or an array",
            ))
        }
        _ => Err(malformed(
            "LSP hover contents were not MarkupContent, MarkedString, or an array",
        )),
    }
}

/// 渲染一条 `MarkedString`：字符串原样，对象形态变成带语言的围栏块。
fn render_marked_string(value: &Value) -> Result<String, LspError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Object(record) => {
            let language = record.get("language").and_then(Value::as_str);
            let body = record.get("value").and_then(Value::as_str);
            match (language, body) {
                (Some(language), Some(body)) => Ok(format!("```{language}\n{body}\n```")),
                _ => Err(malformed("LSP hover contents contained a malformed MarkedString")),
            }
        }
        _ => Err(malformed("LSP hover contents contained a malformed MarkedString")),
    }
}

/// 服务端结果不合约时的结构化错误（码固定为 `LSP_MALFORMED_RESPONSE`）。
fn malformed(message: &str) -> LspError {
    LspError::new(LspErrorCode::MalformedResponse, message)
}