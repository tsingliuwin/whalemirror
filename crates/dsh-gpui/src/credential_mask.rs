//! 用户文本凭据掩码（上游 ui-secret-mask 的 Rust 等价核心，#32）。
//!
//! 发送进会话的用户文本可能携带凭据（模型 API key、云密钥、令牌）。
//! 上游在渲染层按「已发布格式」识别并掩码显示：值本身原样入列（智能体
//! 仍可使用），仅显示替换为 `*`，防肩窥/截屏泄漏。格式以字面量前缀 +
//! 字符类 run 识别（credential-formats 表），另识别私钥块与
//! `scheme://user:password@` 地址密码段。
//!
//! 本实现为语义等价的简化版：线性扫描（无 memo 表——聊天文本量级下
//! 一次 O(n×格式数) 足够），掩码显示形式对齐上游（前缀键保留前 8 字符、
//! 私钥块保留 BEGIN/END 行、地址密码整段掩码）。

/// 字符类（ASCII 位标志，对齐上游 classes 表）。
mod classes {
    pub const KEY: u16 = 1; // [A-Za-z0-9_-]
    pub const ALNUM: u16 = 2; // [A-Za-z0-9]
    pub const WORD: u16 = 4; // \w
    pub const ALNUM_DASH: u16 = 8; // [A-Za-z0-9-]
    pub const UPPER_ALNUM: u16 = 16; // [A-Z0-9]
    pub const LOWER: u16 = 128; // [a-z]
}

/// 字面量种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredKind {
    /// `-----BEGIN` 开头的私钥块。
    KeyBlock,
    /// 携带凭据的地址段（scheme://user:password，不含 @ 后主机）。
    Address,
    /// 固定前缀后的 key/token。
    Prefixed,
}

/// run 形状（前缀字面量之后 run 的校验方式，对齐上游 shapes 表）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// 整段 run，长度 ≥ min。
    Run,
    /// 长度恰为 min（GOOGLE）。
    Exact,
    /// min ≤ 长度 ≤ max，且 run 后一字符不在同类（BOUNDED 的 \b）。
    Bounded,
    /// 长度 ≥ min，且 run 前 256 字符内含 ASCII 数字（SK）。
    Sk,
    /// 三段 KEY run 以 `.` 相连，总长 ≤ max（JWT）。
    Jwt,
}

struct FormatSpec {
    literal: &'static str,
    kind: CredKind,
    run_class: u16,
    min: usize,
    max: usize,
    shape: Shape,
}

const fn prefixed(literal: &'static str, run_class: u16, min: usize) -> FormatSpec {
    FormatSpec { literal, kind: CredKind::Prefixed, run_class, min, max: 0, shape: Shape::Run }
}

/// 全部已识别格式（对齐上游 CREDENTIAL_FORMATS 表，逐项一致）。
const FORMATS: &[FormatSpec] = &[
    FormatSpec { literal: "-----BEGIN ", kind: CredKind::KeyBlock, run_class: 0, min: 0, max: 0, shape: Shape::Run },
    // 模型服务：sk- 键的 run 必含数字（连字符标识符鲜有数字）。
    FormatSpec { literal: "sk-", kind: CredKind::Prefixed, run_class: classes::KEY, min: 20, max: 0, shape: Shape::Sk },
    FormatSpec { literal: "AIza", kind: CredKind::Prefixed, run_class: classes::KEY, min: 35, max: 0, shape: Shape::Exact },
    // 代码托管与包仓库。
    prefixed("ghp_", classes::ALNUM, 36),
    prefixed("gho_", classes::ALNUM, 36),
    prefixed("ghu_", classes::ALNUM, 36),
    prefixed("ghs_", classes::ALNUM, 36),
    prefixed("ghr_", classes::ALNUM, 36),
    prefixed("github_pat_", classes::WORD, 22),
    prefixed("glpat-", classes::KEY, 20),
    prefixed("npm_", classes::ALNUM, 36),
    prefixed("hf_", classes::ALNUM, 30),
    // 云访问密钥 ID（有界 + 词边界）。
    FormatSpec { literal: "AKIA", kind: CredKind::Prefixed, run_class: classes::UPPER_ALNUM, min: 16, max: 16, shape: Shape::Bounded },
    FormatSpec { literal: "ASIA", kind: CredKind::Prefixed, run_class: classes::UPPER_ALNUM, min: 16, max: 16, shape: Shape::Bounded },
    FormatSpec { literal: "LTAI", kind: CredKind::Prefixed, run_class: classes::ALNUM, min: 12, max: 20, shape: Shape::Bounded },
    FormatSpec { literal: "AKID", kind: CredKind::Prefixed, run_class: classes::ALNUM, min: 28, max: 36, shape: Shape::Bounded },
    // 协作与支付服务。
    prefixed("xoxa-", classes::ALNUM_DASH, 10),
    prefixed("xoxb-", classes::ALNUM_DASH, 10),
    prefixed("xoxp-", classes::ALNUM_DASH, 10),
    prefixed("xoxr-", classes::ALNUM_DASH, 10),
    prefixed("xoxs-", classes::ALNUM_DASH, 10),
    prefixed("sk_live_", classes::ALNUM, 16),
    prefixed("sk_test_", classes::ALNUM, 16),
    prefixed("rk_live_", classes::ALNUM, 16),
    prefixed("rk_test_", classes::ALNUM, 16),
    prefixed("whsec_", classes::ALNUM, 24),
    // JWT：三段 base64url 以 `.` 相连，头段以 eyJ 开始。
    FormatSpec { literal: "eyJ", kind: CredKind::Prefixed, run_class: classes::KEY, min: 8, max: 2048, shape: Shape::Jwt },
];

