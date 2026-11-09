//! 会话标题域（上游 `session/session-title` + `session-title-all-prompts-llm`
//! 的 1:1 移植：清洗/截断/回退/输出解析 + 插件提示词与输入帧）。
//!
//! 上游语义逐条照抄：
//! - `normalizeSessionTitle`：剥 OSC/CSI/ESC/控制符/方向性不可见符，
//!   折叠空白，UTF-8 字节预算内按码点截断；
//! - `fallbackSessionTitle`：首条合格提问的前 N 个词（字节预算兜底）；
//! - all-prompts 插件配置（bundle `cordis.patch.yml`）：`targetWords: 5`、
//!   `targetCjkCharacters: 10`、`maxInputBytes: 4096`、
//!   `maxOutputTokens: 64`、`timeoutMs: 60000`；
//! - 输入超预算**拒绝请求**而不是截断会话历史（README Known Limitations）；
//! - 输出取首个非空行；整行被 `*强调*` 包裹时剥掉标记对；
//! - 已有 provider 标题作为 `currentTitle` 锚点，仍然准确则原样保留。

use serde::Serialize;

/// all-prompts 插件的策略常量（上游 bundle `cordis.patch.yml` 逐字）。
pub const TARGET_WORDS: usize = 5;
pub const TARGET_CJK_CHARACTERS: usize = 10;
pub const MAX_INPUT_BYTES: usize = 4096;
pub const MAX_OUTPUT_TOKENS: u32 = 64;
pub const TIMEOUT_MS: u64 = 60_000;

/// 回退标题策略（上游 base bundle `cordis.patch.yml` 的 session-title 行）。
pub const FALLBACK_MAX_WORDS: usize = 5;
pub const FALLBACK_MAX_BYTES: usize = 40;
/// 存储标题字节上限（上游 `maxTitleBytes`）。
pub const MAX_TITLE_BYTES: usize = 80;

/// 一条合格标题输入（上游 `SessionTitleUserMessage`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TitleUserMessage {
    /// 源 `user/message` 事件 seq。
    pub seq: u64,
    /// 文本块拼接内容。
    pub text: String,
}

