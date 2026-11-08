//! reasoning 档位在 settings.yaml 形状上的往返（#42 数据面）。
//!
//! 上游把 `reasoningEfforts` 存成 `{level: wire|null}`；`off` 的空值序列化成
//! YAML `null`，其余档位必须是字符串。这里验证：
//! 1. 解析：缺省 / 空映射 → 未声明；映射按上游升序排列，未知键保留在末尾；
//! 2. 写回：`None` 值写成 `null`，空声明不写字段；
//! 3. 校验：只有 off、或非 off 空值 → 上游会拒绝，本地校验能提前报出。

use dsh_gpui::reasoning_efforts::{
    EffortProblem, ReasoningEffort, THINKING_LEVELS, next_level, validate,
};
use serde_yaml::{Mapping, Value};

/// 复刻 GUI 侧的解析（顺序：已知档位按升序，未知键追加）。
fn parse(value: Option<&Value>) -> Option<Vec<ReasoningEffort>> {
    let map = value?.as_mapping()?;
    if map.is_empty() {
        return None;
    }
    let mut entries = Vec::new();
    for level in THINKING_LEVELS {
        let key = Value::String(level.to_string());
        if let Some(wire) = map.get(&key) {
            entries.push(ReasoningEffort {
                level: level.to_string(),
                wire: wire.as_str().map(str::to_string),
            });
        }
    }
    for (key, wire) in map {
        let Some(level) = key.as_str() else { continue };
        if !THINKING_LEVELS.contains(&level) {
            entries.push(ReasoningEffort {
                level: level.to_string(),
                wire: wire.as_str().map(str::to_string),
            });
        }
    }
    Some(entries)
}

fn to_yaml(entries: &[ReasoningEffort]) -> Value {
    let mut map = Mapping::new();
    for entry in entries {
        map.insert(
            Value::String(entry.level.clone()),
            match &entry.wire {
                Some(wire) => Value::String(wire.clone()),
                None => Value::Null,
            },
        );
    }
    Value::Mapping(map)
}

#[test]
fn yaml_round_trip_preserves_levels_and_wires() {
    let yaml: Value = serde_yaml::from_str(
        "reasoningEfforts:\n  high: high_v2\n  off: null\n  low: low\n",
    )
    .unwrap();
    let entries = parse(yaml.get("reasoningEfforts")).expect("declared");
    // 上游升序：off 在 low 之前
    assert_eq!(
        entries.iter().map(|e| e.level.as_str()).collect::<Vec<_>>(),
        vec!["off", "low", "high"]
    );
    assert_eq!(entries[0].wire, None, "off 不发送值");
    assert_eq!(entries[2].wire.as_deref(), Some("high_v2"));

    let back = to_yaml(&entries);
    assert_eq!(back.get("off"), Some(&Value::Null));
    assert_eq!(back.get("high").and_then(|v| v.as_str()), Some("high_v2"));
    assert_eq!(validate(&entries), None);
}

#[test]
fn absent_and_empty_declarations_mean_inherit() {
    assert_eq!(parse(None), None);
    let empty: Value = serde_yaml::from_str("reasoningEfforts: {}").unwrap();
    assert_eq!(parse(empty.get("reasoningEfforts")), None);
}

#[test]
fn unknown_levels_survive_a_round_trip() {
    let yaml: Value = serde_yaml::from_str("reasoningEfforts:\n  low: low\n  swedish: lagom\n").unwrap();
    let entries = parse(yaml.get("reasoningEfforts")).expect("declared");
    assert!(entries.iter().any(|e| e.level == "swedish"));
    let back = to_yaml(&entries);
    assert_eq!(back.get("swedish").and_then(|v| v.as_str()), Some("lagom"));
}

#[test]
fn refusal_cases_are_caught_before_writing() {
    // 只有 off
    let only_off = vec![ReasoningEffort::with_default_wire("off")];
    assert_eq!(validate(&only_off), Some(EffortProblem::NeedsLevel));
    // 非 off 缺值
    let missing = vec![
        ReasoningEffort::with_default_wire("off"),
        ReasoningEffort { level: "medium".into(), wire: None },
    ];
    assert_eq!(validate(&missing), Some(EffortProblem::ValueRequired));
    // 加档时优先补更高的档
    assert_eq!(next_level(&only_off), Some("minimal"));
}
