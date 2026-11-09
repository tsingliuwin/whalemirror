//! dsh-plugin — 插件系统的应用面：内置插件目录、启用选择（settings.yaml
//! `plugins` 段）与会话标题域纯函数。
//!
//! 上游把「随包发行、默认关闭、用户开启」的能力叫 OPTIONAL_BUNDLES，
//! 由 plugin-manager 管理选择、由 cordis 组装器挂载行；本 crate 提供其
//! Rust 侧对应物中不依赖运行时的部分。外部包安装 / Discover 市场 /
//! `plugin_manager` 等 agent 工具依赖 JS 运行时与 pnpm，面外（backlog）。

pub mod catalog;
pub mod title;

pub use catalog::{
    enabled_from_doc, is_enabled, set_enabled_in_doc, BuiltinPlugin, OFFICIAL_PLUGINS,
    SESSION_TITLES_ID,
};
pub use title::{
    all_prompts_system_prompt, clean_title_text, fallback_title, frame_messages,
    normalize_title, plan_all_prompts_request, title_from_output, truncate_utf8,
    TitleRequestPlan, TitleUserMessage, FALLBACK_MAX_BYTES, FALLBACK_MAX_WORDS, MAX_INPUT_BYTES,
    MAX_OUTPUT_TOKENS, MAX_TITLE_BYTES, TARGET_CJK_CHARACTERS, TARGET_WORDS, TIMEOUT_MS,
};