fn char_class(b: u8) -> u16 {
    match b {
        b'a'..=b'z' => classes::KEY | classes::ALNUM | classes::WORD | classes::ALNUM_DASH | classes::LOWER,
        b'A'..=b'Z' => classes::KEY | classes::ALNUM | classes::WORD | classes::ALNUM_DASH | classes::UPPER_ALNUM,
        b'0'..=b'9' => classes::KEY | classes::ALNUM | classes::WORD | classes::ALNUM_DASH | classes::UPPER_ALNUM,
        b'_' => classes::KEY | classes::WORD,
        b'-' => classes::ALNUM_DASH,
        _ => 0,
    }
}

fn run_end(text: &str, start: usize, class: u16) -> usize {
    start + text.as_bytes()[start..]
        .iter()
        .take_while(|&&b| char_class(b) & class != 0)
        .count()
}

fn next_char_boundary(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// 一个凭据区间（字节偏移，均为 char 边界）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredSpan {
    pub start: usize,
    pub end: usize,
    pub kind: CredKind,
}

/// 从 `from` 起首个凭据区间；同位置重叠候选取格式表中先者。
pub fn find_credential(text: &str, from: usize) -> Option<CredSpan> {
    let bytes = text.as_bytes();
    let mut i = from.min(text.len());
    while i < text.len() {
        if text[i..].starts_with("-----BEGIN ") {
            if let Some(end) = key_block_end(text, i) {
                return Some(CredSpan { start: i, end, kind: CredKind::KeyBlock });
            }
        } else if bytes[i] == b':' && text[i..].starts_with("://") {
            if let Some((s, e)) = address_password(text, i) {
                return Some(CredSpan { start: s, end: e, kind: CredKind::Address });
            }
        } else {
            let rest = &text[i..];
            for spec in FORMATS {
                if !rest.starts_with(spec.literal) {
                    continue;
                }
                let run_start = i + spec.literal.len();
                let run_end = run_end(text, run_start, spec.run_class);
                let run_len = run_end - run_start;
                let mut span_end = run_end;
                let ok = match spec.shape {
                    Shape::Run => run_len >= spec.min,
                    Shape::Exact => run_len == spec.min,
                    Shape::Bounded => {
                        run_len >= spec.min
                            && run_len <= spec.max
                            && (run_end >= text.len()
                                || char_class(bytes[run_end]) & spec.run_class == 0)
                    }
                    Shape::Sk => {
                        run_len >= spec.min
                            && text.as_bytes()[run_start..run_end]
                                .iter()
                                .take(256)
                                .any(|b| b.is_ascii_digit())
                    }
                    // JWT 的凭据跨三段：span 覆盖到尾段结束。
                    Shape::Jwt => {
                        let seg = |t: &str| !t.is_empty() && t.bytes().all(|b| char_class(b) & classes::KEY != 0);
                        let mut parts = text[run_start..].splitn(3, '.');
                        let (p1, p2, p3) = (parts.next(), parts.next(), parts.next());
                        match (p1, p2, p3) {
                            (Some(p1), Some(p2), Some(p3)) if seg(p1) && seg(p2) && seg(p3) => {
                                let total = 3 + p1.len() + p2.len() + p3.len();
                                if total <= spec.max {
                                    span_end = run_start + p1.len() + 1 + p2.len() + 1 + p3.len();
                                    true
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        }
                    }
                };
                if ok {
                    return Some(CredSpan { start: i, end: span_end, kind: spec.kind });
                }
            }
        }
        // 步进到下一字符边界：字面量与 run 判定均为 ASCII，多字节首字节
        // 不匹配任何前缀。
        i = next_char_boundary(text, i + 1);
    }
    None
}

/// `-----BEGIN <label>PRIVATE KEY-----` 头之后找 END 行；两行 label 均
/// 须合法（LABEL 字符后接 `PRIVATE KEY-----`）。
fn key_block_end(text: &str, start: usize) -> Option<usize> {
    // label 惰性：[A-Z0-9 ]* 后接 PRIVATE KEY-----（贪婪扫描会把
    // PRIVATE KEY 一并吃进 label，永不匹配）。
    let label_end = |pos: usize| -> Option<usize> {
        let slice = &text[pos..];
        let window = &slice[..slice.len().min(68)];
        let rel = window.find("PRIVATE KEY-----")?;
        let prefix_ok = slice[..rel]
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'0'..=b'9' | b' '));
        prefix_ok.then(|| pos + rel + "PRIVATE KEY-----".len())
    };
    let header_end = label_end(start + "-----BEGIN ".len())?;
    let body = &text[header_end..];
    let end_line = body.find("-----END ")?;
    let end_start = header_end + end_line;
    label_end(end_start + "-----END ".len())
}

