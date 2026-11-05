//! `catalog_names` 单测：对齐上游 `packages/subagent/subagent/src/names.ts` 的
//! `isAgentName` / `ROOT_AGENT_NAME`，以及 `catalog.ts` 的 `SUBAGENT_CATALOG_VERSION`
//! / `eventDataSchema` / `catalogNames` / `assertReleasedV4Relationships`。
//! 所有判据都取自上游源码或其已发布的 schema，不另立本地规则。

use dsh_session::catalog_names::{
    CatalogConflict, CatalogRow, ROOT_AGENT_NAME, SUBAGENT_CATALOG_VERSION, AGENT_NAME_MAX_LEN,
    catalog_mode_allowed, catalog_mode_requires_label, catalog_names, catalog_version_modes,
    catalog_version_requires_name, find_catalog_conflict, is_agent_name,
};

/// 一行目录事实的构造辅助（版本/名字段是本次校验的两个自变量）。
fn row(version: u8, child_id: &str, seq: u64, name: Option<&str>, mode: &str) -> CatalogRow {
    CatalogRow {
        version,
        child_id: child_id.to_string(),
        seq,
        name: name.map(str::to_string),
        mode: mode.to_string(),
    }
}

// ---- isAgentName（上游 /^[a-z][a-z0-9-]{0,31}$/u 且 !== 'lead'）----

#[test]
fn agent_names_are_lowercase_alnum_hyphen_starting_with_a_letter() {
    // 上游正例：小写字母 + 数字 + 连字符，以字母开头
    assert!(is_agent_name("researcher"));
    assert!(is_agent_name("a1"));
    assert!(is_agent_name("code-review-2"));
    assert!(is_agent_name("z"));
}

#[test]
fn agent_names_reject_uppercase_digits_leading_and_reserved_root() {
    // 上游：首字符必须是 ASCII 小写字母
    assert!(!is_agent_name("1worker"));
    assert!(!is_agent_name("-worker"));
    assert!(!is_agent_name("Worker"));
    assert!(!is_agent_name("WORKER"));
    // 全大写/空格/下划线/点/中文都不在字符集内
    assert!(!is_agent_name("code review"));
    assert!(!is_agent_name("code_review"));
    assert!(!is_agent_name("code.review"));
    assert!(!is_agent_name("编码"));
    // 根 agent 的固定名不可被任何子占用
    assert!(!is_agent_name(ROOT_AGENT_NAME));
    assert!(!is_agent_name("LEAD"));
    // 长度边界：0 位太短、33 位太长；1 位合法（上游 {0,31} 可为 0）
    assert!(!is_agent_name(""));
    assert!(!is_agent_name("a".repeat(33).as_str()));
    assert!(is_agent_name("a"));
    assert!(is_agent_name("a".repeat(32).as_str()));
    assert_eq!(AGENT_NAME_MAX_LEN, 32);
}

// ---- 版本与 mode（上游 eventDataSchema 的判别联合）----

#[test]
fn catalog_write_version_is_three() {
    // 上游 SUBAGENT_CATALOG_VERSION = 3：本地与外部子创建都写 v3
    assert_eq!(SUBAGENT_CATALOG_VERSION, 3);
    assert!(catalog_version_requires_name(SUBAGENT_CATALOG_VERSION));
    // 历史版本（v0–v2）不带 name，读侧回放时不要求
    assert!(!catalog_version_requires_name(0));
    assert!(!catalog_version_requires_name(1));
    assert!(!catalog_version_requires_name(2));
}

#[test]
fn catalog_mode_sets_per_version() {
    // v0：仅 one-shot（旧本地子创建）
    assert_eq!(catalog_version_modes(0), &["one-shot"]);
    assert!(catalog_mode_allowed(0, "one-shot"));
    // v1：one-shot 之外允许 unknown（上游注释：v1 增 unknown 子）
    assert_eq!(catalog_version_modes(1), &["one-shot", "unknown"]);
    assert!(catalog_mode_allowed(1, "unknown"));
    assert!(!catalog_mode_allowed(1, "external"));
    // v2：external（上游注释：v2 记录外部执行）
    assert_eq!(catalog_version_modes(2), &["external"]);
    // v3：continuable（label 必填）或 external，均带 name
    assert_eq!(catalog_version_modes(3), &["continuable", "external"]);
    assert!(catalog_mode_allowed(3, "continuable"));
    assert!(catalog_mode_allowed(3, "external"));
    // 上游 mode 是开放词汇（rustdsh 本地曾出现未知值），未知 mode 不算非法
    assert!(!catalog_mode_allowed(3, "mystery"));
    assert!(!catalog_mode_allowed(2, "one-shot"));
    // 未知版本（未来升级）不硬拒
    assert_eq!(catalog_version_modes(99), &["continuable", "external"]);
}

