//! Exact model-facing rendering: coordinate conversion, location lines, caps, URI
//! resolution. Every expected string here is the upstream literal.

use dsh_lsp::render::{
    format_hover, format_locations, operation_list, parse_lsp_args, present_lsp_call, render_uri,
    DEFAULT_MAX_LOCATIONS, DEFAULT_MAX_RESULT_CHARS,
};
use dsh_lsp::types::{LspHover, LspLocation, LspOperation, LspPosition, LspRange};

fn location(uri: &str, line: u32, character: u32) -> LspLocation {
    LspLocation {
        uri: uri.to_string(),
        range: LspRange {
            start: LspPosition { line, character },
            end: LspPosition { line: line + 1, character: 0 },
        },
    }
}

#[test]
fn defaults_match_upstream() {
    assert_eq!(DEFAULT_MAX_LOCATIONS, 100);
    assert_eq!(DEFAULT_MAX_RESULT_CHARS, 16_000);
    assert_eq!(
        operation_list(),
        "goToDefinition, findReferences, goToImplementation, hover"
    );
}

#[test]
fn converts_one_based_cursor_to_zero_based() {
    let input = parse_lsp_args("goToDefinition", "src/a.rs", 3.0, 7.0).unwrap();
    assert_eq!(input.operation, LspOperation::GoToDefinition);
    assert_eq!(input.file_path, "src/a.rs");
    assert_eq!(input.position, LspPosition { line: 2, character: 6 });
}

#[test]
fn rejects_bad_arguments_with_upstream_messages() {
    assert_eq!(
        parse_lsp_args("nope", "a.rs", 1.0, 1.0).unwrap_err(),
        "operation must be one of goToDefinition, findReferences, goToImplementation, hover"
    );
    assert_eq!(
        parse_lsp_args("hover", "   ", 1.0, 1.0).unwrap_err(),
        "file_path must be a non-empty string"
    );
    assert_eq!(
        parse_lsp_args("hover", "a.rs", 0.0, 1.0).unwrap_err(),
        "line must be a positive integer (one-based)"
    );
    assert_eq!(
        parse_lsp_args("hover", "a.rs", 1.0, 1.5).unwrap_err(),
        "character must be a positive integer (one-based)"
    );
}

#[test]
fn renders_no_results() {
    assert_eq!(format_locations(&[], "file:///c:/ws", 100, 16_000), "No results.");
}

#[test]
fn renders_one_based_location_lines_workspace_relative() {
    let locations = vec![
        location("file:///c:/ws/src/a.ts", 0, 1),
        location("file:///c:/ws/src/b.ts", 9, 0),
        location("file:///c:/ws/src/a.ts", 4, 2),
    ];
    assert_eq!(
        format_locations(&locations, "file:///c:/ws", 100, 16_000),
        // Grouped by path in first-seen order, one-based.
        "src/a.ts:1:2\nsrc/a.ts:5:3\nsrc/b.ts:10:1"
    );
}

#[test]
fn renders_outside_and_non_file_uris() {
    let locations = vec![
        location("file:///c:/other/b.ts", 1, 1),
        location("untitled:Untitled-1", 0, 0),
    ];
    assert_eq!(
        format_locations(&locations, "file:///c:/ws", 100, 16_000),
        "c:/other/b.ts:2:2\nuntitled:Untitled-1:1:1"
    );
}

#[test]
fn appends_singular_and_plural_omission_markers() {
    let two = vec![location("file:///c:/ws/a.ts", 0, 0), location("file:///c:/ws/b.ts", 0, 0)];
    let three = vec![
        location("file:///c:/ws/a.ts", 0, 0),
        location("file:///c:/ws/b.ts", 0, 0),
        location("file:///c:/ws/c.ts", 0, 0),
    ];
    assert_eq!(
        format_locations(&two, "file:///c:/ws", 1, 16_000),
        "a.ts:1:1\n… 1 more location omitted (limit 1)."
    );
    assert_eq!(
        format_locations(&three, "file:///c:/ws", 1, 16_000),
        "a.ts:1:1\n… 2 more locations omitted (limit 1)."
    );
}

#[test]
fn renders_hover_absence_and_content() {
    assert_eq!(format_hover(None, 16_000), "No hover information.");
    let hover = LspHover { contents: "fn main()".to_string(), range: None };
    assert_eq!(format_hover(Some(&hover), 16_000), "fn main()");
}

#[test]
fn truncation_keeps_the_marker_inside_the_cap() {
    let hover = LspHover { contents: "x".repeat(200), range: None };
    let rendered = format_hover(Some(&hover), 48);
    assert_eq!(rendered.chars().count(), 48);
    assert!(rendered.ends_with("\n… hover truncated (limit 48 characters)."), "{rendered}");
}

#[test]
fn truncation_falls_back_to_the_capped_notice() {
    let hover = LspHover { contents: "abcdef".to_string(), range: None };
    // The notice alone is longer than the cap, so the result is its prefix.
    assert_eq!(format_hover(Some(&hover), 5), "\n… ho");
}

#[test]
fn resolves_file_uris_against_the_workspace() {
    let workspace = "file:///c:/ws";
    assert_eq!(render_uri("file:///c:/ws/src/a.ts", workspace), "src/a.ts");
    assert_eq!(render_uri("file:///c:/ws", workspace), ".");
    // A sibling sharing the prefix is outside, not relative.
    assert_eq!(render_uri("file:///c:/wsx/a.ts", workspace), "c:/wsx/a.ts");
    assert_eq!(render_uri("file:///c:/other/b.ts", workspace), "c:/other/b.ts");
}

#[test]
fn keeps_non_file_and_malformed_uris_verbatim() {
    let workspace = "file:///c:/ws";
    assert_eq!(render_uri("untitled:Untitled-1", workspace), "untitled:Untitled-1");
    assert_eq!(render_uri("file:%zz", workspace), "file:%zz");
    // A non-file workspace cannot relativize anything.
    assert_eq!(render_uri("file:///c:/ws/a.ts", "https://example.com"), "file:///c:/ws/a.ts");
}

#[test]
fn resolves_posix_world_uris() {
    let workspace = "file:///home/u/proj";
    assert_eq!(render_uri("file:///home/u/proj/src/a.rs", workspace), "src/a.rs");
    assert_eq!(render_uri("file:///home/u/other/b.rs", workspace), "/home/u/other/b.rs");
    // Percent-encoded spaces decode into the display path.
    assert_eq!(render_uri("file:///home/u/proj/a%20b.rs", workspace), "a b.rs");
}

#[test]
fn presents_a_generic_search_card() {
    let presentation = present_lsp_call("hover", "src/a.rs", 3.0, 7.0);
    assert_eq!(presentation["card"], "generic");
    assert_eq!(presentation["kind"], "search");
    assert_eq!(presentation["title"], "LSP hover src/a.rs:3:7");
    assert_eq!(presentation["locations"][0]["path"], "src/a.rs");
    assert_eq!(presentation["locations"][0]["line"], 3);
}