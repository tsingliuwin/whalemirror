//! 轨迹 run_code 单元的记录事实解析（上游 `ui-trajectory/code-program.ts`
//! 的等价纯函数面，#54）。
//!
//! 轨迹里的 `run_code`（PTC 入口）单元显示程序源码；程序语言只从**记录**
//! 里的两处线索推断，绝不从源码内容猜：
//! - 工具 schema 顶层的声明式前缀 ``Execute a program in `typescript` …``
//!   （历史 tool-executor-ptc 的固定措辞，只认行首、大小写敏感）；
//! - `parameters.properties.code.description` 里的 TypeScript / Python 提示。
//!
//! 规则（上游 728bf7e71c 后的净态）：两个线索都只做「提升到对应语言」，
//! 因此——同语言的双线索仍可用；两线索冲突或都没有 → 不着色。

use serde_json::Value;

/// run_code 单元解析出的程序事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeProgram {
    /// 程序源码（原样）。
    pub source: String,
    /// 单元标题用的描述：显式 description 非空则用它，否则取首行非空源码。
    pub description: String,
    /// 推断出的语言；None = 不着色。
    pub language: Option<CodeLanguage>,
}

/// run_code 程序语言（只这两种被上游识别）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeLanguage {
    TypeScript,
    Python,
}

/// PTC 入口的工具名（上游 `PTC_TOOL_NAME`）。
pub const PTC_TOOL_NAME: &str = "run_code";

fn parse_record(raw: Option<&str>) -> Option<serde_json::Map<String, Value>> {
    let raw = raw?;
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// `parameters.properties.code.description` 的旧式提示：只认单词边界上的
/// TypeScript / Python。
fn code_hint(schema: &serde_json::Map<String, Value>) -> (bool, bool) {
    let description = schema
        .get("parameters")
        .and_then(Value::as_object)
        .and_then(|parameters| parameters.get("properties"))
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("code"))
        .and_then(Value::as_object)
        .and_then(|code| code.get("description"))
        .and_then(Value::as_str);
    let Some(description) = description else {
        return (false, false);
    };
    (
        word_present(description, "TypeScript"),
        word_present(description, "Python"),
    )
}

