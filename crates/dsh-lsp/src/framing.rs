//! LSP base-protocol framing: `Content-Length`-delimited JSON-RPC over a byte stream.
//!
//! Mirrors [`packages/lsp/lsp-stdio/src/framing.ts`]: the encoder produces one framed
//! buffer; the decoder buffers incoming bytes and yields complete message bodies,
//! bounding the header and total message size so a hostile or broken server cannot
//! exhaust memory.

/// The header/body separator in the LSP base protocol.
pub const HEADER_SEPARATOR: &[u8] = b"\r\n\r\n";

/// Cap on the header section so a server that never sends the separator cannot grow the
/// buffer forever.
pub const MAX_HEADER_BYTES: usize = 1 << 16;

/// Encode one JSON-RPC message as a framed LSP buffer
/// (`Content-Length: N\r\n\r\n<utf-8 json>`).
pub fn encode_message(message: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("JSON-RPC message serializes");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// A streaming decoder for `Content-Length`-framed JSON-RPC.
///
/// Feed it stdout chunks; it returns any whole message bodies that completed. It parses
/// only the `Content-Length` header and ignores other headers (e.g. `Content-Type`),
/// matching the base protocol.
pub struct MessageDecoder {
    buffer: Vec<u8>,
    max_message_bytes: usize,
}

impl MessageDecoder {
    /// `max_message_bytes` rejects any single framed body larger than this (guards memory).
    pub fn new(max_message_bytes: usize) -> Self {
        Self { buffer: Vec::new(), max_message_bytes }
    }

    /// Append a chunk and return every message body that is now complete, in arrival
    /// order (possibly empty).
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<serde_json::Value>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            match self.next()? {
                None => break,
                Some(message) => messages.push(message),
            }
        }
        Ok(messages)
    }

    /// Parse and consume the next complete message, or report that more bytes are needed.
    fn next(&mut self) -> Result<Option<serde_json::Value>, String> {
        let separator = match find(&self.buffer, HEADER_SEPARATOR) {
            Some(index) => index,
            None => {
                if self.buffer.len() > MAX_HEADER_BYTES {
                    return Err(format!(
                        "LSP header exceeded {MAX_HEADER_BYTES} bytes without a terminator"
                    ));
                }
                return Ok(None);
            }
        };
        if separator > MAX_HEADER_BYTES {
            return Err(format!("LSP header exceeded {MAX_HEADER_BYTES} bytes"));
        }
        let header_text = String::from_utf8_lossy(&self.buffer[..separator]).into_owned();
        let content_length = parse_content_length(&header_text)?;
        if content_length > self.max_message_bytes {
            return Err(format!(
                "LSP message length {content_length} exceeds the {}-byte limit",
                self.max_message_bytes
            ));
        }
        let body_start = separator + HEADER_SEPARATOR.len();
        let body_end = body_start + content_length;
        if self.buffer.len() < body_end {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&self.buffer[body_start..body_end]).into_owned();
        self.buffer.drain(..body_end);
        match serde_json::from_str(&body) {
            Ok(message) => Ok(Some(message)),
            Err(error) => Err(format!("LSP message body was not valid JSON: {error}")),
        }
    }
}

/// Read the `Content-Length` header value (case-insensitive), rejecting a missing or
/// non-numeric one.
fn parse_content_length(header_text: &str) -> Result<usize, String> {
    for line in header_text.split("\r\n") {
        let Some(colon) = line.find(':') else { continue };
        if line[..colon].trim().to_lowercase() != "content-length" {
            continue;
        }
        let value = line[colon + 1..].trim();
        return match value.parse::<i64>() {
            Ok(number) if number >= 0 => Ok(number as usize),
            _ => Err(format!("invalid Content-Length header: {}", quote(line))),
        };
    }
    Err(format!(
        "LSP header block missing Content-Length: {}",
        quote(header_text)
    ))
}

/// `JSON.stringify`-equivalent quoting for header diagnostics.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

/// `Buffer.indexOf` equivalent over bytes.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}