//! `catalog_child_name` 单测：派生出的子名字段必须恒满足上游
//! `isAgentName`（`/^[a-z][a-z0-9-]{0,31}$/`、非 `lead`），因为写侧把它
//! 落进 `subagent/catalog` 的 v3 `name`。description 形态千变万化，派生
//! 必须永不产出非法值。

use dsh_session::{AGENT_NAME_MAX_LEN, ROOT_AGENT_NAME, is_agent_name};
use dsh_subagent::catalog_child_name;

/// 断言派生名满足上游契约（可命名、唯一占位、不撞根名）。
fn assert_valid(name: &str) {
    assert!(is_agent_name(name), "派生名 {name:?} 不满足上游 isAgentName");
    assert_ne!(name, ROOT_AGENT_NAME, "根 agent 名不可用");
    assert!(name.len() <= AGENT_NAME_MAX_LEN, "派生名超上限: {name:?}");
}

#[test]
fn plain_lowercase_description_is_kept_as_a_slug() {
    // 理想形：已经是小写字母描述 → 直接成 name
    let name = catalog_child_name("review the diff", "session-abc");
    assert_valid(&name);
    assert_eq!(name, "review-the-diff");
}

#[test]
fn mixed_case_and_punctuation_are_normalized() {
    // 大写转小写、非字母数字一律压成单连字符、首尾连字符裁掉
    let name = catalog_child_name("Fix Login! (v2)", "session-xyz");
    assert_valid(&name);
    assert_eq!(name, "fix-login-v2");
}

#[test]
fn empty_or_symbol_only_description_falls_back_to_agent_prefix() {
    // 无可用字符：slug 为空 → 回退 agent-<childId 派生>
    for desc in ["", "   ", "!!!", "——", "/\\"] {
        let name = catalog_child_name(desc, "session-1234abcd");
        assert_valid(&name);
        assert!(name.starts_with("agent-"), "回退名应带 agent- 前缀: {name:?}");
    }
}

#[test]
fn fallback_never_collides_with_the_root_name() {
    // 极端：childId 为空时回退到 "child"，仍须合法
    let name = catalog_child_name("", "");
    assert_valid(&name);
    assert_eq!(name, "agent-child");
}

#[test]
fn overlong_description_stays_a_valid_name() {
    // slug 限 8 个词、回退标识限 24 位，最终必须 ≤32 且始终合法
    for desc in [
        "a".repeat(500),
        "one two three four five six seven eight nine ten".to_string(),
        "word ".repeat(200),
        "a-b-c-".repeat(100),
    ] {
        let name = catalog_child_name(&desc, "session-long");
        assert_valid(&name);
        assert!(name.len() <= AGENT_NAME_MAX_LEN, "超上限: {name:?}");
    }
}

#[test]
fn name_is_stable_for_the_same_inputs() {
    // 同一 (description, childId) 两次派生必须一致——父目录名由构造保证唯一，
    // 派生不稳定会破坏「兄弟名唯一」不变量
    let a = catalog_child_name("write tests", "session-same");
    let b = catalog_child_name("write tests", "session-same");
    assert_eq!(a, b);
    assert_eq!(a, "write-tests");
}

#[test]
fn distinct_descriptions_yield_distinct_names() {
    // 上游：兄弟名唯一是父目录的不变量；派生至少按 description 区分
    let a = catalog_child_name("alpha task", "session-1");
    let b = catalog_child_name("beta task", "session-2");
    assert_ne!(a, b, "不同子应派生不同 name，否则撞名");
    assert_valid(&a);
    assert_valid(&b);
}
