//! dsh-skill — 技能发现、目录注入与按需加载（上游 `packages/skill/*` 的等价面）。
//!
//! 三个包合成一个 crate：
//! - `skill`：注册表词汇（kebab-case 名字规则、`renderSkillContent`）；
//! - `skill-filesystem`：磁盘发现（五个根 + 秩、`<name>/SKILL.md` 与
//!   顶层 `<name>.md` 两种形态、frontmatter 解析）；
//! - `tool-skill`：会话目录消息与 `skill` 加载工具。
//!
//! 目录与正文生命周期分离：发现只解析 frontmatter，每次加载**重读文件**。

pub mod tools;

use std::path::{Path, PathBuf};

pub use tools::{SkillCatalog, SkillTool};

use dsh_llm::ContentBlock;

/// 技能名规则（上游 `SKILL_NAME`：kebab-case）。
pub fn is_skill_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut prev_dash = true; // 首字符不能是 '-'
    for ch in name.chars() {
        match ch {
            'a'..='z' | '0'..='9' => prev_dash = false,
            '-' => {
                if prev_dash {
                    return false;
                }
                prev_dash = true;
            }
            _ => return false,
        }
    }
    !prev_dash
}

/// 一条目录项（上游 `SkillSummary` 的模型可见子集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// 指令文件绝对路径（加载时重读它）。
    pub path: PathBuf,
    /// 来源桶（`project-dsh` / `project-agents` / `custom` / `user-dsh` / `user-agents` / `bundled`）。
    pub source: String,
    /// 是否进模型面目录与加载器（`disable-model-invocation: true` 时为 false）。
    pub model_invocable: bool,
    /// 是否可被 `/name` 直接调用（`user-invocable: false` 时为 false）。
    pub user_invocable: bool,
}

/// 发现根（秩越小优先级越高）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRoot {
    pub source: &'static str,
    pub rank: u32,
    pub path: PathBuf,
}

/// 默认根顺序（上游秩表：project-dsh 100 / project-agents 200 / custom 300 /
/// user-dsh 400 / user-agents 500；bundled 600 由调用方追加）。
pub fn default_roots(project_root: &Path, dsh_home: &Path, agents_home: &Path) -> Vec<SkillRoot> {
    vec![
        SkillRoot { source: "project-dsh", rank: 100, path: project_root.join(".dsh/skills") },
        SkillRoot { source: "project-agents", rank: 200, path: project_root.join(".agents/skills") },
        SkillRoot { source: "user-dsh", rank: 400, path: dsh_home.join("skills") },
        SkillRoot { source: "user-agents", rank: 500, path: agents_home.join("skills") },
    ]
}

/// 项目根：最近的含 `.git` 的祖先目录；找不到则用 cwd（上游同语义）。
pub fn project_root_of(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return dir;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return start.to_path_buf(),
        }
    }
}

/// 解析 frontmatter（`---` 包裹的顶层 `key: value`）。
///
/// 只取我们需要的键；布尔键接受 YAML 的 `true/false/yes/no/on/off/1/0`
/// （大小写不敏感）。被拒的拼写 → 整条技能丢弃（上游「不静默放行」语义）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub model_invocable: bool,
    pub user_invocable: bool,
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 {
        let bytes = v.as_bytes();
        if (bytes[0] == b'"' && bytes[v.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[v.len() - 1] == b'\'')
        {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// 解析一份 SKILL.md：返回 frontmatter 与正文。
pub fn parse_skill(text: &str) -> Result<(SkillFrontmatter, String), String> {
    let mut lines = text.lines();
    let mut fm: Vec<&str> = Vec::new();
    let mut body = String::new();
    // 首行必须是 `---`（允许前置空行）
    let mut started = false;
    for line in lines.by_ref() {
        if !started {
            if line.trim().is_empty() {
                continue;
            }
            if line.trim() == "---" {
                started = true;
                continue;
            }
            return Err("skill is missing its frontmatter block".into());
        }
        if line.trim() == "---" {
            break;
        }
        fm.push(line);
    }
    if !started {
        return Err("skill is missing its frontmatter block".into());
    }
    // 剩余即正文
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    let mut out = SkillFrontmatter { model_invocable: true, user_invocable: true, ..Default::default() };
    for line in fm {
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => out.name = unquote(value),
            "description" => out.description = unquote(value),
            "whenToUse" => out.when_to_use = Some(unquote(value)),
            "disable-model-invocation" => {
                out.model_invocable = !parse_bool(value)
                    .ok_or_else(|| format!("invalid boolean for disable-model-invocation: {value}"))?;
            }
            "user-invocable" => {
                out.user_invocable = parse_bool(value)
                    .ok_or_else(|| format!("invalid boolean for user-invocable: {value}"))?;
            }
            _ => {}
        }
    }
    if out.name.is_empty() {
        return Err("skill frontmatter is missing `name`".into());
    }
    if out.description.is_empty() {
        return Err("skill frontmatter is missing `description`".into());
    }
    if !is_skill_name(&out.name) {
        return Err(format!("invalid skill name {:?}", out.name));
    }
    Ok((out, body))
}

/// 扫描一个根：只认顶层 `<name>/SKILL.md` 与 `<name>.md`（嵌套不发现）。
/// 返回技能列表与失败计数（失败 = 解析不通过的目录项）。
pub fn scan_root(root: &SkillRoot) -> (Vec<SkillEntry>, usize) {
    let mut out = Vec::new();
    let mut failed = 0usize;
    let Ok(entries) = std::fs::read_dir(&root.path) else { return (out, failed) };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                dirs.push(skill);
            }
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            dirs.push(path);
        }
    }
    dirs.sort();
    for file in dirs {
        match std::fs::read_to_string(&file) {
            Ok(text) => match parse_skill(&text) {
                Ok((fm, _)) => out.push(SkillEntry {
                    name: fm.name,
                    description: fm.description,
                    path: file.canonicalize().unwrap_or(file),
                    source: root.source.to_string(),
                    model_invocable: fm.model_invocable,
                    user_invocable: fm.user_invocable,
                }),
                Err(_) => failed += 1,
            },
            Err(_) => failed += 1,
        }
    }
    (out, failed)
}

