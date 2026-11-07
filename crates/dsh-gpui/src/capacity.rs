//! 路由容量（context window）的解析、格式化与兜底（补缺 #95 的配置面）。
//!
//! 上游把容量当作 **provider 声明的元数据**：`llm-deepseek/src/model-info.ts`
//! 的 `context: { contextWindow }` 来自 `/models` 列表或目录声明，`ContextMeter`
//! 在拿不到容量时整枚不渲染。rustdsh 的模型行把容量存成字符串（编辑卡展示
//! `128K`，磁盘写裸数字），因此这里统一：
//!
//! 1. `parse_capacity`：解析 `128000` / `128K` / `1M` / `1.5K`；
//! 2. `format_capacity`：格式化回 `128K` / `1M`（能整除才缩写）；
//! 3. `catalog_capacity`：按模型 id / 路由名兜底官方容量
//!    （DeepSeek 官方目录 `OFFICIAL_CONTEXT_WINDOW = 1_048_576`）；
//! 4. `resolve_capacity`：模型行容量 → provider 内置表 → 官方兜底 → `None`。

/// DeepSeek 官方 `/models` 目录声明的容量（上游 `OFFICIAL_CONTEXT_WINDOW`）。
pub const OFFICIAL_DEEPSEEK_CONTEXT_WINDOW: u64 = 1_048_576;

/// 解析容量字符串。空、非正数、无法解析 → `None`。
pub fn parse_capacity(raw: &str) -> Option<u64> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (digits, scale) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1_000u64),
        'm' | 'M' => (&text[..text.len() - 1], 1_000_000u64),
        _ => (text, 1),
    };
    let value: f64 = digits.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    Some((value * scale as f64).round() as u64)
}

/// 格式化容量：能整除 1M → `NM`，能整除 1K → `NK`，否则原样。
pub fn format_capacity(value: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000 * 1_000;
    if value % M == 0 {
        format!("{}M", value / M)
    } else if value % K == 0 {
        format!("{}K", value / K)
    } else {
        value.to_string()
    }
}

/// 目录兜底：已知官方目录里的路由/模型直接给容量，避免占用表因配置缺字段而消失。
///
/// 判据（保守，只认能确证的）：
/// - 路由 `deepseek`（内置官方路由，任何模型）→ 官方 1_048_576；
/// - 模型 id 以 `deepseek` 开头（DeepSeek 系，含各云厂商转售的
///   `deepseek-v4.x` / `deepseek-chat`）→ 官方 1_048_576。
///
/// 其它路由/模型一律 `None`（不猜，交给用户声明或探测结果）。
pub fn catalog_capacity(provider: &str, model: &str) -> Option<u64> {
    if provider == "deepseek" || model.to_ascii_lowercase().starts_with("deepseek") {
        return Some(OFFICIAL_DEEPSEEK_CONTEXT_WINDOW);
    }
    None
}

/// 容量解析链：模型行声明 → 目录兜底。
pub fn resolve_capacity(declared: &str, provider: &str, model: &str) -> Option<u64> {
    parse_capacity(declared).or_else(|| catalog_capacity(provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_scaled_capacities() {
        assert_eq!(parse_capacity("128000"), Some(128_000));
        assert_eq!(parse_capacity("128K"), Some(128_000));
        assert_eq!(parse_capacity("1M"), Some(1_000_000));
        assert_eq!(parse_capacity(" 64k "), Some(64_000));
        assert_eq!(parse_capacity("1.5K"), Some(1_500));
    }

    #[test]
    fn rejects_unusable_capacities() {
        assert_eq!(parse_capacity(""), None);
        assert_eq!(parse_capacity("   "), None);
        assert_eq!(parse_capacity("0"), None);
        assert_eq!(parse_capacity("n/a"), None);
        assert_eq!(parse_capacity("-5K"), None);
    }

    #[test]
    fn formats_capacity_back_to_compact_form() {
        assert_eq!(format_capacity(1_048_576), "1048576");
        assert_eq!(format_capacity(128_000), "128K");
        assert_eq!(format_capacity(1_000_000), "1M");
        assert_eq!(format_capacity(64_000), "64K");
        // 往返：磁盘上写裸数字，界面显示缩写，再解析回来必须同值
        assert_eq!(parse_capacity(&format_capacity(128_000)), Some(128_000));
    }

    #[test]
    fn catalog_supplies_official_deepseek_capacity() {
        assert_eq!(catalog_capacity("deepseek", "anything"), Some(OFFICIAL_DEEPSEEK_CONTEXT_WINDOW));
        assert_eq!(
            catalog_capacity("huoshan", "deepseek-v4.1-flash"),
            Some(OFFICIAL_DEEPSEEK_CONTEXT_WINDOW)
        );
        // 非 DeepSeek 系模型不猜
        assert_eq!(catalog_capacity("zai-coding-cn", "glm-5.3"), None);
    }

    #[test]
    fn declared_capacity_wins_over_catalog() {
        assert_eq!(resolve_capacity("200K", "huoshan", "deepseek-v4.1-flash"), Some(200_000));
        assert_eq!(
            resolve_capacity("", "huoshan", "deepseek-v4.1-flash"),
            Some(OFFICIAL_DEEPSEEK_CONTEXT_WINDOW)
        );
        assert_eq!(resolve_capacity("", "zai-coding-cn", "glm-5.3"), None);
    }
}
