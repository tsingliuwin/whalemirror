//! shell 退出状态（上游 shell seam 标记契约 + terminal-card-model 的
//! parseExitStatus/terminalFailed/terminalExitLabel 等价纯函数，#35①）。
//!
//! 上游约定：bash/pwsh 工具渲染结果以机器可读一行收尾——
//! `[exit code: N]`、无退出码时 `[exit code: null]`、信号终止时
//! `[killed by signal: X]`；非零退出是 completed call（isError=false，
//! 退出状态是结果数据）。呈现侧剥出标记画成胶囊，标记留在输出里会
//! 双重渲染。

/// 结构化退出状态：码（None = 已结束但无码可用）或信号名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellExit {
    Code(Option<i32>),
    Signal(String),
}

/// 渲染结果拆解：剥标记后的输出体 + 结构化状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedShellResult {
    pub body: String,
    pub exit: ShellExit,
}

/// 上游 parseExitStatus：只认「换行 + 标记 + 串尾」——标记必须是最后
/// 一行且格式闭合，普通输出碰巧以 marker 样文本结尾不误匹配。信号
/// 优先于退出码；两者皆无 = 干净退出 0。超时/沙箱拒绝等其他标记
/// 承载胶囊不显示的事实，留在正文不剥。
pub fn parse_exit_status(text: &str) -> ParsedShellResult {
    if let Some((body, signal)) = split_tail_marker(text, "[killed by signal: ") {
        return ParsedShellResult { body: body.into(), exit: ShellExit::Signal(signal.into()) };
    }
    if let Some((body, code)) = split_tail_marker(text, "[exit code: ") {
        let exit = if code == "null" {
            ShellExit::Code(None)
        } else {
            match code.parse::<i32>() {
                Ok(n) => ShellExit::Code(Some(n)),
                Err(_) => return clean(text),
            }
        };
        return ParsedShellResult { body: body.into(), exit };
    }
    clean(text)
}

fn clean(text: &str) -> ParsedShellResult {
    ParsedShellResult { body: text.into(), exit: ShellExit::Code(Some(0)) }
}

/// 尾标记拆解：`text` 以 `head + payload + "]"` 收尾，payload 是不含
/// 换行与 `]` 的非空单行（上游正则 `[^\]\n]+$` 的手写等价）。
fn split_tail_marker<'a>(text: &'a str, head: &'a str) -> Option<(&'a str, &'a str)> {
    if !text.ends_with(']') {
        return None;
    }
    let marker_start = text.rfind(&format!("\n{head}"))?;
    let payload = &text[marker_start + 1 + head.len()..text.len() - 1];
    (!payload.is_empty() && !payload.contains('\n') && !payload.contains(']'))
        .then_some((&text[..marker_start], payload))
}

/// terminalFailed：已结束且（非零码 / 码缺失 / 信号）。码缺失是显式
/// 失败（上游 e8488f6b41：视图说命令结束了但拿不到码，不当干净退出）。
pub fn shell_exit_failed(exit: &ShellExit) -> bool {
    match exit {
        ShellExit::Signal(_) => true,
        ShellExit::Code(None) => true,
        ShellExit::Code(Some(0)) => false,
        ShellExit::Code(Some(_)) => true,
    }
}

/// terminalExitLabel：仅失败命令有胶囊文案（zh 文案照上游 locale
/// terminal.signal/exitCode/noExitCode）。
pub fn shell_exit_label(exit: &ShellExit) -> Option<String> {
    if !shell_exit_failed(exit) {
        return None;
    }
    match exit {
        ShellExit::Signal(s) => Some(format!("信号 {s}")),
        ShellExit::Code(None) => Some("未正常退出".into()),
        ShellExit::Code(Some(n)) => Some(format!("退出码 {n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> ParsedShellResult {
        parse_exit_status(text)
    }

    #[test]
    fn clean_exit_zero_has_no_badge() {
        let p = parsed("hello\n[exit code: 0]");
        assert_eq!(p.body, "hello");
        assert_eq!(p.exit, ShellExit::Code(Some(0)));
        assert!(!shell_exit_failed(&p.exit));
        assert_eq!(shell_exit_label(&p.exit), None);
    }

    #[test]
    fn non_zero_exit_yields_badge() {
        let p = parsed("out\n[exit code: 3]");
        assert_eq!(p.body, "out");
        assert_eq!(p.exit, ShellExit::Code(Some(3)));
        assert_eq!(shell_exit_label(&p.exit).as_deref(), Some("退出码 3"));
    }

    #[test]
    fn negative_exit_code_matches() {
        // 上游正则 -?\d+：Windows 负退出码
        let p = parsed("\n[exit code: -1]");
        assert_eq!(p.exit, ShellExit::Code(Some(-1)));
        assert_eq!(shell_exit_label(&p.exit).as_deref(), Some("退出码 -1"));
    }

    #[test]
    fn missing_exit_code_is_explicit_failure() {
        let p = parsed("out\n[exit code: null]");
        assert_eq!(p.body, "out");
        assert_eq!(p.exit, ShellExit::Code(None));
        assert!(shell_exit_failed(&p.exit));
        assert_eq!(shell_exit_label(&p.exit).as_deref(), Some("未正常退出"));
    }

    #[test]
    fn signal_takes_precedence() {
        let p = parsed("out\n[killed by signal: TERM]");
        assert_eq!(p.body, "out");
        assert_eq!(p.exit, ShellExit::Signal("TERM".into()));
        assert_eq!(shell_exit_label(&p.exit).as_deref(), Some("信号 TERM"));
    }

    #[test]
    fn markerless_output_is_clean_exit() {
        let p = parsed("plain output\nmore");
        assert_eq!(p.body, "plain output\nmore");
        assert_eq!(p.exit, ShellExit::Code(Some(0)));
    }

    #[test]
    fn marker_like_text_without_line_start_does_not_match() {
        // 上游「要求前导换行 + 串尾」：marker 样文本不作末行收尾不误剥
        let p = parsed("result: [exit code: 3]");
        assert_eq!(p.body, "result: [exit code: 3]");
        assert_eq!(p.exit, ShellExit::Code(Some(0)));
        // 末行有前缀噪声也不剥
        let p = parsed("x\ny [exit code: 3]");
        assert_eq!(p.body, "x\ny [exit code: 3]");
    }

    #[test]
    fn malformed_marker_stays_in_body() {
        let p = parsed("out\n[exit code: abc]");
        assert_eq!(p.body, "out\n[exit code: abc]");
        assert_eq!(p.exit, ShellExit::Code(Some(0)));
    }

    #[test]
    fn stderr_section_then_marker_still_parses() {
        let text = "out\n[stderr]\nsome error\n[exit code: 2]";
        let p = parsed(text);
        assert_eq!(p.body, "out\n[stderr]\nsome error");
        assert_eq!(shell_exit_label(&p.exit).as_deref(), Some("退出码 2"));
    }
}