/// 全根发现：按秩升序，同名**首个**生效（上游「优先级高的定义胜出」）。
/// 返回（技能表, 是否有失败）。
pub fn discover(roots: &[SkillRoot]) -> (Vec<SkillEntry>, bool) {
    let mut ordered: Vec<&SkillRoot> = roots.iter().collect();
    ordered.sort_by_key(|r| r.rank);
    let mut out: Vec<SkillEntry> = Vec::new();
    let mut incomplete = false;
    for root in ordered {
        let (found, failed) = scan_root(root);
        incomplete |= failed > 0;
        for entry in found {
            if out.iter().any(|e| e.name == entry.name) {
                continue; // 低秩同名让位
            }
            out.push(entry);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, incomplete)
}

/// 会话目录消息正文（上游 `renderCatalogMessage` 逐字）。
pub fn render_catalog(entries: &[SkillEntry], incomplete: bool, description_max: usize) -> String {
    let lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let description: String = e.description.chars().take(description_max).collect();
            format!("- `{}`: {}", e.name, description)
        })
        .collect();
    let mut text: Vec<String> = vec!["<system-reminder>".into()];
    text.push(if entries.is_empty() {
        "No skills could be confirmed in this discovery attempt.".into()
    } else {
        "A skill is a reusable set of task-specific instructions. The following skills are available in this session:".into()
    });
    text.push(String::new());
    text.push("<available_skills>".into());
    text.extend(lines);
    text.push("</available_skills>".into());
    text.push(String::new());
    if incomplete {
        text.push("Some skills could not be checked. Only confirmed skills are listed; a lower-priority definition of the same name may be selected when a preferred source is unavailable. Discovery will be retried on the next request.".into());
        text.push(String::new());
    }
    text.push(if entries.is_empty() {
        "Wait for an updated catalog before calling the `skill` tool. Do not infer skill instructions from an empty catalog.".into()
    } else {
        "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.".into()
    });
    text.push("A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.".into());
    text.push("</system-reminder>".into());
    text.join("\n")
}

/// 加载结果的规范块（上游 `renderSkillContent` 逐字）。
pub fn render_skill_content(name: &str, base_dir: &Path, body: &str) -> String {
    format!(
        "<skill_content name=\"{name}\">\n\
         <skill_resources>\n\
         Base directory for this skill: {base}\n\
         Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n\
         </skill_resources>\n\
         \n\
         <skill_instructions>\n{body}</skill_instructions>\n\
         </skill_content>",
        base = base_dir.display()
    )
}