/// ASCII 单词边界上的、不区分大小写的出现（上游 `/\bTypeScript\b/i`）。
fn word_present(text: &str, word: &str) -> bool {
    let lowered = text.to_lowercase();
    let needle = word.to_lowercase();
    let bytes = lowered.as_bytes();
    let mut from = 0;
    while let Some(offset) = lowered[from..].find(&needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= lowered.len() {
            break;
        }
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// 从 schema 线索推断语言（上游 `recordedLanguage` 净态）：顶层声明式
/// 前缀里的语言必须是 typescript / python，否则**整体不着色**（不降级去
/// 读参数提示）；否则把声明与 `parameters.code.description` 的旧式提示取
/// 并集——两处都成立（冲突）或都不成立 → 不着色。
fn recorded_language(schema_raw: Option<&str>) -> Option<CodeLanguage> {
    let schema = parse_record(schema_raw)?;
    let declared = declared_language_strict(&schema)?;
    let (typescript_hint, python_hint) = code_hint(&schema);
    let typescript = declared == Some(CodeLanguage::TypeScript) || typescript_hint;
    let python = declared == Some(CodeLanguage::Python) || python_hint;
    if typescript == python {
        return None;
    }
    typescript.then_some(CodeLanguage::TypeScript).or(Some(CodeLanguage::Python))
}

/// 声明式前缀解码：无前缀 → `Some(None)`（未声明，可读参数提示）；
/// 前缀里是 typescript/python → `Some(Some(lang))`；前缀里是别的语言 →
/// `None`（整条线索作废）。只认行首、大小写敏感的 `Execute a program
/// in \``（上游正则）。
fn declared_language_strict(
    schema: &serde_json::Map<String, Value>,
) -> Option<Option<CodeLanguage>> {
    let Some(description) = schema.get("description").and_then(Value::as_str) else {
        return Some(None);
    };
    let Some(rest) = description.strip_prefix("Execute a program in `") else {
        return Some(None);
    };
    let Some(end) = rest.find('`') else {
        return Some(None);
    };
    match rest[..end].to_lowercase().as_str() {
        "typescript" => Some(Some(CodeLanguage::TypeScript)),
        "python" => Some(Some(CodeLanguage::Python)),
        _ => None,
    }
}

/// 解析一个轨迹工具单元。非 run_code、或参数里没有字符串 `code` → None
/// （上游 `codeProgram`）。
pub fn code_program(tool_name: &str, input_detail: Option<&str>, schema_detail: Option<&str>) -> Option<CodeProgram> {
    if tool_name != PTC_TOOL_NAME {
        return None;
    }
    let args = parse_record(input_detail)?;
    let source = args.get("code")?.as_str()?.to_string();
    // description 存在但非字符串 → 记录不可用
    let description = match args.get("description") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return None,
    };
    let description = match description {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            let line = source
                .split('\n')
                .map(|line| line.trim_end_matches('\r'))
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            line.trim().to_string()
        }
    };
    Some(CodeProgram {
        source,
        description,
        language: recorded_language(schema_detail),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cell_schema(value: serde_json::Value) -> String {
        value.to_string()
    }

    fn input() -> String {
        json!({ "code": "return 42\n", "description": "Compute the answer" }).to_string()
    }

    fn language(input: &str, schema: Option<&str>) -> Option<CodeLanguage> {
        code_program(PTC_TOOL_NAME, Some(input), schema).and_then(|program| program.language)
    }

    #[test]
    fn keeps_the_program_and_arguments_verbatim() {
        let program = code_program(PTC_TOOL_NAME, Some(&input()), None).unwrap();
        assert_eq!(program.source, "return 42\n");
        assert_eq!(program.description, "Compute the answer");
        assert_eq!(program.language, None);
    }

    #[test]
    fn non_run_code_or_bad_arguments_are_not_programs() {
        assert!(code_program("bash", Some(&input()), None).is_none());
        assert!(code_program(PTC_TOOL_NAME, None, None).is_none());
        assert!(code_program(PTC_TOOL_NAME, Some("{"), None).is_none());
        assert!(code_program(PTC_TOOL_NAME, Some("[]"), None).is_none());
        let no_code = json!({ "description": "x" }).to_string();
        assert!(code_program(PTC_TOOL_NAME, Some(&no_code), None).is_none());
        let numeric_code = json!({ "code": 42 }).to_string();
        assert!(code_program(PTC_TOOL_NAME, Some(&numeric_code), None).is_none());
        // description 存在但非字符串 → 记录不可用
        let bad_description = json!({ "code": "x", "description": 42 }).to_string();
        assert!(code_program(PTC_TOOL_NAME, Some(&bad_description), None).is_none());
    }

    #[test]
    fn parameter_description_hints() {
        let schema = |description: &str| {
            cell_schema(json!({ "parameters": { "properties": { "code": { "description": description } } } }))
        };
        let cases = [
            ("The program: the body of an async TypeScript function.", Some(CodeLanguage::TypeScript)),
            ("The program: the body of an async Python function.", Some(CodeLanguage::Python)),
            ("A TypeScript or Python program.", None),
            ("An executable program.", None),
        ];
        for (description, expected) in cases {
            assert_eq!(language(&input(), Some(&schema(description))), expected, "{description}");
        }
    }

    #[test]
    fn unusable_schemas_stay_plain() {
        for schema in ["{", "{}", "null", "[]"] {
            assert_eq!(language(&input(), Some(schema)), None, "{schema}");
        }
    }

    #[test]
    fn leading_tool_level_declaration_only() {
        let cases = [
            ("Execute a program in `typescript` against the available tools.", Some(CodeLanguage::TypeScript)),
            ("Execute a program in `python` against the available tools.", Some(CodeLanguage::Python)),
            ("Execute a program in `Python` against the available tools.", Some(CodeLanguage::Python)),
            ("Execute a program in `ruby` against the available tools.", None),
            // 正文里出现的另一种语言在参数提示之外，不参与判定
            ("Execute a program in `typescript` against the available tools. Example: call a Python tool.", Some(CodeLanguage::TypeScript)),
            ("Execute a program in `python` against the available tools. Example: call a TypeScript tool.", Some(CodeLanguage::Python)),
            // 声明了不认识的语言：整体不着色（不降级读参数提示）
            ("Execute a program in `ruby` against the available tools.", None),
            ("Execute a program. Examples use TypeScript and Python.", None),
            ("Execute a program against the available tools.", None),
            ("An example: Execute a program in `typescript`.", None),
            // 大小写敏感：小写 execute 不算声明
            ("execute a program in `typescript` against the available tools.", None),
        ];
        for (description, expected) in cases {
            let schema = cell_schema(json!({
                "description": description,
                "parameters": { "properties": { "code": {
                    "description": "The program to execute, following this tool's execution instructions."
                } } },
            }));
            assert_eq!(language(&input(), Some(&schema)), expected, "{description}");
        }
    }

    #[test]
    fn declaration_combines_with_the_legacy_hint() {
        let combine = |declared: &str, hint: &str| {
            let schema = cell_schema(json!({
                "description": format!("Execute a program in `{declared}` against the available tools."),
                "parameters": { "properties": { "code": { "description": hint } } },
            }));
            language(&input(), Some(&schema))
        };
        assert_eq!(combine("typescript", "A TypeScript program."), Some(CodeLanguage::TypeScript));
        assert_eq!(combine("python", "A Python program."), Some(CodeLanguage::Python));
        // 声明与提示取并集：两处冲突 → 不着色（上游表）
        assert_eq!(combine("typescript", "A Python program."), None);
        assert_eq!(combine("python", "A TypeScript program."), None);
        assert_eq!(combine("typescript", "A TypeScript or Python program."), None);
        assert_eq!(combine("python", "A TypeScript or Python program."), None);
        assert_eq!(combine("ruby", "A TypeScript program."), None);
        assert_eq!(combine("ruby", "A Python program."), None);
    }

    #[test]
    fn declaration_alone_suffices_without_a_parameter_hint() {
        for (language_name, expected) in [
            ("typescript", CodeLanguage::TypeScript),
            ("python", CodeLanguage::Python),
        ] {
            for description_value in [json!(null), json!(42)] {
                let mut code = json!({ "description": Value::Null });
                code["description"] = description_value.clone();
                let schema = cell_schema(json!({
                    "description": format!("Execute a program in `{language_name}` against the available tools."),
                    "parameters": { "properties": { "code": code } },
                }));
                assert_eq!(language(&input(), Some(&schema)), Some(expected), "{language_name} {description_value}");
            }
        }
    }

    #[test]
    fn legacy_hint_alone_survives_a_non_leading_tool_description() {
        for tool_description in [
            json!(null),
            json!(42),
            json!("Execute a TypeScript program against the available tools."),
        ] {
            let schema = cell_schema(json!({
                "description": tool_description.clone(),
                "parameters": { "properties": { "code": { "description": "A Python program." } } },
            }));
            assert_eq!(language(&input(), Some(&schema)), Some(CodeLanguage::Python), "{tool_description}");
        }
    }

    #[test]
    fn description_falls_back_to_the_first_nonempty_line() {
        let source = "\n  const x = 1\nreturn x\n";
        for description in [None, Some(""), Some("   ")] {
            let input = match description {
                None => json!({ "code": source }).to_string(),
                Some(text) => json!({ "code": source, "description": text }).to_string(),
            };
            let program = code_program(PTC_TOOL_NAME, Some(&input), None).unwrap();
            assert_eq!(program.description, "const x = 1");
            assert_eq!(program.source, source);
        }
        // 首行带 \r 的 CRLF 源码：尾随回车不进入描述
        let crlf = json!({ "code": "\r\n  return 1\r\n" }).to_string();
        let program = code_program(PTC_TOOL_NAME, Some(&crlf), None).unwrap();
        assert_eq!(program.description, "return 1");
    }

    #[test]
    fn ascii_word_boundaries_hold_for_the_hints() {
        assert!(word_present("A typescript program", "TypeScript"));
        assert!(!word_present("ATypeScript", "TypeScript"));
        assert!(!word_present("TypeScripts", "TypeScript"));
        assert!(word_present("TypeScript.", "TypeScript"));
        assert!(word_present("（Python）", "Python"));
    }
}