//! 翻译层的纯函数契约：能力判定、位置编码协商、导航与 hover 归一。
//!
//! 逐条对应上游 `lsp-stdio/tests/translate.spec.ts` 的用例，重点是**错误文案逐字**
//! 与「`null`（没有结果）不等于缺 `result` 字段（协议错误）」这两条边界。

use dsh_lsp::translate::{
    negotiate_position_encoding, normalize_hover, normalize_locations, supports_operation,
    supports_transient_open,
};
use dsh_lsp::{LspOperation, LspPosition, LspRange};
use serde_json::json;

#[test]
fn operation_capabilities_accept_true_and_options_object_only() {
    let capabilities = json!({
        "definitionProvider": true,
        "referencesProvider": { "workDoneProgress": true },
        "implementationProvider": false,
        "hoverProvider": null,
    });
    assert!(supports_operation(&capabilities, LspOperation::GoToDefinition));
    assert!(supports_operation(&capabilities, LspOperation::FindReferences));
    assert!(!supports_operation(&capabilities, LspOperation::GoToImplementation));
    // 上游 `supportsCapability` 只把「缺省」与布尔 `false` 判为不支持；`null` 走
    // `typeof !== 'boolean'` 分支，与对象一样算支持（这里如实复刻该语义）。
    assert!(supports_operation(&capabilities, LspOperation::Hover));
    assert!(!supports_operation(&json!({}), LspOperation::GoToDefinition));
}

#[test]
fn transient_open_follows_enum_and_options_forms() {
    assert!(!supports_transient_open(None));
    assert!(!supports_transient_open(Some(&json!(0))));
    assert!(supports_transient_open(Some(&json!(1))));
    assert!(supports_transient_open(Some(&json!(2))));
    assert!(!supports_transient_open(Some(&json!(3))));
    // 选项形态必须显式 openClose: true——缺省按协议的 false 处理。
    assert!(supports_transient_open(Some(&json!({ "openClose": true, "change": 1 }))));
    assert!(!supports_transient_open(Some(&json!({ "openClose": false }))));
    assert!(!supports_transient_open(Some(&json!({ "change": 1 }))));
}

#[test]
fn position_encoding_negotiation_rejects_non_utf16() {
    assert!(negotiate_position_encoding(None).is_ok());
    assert!(negotiate_position_encoding(Some("utf-16")).is_ok());
    let error = negotiate_position_encoding(Some("utf-8")).expect_err("utf-8 不支持");
    assert_eq!(error.code(), "LSP_MALFORMED_RESPONSE");
    assert_eq!(
        error.message,
        "server negotiated unsupported position encoding \"utf-8\"; this host requires utf-16"
    );
}

#[test]
fn null_navigation_result_is_empty_but_missing_result_is_an_error() {
    assert_eq!(normalize_locations(Some(&json!(null))).unwrap(), Vec::new());
    let error = normalize_locations(None).expect_err("缺 result 字段要报错");
    assert_eq!(error.code(), "LSP_MALFORMED_RESPONSE");
    assert_eq!(error.message, "LSP navigation result was missing");
}

#[test]
fn single_location_object_and_array_both_normalize() {
    let location = json!({
        "uri": "file:///w/a.ts",
        "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } },
    });
    let expected = vec![dsh_lsp::LspLocation {
        uri: "file:///w/a.ts".into(),
        range: LspRange {
            start: LspPosition { line: 1, character: 2 },
            end: LspPosition { line: 1, character: 5 },
        },
    }];
    assert_eq!(normalize_locations(Some(&location)).unwrap(), expected);
    assert_eq!(normalize_locations(Some(&json!([location]))).unwrap(), expected);
}

#[test]
fn location_link_uses_target_uri_and_selection_range() {
    let link = json!({
        "targetUri": "file:///w/b.ts",
        "targetRange": {
            "start": { "line": 0, "character": 0 },
            "end": { "line": 9, "character": 0 },
        },
        "targetSelectionRange": {
            "start": { "line": 3, "character": 7 },
            "end": { "line": 3, "character": 9 },
        },
    });
    let locations = normalize_locations(Some(&link)).unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].uri, "file:///w/b.ts");
    assert_eq!(locations[0].range.start, LspPosition { line: 3, character: 7 });
    assert_eq!(locations[0].range.end, LspPosition { line: 3, character: 9 });
}