fn is_scheme_byte(b: u8) -> bool {
    char_class(b) & (classes::LOWER | classes::ALNUM) != 0
        || b == b'+'
        || b == b'.'
        || b == b'-'
}

fn scheme_start(text: &str, scheme_end: usize) -> usize {
    let bytes = text.as_bytes();
    let mut start = scheme_end;
    while start > 0 && scheme_end - start < 16 && is_scheme_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

/// scheme 合法（[a-z][a-z0-9+.-]{1,15} 且独立成词）。
fn scheme_before(text: &str, scheme_end: usize) -> bool {
    let start = scheme_start(text, scheme_end);
    let len = scheme_end - start;
    if len < 2 {
        return false;
    }
    let bytes = text.as_bytes();
    // 首字符小写，且前一字符不是 \w（独立成词，排除标识符中段）。
    char_class(bytes[start]) & classes::LOWER != 0
        && (start == 0 || char_class(bytes[start - 1]) & classes::WORD == 0)
}

/// `scheme://user:password` 中密码段的 (scheme 起, 密码止)。
/// user 段 ≤ 256；user/password 以 `\s / @ ? #` 截止（user 多含 `:`）。
fn address_password(text: &str, scheme_sep: usize) -> Option<(usize, usize)> {
    if !scheme_before(text, scheme_sep) {
        return None;
    }
    let user_start = scheme_sep + 3;
    let user_end = user_start
        + text[user_start..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, '/' | ':' | '@' | '?' | '#'))
            .map(|(i, _)| i)
            .unwrap_or(text.len() - user_start);
    if user_end - user_start > 256 || text.as_bytes().get(user_end) != Some(&b':') {
        return None;
    }
    let pass_start = user_end + 1;
    let pass_end = pass_start
        + text[pass_start..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#'))
            .map(|(i, _)| i)
            .unwrap_or(text.len() - pass_start);
    (pass_end > pass_start).then_some((scheme_start(text, scheme_sep), pass_end))
}

/// 单个凭据区间的掩码显示。
pub fn mask_span(text: &str, span: &CredSpan) -> String {
    match span.kind {
        // 地址段整段掩码（scheme+user+password 全 `*`）。
        CredKind::Address => mask_range(text, span.start, span.end),
        // 前缀键保留前 8 字符（识别面纯 ASCII，字节长=字符长）。
        CredKind::Prefixed => {
            let total = span.end - span.start;
            let keep = total.min(8);
            let mut out = String::from(&text[span.start..span.start + keep]);
            out.extend(std::iter::repeat('*').take(total - keep));
            out
        }
        // 私钥块保留 BEGIN 头行与 END 行，掩码中间体（保留换行）。
        CredKind::KeyBlock => {
            let body_start = text[span.start..span.end]
                .find('\n')
                .map(|i| span.start + i + 1)
                .unwrap_or(span.end);
            let body_end = text[body_start..span.end]
                .find("-----END ")
                .map(|i| body_start + i)
                .unwrap_or(body_start);
            let mut out = String::from(&text[span.start..body_start]);
            out.push_str(&mask_range(text, body_start, body_end));
            out.push_str(&text[body_end..span.end]);
            out
        }
    }
}

/// 区间内每个字符替换为 `*`，换行保留。
pub fn mask_range(text: &str, start: usize, end: usize) -> String {
    text[start..end]
        .chars()
        .map(|c| if c == '\n' || c == '\r' { c } else { '*' })
        .collect()
}

/// 用户文本的掩码显示全文：无凭据返回 None（渲染端保持原管线），
/// 有则返回原文切片 + 掩码段拼接的显示串。存储/发送的原文不变。
pub fn mask_user_text(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut pos = 0usize;
    let mut found = false;
    while pos <= text.len() {
        let Some(span) = find_credential(text, pos) else { break };
        out.push_str(&text[pos..span.start]);
        out.push_str(&mask_span(text, &span));
        pos = span.end;
        found = true;
    }
    if !found {
        return None;
    }
    out.push_str(&text[pos..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(text: &str) -> String {
        mask_user_text(text).expect("应识别出凭据")
    }

    #[test]
    fn plain_text_has_no_credential() {
        assert!(mask_user_text("你好，请帮我看看这段日志").is_none());
        assert!(mask_user_text("sk- 这种前缀不算，太短").is_none());
        assert!(mask_user_text("https://example.com/path 不带密码不掩码").is_none());
    }

    #[test]
    fn sk_key_needs_digit_and_min_length() {
        // 无数字：不识别。
        assert!(mask_user_text("sk-abcdefghijklmnopqrst").is_none());
        // 有数字且 run ≥20：识别，保留前 8 字符（3+20=23，掩 15）。
        let out = masked("密钥 sk-abc12345efgh67890ijk 敏感");
        assert_eq!(out, format!("密钥 sk-abc12{} 敏感", "*".repeat(15)));
    }

    #[test]
    fn github_token_masked() {
        let tok = format!("ghp_{}", "a".repeat(36));
        let out = masked(&format!("token: {tok} 请用"));
        assert_eq!(out, format!("token: ghp_aaaa{} 请用", "*".repeat(32)));
    }

    #[test]
    fn jwt_three_segments() {
        let jwt = format!("eyJ{}.{}.{}", "a".repeat(10), "b".repeat(10), "c".repeat(8));
        let out = masked(&format!("Bearer {jwt}"));
        assert_eq!(out, format!("Bearer eyJaaaaa{}", "*".repeat(25)));
        // 两段不成 JWT。
        assert!(mask_user_text(&format!("Bearer eyJ{}.{}", "a".repeat(10), "b".repeat(10))).is_none());
    }

    #[test]
    fn aws_key_bounded_exact() {
        let key = format!("AKIA{}", "ABCDEFGH".repeat(2)); // 16 位大写数字
        let out = masked(&format!("aws {key} x"));
        assert_eq!(out, format!("aws AKIAABCD{} x", "*".repeat(12)));
        // 第 17 位同类字符：词边界失败不识别。
        assert!(mask_user_text(&format!("aws AKIA{}X", "ABCDEFGH".repeat(2))).is_none());
    }

    #[test]
    fn address_password_fully_masked() {
        let out = masked("连 https://user:secretpass@example.com/api 试试");
        // scheme+user+password 共 23 字符全部掩码。
        assert_eq!(out, format!("连 {}@example.com/api 试试", "*".repeat(23)));
        // 无密码的地址不掩码。
        assert!(mask_user_text("去 https://example.com/path").is_none());
    }

    #[test]
    fn key_block_keeps_begin_end_lines() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nabc\ndef\n-----END RSA PRIVATE KEY-----\n后面";
        let out = masked(text);
        assert_eq!(
            out,
            "-----BEGIN RSA PRIVATE KEY-----\n***\n***\n-----END RSA PRIVATE KEY-----\n后面"
        );
    }

    #[test]
    fn multiple_credentials_all_masked() {
        let gh = format!("ghp_{}", "x".repeat(36));
        let sk = "sk-abc123xyz456def789gh";
        let out = masked(&format!("{gh} 和 {sk}"));
        assert!(out.contains('*') && !out.contains(&gh) && !out.contains(sk));
        assert!(out.starts_with("ghp_xxx"));
    }
}
