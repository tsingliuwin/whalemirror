//! settings.yaml 提供方段的合并语义（persist 的纯面，bin 调用）。
//!
//! 历史：persist 曾对 `llm-pi-ai.providers` 做**全量替换**——内存形态陈旧
//! （缺路由/缺新字段）时把磁盘上完好的提供方静默抹掉（2026-11-13 事故：
//! zai-coding-cn 整条路由消失，凭据完好却无模型可路由，用户完全无法发送）。
//! 合并规则：磁盘既有条目为基底 → 内存已知路由逐项 upsert → 只有显式
//! 墓碑才删。残留偏差：单条路由内部仍是内存权威（旧字段不会被本函数
//! 复原——加载器本就只读已知键，属版本升级边界问题，非会话内丢失）。

/// 以磁盘既有 `llm-pi-ai.providers` mapping 为基底：`defs`（路由 → 定义）
/// 逐项 upsert，`removed` 墓碑删除，返回合并后的 providers mapping。
pub fn merge_provider_defs(
    existing: Option<serde_yaml::Mapping>,
    defs: Vec<(String, serde_yaml::Mapping)>,
    removed: &[String],
) -> serde_yaml::Mapping {
    let mut providers = existing.unwrap_or_default();
    for (route, def) in defs {
        providers.insert(serde_yaml::Value::String(route), serde_yaml::Value::Mapping(def));
    }
    for route in removed {
        providers.remove(serde_yaml::Value::String(route.clone()));
    }
    providers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(pairs: &[(&str, &str)]) -> serde_yaml::Mapping {
        let mut m = serde_yaml::Mapping::new();
        for (k, v) in pairs {
            m.insert(serde_yaml::Value::String((*k).into()), serde_yaml::Value::String((*v).into()));
        }
        m
    }

    #[test]
    fn keeps_disk_routes_missing_from_memory() {
        // 事故形态：内存只有 huoshan（陈旧进程/缺路由），磁盘还有 zai——
        // 合并必须保留 zai。
        let disk = mapping(&[("zai-coding-cn", "keep-me")]);
        let defs = vec![("huoshan".to_string(), mapping(&[("api", "openai-completions")]))];
        let merged = merge_provider_defs(Some(disk), defs, &[]);
        assert_eq!(merged.len(), 2, "磁盘条目不得被内存缺失抹掉");
        assert!(merged.contains_key(serde_yaml::Value::String("zai-coding-cn".into())));
        assert!(merged.contains_key(serde_yaml::Value::String("huoshan".into())));
    }

    #[test]
    fn upserts_known_routes_over_disk_shape() {
        let disk = mapping(&[("huoshan", "old-stale-def")]);
        let defs = vec![("huoshan".to_string(), mapping(&[("baseURL", "https://new")]))];
        let merged = merge_provider_defs(Some(disk), defs, &[]);
        assert_eq!(merged.len(), 1);
        match merged.get(serde_yaml::Value::String("huoshan".into())) {
            Some(serde_yaml::Value::Mapping(m)) => {
                assert_eq!(m.get(serde_yaml::Value::String("baseURL".into())).and_then(|v| v.as_str()), Some("https://new"), "内存已知路由以内存为准");
            }
            other => panic!("应为 mapping，实际 {other:?}"),
        }
    }

    #[test]
    fn tombstone_removes_explicitly_deleted_route_only() {
        let disk = mapping(&[("a", "x"), ("b", "y"), ("c", "z")]);
        let merged = merge_provider_defs(Some(disk), Vec::new(), &["b".to_string()]);
        assert_eq!(merged.len(), 2, "墓碑只删被点名的");
        assert!(!merged.contains_key(serde_yaml::Value::String("b".into())));
        assert!(merged.contains_key(serde_yaml::Value::String("a".into())));
        assert!(merged.contains_key(serde_yaml::Value::String("c".into())));
    }

    #[test]
    fn empty_disk_creates_fresh_mapping() {
        let defs = vec![("deepseek-official".to_string(), mapping(&[("api", "anthropic-messages")]))];
        let merged = merge_provider_defs(None, defs, &[]);
        assert_eq!(merged.len(), 1);
    }
}
