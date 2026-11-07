//! composer 上下文占用表（上游 ui-conversation `ContextMeter` +
//! `context-occupancy.ts` + token-meter `breakdown-projection.ts` 的纯函数部分）。
//!
//! 上游把两件事分开：**占用率**来自 provider 上报的 `contextPressure`
//! （缺任一半不渲染），**构成分布**来自启发式 `contextBreakdown`（只用来
//! 按比例给色条分段，环的读数仍以 provider 精确值为准）。rustdsh 没有独立
//! 的 pressure 投影，因此这里用同一套启发式给「渲染后的请求」定价：
//! 系统提示词（最后一非空系统节点）、工具定义（请求信封里的 tools 数组）、
//! 对话消息（其余可见节点），容量取路由声明。口径与 web 完全一致：
//! 每 4 字符 1 token、每块结构开销 4、每条消息角色开销 4。

/// 每 4 字符一个 token 的固定密度估算（上游 `CHARS_PER_TOKEN`）。
pub const CHARS_PER_TOKEN: u32 = 4;
/// 每个内容块的 JSON 结构开销（上游 `BLOCK_OVERHEAD`）。
pub const BLOCK_OVERHEAD: u64 = 4;
/// 每条消息的角色字段开销（上游 `ROLE_OVERHEAD`）。
pub const ROLE_OVERHEAD: u64 = 4;

fn ceil_div(value: usize, divisor: u32) -> u64 {
    (value as u64).div_ceil(divisor as u64)
}

/// 系统提示词的启发式价格：文本密度 + 角色开销，无逐块开销
/// （上游 `estimateSystemMessage`；空内容 0 = 视为无系统提示词）。
pub fn estimate_system_prompt(system: Option<&str>) -> u64 {
    match system {
        Some(text) if !text.is_empty() => ceil_div(text.chars().count(), CHARS_PER_TOKEN) + ROLE_OVERHEAD,
        _ => 0,
    }
}

/// 工具定义的启发式价格：tools 数组的序列化长度 + 一个块开销
/// （上游 `estimateToolsTokens`）。
pub fn estimate_tools_tokens(tools_json: Option<&str>) -> u64 {
    match tools_json {
        Some(json) if !json.is_empty() => ceil_div(json.chars().count(), CHARS_PER_TOKEN) + BLOCK_OVERHEAD,
        _ => 0,
    }
}

/// 一条可见消息的价格：正文文本密度 + 每块开销 + 角色开销
/// （上游 `estimateMessage` 的文本路径；调用/图片块按结构近似计费）。
pub fn estimate_message_chars(chars: usize, blocks: usize) -> u64 {
    ceil_div(chars, CHARS_PER_TOKEN) + BLOCK_OVERHEAD * blocks as u64 + ROLE_OVERHEAD
}

/// 上下文占用的展示读数（上游 `ContextOccupancy`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextOccupancy {
    /// 已用 token（估算）。
    pub used_tokens: u64,
    /// 路由容量。
    pub context_window: u64,
    /// 已用百分比（0–100，四舍五入）。
    pub percent: u32,
}

/// 由已用估算与容量求读数；容量缺失（0）或用量缺失 → `None`（不渲染）。
pub fn context_occupancy(used_tokens: u64, context_window: u64) -> Option<ContextOccupancy> {
    if used_tokens == 0 || context_window == 0 {
        return None;
    }
    let percent = ((used_tokens as f64 / context_window as f64) * 100.0).round() as u32;
    Some(ContextOccupancy { used_tokens, context_window, percent: percent.min(100) })
}

/// 三段构成（上游 `contextBreakdown`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextBreakdown {
    pub system_tokens: u64,
    pub tools_tokens: u64,
    pub message_tokens: u64,
}

impl ContextBreakdown {
    /// 三段之和（色条按比例分段时的分母）。
    pub fn total(&self) -> u64 {
        self.system_tokens + self.tools_tokens + self.message_tokens
    }

    /// 按占用率给出各段占整条色条的百分比（上游 `parts` 逻辑）：
    /// 总和为 0 时退化为单一「总量」段。
    pub fn segment_widths(&self, percent: u32) -> Vec<f32> {
        let total = self.total();
        if total == 0 {
            return vec![percent as f32];
        }
        [self.system_tokens, self.tools_tokens, self.message_tokens]
            .into_iter()
            .map(|part| percent as f32 * part as f64 as f32 / total as f32)
            .filter(|w| *w > 0.0)
            .collect()
    }
}

/// 紧凑数字（上游 `formatTokens`）：<1000 原样、K/M 保留一位小数。
pub fn format_tokens_compact(value: u64) -> String {
    fn scaled(candidate: f64) -> String {
        if candidate >= 100.0 {
            format!("{}", candidate.round() as u64)
        } else {
            format!("{}", (candidate * 10.0).round() / 10.0)
        }
    }
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{}K", scaled(value as f64 / 1_000.0))
    } else {
        format!("{}M", scaled(value as f64 / 1_000_000.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_matches_the_web_fixed_density() {
        // 4 字符/ token + 角色 4
        assert_eq!(estimate_system_prompt(Some("abcdefgh")), 2 + ROLE_OVERHEAD);
        assert_eq!(estimate_system_prompt(None), 0);
        assert_eq!(estimate_system_prompt(Some("")), 0);
        // tools：JSON 长度 8 → 2 + 块开销 4
        assert_eq!(estimate_tools_tokens(Some("[1,2,3]")), 2 + BLOCK_OVERHEAD);
        assert_eq!(estimate_tools_tokens(None), 0);
        // 消息：8 字符、2 块 → 2 + 8 + 4
        assert_eq!(estimate_message_chars(8, 2), 2 + 2 * BLOCK_OVERHEAD + ROLE_OVERHEAD);
    }

    #[test]
    fn occupancy_rounds_and_refuses_missing_halves() {
        let o = context_occupancy(45_000, 128_000).unwrap();
        assert_eq!(o.percent, 35);
        assert_eq!((o.used_tokens, o.context_window), (45_000, 128_000));
        // 缺任一半都不渲染（上游 contextOccupancy 返回 null）
        assert_eq!(context_occupancy(0, 128_000), None);
        assert_eq!(context_occupancy(1_000, 0), None);
        // 超容量夹到 100
        assert_eq!(context_occupancy(200_000, 128_000).unwrap().percent, 100);
    }

    #[test]
    fn segment_widths_split_the_bar_by_composition() {
        let b = ContextBreakdown { system_tokens: 1_000, tools_tokens: 3_000, message_tokens: 4_000 };
        assert_eq!(b.total(), 8_000);
        let w = b.segment_widths(50);
        assert_eq!(w.len(), 3);
        assert!((w[0] - 6.25).abs() < 0.01);
        assert!((w[1] - 18.75).abs() < 0.01);
        assert!((w[2] - 25.0).abs() < 0.01);
        // 零段被丢弃；全零退化为单段
        let zero_tools = ContextBreakdown { system_tokens: 1_000, tools_tokens: 0, message_tokens: 1_000 };
        assert_eq!(zero_tools.segment_widths(40).len(), 2);
        assert_eq!(ContextBreakdown::default().segment_widths(40), vec![40.0]);
    }

    #[test]
    fn compact_numbers_follow_the_web_templates() {
        assert_eq!(format_tokens_compact(864), "864");
        assert_eq!(format_tokens_compact(8_640), "8.6K");
        assert_eq!(format_tokens_compact(128_000), "128K");
        assert_eq!(format_tokens_compact(1_250_000), "1.3M");
    }
}
