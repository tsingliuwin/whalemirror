//! 内置插件目录与启用选择（上游 plugin-manager 的 1:1 可行子面）。
//!
//! 上游把「随包发行、默认关闭、用户在 Plugins 页开启」的能力叫
//! OPTIONAL_BUNDLES（`util/official-bundles`）：安装选择持久化在 profile
//! 的 bundle 列表里，关闭 = 从选择中移除。rustdsh 把选择写成
//! settings.yaml 的 `plugins.enabled` 列表（同样语义：开启 = 追加、
//! 关闭 = 移除，其余段原样）。
//!
//! 外部 npm/pnpm 包安装、Discover 市场、`plugin_manager`/`plugin_market`
//! agent 工具依赖 JS 运行时与包管理器，面外（backlog 登记）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 一个内置插件条目（上游 bundle 卡的展示元数据 + 行选择）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinPlugin {
    /// 稳定 id（磁盘选择列表用它）。
    pub id: &'static str,
    /// 展示名（zh）。
    pub name: &'static str,
    /// 一句话描述（zh）。
    pub description: &'static str,
    /// 上游实验包标记（卡片上的 Experimental 徽标）。
    pub experimental: bool,
}

/// 内置插件目录（上游 OPTIONAL_BUNDLES 中 rustdsh 已实装者；其余在
/// backlog 逐个评估，不放空壳条目）。
pub const OFFICIAL_PLUGINS: &[BuiltinPlugin] = &[
    BuiltinPlugin {
        id: "session-titles",
        name: "会话随动标题",
        description: "从会话中合格的用户提问持续生成标题（随新提问刷新），替换默认的首条提问标题。",
        experimental: true,
    },
];

/// session-titles 的插件 id（触发点按目录匹配）。
pub const SESSION_TITLES_ID: &str = "session-titles";

/// 读 settings.yaml 文档里的启用集合（`plugins.enabled`，未知 id 保留）。
pub fn enabled_from_doc(doc: &serde_yaml::Value) -> Vec<String> {
    let Some(serde_yaml::Value::Sequence(items)) =
        doc.get("plugins").and_then(|p| p.get("enabled"))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// 把启用集合写回 settings.yaml 文档（仅动我们拥有的 `plugins` 段；
/// 返回文档是否变化）。
pub fn set_enabled_in_doc(doc: &mut serde_yaml::Value, id: &str, on: bool) -> bool {
    if !doc.is_mapping() {
        return false;
    }
    let mut enabled: Vec<String> = enabled_from_doc(doc);
    let present = enabled.iter().any(|e| e == id);
    if on == present {
        return false;
    }
    if on {
        enabled.push(id.to_string());
    } else {
        enabled.retain(|e| e != id);
    }
    // 稳定顺序：去重排序（磁盘形状确定性）
    let set: BTreeSet<String> = enabled.into_iter().collect();
    let seq: serde_yaml::Value = serde_yaml::Value::Sequence(
        set.into_iter().map(serde_yaml::Value::String).collect(),
    );
    let mapping = doc.as_mapping_mut().expect("checked above");
    let plugins = mapping
        .entry(serde_yaml::Value::String("plugins".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    if let serde_yaml::Value::Mapping(pm) = plugins {
        pm.insert(
            serde_yaml::Value::String("enabled".into()),
            seq,
        );
    }
    true
}

/// 某插件当前是否启用。
pub fn is_enabled(enabled: &[String], id: &str) -> bool {
    enabled.iter().any(|e| e == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(text: &str) -> serde_yaml::Value {
        serde_yaml::from_str(text).unwrap()
    }

    #[test]
    fn catalog_matches_the_upstream_optional_bundle_shape() {
        assert_eq!(OFFICIAL_PLUGINS.len(), 1);
        let t = &OFFICIAL_PLUGINS[0];
        assert_eq!(t.id, "session-titles");
        assert!(t.experimental);
        assert!(is_enabled(&["session-titles".into()], SESSION_TITLES_ID));
    }

    #[test]
    fn reads_enabled_list_and_tolerates_missing_section() {
        let empty = doc_with("ui-theme:\n  preference: dark\n");
        assert!(enabled_from_doc(&empty).is_empty());
        let full = doc_with("plugins:\n  enabled:\n  - session-titles\n");
        assert_eq!(enabled_from_doc(&full), vec!["session-titles".to_string()]);
        // 形状不对（非列表/非字符串）按空处理
        let bad = doc_with("plugins:\n  enabled: 3\n");
        assert!(enabled_from_doc(&bad).is_empty());
    }

    #[test]
    fn toggling_writes_only_the_plugins_section() {
        let mut doc = doc_with("ui-theme:\n  preference: dark\nlocale:\n  preference: zh\n");
        assert!(set_enabled_in_doc(&mut doc, "session-titles", true));
        assert_eq!(
            enabled_from_doc(&doc),
            vec!["session-titles".to_string()]
        );
        assert_eq!(
            doc.get("ui-theme").and_then(|v| v.get("preference")).and_then(|v| v.as_str()),
            Some("dark"),
            "其余段原样"
        );
        // 幂等：重复开不变
        assert!(!set_enabled_in_doc(&mut doc, "session-titles", true));
        // 关闭 = 移除
        assert!(set_enabled_in_doc(&mut doc, "session-titles", false));
        assert!(enabled_from_doc(&doc).is_empty());
        // 关到已关 = 不变
        assert!(!set_enabled_in_doc(&mut doc, "session-titles", false));
    }

    #[test]
    fn enabled_list_is_deduplicated_and_sorted() {
        let mut doc = doc_with("plugins:\n  enabled:\n  - session-titles\n  - session-titles\n");
        assert!(set_enabled_in_doc(&mut doc, "another", true));
        let enabled = enabled_from_doc(&doc);
        assert_eq!(enabled, vec!["another".to_string(), "session-titles".to_string()]);
    }
}