/// 目录消息（`source` 走 plugin 形，`form: catalog`）。
pub fn catalog_message(text: String, entries: usize) -> dsh_llm::Message {
    dsh_llm::Message::new(
        dsh_llm::Role::User,
        vec![ContentBlock::text(text.clone())],
        dsh_llm::MessageSource::Context {
            context_kind: "skill-catalog".into(),
            plugin: None,
            form: Some("catalog".into()),
            summary: Some(format!("{entries} skills available")),
            changes_paths: Vec::new(),
            reference_labels: Vec::new(),
            name: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, description: &str, extra: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("SKILL.md");
        std::fs::write(
            &file,
            format!("---\nname: {name}\ndescription: {description}\n{extra}---\n{body}"),
        )
        .unwrap();
        file
    }

    #[test]
    fn names_are_kebab_case() {
        assert!(is_skill_name("office-docx"));
        assert!(is_skill_name("a"));
        assert!(is_skill_name("a1-b2"));
        assert!(!is_skill_name(""));
        assert!(!is_skill_name("Office"));
        assert!(!is_skill_name("-a"));
        assert!(!is_skill_name("a-"));
        assert!(!is_skill_name("a--b"));
        assert!(!is_skill_name("a_b"));
    }

    #[test]
    fn frontmatter_parses_required_and_optional_keys() {
        let (fm, body) = parse_skill(
            "---\nname: demo\ndescription: \"A demo skill\"\nwhenToUse: 'when testing'\n---\nBody line\n",
        )
        .unwrap();
        assert_eq!(fm.name, "demo");
        assert_eq!(fm.description, "A demo skill");
        assert_eq!(fm.when_to_use.as_deref(), Some("when testing"));
        assert!(fm.model_invocable && fm.user_invocable);
        assert_eq!(body, "Body line\n");
    }

    #[test]
    fn invocation_flags_follow_the_documented_spellings() {
        let (fm, _) = parse_skill(
            "---\nname: demo\ndescription: d\ndisable-model-invocation: YES\nuser-invocable: 0\n---\n",
        )
        .unwrap();
        assert!(!fm.model_invocable, "YES 视为 true → 对模型不可见");
        assert!(!fm.user_invocable, "0 视为 false → 对人不开放");
        // 拒绝的拼写：整条丢弃
        assert!(parse_skill("---\nname: demo\ndescription: d\nuser-invocable: maybe\n---\n").is_err());
    }

    #[test]
    fn required_keys_and_name_grammar_are_enforced() {
        assert!(parse_skill("---\ndescription: d\n---\n").is_err(), "缺 name");
        assert!(parse_skill("---\nname: demo\n---\n").is_err(), "缺 description");
        assert!(parse_skill("---\nname: Demo\ndescription: d\n---\n").is_err(), "非法名字");
        assert!(parse_skill("no frontmatter").is_err());
    }

    #[test]
    fn discovery_prefers_the_higher_rank_and_ignores_nested_files() {
        let root_dir = std::env::temp_dir().join(format!("dsh-skill-{}-1", std::process::id()));
        let _ = std::fs::remove_dir_all(&root_dir);
        let high = root_dir.join("high");
        let low = root_dir.join("low");
        std::fs::create_dir_all(&high).unwrap();
        std::fs::create_dir_all(&low).unwrap();
        write_skill(&high, "shared", "from high", "", "HIGH\n");
        write_skill(&low, "shared", "from low", "", "LOW\n");
        write_skill(&high, "only-high", "unique", "", "X\n");
        // 嵌套的 SKILL.md 不应被发现
        let nested = high.join("nested/deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("SKILL.md"),
            "---\nname: nested\ndescription: n\n---\n",
        )
        .unwrap();

        let roots = vec![
            SkillRoot { source: "low", rank: 500, path: low },
            SkillRoot { source: "high", rank: 100, path: high },
        ];
        let (entries, incomplete) = discover(&roots);
        assert!(!incomplete);
        assert_eq!(entries.len(), 2, "{entries:?}");
        let shared = entries.iter().find(|e| e.name == "shared").unwrap();
        assert_eq!(shared.description, "from high", "高秩胜出");
        assert!(entries.iter().all(|e| e.name != "nested"), "嵌套不发现");
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn flat_md_files_are_discovered_and_bad_skills_count_as_incomplete() {
        let root_dir = std::env::temp_dir().join(format!("dsh-skill-{}-2", std::process::id()));
        let _ = std::fs::remove_dir_all(&root_dir);
        std::fs::create_dir_all(&root_dir).unwrap();
        std::fs::write(
            root_dir.join("flat-one.md"),
            "---\nname: flat-one\ndescription: flat\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(root_dir.join("broken.md"), "---\nname: Broken\ndescription: x\n---\n").unwrap();
        let (entries, incomplete) = discover(&[SkillRoot {
            source: "custom",
            rank: 300,
            path: root_dir.clone(),
        }]);
        assert!(incomplete, "解析失败要报 incomplete");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "flat-one");
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn catalog_and_content_render_in_upstream_shape() {
        let entries = vec![SkillEntry {
            name: "office-docx".into(),
            description: "Word workflows".into(),
            path: PathBuf::from("/x/SKILL.md"),
            source: "project-agents".into(),
            model_invocable: true,
            user_invocable: true,
        }];
        let text = render_catalog(&entries, false, 500);
        assert!(text.starts_with("<system-reminder>\nA skill is a reusable set"), "{text}");
        assert!(text.contains("<available_skills>"), "{text}");
        assert!(text.contains("- `office-docx`: Word workflows"), "{text}");
        assert!(text.contains("</available_skills>"), "{text}");
        assert!(text.ends_with("</system-reminder>"), "{text}");
        assert!(text.contains("call the `skill` tool with the exact skill name"), "{text}");

        let empty = render_catalog(&[], false, 500);
        assert!(empty.contains("No skills could be confirmed"), "{empty}");
        assert!(empty.contains("Do not infer skill instructions from an empty catalog."), "{empty}");

        let incomplete = render_catalog(&entries, true, 500);
        

        let content = render_skill_content("office-docx", Path::new("/skills/office-docx"), "Do the thing\n");
        assert!(content.starts_with("<skill_content name=\"office-docx\">"), "{content}");
        assert!(content.contains("<skill_resources>"), "{content}");
        assert!(content.contains("Base directory for this skill: /skills/office-docx"), "{content}");
        assert!(content.contains("<skill_instructions>\nDo the thing\n</skill_instructions>"), "{content}");
        assert!(content.ends_with("</skill_content>"), "{content}");
    }
}
