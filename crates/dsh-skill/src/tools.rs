//! `skill` 加载工具（上游 `tool-skill` 的模型面）。
//!
//! 模型用**精确技能名**调用，拿到规范 `<skill_content>` 块（含资源基目录提示
//! 与全文正文）。每次加载**重读文件**（目录与正文生命周期分离）。
//! 错误文案对齐上游：非法名 / 未知名 / 对模型不可见。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dsh_llm::ContentBlock;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

use crate::{is_skill_name, render_skill_content, SkillEntry};

/// 目录快照句柄：宿主发现后写入，工具调用时读取（可直接换新目录）。
pub type SkillCatalog = Arc<Mutex<Vec<SkillEntry>>>;

#[derive(Clone)]
pub struct SkillTool {
    catalog: SkillCatalog,
}

impl SkillTool {
    pub fn new(catalog: SkillCatalog) -> Self {
        Self { catalog }
    }

    fn entries(&self) -> Vec<SkillEntry> {
        self.catalog.lock().unwrap().clone()
    }

    /// 目录快照句柄（宿主另用，如 `/name` 显式调用的 user-invocable 判定）。
    pub fn catalog(&self) -> SkillCatalog {
        Arc::clone(&self.catalog)
    }

    /// 按名字取模型可见技能。
    pub fn find(&self, name: &str) -> Option<SkillEntry> {
        self.entries()
            .into_iter()
            .find(|e| e.name == name && e.model_invocable)
    }

    /// 加载一次：重读文件并渲染规范块。
    pub fn load(&self, name: &str) -> Result<String, String> {
        if !is_skill_name(name) {
            return Err(format!("Error: invalid skill name \"{name}\""));
        }
        let entries = self.entries();
        let Some(entry) = entries.iter().find(|e| e.name == name) else {
            return Err(format!(
                "Error: skill \"{name}\" is unknown or no longer available"
            ));
        };
        if !entry.model_invocable {
            return Err(format!(
                "Error: skill \"{name}\" is not available for model invocation"
            ));
        }
        let text = std::fs::read_to_string(&entry.path)
            .map_err(|e| format!("Error: failed to read skill \"{name}\": {e}"))?;
        let (_, body) = crate::parse_skill(&text)
            .map_err(|e| format!("Error: failed to load skill \"{name}\": {e}"))?;
        let base: PathBuf = entry
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(render_skill_content(name, &base, &body))
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "skill".into(),
            description: "Load the full instructions for one available skill.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The exact skill name from the available skills list." }
                },
                "required": ["name"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let name = input
            .arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return ToolExecutionResult::error("Error: invalid skill name \"\"");
        }
        match self.load(&name) {
            Ok(text) => ToolExecutionResult {
                content: vec![ContentBlock::Text { text }],
                is_error: false,
                concludes_turn: false,
                presentation: Some(json!({ "card": "read", "skill": name })),
            },
            Err(text) => ToolExecutionResult::error(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &std::path::Path, name: &str, model_invocable: bool) -> SkillEntry {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: demo\n---\nDo the thing.\n"),
        )
        .unwrap();
        SkillEntry {
            name: name.into(),
            description: "demo".into(),
            path,
            source: "project-agents".into(),
            model_invocable,
            user_invocable: true,
        }
    }

    fn tool(entries: Vec<SkillEntry>) -> SkillTool {
        SkillTool::new(Arc::new(Mutex::new(entries)))
    }

    #[test]
    fn load_returns_the_canonical_block() {
        let root = std::env::temp_dir().join(format!("dsh-skill-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let t = tool(vec![entry(&root, "demo-skill", true)]);
        let text = t.load("demo-skill").unwrap();
        assert!(text.starts_with("<skill_content name=\"demo-skill\">"), "{text}");
        assert!(text.contains("Base directory for this skill:"), "{text}");
        assert!(text.contains("<skill_instructions>\nDo the thing.\n</skill_instructions>"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn errors_match_upstream_wording() {
        let root = std::env::temp_dir().join(format!("dsh-skill-tool-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let t = tool(vec![entry(&root, "demo-skill", true)]);
        assert_eq!(t.load("Bad Name").unwrap_err(), "Error: invalid skill name \"Bad Name\"");
        assert_eq!(
            t.load("missing-skill").unwrap_err(),
            "Error: skill \"missing-skill\" is unknown or no longer available"
        );
        let hidden = tool(vec![entry(&root, "hidden-skill", false)]);
        assert_eq!(
            hidden.load("hidden-skill").unwrap_err(),
            "Error: skill \"hidden-skill\" is not available for model invocation"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_rereads_the_file_every_time() {
        let root = std::env::temp_dir().join(format!("dsh-skill-tool-reread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let e = entry(&root, "demo-skill", true);
        let path = e.path.clone();
        let t = tool(vec![e]);
        assert!(t.load("demo-skill").unwrap().contains("Do the thing."));
        std::fs::write(&path, "---\nname: demo-skill\ndescription: demo\n---\nNew body.\n").unwrap();
        assert!(t.load("demo-skill").unwrap().contains("New body."));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn tool_reports_errors_as_tool_errors() {
        let root = std::env::temp_dir().join(format!("dsh-skill-tool-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let t = tool(vec![entry(&root, "demo-skill", true)]);
        let r = t
            .execute(&ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("c1".into()),
                "skill".into(),
                r#"{"name":"missing-skill"}"#.into(),
            ))
            .await;
        assert!(r.is_error);
        let _ = std::fs::remove_dir_all(&root);
    }
}
