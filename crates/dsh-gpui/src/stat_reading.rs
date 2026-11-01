//! composer 统计 pill 的读数切分（上游 `StatsPills.module.css` 的
//! `font-variant-numeric: tabular-nums`）。
//!
//! 上游 `.pill` 声明 `font-variant-numeric: tabular-nums`，让「1 轮 22 步」
//! 这类计数在数值变化时**不左右抖动**（比例字体里 1 比 8 窄，读数一涨整条
//! pill 就跳）。GPUI 没有 `font-variant-numeric`，等价做法是给数字片段单独
//! 指定等宽字体：拉丁字母/汉字走比例字体，只有数字（含小数点）走 mono。
//!
//! 切分逻辑写成纯函数便于单测——UI 层只负责把 `Eq` 片段套上
//! `font_family(theme_mono())`。

/// 读数里的一段：`Eq` = 应该用等宽字体渲染的「数字」段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingRun {
    pub text: String,
    pub eq_width: bool,
}

/// 把一条读数切成「数字段 / 文字段」交替的序列。
///
/// 数字段判据：连续的 ASCII 数字，且**相邻**有小数点时把小数点并进同一段，
/// 这样 `86.4k`、`12.5` 这类读数整体等宽；单独的 `.`（如句末）不并。
/// 文字段（汉字、空格、`k`/`%` 等）保持原样式，避免整条读数变成等宽字体。
pub fn split_reading(text: &str) -> Vec<ReadingRun> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<ReadingRun> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let is_digit = chars[i].is_ascii_digit();
        let is_dot = chars[i] == '.';
        // 小数点只有夹在数字之间才算数值的一部分
        let starts_number = is_digit
            || (is_dot && i + 1 < chars.len() && chars[i + 1].is_ascii_digit());
        if !starts_number {
            push_run(&mut runs, chars[i], false);
            i += 1;
            continue;
        }
        let mut buf = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                buf.push(c);
                i += 1;
            } else if c == '.'
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_digit()
                && buf.chars().last().is_some_and(|p| p.is_ascii_digit())
            {
                buf.push(c);
                i += 1;
            } else {
                break;
            }
        }
        for c in buf.chars() {
            push_run(&mut runs, c, true);
        }
    }
    runs
}

fn push_run(runs: &mut Vec<ReadingRun>, c: char, eq_width: bool) {
    match runs.last_mut() {
        Some(last) if last.eq_width == eq_width => last.text.push(c),
        _ => runs.push(ReadingRun { text: c.to_string(), eq_width }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_text(runs: &[ReadingRun]) -> String {
        runs.iter().filter(|r| r.eq_width).map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn counts_split_into_digit_and_text_runs() {
        // 上游 stats.counts 的 zh 形态
        let runs = split_reading("1 轮 22 步");
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0], ReadingRun { text: "1".into(), eq_width: true });
        assert_eq!(runs[1], ReadingRun { text: " 轮 ".into(), eq_width: false });
        assert_eq!(runs[2], ReadingRun { text: "22".into(), eq_width: true });
        assert_eq!(runs[3], ReadingRun { text: " 步".into(), eq_width: false });
        // 数字段拼起来是 1 + 22
        assert_eq!(eq_text(&runs), "122");
    }

    #[test]
    fn decimals_stay_in_one_run() {
        // decodeSpeed 的读数：86.4k 里的 k 不并进数字段
        let runs = split_reading("86.4k");
        assert_eq!(runs[0], ReadingRun { text: "86.4".into(), eq_width: true });
        assert_eq!(runs[1], ReadingRun { text: "k".into(), eq_width: false });
        // 只有一位小数时也整体等宽
        assert_eq!(eq_text(&split_reading("12.5")), "12.5");
    }

    #[test]
    fn percent_and_cache_text_keep_text_runs() {
        let runs = split_reading("缓存命中 93%");
        assert_eq!(eq_text(&runs), "93");
        assert_eq!(runs.last().unwrap().text, "%");
        assert!(!runs.last().unwrap().eq_width);
    }

    #[test]
    fn lone_dot_is_not_a_number() {
        // 句末的点不该被当作数值起点
        let runs = split_reading("步数 3.");
        assert_eq!(eq_text(&runs), "3");
        assert_eq!(runs.last().unwrap().text, ".");
        assert!(!runs.last().unwrap().eq_width);
    }

    #[test]
    fn plain_text_has_no_eq_runs() {
        let runs = split_reading("会话统计");
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].eq_width);
        assert!(split_reading("").is_empty());
    }
}
