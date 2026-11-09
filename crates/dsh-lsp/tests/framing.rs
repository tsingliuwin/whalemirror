//! Framing round-trips and hardening, against the upstream base-protocol behaviour.

use dsh_lsp::framing::{encode_message, MessageDecoder, MAX_HEADER_BYTES};
use serde_json::json;

fn decoder() -> MessageDecoder {
    MessageDecoder::new(16_000_000)
}

#[test]
fn encodes_content_length_then_body() {
    let frame = encode_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
    let text = String::from_utf8(frame.clone()).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    assert_eq!(header, format!("Content-Length: {}", body.len()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})
    );
}

#[test]
fn decodes_one_message() {
    let mut decoder = decoder();
    let frame = encode_message(&json!({"id": 7}));
    let messages = decoder.push(&frame).unwrap();
    assert_eq!(messages, vec![json!({"id": 7})]);
}

#[test]
fn decodes_two_messages_in_one_chunk() {
    let mut decoder = decoder();
    let mut chunk = encode_message(&json!({"id": 1}));
    chunk.extend(encode_message(&json!({"id": 2})));
    assert_eq!(decoder.push(&chunk).unwrap(), vec![json!({"id": 1}), json!({"id": 2})]);
}

#[test]
fn waits_for_split_chunks() {
    let mut decoder = decoder();
    let frame = encode_message(&json!({"id": 11, "result": "complete"}));
    // Split inside the header, then mid-body, then the tail.
    assert!(decoder.push(&frame[..8]).unwrap().is_empty());
    assert!(decoder.push(&frame[8..24]).unwrap().is_empty());
    let messages = decoder.push(&frame[24..]).unwrap();
    assert_eq!(messages, vec![json!({"id": 11, "result": "complete"})]);
}

#[test]
fn ignores_other_headers_and_header_case() {
    let mut decoder = decoder();
    let body = br#"{"id":3}"#;
    let mut frame =
        format!("content-LENGTH: {}\r\nContent-Type: application/json\r\n\r\n", body.len())
            .into_bytes();
    frame.extend_from_slice(body);
    assert_eq!(decoder.push(&frame).unwrap(), vec![json!({"id": 3})]);
}

#[test]
fn rejects_oversized_body() {
    let mut decoder = MessageDecoder::new(4);
    let frame = encode_message(&json!({"id": 5}));
    let error = decoder.push(&frame).unwrap_err();
    assert!(error.starts_with("LSP message length "), "{error}");
    assert!(error.ends_with("exceeds the 4-byte limit"), "{error}");
}

#[test]
fn rejects_unterminated_header_past_the_cap() {
    let mut decoder = decoder();
    let error = decoder
        .push(&vec![b'X'; MAX_HEADER_BYTES + 1])
        .unwrap_err();
    assert_eq!(
        error,
        format!("LSP header exceeded {MAX_HEADER_BYTES} bytes without a terminator")
    );
}

#[test]
fn rejects_missing_content_length() {
    let mut decoder = decoder();
    let error = decoder.push(b"Content-Type: application/json\r\n\r\n{}").unwrap_err();
    assert!(error.starts_with("LSP header block missing Content-Length: "), "{error}");
}

#[test]
fn rejects_non_numeric_content_length() {
    let mut decoder = decoder();
    let error = decoder.push(b"Content-Length: abc\r\n\r\n{}").unwrap_err();
    assert_eq!(error, "invalid Content-Length header: \"Content-Length: abc\"");
}

#[test]
fn rejects_invalid_json_body() {
    let mut decoder = decoder();
    let error = decoder.push(b"Content-Length: 1\r\n\r\n{").unwrap_err();
    assert!(error.starts_with("LSP message body was not valid JSON: "), "{error}");
}