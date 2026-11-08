//! 手声明路由的 reasoning 档位（上游 `llm-pi-ai/src/catalog.ts` 的
//! `reasoningEfforts` 解析规则 + ui-settings-models `ModelReasoningEfforts`）。
//!
//! 上游语义（逐条照抄）：
//! - 档位顺序 = `THINKING_LEVELS`：`off, minimal, low, medium, high, xhigh, max`（升序）；
//! - 每个档位声明**发送值**（wire）；`off` 可以是空（`null` = 不发）；
//! - 非 `off` 档位的值**不能为空字符串**，也不能缺值；
//! - 至少要有**一个非 off 档位**，否则该模型等于没开推理（应设 `false`）；
//! - `false` = 明确关闭推理；字段缺省 = 沿用安装目录的能力。
//!
//! 这里把「档位列表 + 每档 wire」写成可校验、可序列化的纯数据，UI 只负责
//! 改这份数据。

/// 全部合法档位，升序（上游 `THINKING_LEVELS`）。
pub const THINKING_LEVELS: [&str; 7] =
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 打开开关时写入的初始档位（上游 `INITIAL_LEVELS`）：Off + 常见三档。
pub const INITIAL_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];

/// 一档声明：`level` 是档位名，`wire` 是发送给 provider 的值
/// （`None` 只在 `off` 上合法）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningEffort {
    pub level: String,
    pub wire: Option<String>,
}

impl ReasoningEffort {
    /// 某档的默认发送值（上游 `defaultValue`）：`off` 为空，其余等于档位名。
    pub fn with_default_wire(level: &str) -> Self {
        Self {
            level: level.to_string(),
            wire: if level == "off" { None } else { Some(level.to_string()) },
        }
    }

    /// 开关打开时的初始条目。
    pub fn initial_entries() -> Vec<Self> {
        INITIAL_LEVELS.iter().map(|l| Self::with_default_wire(l)).collect()
    }

    /// 该条目是否会被上游接受（`acceptedValue`）：非空字符串，或 `off` 的 `None`。
    pub fn accepted(&self) -> bool {
        match (&self.wire, self.level.as_str()) {
            (Some(v), _) => !v.is_empty(),
            (None, "off") => true,
            (None, _) => false,
        }
    }
}

/// 校验一份档位声明的第一个问题；`None` = 上游会接受。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffortProblem {
    /// 某档的发送值为空（非 off 档缺值，或写成空字符串）。
    ValueRequired,
    /// 除 off 之外一档都没有。
    NeedsLevel,
}

/// 按上游顺序校验：先逐档看值，再看是否有非 off 档。
pub fn validate(entries: &[ReasoningEffort]) -> Option<EffortProblem> {
    if entries.iter().any(|e| !e.accepted()) {
        return Some(EffortProblem::ValueRequired);
    }
    if !entries.iter().any(|e| e.level != "off") {
        return Some(EffortProblem::NeedsLevel);
    }
    None
}

/// 某档是否已在声明里（上游 `used`）。
pub fn is_used(entries: &[ReasoningEffort], level: &str) -> bool {
    entries.iter().any(|e| e.level == level)
}

/// 「添加档位」时选哪一档（上游 `addLevel`）：优先加入**高于当前最高档**
/// 的第一个未用档；都已用到最高档则退而取任意未用档；全用完返回 `None`。
pub fn next_level(entries: &[ReasoningEffort]) -> Option<&'static str> {
    let highest = entries
        .iter()
        .filter_map(|e| THINKING_LEVELS.iter().position(|l| *l == e.level))
        .max();
    match highest {
        Some(pos) => THINKING_LEVELS
            .iter()
            .skip(pos + 1)
            .find(|l| !is_used(entries, l))
            .or_else(|| THINKING_LEVELS.iter().find(|l| !is_used(entries, l)))
            .copied(),
        None => THINKING_LEVELS.iter().find(|l| !is_used(entries, l)).copied(),
    }
}

/// 改某行的档位名，并按上游规则决定是否沿用原发送值：
/// 两个**思考档之间**的自定义值才带过去；跨到/离开 `off` 或值仍等于旧档名
/// 的一律重置为默认（`changeLevel`）。
pub fn change_level(entry: &ReasoningEffort, to: &str) -> ReasoningEffort {
    let carried = entry
        .wire
        .as_deref()
        .is_some_and(|w| !w.is_empty() && w != entry.level && entry.level != "off" && to != "off");
    let wire = if carried {
        entry.wire.clone()
    } else if to == "off" {
        None
    } else {
        Some(to.to_string())
    };
    ReasoningEffort { level: to.to_string(), wire }
}