/// 剥终端转义与控制字符，折叠空白，产出单行清洗文本。
pub fn clean_title_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            // ESC（0x1B）/ 单字节 CSI（0x9B）/ OSC（0x9D）引导的序列
            '\u{1B}' | '\u{9B}' | '\u{9D}' => {
                i = skip_escape_sequence(&chars, i);
            }
            // C0/C1 控制字符（\t\n\r 等空白控制符也一并剥除，随后折叠空白）
            '\u{0}'..='\u{08}' | '\u{0B}' | '\u{0C}' | '\u{0E}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => {
                i += 1;
            }
            // 方向性与不可见控制（上游 DIRECTIONAL_CONTROL）
            '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' => {
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // 上游 `replace(/\s+/gu, ' ').trim()`
    let mut collapsed = String::with_capacity(out.len());
    let mut in_space = false;
    for c in out.chars() {
        if c.is_whitespace() {
            if !in_space {
                collapsed.push(' ');
                in_space = true;
            }
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    collapsed.trim().to_string()
}

/// 从 `i` 起跳过一个转义序列（含未终止尾部），返回下一个读取位置。
fn skip_escape_sequence(chars: &[char], start: usize) -> usize {
    let lead = chars[start];
    let next = chars.get(start + 1).copied();
    match lead {
        // 单字节 OSC 引介符：吃 BEL 或 ST（ESC \）为止；未终止吃到尾
        '\u{9D}' => skip_osc_body(chars, start + 1),
        // ESC ] 引介的 OSC
        '\u{1B}' if next == Some(']') => skip_osc_body(chars, start + 2),
        // 单字节 CSI 引介符：参数 0x30-0x3F + 中介 0x20-0x2F + 终结 0x40-0x7E
        '\u{9B}' => skip_csi_params(chars, start + 1),
        // ESC [ 引介的 CSI
        '\u{1B}' if next == Some('[') => skip_csi_params(chars, start + 2),
        // 其余两字节 ESC 序列：ESC [@-_]
        '\u{1B}' => {
            if matches!(next, Some(c) if ('\u{40}'..='\u{5F}').contains(&c)) {
                start + 2
            } else {
                start + 1
            }
        }
        // 安全兜底：不识别的引介符也必须前进
        _ => start + 1,
    }
}

/// OSC 体：直到 BEL（0x07）或 ST（ESC \）；未终止吃到尾。
fn skip_osc_body(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() {
        if chars[i] == '\u{07}' {
            return i + 1;
        }
        if chars[i] == '\u{1B}' && chars.get(i + 1) == Some(&'\\') {
            return i + 2;
        }
        i += 1;
    }
    i
}

/// CSI 参数与中介字节，随后一个终结字节（0x40-0x7E）；未终止吃到尾。
fn skip_csi_params(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && ('\u{30}'..='\u{3F}').contains(&chars[i]) {
        i += 1;
    }
    while i < chars.len() && ('\u{20}'..='\u{2F}').contains(&chars[i]) {
        i += 1;
    }
    if i < chars.len() && ('\u{40}'..='\u{7E}').contains(&chars[i]) {
        i + 1
    } else {
        i
    }
}

/// UTF-8 字节预算内按码点截断（不劈开码点）。
pub fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    assert!(max_bytes > 0, "maxBytes must be positive");
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut used = 0usize;
    let mut out = String::new();
    for c in input.chars() {
        let len = c.len_utf8();
        if used + len > max_bytes {
            break;
        }
        out.push(c);
        used += len;
    }
    out
}

/// 规范化一个接受的标题并施加 UTF-8 字节预算。
pub fn normalize_title(input: &str, max_bytes: usize) -> String {
    truncate_utf8(&clean_title_text(input), max_bytes).trim_end().to_string()
}

/// 确定性首条回退标题：清洗后前 `max_words` 个词，字节预算内。
pub fn fallback_title(input: &str, max_words: usize, max_bytes: usize) -> String {
    assert!(max_words > 0, "maxWords must be positive");
    let cleaned = clean_title_text(input);
    let words: Vec<&str> = cleaned
        .split(' ')
        .filter(|w| !w.is_empty())
        .take(max_words)
        .collect();
    truncate_utf8(&words.join(" "), max_bytes).trim_end().to_string()
}

/// 从模型输出取标题：首个非空行；整行 `*强调*` 包裹时剥掉标记对。
pub fn title_from_output(text: &str) -> String {
    let line = text
        .split(['\r', '\n'])
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    strip_line_emphasis(&line)
}

/// 上游 EMPHASIS_WRAPPER：`^\*{1,3}(\S.*\S)\1$`（贪婪回溯：先试 3 星
/// 标记，回引同标记；内侧不含同标记串才剥）。
fn strip_line_emphasis(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    for marker_len in [3usize, 2, 1] {
        if chars.len() >= marker_len * 2
            && chars[..marker_len].iter().all(|&c| c == '*')
            && chars[chars.len() - marker_len..].iter().all(|&c| c == '*')
        {
            let inner: String = chars[marker_len..chars.len() - marker_len].iter().collect();
            // 内侧必须无首尾空白（\S 起止），且不含同标记串（回引可遇即剥）
            let t = inner.trim();
            let marker = "*".repeat(marker_len);
            if !t.is_empty() && t == inner && !inner.contains(&marker) {
                return inner;
            }
        }
    }
    line.to_string()
}

/// 语言感知系统指令；有锚点标题时附稳定性规则（上游逐字，英文）。
pub fn all_prompts_system_prompt(has_current_title: bool) -> String {
    let mut lines = vec![
        "Create a concise title for an AI coding-assistant session from the supplied human messages.".to_string(),
        "Return only the title on one line, **in plain text of natural language**, with no quotes, prefix, explanation, Markdown, XML, or terminal control codes. No code is allowed.".to_string(),
        "Use the language of the messages.".to_string(),
        format!("Aim for about {TARGET_WORDS} words in non-CJK languages or {TARGET_CJK_CHARACTERS} CJK characters."),
        "If the messages give little to name, still return a short best-effort title, such as Greeting, instead of explaining.".to_string(),
    ];
    if has_current_title {
        lines.push("An existing title is supplied as currentTitle. If it still accurately describes the main topic or task, return it exactly unchanged.".to_string());
        lines.push("Follow-up questions, additional details within the same topic, acknowledgements such as \"thanks\", and requests to continue do not by themselves justify a title change.".to_string());
        lines.push("Do not reword, polish, shorten, or replace synonyms in an adequate title. Keeping its exact wording takes priority over the target length.".to_string());
        lines.push("Change the title only when the messages materially change or expand the main topic or task so that the existing title is no longer accurate.".to_string());
    }
    lines.join("\n")
}

/// 把消息帧成 JSON（上游 frameMessages：结构化，防文本破坏分隔符）。
pub fn frame_messages(messages: &[TitleUserMessage], current_title: Option<&str>) -> String {
    match current_title {
        Some(current) => {
            let body = serde_json::json!({ "currentTitle": current, "messages": messages });
            format!("Update the session title from this JSON object:\n{body}")
        }
        None => {
            let body = serde_json::json!({ "messages": messages });
            format!("Generate the session title from this JSON array of human messages:\n{body}")
        }
    }
}

/// 一次随动标题请求计划（触发点按它决定发不发、带不带锚）。
pub struct TitleRequestPlan {
    /// 系统指令。
    pub system: String,
    /// 帧后的用户输入。
    pub input: String,
}

/// 组一次请求：provider 源标题才作锚（上游 `source.kind === 'provider'`）；
/// 输入超预算返回 `None`（拒绝请求，不截断历史）。
pub fn plan_all_prompts_request(
    prompts: &[TitleUserMessage],
    current_title: Option<&str>,
    current_is_provider: bool,
) -> Option<TitleRequestPlan> {
    let anchor = current_title.filter(|_| current_is_provider);
    let input = frame_messages(prompts, anchor);
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    Some(TitleRequestPlan {
        system: all_prompts_system_prompt(anchor.is_some()),
        input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_escapes_controls_and_whitespace() {
        // OSC（BEL 与 ST 终止）+ 未终止尾部
        assert_eq!(clean_title_text("\u{1B}]0;title\u{07}abc"), "abc");
        assert_eq!(clean_title_text("\u{9D}2;1\u{1B}\\xyz"), "xyz");
        assert_eq!(clean_title_text("\u{1B}]0;unterminated"), "");
        // SGR 颜色 + 两字节 ESC + C0
        assert_eq!(clean_title_text("\u{1B}[31mre\u{1B}[0md"), "red");
        assert_eq!(clean_title_text("\u{1B}Ma\u{07}b"), "ab");
        assert_eq!(clean_title_text("a\u{0}b\u{7F}c"), "abc");
        // 方向性不可见符
        assert_eq!(clean_title_text("a\u{200B}\u{200E}b\u{FEFF}c"), "abc");
        assert_eq!(clean_title_text("a\u{202A}b\u{202E}c"), "abc");
        // 空白折叠
        assert_eq!(clean_title_text("  a \t b\n\nc "), "a b c");
    }

    #[test]
    fn truncates_on_utf8_boundaries() {
        assert_eq!(truncate_utf8("中文标题", 7), "中文"); // 3*2+1 劈不开第三个
        assert_eq!(truncate_utf8("abc", 10), "abc");
        assert_eq!(truncate_utf8("ab中文", 8), "ab中文"); // 8 字节恰好贴线
        assert_eq!(truncate_utf8("ab中文", 7), "ab中");
        assert_eq!(normalize_title("  多余  空白\u{1B}[0m ", 100), "多余 空白");
    }

    #[test]
    fn fallback_takes_leading_words_within_budget() {
        assert_eq!(fallback_title(" 修复 侧栏 点击 漂移 问题", 3, 100), "修复 侧栏 点击");
        // 字节预算截断词序列（"ab cd " 已 6 字节，中 3 字节超预算 8）
        assert_eq!(fallback_title("ab cd 中文长", 3, 8), "ab cd");
    }

    #[test]
    fn output_takes_first_nonempty_line_and_strips_emphasis() {
        assert_eq!(title_from_output("\n \n修复侧栏\n备注"), "修复侧栏");
        assert_eq!(title_from_output("**标题**"), "标题");
        assert_eq!(title_from_output("*标题*"), "标题");
        // 三星标记（贪婪先试 3）
        assert_eq!(title_from_output("***深***"), "深");
        // 内侧不含同标记串（"a*b" 无 "**"）→ 上游正则照样剥
        assert_eq!(title_from_output("**a*b**"), "a*b");
        // 未成对：不剥
        assert_eq!(title_from_output("*未成对"), "*未成对");
        // 内侧含同标记串：不剥
        assert_eq!(title_from_output("**a**b**"), "**a**b**");
        assert_eq!(title_from_output(""), "");
    }

    #[test]
    fn frames_messages_as_structured_json() {
        let ms = vec![
            TitleUserMessage { seq: 0, text: "第一问".into() },
            TitleUserMessage { seq: 3, text: "second".into() },
        ];
        let fresh = frame_messages(&ms, None);
        assert!(fresh.starts_with("Generate the session title from this JSON array of human messages:"));
        assert!(fresh.contains("\"seq\":0"));
        let anchored = frame_messages(&ms, Some("旧标题"));
        assert!(anchored.starts_with("Update the session title from this JSON object:"));
        assert!(anchored.contains("\"currentTitle\":\"旧标题\""));
    }

    #[test]
    fn plans_anchor_only_for_provider_titles_and_rejects_oversize() {
        let prompts = vec![TitleUserMessage { seq: 0, text: "帮我修 bug".into() }];
        // provider 源：带锚 + 稳定性规则
        let plan = plan_all_prompts_request(&prompts, Some("修 bug"), true).unwrap();
        assert!(plan.system.contains("currentTitle"));
        assert!(plan.input.contains("修 bug"));
        // fallback 源：不带锚
        let plan = plan_all_prompts_request(&prompts, Some("修 bug"), false).unwrap();
        assert!(!plan.system.contains("currentTitle"));
        assert!(!plan.input.contains("currentTitle"));
        // 超预算：拒绝请求（None），不截断历史
        let big = (0..200)
            .map(|i| TitleUserMessage { seq: i, text: "x".repeat(100) })
            .collect::<Vec<_>>();
        assert!(plan_all_prompts_request(&big, None, false).is_none());
        // 空提问列同样超不了预算但也没有意义——帧仍成立，交由调用方不用
        assert!(plan_all_prompts_request(&[], None, false).is_some());
    }

    #[test]
    fn system_prompt_carries_upstream_word_targets() {
        let s = all_prompts_system_prompt(false);
        assert!(s.contains(&format!("about {TARGET_WORDS} words")));
        assert!(s.contains(&format!("{TARGET_CJK_CHARACTERS} CJK characters")));
        assert!(s.starts_with("Create a concise title"));
    }
}
