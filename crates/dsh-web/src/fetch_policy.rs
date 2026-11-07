//! `web_fetch` 的纯策略层（对齐上游 `web-fetch-http/policy.ts` 与
//! `tool-web/fetch.ts` 的 `renderBody`）。
//!
//! 上游把这次调用拆成两半：**传输**（重定向、字节上限、超时）与**纯策略**
//! （URL 校验、content-type 分类、charset、HTML→文本、截断）。这里实现纯策略
//! 那一半，传输留在 `WebTool` 里用 reqwest 做。数值全照上游默认：
//! URL ≤ 2048、响应体 ≤ 5MB、正文 ≤ 100k 字符、超时 30s、同源重定向 ≤ 5 跳。

/// 请求 URL 最大长度（上游 `WEB_FETCH_MAX_URL_LENGTH`）。
pub const MAX_URL_LENGTH: usize = 2048;
/// 响应体字节上限（上游 `maxResponseBytes` 默认）。
pub const MAX_RESPONSE_BYTES: usize = 5_000_000;
/// 解码后正文字符上限（上游 `maxBodyChars` 默认）。
pub const MAX_BODY_CHARS: usize = 100_000;
/// 单次抓取超时（上游 `timeoutMs` 默认）。
pub const TIMEOUT_MS: u64 = 30_000;
/// 同源重定向跳数上限（上游 `maxRedirects` 默认）。
pub const MAX_REDIRECTS: usize = 5;
/// 产物截断时追加的说明（上游 `TRUNCATION_FOOTER`）。
pub const TRUNCATION_FOOTER: &str =
    "\n\n(Content truncated. Fetch a more specific URL or section for the full text.)";
/// HTML 无法安全转换时的替代文本（上游同款文案）。
pub const HTML_OMITTED: &str = "[HTML content omitted: unable to convert safely.]";

/// 可解码的正文种类（上游 `FetchableKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchableKind {
    Html,
    Text,
}

/// URL 校验失败的原因（上游 `WebError` 的码位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlRejection {
    /// 不是合法 URL 或超长。
    Invalid,
    /// 非 http/https。
    Scheme,
    /// URL 内嵌账号密码。
    Credentials,
}

impl UrlRejection {
    /// 面向模型的错误文本（照上游措辞）。
    pub fn message(self, input: &str) -> String {
        match self {
            UrlRejection::Invalid => format!("invalid URL: {input}"),
            UrlRejection::Scheme => {
                "unsupported URL scheme (only http and https are allowed)".to_string()
            }
            UrlRejection::Credentials => "credentials in URLs are not allowed".to_string(),
        }
    }
}

/// 校验请求 URL：长度上限 → scheme → 内嵌凭据。
pub fn validate_url(input: &str) -> Result<(), UrlRejection> {
    if input.len() > MAX_URL_LENGTH || !input.contains("://") {
        return Err(UrlRejection::Invalid);
    }
    let lower = input.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else {
        return Err(UrlRejection::Scheme);
    };
    // 主机部分（到第一个 '/' '?' '#'）里出现 '@' 即内嵌凭据
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest);
    if host.contains('@') {
        return Err(UrlRejection::Credentials);
    }
    Ok(())
}