/// 命令行式声明 → 条目（`off,low,medium,high` 或 `low=low_v2`）。
///
/// 语法（给编辑框用，比点选区更适合不可点环境）：
/// - 逗号分隔，忽略空白与空项；
/// - `level` 单独出现 = 该档用默认发送值；
/// - `level=wire` = 显式发送值；`off=` 等价于 off 的空值；
/// - 非法档位名保留（上游「未知键不丢」），校验交给 `validate`。
pub fn parse_declaration(text: &str) -> Vec<ReasoningEffort> {
    let mut entries: Vec<ReasoningEffort> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (level, wire) = match part.split_once('=') {
            Some((level, wire)) => {
                let wire = wire.trim();
                (level.trim(), if wire.is_empty() { None } else { Some(wire.to_string()) })
            }
            None => (part, None),
        };
        if level.is_empty() {
            continue;
        }
        // 已声明同档位：后者覆盖（当作改发送值）
        if let Some(existing) = entries.iter_mut().find(|e| e.level == level) {
            existing.wire = wire.or_else(|| {
                if level == "off" { None } else { Some(level.to_string()) }
            });
            continue;
        }
        entries.push(ReasoningEffort {
            level: level.to_string(),
            wire: wire.or_else(|| {
                if level == "off" { None } else { Some(level.to_string()) }
            }),
        });
    }
    // 已知档位按上游升序，未知档位保持输入顺序排在后面
    entries.sort_by_key(|e| {
        THINKING_LEVELS
            .iter()
            .position(|l| *l == e.level)
            .unwrap_or(THINKING_LEVELS.len())
    });
    entries
}

/// 条目 → 命令行式声明（`parse_declaration` 的反向），值等于档位名时省略。
pub fn format_declaration(entries: &[ReasoningEffort]) -> String {
    entries
        .iter()
        .map(|e| match &e.wire {
            Some(wire) if *wire == e.level => e.level.clone(),
            Some(wire) => format!("{}={}", e.level, wire),
            None => e.level.clone(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 序列化成磁盘形状：`{level: wire}`，`off` 的空值写成 YAML `null`。
pub fn to_pairs(entries: &[ReasoningEffort]) -> Vec<(String, Option<String>)> {
    entries.iter().map(|e| (e.level.clone(), e.wire.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_upstream_escalation_order() {
        assert_eq!(
            THINKING_LEVELS,
            ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        // 初始档位是 Off + 常见三档，且全部合法
        let initial = ReasoningEffort::initial_entries();
        assert_eq!(initial.len(), 4);
        assert_eq!(validate(&initial), None);
    }

    #[test]
    fn declaration_text_round_trips() {
        let parsed = parse_declaration("high, off , low=low_v2");
        // 上游升序：off 在 low 前，high 在最后
        assert_eq!(
            parsed.iter().map(|e| e.level.as_str()).collect::<Vec<_>>(),
            vec!["off", "low", "high"]
        );
        assert_eq!(parsed[0].wire, None);
        assert_eq!(parsed[1].wire.as_deref(), Some("low_v2"));
        assert_eq!(parsed[2].wire.as_deref(), Some("high"));
        assert_eq!(validate(&parsed), None);
        assert_eq!(format_declaration(&parsed), "off,low=low_v2,high");
        // 空文本 = 未声明
        assert!(parse_declaration("   ").is_empty());
        // 只有 off → 上游会拒
        assert_eq!(
            validate(&parse_declaration("off")),
            Some(EffortProblem::NeedsLevel)
        );
    }

    #[test]
    fn validation_mirrors_the_pi_ai_refusals() {
        // 只有 off → 没有思考档
        let only_off = vec![ReasoningEffort::with_default_wire("off")];
        assert_eq!(validate(&only_off), Some(EffortProblem::NeedsLevel));
        // 非 off 档空值
        let empty_low = vec![
            ReasoningEffort::with_default_wire("off"),
            ReasoningEffort { level: "low".into(), wire: None },
        ];
        assert_eq!(validate(&empty_low), Some(EffortProblem::ValueRequired));
        // 非 off 档空字符串也算空
        let blank = vec![ReasoningEffort { level: "high".into(), wire: Some(String::new()) }];
        assert_eq!(validate(&blank), Some(EffortProblem::ValueRequired));
        // 完整声明通过
        let ok = vec![
            ReasoningEffort::with_default_wire("off"),
            ReasoningEffort { level: "low".into(), wire: Some("low_v2".into()) },
        ];
        assert_eq!(validate(&ok), None);
    }

    #[test]
    fn adding_a_level_prefers_the_next_higher_one() {
        let mut entries = ReasoningEffort::initial_entries(); // off/low/medium/high
        assert_eq!(next_level(&entries), Some("xhigh"));
        entries.push(ReasoningEffort::with_default_wire("xhigh"));
        assert_eq!(next_level(&entries), Some("max"));
        // 全部用满 → 无可加
        entries.push(ReasoningEffort::with_default_wire("max"));
        entries.push(ReasoningEffort::with_default_wire("minimal"));
        assert_eq!(next_level(&entries), None);
    }

    #[test]
    fn changing_level_keeps_custom_wire_only_between_thinking_levels() {
        let custom = ReasoningEffort { level: "low".into(), wire: Some("low_v2".into()) };
        // 思考档之间：自定义值带过去
        assert_eq!(change_level(&custom, "high").wire.as_deref(), Some("low_v2"));
        // 离开 off 边界：重置
        assert_eq!(change_level(&custom, "off").wire, None);
        // 从 off 出来：重置为档位名
        let off = ReasoningEffort::with_default_wire("off");
        assert_eq!(change_level(&off, "medium").wire.as_deref(), Some("medium"));
        // 值仍等于旧档名：重置为新档名
        let plain = ReasoningEffort::with_default_wire("low");
        assert_eq!(change_level(&plain, "high").wire.as_deref(), Some("high"));
    }
}
