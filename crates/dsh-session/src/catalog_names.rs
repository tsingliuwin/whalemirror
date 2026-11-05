//! 子代理目录事实的版本与名字规则（上游 `packages/subagent/subagent/src/catalog.ts`
//! 的 `SUBAGENT_CATALOG_VERSION` / `nameSchema` 与 `names.ts` 的 `isAgentName`
//! / `catalogNames` 的等价实现）。
//!
//! 上游把 `subagent/catalog` 事件的字段按版本区分：
//!   - v0/v1：`one-shot`（v1 亦可 `unknown`，label 可选）
//!   - v2：     `external`
//!   - v3：     `continuable`（label 必填）或 `external`，**每个子必须带 `name`**，
//!             且 `name` 在父目录内唯一
//!
//! 写侧固定 v3（上游 `SUBAGENT_CATALOG_VERSION = 3`）；读侧要认得 v0–v3 的历史行
//! 才能回放旧日志，所以这里把「某版本是否要求 name」「某 mode 是否合法」「name
//! 是否唯一」写成纯函数，便于单测锁定。

/// 上游 `SUBAGENT_CATALOG_VERSION`：本地与外部子创建的落盘版本。
pub const SUBAGENT_CATALOG_VERSION: u8 = 3;

/// 上游 `ROOT_AGENT_NAME`：根 agent 的固定名，任何子都不得取它。
pub const ROOT_AGENT_NAME: &str = "lead";

/// 上游 `NAME_PATTERN`：`/^[a-z][a-z0-9-]{0,31}$/u`——首字母小写，其后最多 31 位
/// 小写字母/数字/连字符（总长 ≤ 32；36 字符 UUID 永不匹配）。
pub const AGENT_NAME_MAX_LEN: usize = 32;

/// 上游 `isAgentName`：模型面向的子名字段。1–32 位（`NAME_PATTERN`
/// 为 `/^[a-z][a-z0-9-]{0,31}$/`：首字母小写，其后 0–31 位小写字母/数字/连字符），
/// 且不得等于 `lead`。上游错误文案写作「use 1-32 lower-case letters, digits,
/// or hyphens starting with a letter」，与正则一致（单字符名合法）。
pub fn is_agent_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > AGENT_NAME_MAX_LEN {
        return false;
    }
    if value == ROOT_AGENT_NAME {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 某个版本是否要求子必须带 `name`（v3 才有；历史行不带）。
pub fn catalog_version_requires_name(version: u8) -> bool {
    version >= SUBAGENT_CATALOG_VERSION
}

/// 该版本下合法的 mode 集合（上游 `eventDataSchema` 的 discriminated union）：
/// v0/v1 = `one-shot`（v1 另允许 `unknown`）、v2 = `external`、v3 = `continuable`
/// 或 `external`。历史 mode 词汇是开放的（`unknown` 等），所以未知值不算非法。
pub fn catalog_version_modes(version: u8) -> &'static [&'static str] {
    match version {
        0 => &["one-shot"],
        1 => &["one-shot", "unknown"],
        2 => &["external"],
        _ => &["continuable", "external"],
    }
}

/// 某个 mode 在该版本下是否合法。
pub fn catalog_mode_allowed(version: u8, mode: &str) -> bool {
    catalog_version_modes(version).contains(&mode)
}

/// 该 mode 是否要求 `label` 必填（上游 `continuableCatalogSchema` 里 label 为
/// `z.string()`，one-shot/external/unknown 皆可选）。
pub fn catalog_mode_requires_label(mode: &str) -> bool {
    mode == "continuable"
}

/// 父目录下所有已命名子代的 name（历史 v0–v2 行不带 name，自然跳过）。
pub fn catalog_names<'a>(rows: impl IntoIterator<Item = &'a CatalogRow>) -> Vec<&'a str> {
    rows.into_iter().filter_map(|r| r.name.as_deref()).collect()
}

/// 父目录下的子目录行（读侧回放结果）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRow {
    /// 该行的 data.version（v0–v3）。
    pub version: u8,
    /// 该行的 childId（关系校验用）。
    pub child_id: String,
    /// 该行的父日志 seq（关系校验报错时指出首次出现位置）。
    pub seq: u64,
    /// 子名字段（v3 必有，历史行无）。
    pub name: Option<String>,
    /// 创建模式：one-shot / continuable / external / unknown（开放词汇）。
    pub mode: String,
}

/// 父目录行冲突的形态（上游 `assertReleasedV4Relationships` 两条关系错误）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogConflict<'a> {
    /// `duplicate catalog child ${id}`——同一父下 childId 重复。
    DuplicateId { id: &'a str, first_seq: u64 },
    /// `duplicate catalog child name ${name}`——同一父下 name 重复
    /// （兄弟名由构造保证唯一；重复说明日志本身有歧义）。
    DuplicateName { name: &'a str },
}

/// 上游 `assertReleasedV4Relationships` 的 catalog 校验：同一父下 `childId`
/// 唯一；`name` 唯一（历史行不参与判据）。返回首个发现的冲突。
pub fn find_catalog_conflict<'a>(rows: &'a [CatalogRow]) -> Option<CatalogConflict<'a>> {
    let mut seen_ids: Vec<&'a str> = Vec::new();
    let mut first_id_seq: Vec<u64> = Vec::new();
    let mut names: Vec<&'a str> = Vec::new();
    for row in rows {
        if let Some(pos) = seen_ids.iter().position(|id| *id == row.child_id) {
            return Some(CatalogConflict::DuplicateId {
                id: &row.child_id,
                first_seq: first_id_seq[pos],
            });
        }
        seen_ids.push(&row.child_id);
        first_id_seq.push(row.seq);
        if let Some(name) = row.name.as_deref() {
            if names.contains(&name) {
                return Some(CatalogConflict::DuplicateName { name });
            }
            names.push(name);
        }
    }
    None
}