/// 是否同源（scheme + host + port 全等）。跨源重定向要拒绝——上游要求
/// 每个新源都走一次新的工具调用。
pub fn same_origin(a: &str, b: &str) -> bool {
    fn origin(url: &str) -> Option<(String, String, u16)> {
        let lower = url.to_ascii_lowercase();
        let (scheme, rest) = if let Some(r) = lower.strip_prefix("https://") {
            ("https", r)
        } else if let Some(r) = lower.strip_prefix("http://") {
            ("http", r)
        } else {
            return None;
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (
                h.to_string(),
                p.parse().unwrap_or(default_port(scheme)),
            ),
            _ => (authority.to_string(), default_port(scheme)),
        };
        Some((scheme.to_string(), host, port))
    }
    match (origin(a), origin(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// content-type 分类（上游 `classifyContentType`）。
pub fn classify_content_type(content_type: Option<&str>) -> Option<FetchableKind> {
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return Some(FetchableKind::Html);
    }
    if mime.starts_with("text/") {
        return Some(FetchableKind::Text);
    }
    if mime == "application/json"
        || mime == "application/xml"
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
    {
        return Some(FetchableKind::Text);
    }
    None
}

/// 从 content-type 里取 charset（上游 `parseCharset`），缺省 UTF-8。
pub fn parse_charset(content_type: Option<&str>) -> Option<String> {
    let raw = content_type?;
    for part in raw.split(';').skip(1) {
        let part = part.trim();
        if let Some(value) = part
            .strip_prefix("charset=")
            .or_else(|| part.strip_prefix("charset ="))
            .or_else(|| {
                let lower = part.to_ascii_lowercase();
                lower.starts_with("charset") .then(|| part.split_once('=').map(|(_, v)| v)).flatten()
            })
        {
            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// 截断到字符上限，返回（正文, 是否截断）。
pub fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => (text[..idx].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// HTML → 纯文本。上游用 turndown 转 markdown（含 GFM 表格）；rustdsh 不引
/// DOM 依赖，这里做保守的块级文本抽取：
/// - 丢弃 script/style/noscript/template/iframe/object/embed 及其内容；
/// - 块级标签（p/div/br/li/h1-h6/tr/table/section/article…）断行；
/// - `<td>`/`<th>` 之间用 ` | ` 分隔（保留表格可读性）；
/// - 其余标签剥掉、实体解码最常见的几种、连续空白压缩、行首尾去空白。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    let mut depth_skip = 0usize;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt], depth_skip > 0);
        rest = &rest[lt..];
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        let self_closing = tag.ends_with('/');
        match name.as_str() {
            "script" | "style" | "noscript" | "template" | "iframe" | "object" | "embed" => {
                if closing {
                    depth_skip = depth_skip.saturating_sub(1);
                } else if !self_closing {
                    depth_skip += 1;
                }
            }
            "br" => out.push('\n'),
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "ul" | "ol"
            | "table" | "thead" | "tbody" | "tr" | "blockquote" | "pre" | "figure" | "hr" => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            "td" | "th" => {
                // 同行单元格之间用 ` | ` 分隔：`<td>` 开始标签前若已写过
                // 单元格内容且当前不在行首，插入分隔符。
                if !closing && !out.ends_with('\n') && !out.trim_end().is_empty() {
                    out.push_str(" | ");
                }
            }
            _ => {}
        }
        if closing
            && matches!(
                name.as_str(),
                "p" | "div" | "section" | "article" | "li" | "tr" | "h1" | "h2" | "h3" | "h4"
                    | "h5" | "h6" | "blockquote" | "pre"
            )
        {
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    if depth_skip == 0 {
        push_text(&mut out, rest, false);
    }
    normalize_blank_lines(&decode_entities(&out))
}

fn push_text(out: &mut String, raw: &str, skip: bool) {
    if skip || raw.is_empty() {
        return;
    }
    out.push_str(raw);
}

/// 解码最常见的 HTML 实体（完整表需要实体词典；这里覆盖正文里常见的）。
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded: Option<char> = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "lsquo" => Some('‘'),
            "rsquo" => Some('’'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .and_then(char::from_u32)
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                        .and_then(char::from_u32)
                }),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 行内连续空白压成一个空格、行首尾去空白、连续空行压成一行。
fn normalize_blank_lines(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if lines.last().is_some_and(|l: &String| !l.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(collapsed);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// 组装面向模型的抓取结果：头部（URL/状态）+ 正文 + 截断说明。
pub fn render_fetch_output(url: &str, status: u16, kind: FetchableKind, body: &str, truncated: bool) -> String {
    let text = match kind {
        FetchableKind::Html => html_to_text(body),
        FetchableKind::Text => body.to_string(),
    };
    let (bounded, clipped) = truncate_chars(&text, MAX_BODY_CHARS);
    let mut out = format!("URL: {url}\nStatus: {status}\n\n{bounded}");
    if truncated || clipped {
        out.push_str(TRUNCATION_FOOTER);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation_covers_the_upstream_rejections() {
        assert_eq!(validate_url("https://example.com/a"), Ok(()));
        assert_eq!(validate_url("http://example.com"), Ok(()));
        assert_eq!(validate_url("ftp://example.com"), Err(UrlRejection::Scheme));
        assert_eq!(validate_url("file:///etc/passwd"), Err(UrlRejection::Scheme));
        assert_eq!(validate_url("not a url"), Err(UrlRejection::Invalid));
        assert_eq!(
            validate_url("https://user:pw@example.com/"),
            Err(UrlRejection::Credentials)
        );
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH));
        assert_eq!(validate_url(&long), Err(UrlRejection::Invalid));
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin("https://a.com/x", "https://a.com/y"));
        assert!(same_origin("https://a.com/x", "https://a.com:443/y"));
        assert!(!same_origin("https://a.com/x", "http://a.com/x"));
        assert!(!same_origin("https://a.com/x", "https://b.com/x"));
        assert!(!same_origin("https://a.com/x", "https://a.com:8443/x"));
    }

    #[test]
    fn content_types_classify_like_upstream() {
        assert_eq!(classify_content_type(Some("text/html; charset=utf-8")), Some(FetchableKind::Html));
        assert_eq!(classify_content_type(Some("application/xhtml+xml")), Some(FetchableKind::Html));
        assert_eq!(classify_content_type(Some("text/plain")), Some(FetchableKind::Text));
        assert_eq!(classify_content_type(Some("application/json")), Some(FetchableKind::Text));
        assert_eq!(classify_content_type(Some("application/ld+json")), Some(FetchableKind::Text));
        assert_eq!(classify_content_type(Some("image/png")), None);
        assert_eq!(classify_content_type(None), None);
        assert_eq!(parse_charset(Some("text/html; charset=\"GBK\"")), Some("gbk".into()));
        assert_eq!(parse_charset(Some("text/html")), None);
    }

    #[test]
    fn html_conversion_drops_scripts_and_keeps_readable_blocks() {
        let html = r#"<html><head><style>.a{}</style><script>var x = "<b>"</script></head>
        <body><h1>Title</h1><p>Hello &amp; welcome.</p><div>Second&nbsp;line</div>
        <table><tr><td>A</td><td>B</td></tr></table></body></html>"#;
        let text = html_to_text(html);
        assert!(!text.contains("var x"), "script content must be dropped: {text}");
        assert!(!text.contains(".a{}"), "style content must be dropped: {text}");
        assert!(text.contains("Title"));
        assert!(text.contains("Hello & welcome."));
        assert!(text.contains("Second line"));
        assert!(text.contains("A | B"));
    }

    #[test]
    fn output_carries_url_status_and_truncation_footer() {
        let out = render_fetch_output("https://a.com", 200, FetchableKind::Text, "body", false);
        assert!(out.starts_with("URL: https://a.com\nStatus: 200"));
        assert!(!out.contains("Content truncated"));
        let clipped = render_fetch_output("https://a.com", 200, FetchableKind::Text, &"x".repeat(MAX_BODY_CHARS + 10), false);
        assert!(clipped.ends_with(TRUNCATION_FOOTER));
        // 截断按字符不是字节（多字节安全）
        let (t, cut) = truncate_chars("中文内容", 2);
        assert_eq!(t, "中文");
        assert!(cut);
    }
}

#[cfg(test)]
mod redirect_tests {
    use super::*;

    #[test]
    fn relative_locations_resolve_against_the_current_origin() {
        assert_eq!(
            crate::resolve_location("https://a.com/x/y", "/z"),
            "https://a.com/z"
        );
        assert_eq!(
            crate::resolve_location("https://a.com/x/y", "https://b.com/q"),
            "https://b.com/q"
        );
        assert_eq!(
            crate::resolve_location("https://a.com", "next"),
            "https://a.com/next"
        );
    }

    #[test]
    fn decoded_bodies_are_lossy_utf8_never_an_error() {
        // 非法 UTF-8 字节：有损替换（上游只在「畸形 charset 声明」时才 fatal）
        let bytes = b"ok \xff\xfe end";
        let text = crate::decode_body(bytes, Some("text/plain; charset=utf-8")).unwrap();
        assert!(text.starts_with("ok "));
        assert!(text.ends_with(" end"));
        assert!(text.contains('\u{fffd}'));
        assert_eq!(crate::decode_body(b"plain", None).unwrap(), "plain");
    }
}