#[test]
fn malformed_navigation_entries_are_rejected_with_exact_messages() {
    let error = normalize_locations(Some(&json!([1]))).expect_err("非对象条目");
    assert_eq!(error.message, "LSP navigation result contained a non-object entry");

    let error = normalize_locations(Some(&json!([{ "uri": "file:///w/a.ts" }])))
        .expect_err("既不是 Location 也不是 LocationLink");
    assert_eq!(
        error.message,
        "LSP navigation result contained neither a Location nor a LocationLink"
    );

    // 负坐标不是合法协议坐标。
    let negative = json!({
        "uri": "file:///w/a.ts",
        "range": { "start": { "line": -1, "character": 0 }, "end": { "line": 0, "character": 0 } },
    });
    let error = normalize_locations(Some(&negative)).expect_err("负坐标要拒绝");
    assert_eq!(
        error.message,
        "LSP navigation result contained neither a Location nor a LocationLink"
    );
}

#[test]
fn hover_accepts_all_three_contents_encodings() {
    let markup = normalize_hover(Some(&json!({
        "contents": { "kind": "markdown", "value": "**bold**" },
    })))
    .unwrap()
    .expect("有 hover");
    assert_eq!(markup.contents, "**bold**");
    assert_eq!(markup.range, None);

    let plain = normalize_hover(Some(&json!({ "contents": "plain text" })))
        .unwrap()
        .expect("有 hover");
    assert_eq!(plain.contents, "plain text");

    let fenced = normalize_hover(Some(&json!({
        "contents": { "language": "rust", "value": "fn main() {}" },
    })))
    .unwrap()
    .expect("有 hover");
    assert_eq!(fenced.contents, "```rust\nfn main() {}\n```");

    // 数组用空行连接，元素各自的编码都保留。
    let joined = normalize_hover(Some(&json!({
        "contents": ["first", { "language": "ts", "value": "let a = 1" }],
    })))
    .unwrap()
    .expect("有 hover");
    assert_eq!(joined.contents, "first\n\n```ts\nlet a = 1\n```");
}

#[test]
fn hover_range_is_preserved_and_validated() {
    let hovered = normalize_hover(Some(&json!({
        "contents": "body",
        "range": {
            "start": { "line": 4, "character": 1 },
            "end": { "line": 4, "character": 6 },
        },
    })))
    .unwrap()
    .expect("有 hover");
    let range = hovered.range.expect("带 range");
    assert_eq!(range.start, LspPosition { line: 4, character: 1 });
    assert_eq!(range.end, LspPosition { line: 4, character: 6 });

    let error = normalize_hover(Some(&json!({
        "contents": "body",
        "range": { "start": { "line": 0 } },
    })))
    .expect_err("range 结构不完整要报错");
    assert_eq!(error.message, "LSP hover result contained a malformed range");
}

#[test]
fn empty_hover_content_is_no_hover_and_short_circuits_range_checks() {
    // 空内容先返回 None：哪怕 range 是坏的也不校验（与上游顺序一致）。
    let none = normalize_hover(Some(&json!({
        "contents": { "kind": "markdown", "value": "" },
        "range": { "start": { "line": 0 } },
    })))
    .unwrap();
    assert_eq!(none, None);
}

#[test]
fn malformed_hover_payloads_report_exact_messages() {
    let error = normalize_hover(None).expect_err("缺 result 字段");
    assert_eq!(error.message, "LSP hover result was missing");

    let error = normalize_hover(Some(&json!("text"))).expect_err("非对象");
    assert_eq!(error.message, "LSP hover result was not an object");

    let error = normalize_hover(Some(&json!({}))).expect_err("没有 contents");
    assert_eq!(error.message, "LSP hover result had no contents");

    let error = normalize_hover(Some(&json!({
        "contents": { "kind": "markdown", "value": 7 },
    })))
    .expect_err("MarkupContent.value 不是字符串");
    assert_eq!(error.message, "LSP hover MarkupContent value was not a string");

    let error = normalize_hover(Some(&json!({ "contents": [42] })))
        .expect_err("数组元素不是 MarkedString");
    assert_eq!(
        error.message,
        "LSP hover contents contained a malformed MarkedString"
    );

    let error = normalize_hover(Some(&json!({ "contents": { "foo": "bar" } })))
        .expect_err("既不是 MarkupContent 也不是 MarkedString");
    assert_eq!(
        error.message,
        "LSP hover contents were not MarkupContent, MarkedString, or an array"
    );
}