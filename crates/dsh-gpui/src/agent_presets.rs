//! Agent 预设（上游 `preset/agent-preset(-registry)` 的 1:1 可行子面）。
//!
//! 上游把预设声明为 cordis 插件行组合（`bundle/web-app/presets/*.patch.yml`）：
//! 每个预设是一份子插件清单，工具面/提示词/压缩策略都由它决定；会话头落
//! `agentPreset`，注册表为会话解析组合，运行中会话保持其组合不变。
//!
//! rustdsh 侧四个内置预设逐字对齐展示元数据与工具门控策略：
//! - `standard`：全部已注册工具（上游 standard 的组合即当前 rustdsh 工具面）；
//! - `ptc`：工具面同 standard（上游差异是 `tool-executor-ptc` 执行器与
//!   workflow 工具启停——执行器层 rustdsh 无对应缝，backlog #104）；
//! - `minimal`：仅终端（`bash`）+ 图片读取（`read_image`）+ 自动压缩
//!   （上游 persona 也换成 "helpful software engineer assistant"，无检索/
//!   web/子代理/计划/todo/技能/交付）；
//! - `cordis`：工具面同 standard（上游差异是创作者技能包——技能系统
//!   面外，backlog #104）。

/// 一个内置预设的展示元数据（上游插件卡的 zh 文案逐字）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentPreset {
    /// 稳定 id（会话头 `agentPreset` / 设置持久化用它）。
    pub id: &'static str,
    /// 展示名（zh）。
    pub name: &'static str,
    /// 卡片右上角徽标（`新增默认` / `内置`；上游 badge）。
    pub badge: &'static str,
    /// 右下角小字 id（上游卡片的 monospace 角标）。
    pub corner: &'static str,
    /// 一句话描述（zh，上游卡片文案）。
    pub description: &'static str,
    /// 花名册顺序（上游 `order`）。
    pub order: u32,
}

/// 内置预设目录（顺序 = 上游 order 1-4）。
pub const PRESETS: &[AgentPreset] = &[
    AgentPreset {
        id: "standard",
        name: "标准模式",
        badge: "新增默认",
        corner: "standard",
        description: "处理代码、文件和资料，适合大多数任务。Agent 会按需使用检索、编辑和终端等工具。",
        order: 1,
    },
    AgentPreset {
        id: "ptc",
        name: "PTC 模式",
        badge: "内置",
        corner: "ptc",
        description: "包含标准模式的全部能力，更适合批量调用工具，并对结果进行筛选、整理、去重、统计汇总的任务。",
        order: 2,
    },
    AgentPreset {
        id: "minimal",
        name: "极简模式",
        badge: "内置",
        corner: "minimal",
        description: "Agent 使用终端和图片读取工具，并支持上下文压缩，适合测试和对比基础表现。",
        order: 3,
    },
    AgentPreset {
        id: "cordis",
        name: "创造模式",
        badge: "内置",
        corner: "cordis",
        description: "用对话定制 DSH：让 Agent 编写插件，添加新功能或界面；也能组合工具和提示词，创建自己的模式。",
        order: 4,
    },
];

/// 部署默认（上游 registry `default: standard`）。
pub const DEFAULT_PRESET: &str = "standard";

/// 按 id 取预设（未知 id → None；调用方回落默认）。
pub fn preset_by_id(id: &str) -> Option<&'static AgentPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// 归一：未知/空 id 回落部署默认（会话头可能带旧值如 `code`——v2 迁移
/// 已把 code 改写为 ptc，这里再兜一层）。
pub fn normalize_preset(id: Option<&str>) -> &'static str {
    id.and_then(preset_by_id).map(|p| p.id).unwrap_or(DEFAULT_PRESET)
}

/// 极简模式允许的工具名（判据源在 `dsh_tools::MINIMAL_PRESET_TOOLS`，
/// 这里保持单一事实）。
pub const MINIMAL_TOOLS: &[&str] = dsh_tools::MINIMAL_PRESET_TOOLS;

/// 工具准入策略：该预设下某个**已注册**工具是否声明。
///
/// standard/ptc/cordis 全放行（注册面即上游 standard 组合的 rustdsh 等价）；
/// minimal 只留终端与图片读取。工具未注册仍按原语义缺席（条件只做二次门）。
/// 判据委托 `dsh_tools::preset_admits`（agent-loop 测试与 UI 共用一份）。
pub fn admits_tool(preset: &str, tool: &str) -> bool {
    dsh_tools::preset_admits(preset, tool)
}

/// 极简模式的 persona（上游 minimal 组合 `persona.complete`——整句替换
/// 默认人设，不再携带运行时上下文前缀）。
pub fn persona_prefix(preset: &str) -> &'static str {
    match preset {
        "minimal" => "You are a helpful software engineer assistant.",
        _ => "You are a coding agent powered by the {{model}} model.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_the_upstream_web_roster() {
        let ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["standard", "ptc", "minimal", "cordis"]);
        assert_eq!(PRESETS[0].badge, "新增默认");
        assert!(PRESETS.iter().all(|p| p.badge == "内置" || p.badge == "新增默认"));
        // 展示名与上游 zh locale 逐字
        assert_eq!(PRESETS[2].name, "极简模式");
        assert_eq!(PRESETS[3].name, "创造模式");
    }

    #[test]
    fn normalize_falls_back_to_deploy_default() {
        assert_eq!(normalize_preset(Some("minimal")), "minimal");
        assert_eq!(normalize_preset(Some("code")), "standard", "未知 id 回落");
        assert_eq!(normalize_preset(None), "standard");
        assert_eq!(normalize_preset(Some("")), "standard");
        assert!(preset_by_id("ptc").is_some());
    }

    #[test]
    fn minimal_gates_everything_but_shell_and_image_read() {
        assert!(admits_tool("minimal", "bash"));
        assert!(admits_tool("minimal", "read_image"));
        for t in ["read", "write", "edit", "fs", "grep", "glob", "web_fetch",
                  "web_search", "subagent", "present", "exit_plan_mode"] {
            assert!(!admits_tool("minimal", t), "minimal 应拒绝 {t}");
        }
        // 其余预设全放行（上游 ptc/cordis 的差异在执行器/技能层）
        for p in ["standard", "ptc", "cordis"] {
            assert!(admits_tool(p, "write"));
            assert!(admits_tool(p, "subagent"));
        }
    }

    #[test]
    fn minimal_swaps_the_persona_line() {
        assert_eq!(persona_prefix("minimal"), "You are a helpful software engineer assistant.");
        assert!(persona_prefix("standard").starts_with("You are a coding agent"));
        assert_eq!(persona_prefix("ptc"), persona_prefix("cordis"));
    }
}