#[test]
fn continuable_requires_label_others_do_not() {
    // 上游 continuableCatalogSchema 里 label 为 z.string()（必填），
    // one-shot / external / unknown 皆可选
    assert!(catalog_mode_requires_label("continuable"));
    assert!(!catalog_mode_requires_label("one-shot"));
    assert!(!catalog_mode_requires_label("external"));
    assert!(!catalog_mode_requires_label("unknown"));
}

// ---- catalogNames（历史行不参与唯一性判据）----

#[test]
fn catalog_names_skip_unnamed_historical_rows() {
    let rows = [
        row(0, "session-a", 1, None, "one-shot"),      // 历史：无 name
        row(3, "session-b", 2, Some("researcher"), "continuable"), // v3：有 name
        row(2, "session-c", 3, None, "external"),     // 外部历史：无 name
        row(3, "session-d", 4, Some("writer"), "external"),
    ];
    // 上游：只有命名字段参与 name 集合
    assert_eq!(catalog_names(&rows), &["researcher", "writer"]);
    let empty: &[CatalogRow] = &[];
    assert_eq!(catalog_names(empty), &[] as &[&str]);
}

// ---- assertReleasedV4Relationships（重复 childId / 重复 name）----

#[test]
fn unique_ids_and_names_pass() {
    let rows = [
        row(3, "session-a", 1, Some("researcher"), "continuable"),
        row(3, "session-b", 2, Some("writer"), "external"),
        row(0, "session-c", 3, None, "one-shot"), // 历史行：不占 name
    ];
    assert_eq!(find_catalog_conflict(&rows), None);
    assert!(find_catalog_conflict(&[] as &[CatalogRow; 0]).is_none());
}

#[test]
fn duplicate_child_id_is_reported_with_the_first_seq() {
    // 上游：`duplicate catalog child ${id}`；报错必须能指回首次出现位置
    let rows = [
        row(3, "session-a", 10, Some("researcher"), "continuable"),
        row(3, "session-b", 20, Some("writer"), "external"),
        row(3, "session-a", 30, Some("third"), "external"), // 同 id 第二次
    ];
    match find_catalog_conflict(&rows) {
        Some(CatalogConflict::DuplicateId { id, first_seq }) => {
            assert_eq!(id, "session-a");
            assert_eq!(first_seq, 10, "首次出现 seq 应被保留");
        }
        other => panic!("expected DuplicateId, got {other:?}"),
    }
}

#[test]
fn duplicate_name_is_reported_and_id_check_wins_when_both_duplicate() {
    let rows = [
        row(3, "session-a", 1, Some("researcher"), "continuable"),
        row(3, "session-b", 2, Some("writer"), "external"),
        row(3, "session-a", 3, Some("researcher"), "external"),
    ];
    // 同一行既重复 id 又重复 name：上游先查 id（顺序：先 ids 后 names）
    match find_catalog_conflict(&rows) {
        Some(CatalogConflict::DuplicateId { id, first_seq }) => {
            assert_eq!(id, "session-a");
            assert_eq!(first_seq, 1);
        }
        other => panic!("id 检查应先触发，got {other:?}"),
    }
    // 去掉 id 重复后，纯 name 重复应被检出
    let rows2 = [
        row(3, "session-a", 1, Some("researcher"), "continuable"),
        row(3, "session-b", 2, Some("researcher"), "external"),
    ];
    match find_catalog_conflict(&rows2) {
        Some(CatalogConflict::DuplicateName { name }) => assert_eq!(name, "researcher"),
        other => panic!("expected DuplicateName, got {other:?}"),
    }
}

#[test]
fn historical_rows_never_collide_on_name() {
    // 上游：name 唯一性只对 v3 行有意义；v0–v2 无 name 字段，天然不参与
    let rows = [
        row(0, "session-a", 1, None, "one-shot"),
        row(0, "session-b", 2, None, "one-shot"),
        row(0, "session-c", 3, None, "one-shot"),
    ];
    assert_eq!(find_catalog_conflict(&rows), None);
}

#[test]
fn rows_with_distinct_names_but_same_mode_are_independent() {
    // 兄弟名由构造保证唯一：两个 external 子取不同 name 时应通过
    let rows = [
        row(3, "session-a", 1, Some("lint-fix"), "external"),
        row(3, "session-b", 2, Some("doc-build"), "external"),
        row(3, "session-c", 3, Some("test-run"), "external"),
    ];
    assert_eq!(find_catalog_conflict(&rows), None);
}
