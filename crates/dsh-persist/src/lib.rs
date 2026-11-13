//! dsh-persist — 与 web 版 dsh 完全共享的会话持久化。
//!
//! 磁盘布局（web `session-persistence-jsonl` 同款，0.1.3-alpha.1 起 v2）：
//! `{root}/--{projectKey(cwd)}--/{session-id}/session.v4.jsonl.zstd`
//! - 首行 header：`{"type":"session","version":2,"id",…,"isSeeded","cwd","delegationDepth"}`
//! - 事件行信封：`{"type":"<event>","seq":N,"time":ms,"data":{…}}`
//!   （surfaceOp/sourceEventSeqs 是信封层成员；assistant 流内嵌在结算行
//!   `assistant/message.stream` / `assistant/attempt`，不再有 chunk 事件）
//! 读取时转换为本仓库 `SessionEvent` 词汇表；写入时同样转回 web 信封，
//! 两端互读互通。旧代文件（`session.jsonl.zstd` v0）保持可读；写打开旧代
//! 先整档迁移到 v2（`v2::migrate_to_v2`），源文件永不重写。旧的散文件
//! `{root}/{id}.jsonl` 仍可读（迁移兼容）。

mod attachments;
mod v2;
pub use v2::migrate_v3_to_v4;

pub use attachments::{attachments_root_from_sessions_root, file_leaf_name, AttachmentStore};

use dsh_llm::{
    CallId, ContentBlock, FileAttachmentRef, Message, MessageId, MessageSource, Role, SessionId,
    TokenUsage,
};
use dsh_session::{EpochHeader, HeaderReason, Session, SessionEvent};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// web `projectKey(cwd)`：`/`、`\`、`:` 连续合并为 `-`；ASCII 安全字符
/// （字母数字 `._-`，非 `~`）保留；其余按 UTF-16 code unit 转 `~XXXX`
/// （大写 4 位 hex）；去前导 `-`；`--…--` 包裹；251 截断。
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut sep_run = false;
    for ch in cwd.encode_utf16() {
        let c = char::from_u32(ch as u32).unwrap_or('\u{fffd}');
        if c == '/' || c == '\\' || c == ':' {
            if !sep_run {
                readable.push('-');
            }
            sep_run = true;
        } else if c != '~' && (c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            readable.push(c);
            sep_run = false;
        } else {
            readable.push_str(&format!("~{:04X}", ch));
            sep_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let truncated: String = slug.chars().take(251).collect();
    format!("--{}--", truncated)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// web `projectionCacheDomainSpec`（0.1.3-alpha.1）的当前域版本：per-record
/// 布局，`compatibleVersions: [3, 4, 5, 6]`，checkpointIdentity 新增可选
/// `formatVersion`（= 会话格式版本，rustdsh 恒 2）。行形与 v5/v6 全同。
pub const PROJCACHE_DOMAIN_VERSION: u64 = 7;

/// 读侧接受的版本戳并集：released v7 兼容 [3,4,5,6]。戳在集外（或残缺）
/// 的文档按 web `parseRecord` 的 foreign 契约视为 absent，绝不解释。
pub const PROJCACHE_ACCEPTED_VERSIONS: &[u64] = &[3, 4, 5, 6, 7];

/// 读取 web 会话投影缓存的标题表（id → title），双布局：
/// 1) per-record 树 `<dir>/session_projcache/sessions/*.json`（web
///    0.1.2-alpha.5 起的主布局）：信封 `{version, record}`，戳不在
///    [`PROJCACHE_ACCEPTED_VERSIONS`] 内、信封残缺或 `.json.bak.<stamp>`
///    备份文件一律跳过；
/// 2) 旧整档 `<dir>/session_projcache.json`（无树家/旧版 web 的 bootstrap
///    源）：`unit.name`/版本戳验收后按 session 回退。
/// 同 id 两处并存时 per-record 为准（当前布局是权威写面）。
pub fn load_projcache_titles(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let title_of = |record: &serde_json::Value| -> Option<String> {
        record
            .get("rows")
            .and_then(|r| r.get("title"))
            .and_then(|t| t.get("val"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let tree = dir.join("session_projcache").join("sessions");
    if let Ok(entries) = std::fs::read_dir(&tree) {
        for entry in entries.flatten() {
            let path = entry.path();
            // 备份文件（web backupRecord 的 <key>.json.bak.<stamp>）不再以
            // .json 结尾，读侧同款忽略
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let version = doc.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            if !PROJCACHE_ACCEPTED_VERSIONS.contains(&version) {
                continue;
            }
            let Some(record) = doc.get("record").filter(|r| r.is_object()) else {
                continue;
            };
            if let Some(title) = title_of(record)
                && seen.insert(id.to_string())
            {
                out.push((id.to_string(), title));
            }
        }
    }
    let legacy = dir.join("session_projcache.json");
    if let Ok(text) = std::fs::read_to_string(&legacy)
        && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text)
        && doc
            .get("unit")
            .and_then(|u| u.get("name"))
            .and_then(|n| n.as_str())
            == Some("session_projcache")
    {
        let stamped = doc
            .get("unit")
            .and_then(|u| u.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if PROJCACHE_ACCEPTED_VERSIONS.contains(&stamped)
            && let Some(sessions) = doc
                .get("tables")
                .and_then(|t| t.get("sessions"))
                .and_then(|s| s.as_object())
        {
            for (id, row) in sessions {
                if let Some(title) = title_of(row)
                    && seen.insert(id.clone())
                {
                    out.push((id.clone(), title));
                }
            }
        }
    }
    out
}

/// 侧栏列表条目：会话 id + 它的 project cwd（目录名反推不可靠，从 header 读）。
pub struct SessionEntry {
    pub id: SessionId,
    pub cwd: Option<String>,
    pub modified: SystemTime,
}

pub struct SessionRecorder {
    root: PathBuf,
    /// 会话 id → 已解析的会话文件（规范桶锚定 + 最新代；避免 cwd 口径漂移时
    /// 在错误桶里重建/写入副本）。
    resolved: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
    /// 会话 id → 下一个可写 seq（增量帧追加的 O(1) 序号源；首触时由整档
    /// 解码初始化，单写者约束下可靠）。
    seqs: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// 会话 id → 进行中 attempt 的 (time, turn, step, chunk) 缓冲（v2：
    /// chunk 不落行，结算时压缩为 AssistantStreamRecord 嵌入 settlement 行）。
    streams: std::sync::Mutex<std::collections::HashMap<String, Vec<(u64, u64, u64, dsh_llm::StreamChunk)>>>,
    /// 懒物化新会话头写入的 agentPreset（宿主按用户默认预设设置；上游
    /// registry 的 selectedDefault 在新会话解析时落头）。
    default_agent_preset: std::sync::Mutex<String>,
    /// 已解析事件缓存（2 槽 LRU；键 = 路径 + mtime(ns)+size）。切会话的
    /// 重复 load 免全量重解析——大日志曾是秒级卡顿主因。
    parsed: std::sync::Mutex<Vec<(PathBuf, (i128, u64), std::sync::Arc<Vec<SessionEvent>>)>>,
}

/// 文件内容戳（mtime ns + size）：缓存有效性键。读不到（文件被删）返回 None。
fn file_content_stamp(file: &Path) -> Option<(i128, u64)> {
    let meta = std::fs::metadata(file).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    Some((mtime, meta.len()))
}

impl SessionRecorder {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            resolved: std::sync::Mutex::new(Default::default()),
            seqs: std::sync::Mutex::new(Default::default()),
            streams: std::sync::Mutex::new(Default::default()),
            default_agent_preset: std::sync::Mutex::new("standard".to_string()),
            parsed: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// 懒物化写入会话头的 agentPreset（宿主在用户改默认预设时同步）。
    pub fn set_default_agent_preset(&self, preset: impl Into<String>) {
        *self.default_agent_preset.lock().unwrap() = preset.into();
    }

    /// 事件行以独立的 zstd 帧追加到文件尾（zstd 多帧连接是标准格式，
    /// 流式解码跨帧读取）——落盘 O(事件)，替代整档解压+重写。
    fn append_frame(&self, file: &Path, line: &str) -> io::Result<()> {
        use std::io::Write;
        let frame = zstd::stream::encode_all(line.as_bytes(), 0)?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        f.write_all(&frame)
    }

    /// 检查点（上游 `session-checkpoint-policy`）：把已经写入的日志刷到
    /// 稳定存储。追加路径每次都 write+close（内核页缓存已经收到），这里再
    /// 补一次 `sync_all` 才能保证「崩溃后不重做/不丢」——request 与顶层工具
    /// 派发前各一次。
    pub fn checkpoint(&self, id: &SessionId, cwd: &str) -> io::Result<()> {
        let file = self.session_file(id, cwd);
        if !file.exists() {
            return Ok(());
        }
        let f = fs::OpenOptions::new().append(true).open(&file)?;
        f.sync_all()
    }

    /// 下一个可写 seq：缓存优先；首触或缓存缺失时整档解码初始化。
    fn next_seq(&self, id: &SessionId, file: &Path) -> u64 {
        let mut cache = self.seqs.lock().unwrap();
        if let Some(next) = cache.get(id.as_str()) {
            return *next;
        }
        let (_, last) = self.read_lines(file);
        cache.insert(id.as_str().to_string(), last + 1);
        last + 1
    }

    /// 记录 seq 已消耗到 `seq`（下一个 = seq+1）。
    fn mark_seq(&self, id: &SessionId, seq: u64) {
        self.seqs
            .lock()
            .unwrap()
            .insert(id.as_str().to_string(), seq + 1);
    }

    /// 规范桶解析：hint 命中优先，其次全桶扫描取最近修改；桶内多代文件
    /// 取最高代（web generation 语义，`session.lock`/临时文件天然忽略）。
    /// 未找到返回 None（调用方决定是否在 hint 桶新建）。
    fn resolve_file(&self, id: &SessionId, hint: Option<&str>) -> Option<PathBuf> {
        let cache = self.resolved.lock().unwrap();
        if let Some(path) = cache.get(id.as_str()) {
            if path.exists() {
                return Some(path.clone());
            }
        }
        drop(cache);
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        let mut hint_hit: Option<PathBuf> = None;
        if let Ok(projs) = fs::read_dir(&self.root) {
            for proj in projs.flatten() {
                let dir = proj.path().join(id.as_str());
                let Some(file) = v2::latest_generation(&dir) else {
                    continue;
                };
                if let Some(h) = hint
                    && proj.file_name().to_string_lossy() == project_key(h)
                {
                    hint_hit = Some(file.clone());
                }
                let modified = fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                    best = Some((modified, file));
                }
            }
        }
        let found = hint_hit.or(best.map(|(_, f)| f));
        if let Some(f) = &found {
            self.resolved
                .lock()
                .unwrap()
                .insert(id.as_str().to_string(), f.clone());
        }
        found
    }

    fn project_dir(&self, cwd: &str) -> PathBuf {
        self.root.join(project_key(cwd))
    }

    /// 当前写目标：v2 代文件名。
    fn session_file(&self, id: &SessionId, cwd: &str) -> PathBuf {
        self.project_dir(cwd)
            .join(id.as_str())
            .join(format!("session.v{}.jsonl.zstd", v2::SESSION_FORMAT_VERSION))
    }

    fn legacy_file(&self, id: &SessionId) -> PathBuf {
        self.root.join(format!("{}.jsonl", id.as_str()))
    }

    /// 新建会话：建目录并写 header（web 读取的最小要求，v2 形）。
    pub fn create(&self, id: &SessionId, cwd: &str, agent_preset: &str) -> io::Result<()> {
        let file = self.session_file(id, cwd);
        // 已存在的日志（任何代）绝不允许被新 header 覆盖（fail-closed：数据 > 自愈）
        let existing = v2::latest_generation(file.parent().unwrap_or(&file));
        if existing.is_some() {
            self.resolved
                .lock()
                .unwrap()
                .insert(id.as_str().to_string(), existing.unwrap());
            return Ok(());
        }
        fs::create_dir_all(file.parent().unwrap())?;
        let header = serde_json::json!({
            "type": "session",
            "version": v2::SESSION_FORMAT_VERSION,
            "id": id.as_str(),
            "createdAt": now_ms(),
            "isSeeded": false,
            "cwd": cwd,
            "delegationDepth": 0,
            "agentPreset": agent_preset,
        });
        let mut buf = String::new();
        buf.push_str(&header.to_string());
        buf.push('\n');
        self.append_frame(&file, &buf)?;
        self.resolved
            .lock()
            .unwrap()
            .insert(id.as_str().to_string(), file.clone());
        // v2 语义：首条事件 seq = 0（与内存 0 基位置对齐；上游 stage() 同）
        self.seqs.lock().unwrap().insert(id.as_str().to_string(), 0);
        // projcache 身份行：web 侧栏冷启动先读投影（identity 足够定位）
        self.touch_projcache(id, cwd, 0, None);
        Ok(())
    }

    fn projcache_path(&self) -> PathBuf {
        self.root
            .parent()
            .unwrap_or(&self.root)
            .join("storages")
            .join("session_projcache.json")
    }

    /// per-record 单会话文档路径（web 0.1.2-alpha.5 起的投影缓存主布局）。
    /// key 即路径段，沿用 web `SAFE_KEY_RE`（`[A-Za-z0-9_-]+`）——不安全的
    /// id 只走旧整档面。
    fn projcache_record_path(&self, id: &SessionId) -> Option<PathBuf> {
        if id.as_str().is_empty()
            || !id
                .as_str()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        Some(
            self.root
                .parent()
                .unwrap_or(&self.root)
                .join("storages")
                .join("session_projcache")
                .join("sessions")
                .join(format!("{}.json", id.as_str())),
        )
    }

    /// 把会话投影写入 web 投影缓存，按家的现状选布局：
    ///
    /// - per-record 树已存在（`storages/session_projcache/` 目录在）：写
    ///   主布局单会话文档 `…/sessions/<id>.json`（web 0.1.2-alpha.5 起写侧
    ///   只落这里）。信封 `{version, record}`：已有文档带接受版本戳则原样
    ///   保留（不无谓换版），外来戳/残缺信封按空记录新起——web
    ///   `parseRecord` 的 foreign→absent 契约。读-改-写单文档只动
    ///   `identity.cwd` 与 `rows.title`，其余行（sessionStats 等 web 投影
    ///   行）原样保留。
    /// - 树不存在：只写旧整档 `storages/session_projcache.json`（stamp 3，
    ///   在接受集内），**绝不抢先建树**——web 的 bootstrap 是「树缺席时把
    ///   整档记录整体迁移进树」，rustdsh 若先写一个单会话文档就会让
    ///   bootstrap 永久失效，整档里其余会话的缓存行全部丢失。web 首次
    ///   打开域完成迁移后，本函数自然切到 per-record 面。
    /// - 旧整档在两种模式下都持续维护：web 自身从不写它（bootstrap 后
    ///   只读），保留更新可让旧版整档 web 与新布局并存互通。
    ///
    /// `title = None` 只建/保 identity（web SessionTitle 投影行形：
    /// `rows.title = {ver:1, seq, val}`，latest-wins）。identity 不带
    /// lineage 字段（isSeeded/inheritedEventCount）——alpha.5 起按「未播种
    /// 身份」解释：侧栏列表投影精确匹配，播种过的冷读丢弃到冷重建，正确。
    pub fn touch_projcache(&self, id: &SessionId, cwd: &str, seq: u64, title: Option<&str>) {
        let storages = self.root.parent().unwrap_or(&self.root).join("storages");
        let unit_dir = storages.join("session_projcache");
        if unit_dir.exists() {
            if let Some(path) = self.projcache_record_path(id) {
                let (version, mut record) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|doc| {
                        let version = doc.get("version").and_then(|v| v.as_u64())?;
                        let record = doc.get("record").cloned()?;
                        if !PROJCACHE_ACCEPTED_VERSIONS.contains(&version) {
                            return None;
                        }
                        Some((
                            version,
                            if record.is_object() {
                                record
                            } else {
                                serde_json::json!({})
                            },
                        ))
                    })
                    .unwrap_or((PROJCACHE_DOMAIN_VERSION, serde_json::json!({})));
                if let Some(obj) = record.as_object_mut() {
                    let identity = obj
                        .entry("identity".to_string())
                        .or_insert_with(|| serde_json::json!({"createdAt": now_ms(), "cwd": cwd}));
                    if let Some(obj) = identity.as_object_mut() {
                        // cwd 以最近一次写入为准（会话可跨工作区移动）
                        obj.insert("cwd".into(), serde_json::json!(cwd));
                        // v2 语义：checkpointIdentity.formatVersion（web 读
                        // 侧要求与 header.version 精确匹配才作 hydration 捷
                        // 径；rustdsh 写面恒为 v2）
                        obj.entry("formatVersion")
                            .or_insert_with(|| serde_json::json!(v2::SESSION_FORMAT_VERSION));
                    }
                    if let Some(title) = title {
                        let rows = obj
                            .entry("rows".to_string())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(rows) = rows.as_object_mut() {
                            rows.insert(
                                "title".into(),
                                serde_json::json!({"ver": 1, "seq": seq, "val": title}),
                            );
                        }
                    }
                    if let Ok(text) = serde_json::to_string_pretty(&serde_json::json!({
                        "version": version,
                        "record": record,
                    })) {
                        // web serializeRecord 同款：pretty + 尾换行
                        let mut text = text;
                        text.push('\n');
                        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                        let _ = std::fs::write(&path, text);
                    }
                }
            }
        }
        let path = self.projcache_path();
        let mut doc: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| {
                serde_json::json!({
                    "unit": {"name": "session_projcache", "version": 3},
                    "global": null,
                    "tables": {"sessions": {}}
                })
            });
        // 数组判守（上游 storage-json format.ts 修正同语义）：tables 为数组
        // 时按不可见处理直接返回——避免把读不到的记录当空视图覆写丢弃
        if doc.get("tables").is_some_and(|t| t.is_array()) {
            return;
        }
        let Some(tables) = doc.get_mut("tables").and_then(|t| t.as_object_mut()) else {
            return;
        };
        let sessions = tables
            .entry("sessions".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(sessions) = sessions.as_object_mut() else {
            return;
        };
        let row = sessions
            .entry(id.as_str().to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(row) = row.as_object_mut() else {
            return;
        };
        let identity = row
            .entry("identity".to_string())
            .or_insert_with(|| serde_json::json!({"createdAt": now_ms(), "cwd": cwd}));
        if let Some(obj) = identity.as_object_mut() {
            // cwd 以最近一次写入为准（会话可跨工作区移动）
            obj.insert("cwd".into(), serde_json::json!(cwd));
            obj.entry("formatVersion")
                .or_insert_with(|| serde_json::json!(v2::SESSION_FORMAT_VERSION));
        }
        if let Some(title) = title {
            let rows = row
                .entry("rows".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(rows) = rows.as_object_mut() {
                rows.insert(
                    "title".into(),
                    serde_json::json!({"ver": 1, "seq": seq, "val": title}),
                );
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(&path, text);
        }
    }

    /// 追加一个事件（v2 信封行）。
    ///
    /// - 写打开旧代级联迁移（v0/v1→v2→v3→v4）到 `session.v4.jsonl.zstd`（上游
    ///   `ensureCurrentLog` 语义）；迁移 fail-closed 时整个追加失败。
    /// - `assistant/chunk` 不再落行：缓冲进进行中的 attempt，结算
    ///   （`assistant/message`）时压缩嵌入 `data.stream`。
    /// - 重试/轮末等边界事件触发缓冲冲刷：上一次尝试无表面消息 →
    ///   落 `assistant/attempt` 行（上游 agent-loop 的 settle 语义）。
    pub fn append(&self, id: &SessionId, cwd: &str, event: &SessionEvent) -> io::Result<()> {
        // 规范桶锚定：写入永远落在会话自身的日志上（hint 只在 id 尚无
        // 任何文件时作为新建位置），杜绝 cwd 口径漂移产生跨桶副本
        let mut file = match self.resolve_file(id, Some(cwd)) {
            Some(f) => f,
            None => {
                let preset = self.default_agent_preset.lock().unwrap().clone();
                self.create(id, cwd, &preset)?;
                self.session_file(id, cwd)
            }
        };
        // 写打开旧代 → 级联迁移（v0/v1 先到 v2，再统一 v2→v3；源文件保持
        // 原样；此后追加全在当前代）——上游 `ensureCurrentLog` 语义
        {
            let version = v2::header_version(&v2::read_header(&file).unwrap_or_default());
            if version < v2::SESSION_FORMAT_VERSION {
                if version < 2 {
                    file = v2::migrate_to_v2(&file, id)?;
                }
                if version < 3 {
                    file = v2::migrate_v2_to_v3(&file, id)?;
                }
                file = v2::migrate_v3_to_v4(&file, id)?;
                self.resolved
                    .lock()
                    .unwrap()
                    .insert(id.as_str().to_string(), file.clone());
                self.seqs.lock().unwrap().remove(id.as_str());
            }
        }
        // 已存在但解不出内容（非空文件）：拒绝追加（可能是不认识的编码/半写状态）
        if !self.resolved.lock().unwrap().contains_key(id.as_str())
            && file.exists()
            && fs::metadata(&file).map(|m| m.len() > 0).unwrap_or(false)
        {
            let probe = self.read_lines(&file);
            if probe.0.is_empty() {
                self.resolved
                    .lock()
                    .unwrap()
                    .insert(id.as_str().to_string(), file.to_path_buf());
            }
        }
        // chunk：缓冲（不落行、不耗 seq）——v2 词汇中 chunk 不是事件
        if let SessionEvent::AssistantChunk { turn, step, chunk } = event {
            self.streams
                .lock()
                .unwrap()
                .entry(id.as_str().to_string())
                .or_default()
                .push((now_ms(), *turn, *step, chunk.clone()));
            return Ok(());
        }
        // 边界事件：先冲刷进行中的 attempt（缓冲非空 = 上一次尝试已失败/
        // 中断/结束而无表面消息）
        let flush_attempt = matches!(
            event,
            SessionEvent::LlmRetry { .. }
                | SessionEvent::LlmRetryStarted { .. }
                | SessionEvent::TurnEnd { .. }
                | SessionEvent::StepStart { .. }
        );
        if flush_attempt
            && let Some(row) = self.take_attempt_row(id)
        {
            let seq = self.next_seq(id, &file);
            self.append_frame(&file, &format!("{}\n", row))?;
            self.mark_seq(id, seq);
        }
        let seq = self.next_seq(id, &file);
        if let SessionEvent::SessionTitle { title, .. } = event {
            self.touch_projcache(id, cwd, seq, Some(title));
        }
        let mut row = match event_to_web_line(event, seq, now_ms()) {
            Some(row) => row,
            // 不落盘的辅助事件：也不消耗 seq（与整档重写语义一致）
            None => return Ok(()),
        };
        // settlement 行内嵌本次 attempt 的流（上游 v2：message.stream 必填）
        if let SessionEvent::AssistantMessage { .. } = event {
            let stream = self.take_stream_records(id);
            row["data"]["stream"] =
                serde_json::to_value(stream).unwrap_or(serde_json::json!([]));
        }
        self.append_frame(&file, &format!("{}\n", row))?;
        self.mark_seq(id, seq);
        Ok(())
    }

    /// 冲刷进行中的 attempt 缓冲为 `assistant/attempt` 行（缓冲空 → None）。
    fn take_attempt_row(&self, id: &SessionId) -> Option<String> {
        let mut streams = self.streams.lock().unwrap();
        let buf = streams.get_mut(id.as_str())?;
        if buf.is_empty() {
            return None;
        }
        let drained = std::mem::take(buf);
        let (turn, step) = (drained[0].1, drained[0].2);
        let mut acc = dsh_llm::AssistantStreamAccumulator::new();
        let mut last_time = 0u64;
        for (time, _, _, chunk) in drained {
            acc.push(time, chunk);
            last_time = time;
        }
        let row = serde_json::json!({
            "type": "assistant/attempt",
            "seq": 0,
            "time": last_time,
            "data": {
                "turn": turn,
                "step": step,
                "stream": serde_json::to_value(acc.snapshot()).unwrap_or(serde_json::json!([])),
            },
        });
        Some(format!("{row}"))
    }

    /// 取走进行中的流缓冲并压缩为 AssistantStreamRecord 列表。
    fn take_stream_records(&self, id: &SessionId) -> Vec<dsh_llm::AssistantStreamRecord> {
        let mut streams = self.streams.lock().unwrap();
        let Some(buf) = streams.get_mut(id.as_str()) else {
            return Vec::new();
        };
        let drained = std::mem::take(buf);
        let mut acc = dsh_llm::AssistantStreamAccumulator::new();
        for (time, _, _, chunk) in drained {
            acc.push(time, chunk);
        }
        acc.snapshot()
    }

    /// 会话标题（`session/title` 事件的最新值）。
    pub fn title_of(&self, id: &SessionId, cwd: &str) -> Option<String> {
        let file = self
            .resolve_file(id, Some(cwd))
            .unwrap_or_else(|| self.session_file(id, cwd));
        let (lines, _) = self.read_lines(&file);
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("session/title"))
            .filter_map(|o| o.get("data")?.get("title")?.as_str().map(|s| s.to_string()))
            .next_back()
    }

    /// 会话头声明的 agentPreset（解析出日志文件后读头；缺省 None）。
    pub fn agent_preset_of(&self, id: &SessionId, cwd_hint: Option<&str>) -> Option<String> {
        let file = self.resolve_file(id, cwd_hint)?;
        read_header_agent_preset(&file)
    }

    /// 加载会话：优先 web 布局；返回 (Session, 真实 cwd)。
    /// cwd 传空时自动扫描目录定位。
    pub fn load(
        &self,
        id: &SessionId,
        cwd_hint: Option<&str>,
    ) -> io::Result<(Session, Option<String>)> {
        // 1) 按 hint（桶内多代取最高）
        if let Some(cwd) = cwd_hint {
            let dir = self.project_dir(cwd).join(id.as_str());
            if let Some(file) = v2::latest_generation(&dir) {
                v2::check_readable_version(&file)?;
                let events = self.read_events(&file)?;
                return Ok((
                    Session::from_events(id.clone(), events),
                    Some(cwd.to_string()),
                ));
            }
        }
        // 2) 扫描所有 project 目录找该 id（多桶副本取最近修改）
        if let Some(file) = self.resolve_file(id, None) {
            v2::check_readable_version(&file)?;
            let events = self.read_events(&file)?;
            let cwd = read_header_cwd(&file);
            return Ok((Session::from_events(id.clone(), events), cwd));
        }
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                let Some(file) = v2::latest_generation(&dir) else {
                    continue;
                };
                v2::check_readable_version(&file)?;
                let cwd = read_header_cwd(&file);
                let events = self.read_events(&file)?;
                return Ok((Session::from_events(id.clone(), events), cwd));
            }
        }
        // 3) legacy 散文件
        let legacy = self.legacy_file(id);
        if legacy.exists() {
            let reader = BufReader::new(File::open(&legacy)?);
            let mut events = Vec::new();
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_str::<SessionEvent>(&line) {
                    events.push(ev);
                }
            }
            return Ok((Session::from_events(id.clone(), events), None));
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "session not found"))
    }

    /// 列出全部会话：web 布局（含每会话 header 的 cwd）+ 旧散文件。
    pub fn list(&self) -> io::Result<Vec<SessionEntry>> {
        let mut out = Vec::new();
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    if proj.file_type()?.is_file() {
                        // 旧散文件
                        let name = proj.file_name().to_string_lossy().into_owned();
                        if let Some(stem) = name.strip_suffix(".jsonl") {
                            if !stem.starts_with("session-") {
                                let modified = proj
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .unwrap_or(UNIX_EPOCH);
                                out.push(SessionEntry {
                                    id: SessionId::new(stem.to_string()),
                                    cwd: None,
                                    modified,
                                });
                            }
                        }
                    }
                    continue;
                }
                for sess in fs::read_dir(proj.path())? {
                    let sess = sess?;
                    if !sess.file_type()?.is_dir() {
                        continue;
                    }
                let name = sess.file_name().to_string_lossy().into_owned();
                if !name.starts_with("session-") {
                    continue;
                }
                let Some(file) = v2::latest_generation(&sess.path()) else {
                    continue;
                };
                let modified = fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                    let cwd = read_header_cwd(&file);
                    out.push(SessionEntry {
                        id: SessionId::new(name),
                        cwd,
                        modified,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.modified.cmp(&a.modified));
        // 同 id 多桶副本（历史缺陷可能遗留）：只保留最近修改的一条
        let mut seen = std::collections::HashSet::new();
        out.retain(|e| seen.insert(e.id.as_str().to_string()));
        Ok(out)
    }

    /// 删除会话（web 布局整目录；散文件兜底）。
    pub fn delete(&self, id: &SessionId, cwd: Option<&str>) -> io::Result<()> {
        self.resolved.lock().unwrap().remove(id.as_str());
        self.seqs.lock().unwrap().remove(id.as_str());
        self.streams.lock().unwrap().remove(id.as_str());
        if let Some(cwd) = cwd {
            let dir = self.project_dir(cwd).join(id.as_str());
            if dir.exists() {
                return fs::remove_dir_all(dir);
            }
        }
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                if dir.exists() {
                    return fs::remove_dir_all(dir);
                }
            }
        }
        fs::remove_file(self.legacy_file(id))
    }

    // ---- 内部 ----

    /// 读取并分类一整个日志（ignorable 契约的执行点）：
    /// 未知类型 + 非 ignorable → 拒绝（Err）；其余未知/未映射类型按分类跳过。
    /// 带解析缓存（mtime+size 键控）：切换会话的重复 load 免去「整档解压 +
    /// 每行 JSON 解析」的全量重跑（曾是大日志切会话秒级卡顿的主因——40MB
    /// 档被全量解压 3 次、每行双重解析，且每次切换都重来）。
    fn read_events(&self, file: &Path) -> io::Result<Vec<SessionEvent>> {
        if let Some(events) = self.parsed_cache_hit(file) {
            return Ok(events);
        }
        let bytes = read_decompressed(file)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut events = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            // 日志首行的 SessionHeader 记录（type:"session"）不是事件，
            // 不参与 ignorable 分类
            if v.get("type").and_then(|t| t.as_str()) == Some("session") {
                continue;
            }
            match web_line_to_event(&v) {
                Some(e) => events.push(e),
                None => match classify_unknown(&v) {
                    UnknownPolicy::KnownSkip | UnknownPolicy::IgnorableSkip => {}
                    UnknownPolicy::Refuse => {
                        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
                        let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "session log contains event type \"{ty}\" (seq {seq}) unknown to this harness and not marked ignorable; refusing to interpret the log — it was likely written by a newer harness"
                            ),
                        ));
                    }
                },
            }
        }
        self.parsed_cache_store(file, events.clone());
        Ok(events)
    }

    /// 解析缓存命中（键 = 文件 mtime(ns)+size；追加写改变 mtime 自动失效）。
    /// 命中返回深拷贝（事件含大量 String，直接 Arc 共享需要改 Session 存储
    /// 形——拷贝约百毫秒级，仍远低于全量重解析的秒级）。
    fn parsed_cache_hit(&self, file: &Path) -> Option<Vec<SessionEvent>> {
        let stamp = file_content_stamp(file)?;
        let cache = self.parsed.lock().unwrap();
        for (path, (mtime, len), events) in cache.iter() {
            if path == file && (*mtime, *len) == stamp {
                return Some(events.as_ref().clone());
            }
        }
        None
    }

    fn parsed_cache_store(&self, file: &Path, events: Vec<SessionEvent>) {
        let Some(stamp) = file_content_stamp(file) else { return };
        let mut cache = self.parsed.lock().unwrap();
        // 2 槽 LRU：容量与内存换算约=两个大会话的事件树；挤掉最旧的。
        if cache.len() >= 2 {
            cache.remove(0);
        }
        cache.push((file.to_path_buf(), stamp, std::sync::Arc::new(events)));
    }

    fn read_lines(&self, file: &Path) -> (Vec<String>, u64) {
        let Ok(bytes) = read_decompressed(file) else {
            return (Vec::new(), 0);
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut last_seq = 0u64;
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                    if let Some(s) = v.get("seq").and_then(|s| s.as_u64()) {
                        last_seq = last_seq.max(s);
                    }
                }
                l.to_string()
            })
            .collect();
        (lines, last_seq)
    }
}

fn read_decompressed(file: &Path) -> io::Result<Vec<u8>> {
    let f = File::open(file)?;
    let mut dec = zstd::stream::Decoder::new(f)?;
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

/// 只解压前至多 `max` 字节——header 类读取（版本/头行 cwd/agentPreset）只
/// 需要第一行，整档解压是纯浪费（40MB 档解压 3 次/次加载曾是切换卡顿
/// 组成部分）。前缀内没有换行（病态首行超长）则回退全量解压。
fn read_decompressed_prefix(file: &Path, max: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let f = File::open(file)?;
    let mut dec = zstd::stream::Decoder::new(f)?;
    let mut out = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        let n = dec.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() >= max {
            break;
        }
    }
    if out.iter().any(|&b| b == b'\n') {
        Ok(out)
    } else {
        read_decompressed(file)
    }
}

/// 读日志第一行（前缀解压版，见 `read_decompressed_prefix`）。
fn read_first_line(file: &Path) -> Option<Vec<u8>> {
    let bytes = read_decompressed_prefix(file, 512 * 1024).ok()?;
    bytes.split(|&b| b == b'\n').next().map(|l| l.to_vec())
}

fn read_header_cwd(file: &Path) -> Option<String> {
    let first = read_first_line(file)?;
    let v: serde_json::Value = serde_json::from_slice(&first).ok()?;
    v.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string())
}

/// 会话头的 `agentPreset`（v2+ 起随头落盘；缺省 = 上游部署默认 standard）。
pub fn read_header_agent_preset(file: &Path) -> Option<String> {
    let first = read_first_line(file)?;
    let v: serde_json::Value = serde_json::from_slice(&first).ok()?;
    v.get("agentPreset")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

// ---- web 行 ↔ SessionEvent ----

fn blocks_from_web(arr: Option<&serde_json::Value>) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let Some(arr) = arr.and_then(|a| a.as_array()) else {
        return out;
    };
    for b in arr {
        let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match ty {
            "text" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::text(t));
                }
            }
            "reasoning" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::reasoning(t));
                }
            }
            "tool-call" => {
                let id = b.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let args = b.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                out.push(ContentBlock::ToolCall {
                    id: CallId(id.to_string()),
                    name: name.to_string(),
                    arguments: args.to_string(),
                });
            }
            "tool-result" => {
                let call = b
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let content = blocks_from_web(b.get("content"));
                let is_error = b.get("isError").and_then(|v| v.as_bool());
                out.push(ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                });
            }
            "image" => {
                if let Some(a) = b.get("attachment") {
                    out.push(ContentBlock::Image {
                        offloaded: b.get("offloaded").and_then(|v| v.as_bool()).unwrap_or(false),
                        attachment: serde_json::from_value(a.clone()).unwrap_or_else(|_| {
                            dsh_llm::ImageAttachmentRef {
                                attachment_id: String::new(),
                                name: None,
                                media_type: String::new(),
                                bytes: 0,
                                width: 0,
                                height: 0,
                                original_dimensions: None,
                            }
                        }),
                    });
                }
                // 无 attachment 成员的残缺 image 块（更旧写形）：跳过
            }
            "file" => {
                if let Some(a) = b.get("attachment") {
                    out.push(ContentBlock::File {
                        attachment: serde_json::from_value(a.clone()).unwrap_or(FileAttachmentRef {
                            attachment_id: String::new(),
                            name: String::new(),
                            bytes: 0,
                        }),
                    });
                }
            }
            _ => {}
        }
    }
    out
}


/// 上游 producerKind（sources.ts 净态）：V4 起 plugin source 的 kind 直接是
/// 生产者名，`plugin` 字段删除。
fn producer_kind(plugin: &str, role: Option<&str>) -> String {
    if plugin == "@deepseek-ai/dsh-system-prompt" && role == Some("system") {
        return "system-prompt".to_string();
    }
    match plugin {
        "compact" => "compact-checkpoint".to_string(),
        "tools-code-mode" | "tools-ptc" => "ptc-mode".to_string(),
        "dsh-compaction-basic" => "compact-basic".to_string(),
        "agent-instructions" | "session-reference" | "team-message" | "goal"
        | "skill-invocation" | "skill-catalog" | "coordinator" | "subagent-report"
        | "subagent-settled" | "webhook" | "agent-message" | "model-selection"
        | "plan-mode" | "time-context" | "tmux-context" | "user-approval"
        | "repeat-tool-reminder" | "tool-cordis" | "cordis-host-runner" | "tool-goal"
        | "tool-jobs" | "hooks-codex" | "hooks-claude-code" | "schedule"
        | "dsh-session-title-llm" => plugin.to_string(),
        other => format!("plugin:{other}"),
    }
}

/// MessageSource::Plugin 的 V4 序列化（kind = producer 名，plugin 字段删除；
/// form/summary 等附属字段原样保留）。V4 admission 拒绝 plugin 包装源
/// （上游 assertV4SourceRowAdmission：retired syntax）。
fn plugin_source_v4(
    plugin: &str,
    form: &Option<String>,
    summary: &Option<String>,
    sections: &Option<Vec<dsh_llm::ContextSnapshotSection>>,
    role: Option<&str>,
) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert("kind".into(), serde_json::json!(producer_kind(plugin, role)));
    if let Some(f) = form {
        o.insert("form".into(), serde_json::json!(f));
    }
    if let Some(s) = summary {
        o.insert("summary".into(), serde_json::json!(s));
    }
    if let Some(ss) = sections {
        o.insert(
            "sections".into(),
            serde_json::Value::Array(
                ss.iter()
                    .map(|s| serde_json::json!({"name": s.name, "text": s.text}))
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(o)
}

fn blocks_to_web(blocks: &[ContentBlock]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({"type":"text","text":text}),
            ContentBlock::Reasoning { text } => serde_json::json!({"type":"reasoning","text":text}),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => serde_json::json!({
                "type":"tool-call","id":id.0,"name":name,"arguments":arguments
            }),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => serde_json::json!({
                "type":"tool-result","toolCallId":tool_call_id.0,
                "content":blocks_to_web(content),
                "isError":is_error.unwrap_or(false)
            }),
            ContentBlock::Image { attachment, offloaded } => {
                // 上游 wire 形：offloaded 仅 true 时携带（可选 true）
                let mut o = serde_json::json!({ "type": "image", "attachment": attachment });
                if *offloaded {
                    o["offloaded"] = serde_json::json!(true);
                }
                o
            }
            ContentBlock::File { attachment } => serde_json::json!({
                "type": "file", "attachment": attachment
            }),
        })
        .collect();
    serde_json::Value::Array(arr)
}

/// web 信封行 → 我们的事件（消息/回合类；chunk 与辅助事件跳过——
/// assistant/message 已含最终内容）。
pub fn web_line_to_event(v: &serde_json::Value) -> Option<SessionEvent> {
    let ty = v.get("type")?.as_str()?;
    let data = v.get("data");
    // 信封 time（epoch ms）回传：轨迹时长列的日志数据源
    let env_time = v.get("time").and_then(|t| t.as_u64());
    match ty {
        "turn/start" => Some(SessionEvent::TurnStart {
            turn: num(data, "turn"),
        }),
        "turn/end" => Some(SessionEvent::TurnEnd {
            turn: num(data, "turn"),
            reason: data
                .and_then(|d| d.get("reason"))
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .unwrap_or(dsh_session::TurnEndReason::Completed),
        }),
        "step/start" => Some(SessionEvent::StepStart {
            turn: num(data, "turn"),
            step: num(data, "step"),
            time_ms: env_time,
        }),
        "user/message" => {
            let d = data?;
            let content = blocks_from_web(d.get("content"));
            let mut msg = Message::user(content);
            if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                msg.id = MessageId(id.to_string());
            }
            // web source.kind 开放词汇：user 之外都是生产者注入的上下文
            // （ContextInjectionRow 展示语义），kind/plugin/form/summary/
            // label 原料原样保留
            let src = d.get("source");
            let kind = src
                .and_then(|s| s.get("kind"))
                .and_then(|k| k.as_str())
                .unwrap_or("user");
            msg.source = match kind {
                "user" => MessageSource::User,
                // 上游 `{ kind: 'schedule' }`（定时提醒/变更通知的裸形）。
                "schedule" => MessageSource::Schedule,
                "plugin" => MessageSource::Context {
                    context_kind: kind.to_string(),
                    plugin: src
                        .and_then(|s| s.get("plugin"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    form: src
                        .and_then(|s| s.get("form"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    summary: src
                        .and_then(|s| s.get("summary"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    changes_paths: Vec::new(),
                    reference_labels: Vec::new(),
                    name: None,
                },
                other => MessageSource::Context {
                    context_kind: other.to_string(),
                    plugin: src
                        .and_then(|s| s.get("plugin"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    form: src
                        .and_then(|s| s.get("form"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    summary: src
                        .and_then(|s| s.get("summary"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    changes_paths: src
                        .and_then(|s| s.get("changes"))
                        .and_then(|c| c.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| {
                                    c.get("path").and_then(|p| p.as_str()).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    reference_labels: src
                        .and_then(|s| s.get("references"))
                        .and_then(|c| c.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| {
                                    c.get("label").and_then(|p| p.as_str()).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    name: src
                        .and_then(|s| s.get("name"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
            };
            Some(SessionEvent::UserMessage(msg))
        }
        "assistant/message" => {
            let d = data?;
            let m = d.get("message")?;
            let content = blocks_from_web(m.get("content"));
            let msg = Message::assistant(content, "web", "web");
            Some(SessionEvent::AssistantMessage {
                turn: num(data, "turn"),
                step: num(data, "step"),
                message: msg,
                interrupted: false,
                // 轨迹指标列数据源：web 行形 usage 驼峰与 TokenUsage serde
                // 同构；曾恒 None 致历史会话指标列空。
                usage: d
                    .get("usage")
                    .and_then(|u| serde_json::from_value::<TokenUsage>(u.clone()).ok()),
                time_ms: env_time,
            })
        }
        "tool/call" => {
            let d = data?;
            Some(SessionEvent::ToolCall {
                turn: num(data, "turn"),
                step: num(data, "step"),
                call_id: dsh_llm::CallId(
                    d.get("callId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                ),
                name: d
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                arguments: d
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                // 信封 time 回传（toolMs 配对的 dispatch 时刻）
                time_ms: env_time,
            })
        }
        "tool/result" => {
            let d = data?;
            // v2 形：{turn, step, message:{content, source:{kind:'tool', callId}}}
            if let Some(m) = d.get("message").filter(|m| m.is_object()) {
                let mut content = blocks_from_web(m.get("content"));
                // v4 平铺形（role:'tool'、content 为裸结果块）包回 wrapper；
                // v3/v2 形已含 tool-result wrapper 原样
                if !matches!(content.first(), Some(ContentBlock::ToolResult { .. })) {
                    let call4 = m
                        .get("toolCallId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            m.pointer("/source/callId").and_then(|v| v.as_str()).map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    let is_error4 = m.get("isError").and_then(|v| v.as_bool());
                    content = vec![ContentBlock::ToolResult {
                        tool_call_id: CallId(call4),
                        content,
                        is_error: is_error4,
                    }];
                }
                let call = m
                    .get("source")
                    .and_then(|s| s.get("callId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let msg = Message {
                    id: m
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| MessageId(s.to_string()))
                        .unwrap_or_default(),
                    role: dsh_llm::Role::User,
                    content,
                    source: MessageSource::Tool {
                        call_id: CallId(call.to_string()),
                    },
                };
                let presentation = data.as_ref().and_then(|d| d.get("presentation")).cloned();
                return Some(SessionEvent::ToolResult {
                    turn: num(data, "turn"),
                    step: num(data, "step"),
                    message: msg,
                    time_ms: env_time,
                    presentation,
                });
            }
            // legacy 平铺形（rustdsh 旧写形）：{toolCallId, content, isError}
            let call = d
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let content = blocks_from_web(d.get("content"));
            let is_error = d.get("isError").and_then(|v| v.as_bool());
            let msg = Message {
                id: MessageId::default(),
                role: dsh_llm::Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                }],
                source: MessageSource::Tool {
                    call_id: CallId(call.to_string()),
                },
            };
            Some(SessionEvent::ToolResult {
                turn: 0,
                step: 0,
                message: msg,
                time_ms: env_time,
                presentation: None,
            })
        }
        // 我们自己的压缩/步末事件（step/end 落盘保证 seq 1:1 对齐，
        // 压缩的 beforeSeq 依赖它；web 端按未知类型忽略）
        "step/end" => Some(SessionEvent::StepEnd {
            turn: num(data, "turn"),
            step: num(data, "step"),
        }),
        // 会话标题（web SessionTitleEventData：title + source，latest-wins；
        // 旧行缺 source 回 fallback）
        "session/title" => Some(SessionEvent::SessionTitle {
            title: data?
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            source: dsh_session::TitleSource::from_kind(
                data.and_then(|d| d.get("source"))
                    .and_then(|s| s.get("kind"))
                    .and_then(|k| k.as_str()),
            ),
        }),
        "permission/preset" => Some(SessionEvent::PermissionPreset {
            preset: data?
                .get("preset")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "workspace/changes" => Some(SessionEvent::WorkspaceChanges {
            turn: data.and_then(|d| d.get("turn")).and_then(|v| v.as_u64()).unwrap_or(0),
            files: data
                .and_then(|d| d.get("files"))
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
        }),
        "sandbox/mode" => Some(SessionEvent::SandboxModeSwitch {
            mode: data?
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "approval/policy" => Some(SessionEvent::ApprovalPolicy {
            policy: data?
                .get("policy")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "approval/asked" => Some(SessionEvent::ApprovalAsked {
            id: data?
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            tool_name: data?
                .get("toolName")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            call_id: data?.get("callId").and_then(|v| v.as_str()).map(str::to_string),
            reason: data?.get("reason").and_then(|v| v.as_str()).map(str::to_string),
        }),
        "approval/decided" => Some(SessionEvent::ApprovalDecided {
            id: data?
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            outcome: data?
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "todo/write" => Some(SessionEvent::TodoWrite {
            todos: data?
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            Some(dsh_session::TodoEntry {
                                content: item.get("content")?.as_str()?.to_string(),
                                status: item.get("status")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "job/registered" => Some(SessionEvent::JobRegistered(job_record(data?))),
        "job/settled" => Some(SessionEvent::JobSettled(dsh_session::JobSettlement {
            record: job_record(data?),
            finished_at: num(data, "finishedAt"),
            status: data?.get("status").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            cause: data?.get("cause").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            producer_reported: data?
                .get("producerReported")
                .and_then(|v| v.as_bool())
                .unwrap_or_default(),
            detail: data?.get("detail").and_then(|v| v.as_str()).map(str::to_string),
        })),
        "subagent/catalog" => Some(SessionEvent::SubagentCatalog {
            child_id: data?
                .get("childId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            child_created_at: num(data, "childCreatedAt"),
            mode: data?
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("one-shot")
                .to_string(),
            // v3 携带 name（历史 v0–v2 行无此键）；读侧照实保留，供侧栏/路径使用
            name: data?.get("name").and_then(|v| v.as_str()).map(str::to_string),
            label: data?.get("label").and_then(|v| v.as_str()).map(str::to_string),
        }),
        "image/offload" => {
            let d = data?;
            let targets: Vec<dsh_session::ImageOffloadTarget> = d
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| {
                            Some(dsh_session::ImageOffloadTarget {
                                seq: t.get("seq")?.as_u64()?,
                                image_indexes: t
                                    .get("imageIndexes")?
                                    .as_array()?
                                    .iter()
                                    .filter_map(|i| i.as_u64().map(|x| x as usize))
                                    .collect(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(SessionEvent::ImageOffload { targets })
        }
        "deliverables/presented" => {
            let d = data?;
            let files = d
                .get("files")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|f| {
                            let path = f.get("path").and_then(|v| v.as_str())?;
                            Some(dsh_session::PresentedFile {
                                path: path.to_string(),
                                description: f.get("description").and_then(|v| v.as_str()).map(str::to_string),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(SessionEvent::PresentedFiles {
                turn: num(data, "turn"),
                call_id: d
                    .get("callId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                files,
            })
        }
        "compaction/summary" => {
            let d = data?;
            // v2 富形：{compactionId, summary, shadowedRange, shadowedSeqs,
            // shadowedTokenCount, provider, model}
            if let Some(range) = d.get("shadowedRange").filter(|r| r.is_object()) {
                return Some(SessionEvent::Compaction {
                    before_seq: range.get("end").and_then(|v| v.as_u64()).unwrap_or(0),
                    summary: d
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    compaction_id: d
                        .get("compactionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    shadowed_start: range.get("start").and_then(|v| v.as_u64()).unwrap_or(0),
                    shadowed_seqs: d
                        .get("shadowedSeqs")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|s| s.as_u64()).collect())
                        .unwrap_or_default(),
                    shadowed_token_count: d
                        .get("shadowedTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    provider: d
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    model: d
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            // legacy 形（rustdsh 旧写形）：{beforeSeq, summary}
            Some(SessionEvent::Compaction {
                before_seq: num(data, "beforeSeq"),
                summary: data?
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                compaction_id: String::new(),
                shadowed_start: 0,
                shadowed_seqs: Vec::new(),
                shadowed_token_count: 0,
                provider: String::new(),
                model: String::new(),
            })
        }
        "llm/retry" => {
            let d = data?;
            let failure: dsh_llm::LlmFailure = d
                .get("failure")
                .and_then(|f| serde_json::from_value(f.clone()).ok())
                .unwrap_or(dsh_llm::LlmFailure {
                    message: String::new(),
                    code: "UNKNOWN".into(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                    offload_images: None,
                });
            Some(SessionEvent::LlmRetry {
                retry_id: d
                    .get("retryId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                turn: num(Some(d), "turn"),
                step: num(Some(d), "step"),
                provider: d
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                mode: d
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal")
                    .to_string(),
                policy_key: d
                    .get("policyKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                retry: num(Some(d), "retry") as u32,
                max_retries: d
                    .get("maxRetries")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                delay_ms: num(Some(d), "delayMs"),
                failure,
            })
        }
        "llm/retry-started" => Some(SessionEvent::LlmRetryStarted {
            retry_id: data?
                .get("retryId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            turn: num(data, "turn"),
            step: num(data, "step"),
            retry: num(data, "retry") as u32,
        }),
        // 请求纪元（event_to_web_line 的对称读回；reason 缺省按 Change 解释）
        "request/header" => {
            let d = data?;
            let header: EpochHeader =
                serde_json::from_value(d.get("header").cloned().unwrap_or(serde_json::Value::Null))
                    .ok()?;
            let reason: HeaderReason = d
                .get("reason")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .unwrap_or(HeaderReason::Change);
            Some(SessionEvent::RequestHeader { header, reason })
        }
        // plan/mode（log-only 开关，最后者赢）
        "plan/mode" => Some(SessionEvent::PlanMode {
            active: data.and_then(|d| d.get("active")).and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        // v3 system prompt 面节点（上游 emitSystem 形：固定 plugin source，
        // 信封 surfaceOp append 或 {op:'replace',startSeq,endSeq} 精确替换 head）
        "system/message" => {
            let d = data?;
            let m = d.get("message")?;
            let content = blocks_from_web(m.get("content"));
            let source = m.get("source");
            let plugin = source
                .and_then(|s| s.get("plugin"))
                .and_then(|v| v.as_str())
                .unwrap_or("@deepseek-ai/dsh-system-prompt")
                .to_string();
            let msg = Message::with_id(
                MessageId(m.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string()),
                Role::System,
                content,
                MessageSource::Plugin { plugin, form: None, summary: None, sections: None },
            );
            let replace = v.get("surfaceOp").and_then(|op| {
                if op.get("op").and_then(|o| o.as_str()) == Some("replace") {
                    op.get("startSeq").and_then(|s| s.as_u64())
                } else {
                    None
                }
            });
            Some(SessionEvent::SystemMessage {
                turn: num(Some(d), "turn"),
                step: num(Some(d), "step"),
                message: msg,
                replace,
            })
        }
        _ => None,
    }
}

/// 本构建理解的事件词汇全集（上游 `KNOWN_SESSION_EVENT_TYPES`，0.1.5-alpha.1
/// 对齐：新增 `system/message`（v3 system prompt 面节点）与 PTC 改名后的
/// `tool/ptc-dispatch*`；v2 的 `tool/code-dispatch*` 保留——读旧代日志合法）。
///
/// 持久化读取路径按 [`SessionEvent.ignorable`] 契约分类未知类型：不在集合内
/// 且未标 `ignorable` 的事件——多半是更新版本 harness 写的——拒绝解释整份
/// 日志；标了 `ignorable`（纯信息性记录）则跳过。集合内的类型即便本骨架未
/// 映射（approval/hook/team 等 web 宿主事件）也按已知忽略，不触发拒绝。
pub const KNOWN_SESSION_EVENT_TYPES: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/chunk",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "job/registered",
    "job/settled",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "workspace/changes",
    "subagent/model-selection-policy",
    "system/message",
    "workspace/changes",
    "subagent/catalog",
    "image/offload",
    "feedback/message-delete",
    "feedback/message-put",
    "developer/message",
    "deliverables/presented",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// 存储层 chunk 打包记录类型（web `packChunkRuns` 写盘形态，上游读取时经
/// `decodeStorageRecord` 解回 assistant/chunk——构建认识这些记录，不属于
/// 「未知类型」守卫范围）。骨架没有 chunk 回放消费方，按已知跳过。
pub use attachments::image_dimensions;

pub const KNOWN_STORAGE_RECORD_TYPES: &[&str] =
    &["text-chunks", "reasoning-chunks", "tool-call-chunks"];

/// 未知事件类型的分类（上游读取路径的三分支）。
enum UnknownPolicy {
    /// 词汇表内的类型（本骨架未映射）：按已知忽略。
    KnownSkip,
    /// 词汇表外但标了 `ignorable: true`：纯信息记录，安全跳过。
    IgnorableSkip,
    /// 词汇表外且必读：拒绝解释日志（可能来自更新版本 harness）。
    Refuse,
}

fn classify_unknown(v: &serde_json::Value) -> UnknownPolicy {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    if KNOWN_SESSION_EVENT_TYPES.contains(&ty) || KNOWN_STORAGE_RECORD_TYPES.contains(&ty) {
        return UnknownPolicy::KnownSkip;
    }
    if v.get("ignorable").and_then(|i| i.as_bool()) == Some(true) {
        return UnknownPolicy::IgnorableSkip;
    }
    UnknownPolicy::Refuse
}

fn num(data: Option<&serde_json::Value>, key: &str) -> u64 {
    data.and_then(|d| d.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or_default()
}

/// `job/registered` 行 data → 注册事实（上游 JobRegisteredData 的 camelCase 形）。
fn job_record(data: &serde_json::Value) -> dsh_session::JobRecord {
    dsh_session::JobRecord {
        job_id: data.get("jobId").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        owner: data.get("owner").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        kind: data.get("kind").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        label: data.get("label").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        started_at: data.get("startedAt").and_then(|v| v.as_u64()).unwrap_or_default(),
        tool_call: data.get("toolCall").and_then(|tc| {
            let call = tc.get("callId")?.as_str()?.to_string();
            let root = tc.get("rootCallId")?.as_str()?.to_string();
            Some((call, root))
        }),
    }
}

/// 注册事实 + 可选终态 → 行 data（camelCase；缺省字段落 null）。
fn job_record_wire(record: &dsh_session::JobRecord, settled: Option<&serde_json::Value>) -> serde_json::Value {
    let mut data = serde_json::json!({
        "jobId": record.job_id,
        "owner": record.owner,
        "kind": record.kind,
        "label": record.label,
        "startedAt": record.started_at,
        "toolCall": record.tool_call.as_ref().map(|(call, root)| serde_json::json!({
            "callId": call,
            "rootCallId": root,
        })),
    });
    if let (Some(settled), Some(map)) = (settled, data.as_object_mut()) {
        if let Some(fields) = settled.as_object() {
            for (key, value) in fields {
                map.insert(key.clone(), value.clone());
            }
        }
    }
    data
}

/// 我们的事件 → web v2 信封行（None = 不落盘的辅助事件；chunk 由
/// SessionRecorder 缓冲，结算行内嵌 stream，不在此构造）。
pub fn event_to_web_line(ev: &SessionEvent, seq: u64, time: u64) -> Option<serde_json::Value> {
    // 回放再落盘保留原信封 time（往返稳定）；实时事件无承载用落盘时刻
    let time = match ev {
        SessionEvent::StepStart { time_ms: Some(t), .. }
        | SessionEvent::AssistantMessage { time_ms: Some(t), .. }
        | SessionEvent::ToolResult { time_ms: Some(t), .. } => *t,
        _ => time,
    };
    let row = |ty: &str, data: serde_json::Value| {
        serde_json::json!({"type": ty, "seq": seq, "time": time, "data": data})
    };
    match ev {
        SessionEvent::TurnStart { turn } => {
            Some(row("turn/start", serde_json::json!({"turn": turn})))
        }
        SessionEvent::TurnEnd { turn, reason } => Some(row(
            "turn/end",
            serde_json::json!({
                "turn": turn,
                "reason": serde_json::to_value(reason).unwrap_or(serde_json::json!({"kind": "completed"})),
            }),
        )),
        SessionEvent::StepStart { turn, step, .. } => Some(row(
            "step/start",
            serde_json::json!({"turn": turn, "step": step}),
        )),
        SessionEvent::UserMessage(m) => {
            // source 按 V4 词汇序列化：user 原样；plugin → producer kind
            //（kind 即生产者名，plugin 字段删除）；注入类（context kind）原样
            let source = match &m.source {
                MessageSource::User => serde_json::json!({"kind": "user"}),
                MessageSource::Plugin { plugin, form, summary, sections } => {
                    // V4 退役 plugin 包装：producer-owned source kind
                    //（kind 即生产者名，plugin 字段删除；ContextFormed
                    // 附属原样保留——上游 producer-owned sources 重构）
                    plugin_source_v4(plugin, form, summary, sections, Some("user"))
                }
                MessageSource::Context {
                    context_kind,
                    plugin,
                    form,
                    summary,
                    changes_paths,
                    reference_labels,
                    name,
                } => {
                    let mut o = serde_json::Map::new();
                    o.insert("kind".into(), serde_json::json!(context_kind));
                    if let Some(p) = plugin {
                        o.insert("plugin".into(), serde_json::json!(p));
                    }
                    if let Some(f) = form {
                        o.insert("form".into(), serde_json::json!(f));
                    }
                    if let Some(x) = summary {
                        o.insert("summary".into(), serde_json::json!(x));
                    }
                    if !changes_paths.is_empty() {
                        o.insert(
                            "changes".into(),
                            serde_json::Value::Array(
                                changes_paths
                                    .iter()
                                    .map(|p| serde_json::json!({"action": "set", "path": p}))
                                    .collect(),
                            ),
                        );
                    }
                    if !reference_labels.is_empty() {
                        o.insert(
                            "references".into(),
                            serde_json::Value::Array(
                                reference_labels
                                    .iter()
                                    .map(|l| serde_json::json!({"label": l}))
                                    .collect(),
                            ),
                        );
                    }
                    if let Some(n) = name {
                        o.insert("name".into(), serde_json::json!(n));
                    }
                    serde_json::Value::Object(o)
                }
                other => serde_json::to_value(other).unwrap_or(serde_json::json!({"kind": "user"})),
            };
            // v2 处置表（RELEASED_V2：user/message data 必含 role/id/content/
            // source；surfaceOp 是信封层成员，append 缺省不写）
            Some(row(
                "user/message",
                serde_json::json!({
                    "content": blocks_to_web(&m.content),
                    "source": source,
                    "role": "user",
                    "id": m.id.0,
                }),
            ))
        }
        // v2 settlement：message 全形（id/role/content/source）+ usage/
        // interrupted；stream 由 SessionRecorder 在 append 时注入（缓冲
        // 压缩），此处落空数组占位保持函数纯净
        SessionEvent::AssistantMessage {
            turn,
            step,
            message,
            interrupted,
            usage,
            ..
        } => {
            let mut data = serde_json::json!({
                "turn": turn,
                "step": step,
                "message": serde_json::to_value(message).unwrap_or(serde_json::Value::Null),
                "stream": [],
            });
            if let Some(u) = usage {
                data["usage"] = serde_json::to_value(u).unwrap_or(serde_json::Value::Null);
            }
            if *interrupted {
                data["interrupted"] = serde_json::json!(true);
            }
            Some(row("assistant/message", data))
        }
        // v2：chunk 不再是事件（缓冲后内嵌结算行）
        SessionEvent::AssistantChunk { .. } => None,
        SessionEvent::ToolResult { turn, step, message, presentation, .. } => {
            // V4 一等 tool 角色形：message 平铺为 {id, role:'tool', source,
            // toolCallId, content(裸结果块), isError?}——V3 的 user+wrapper
            // 嵌套不再落盘（liftToolResult 净态）；presentation 同前
            let (call_id, content, is_error) = match message.content.first() {
                Some(ContentBlock::ToolResult { tool_call_id, content, is_error }) => {
                    (tool_call_id.0.clone(), content.clone(), *is_error)
                }
                _ => (String::new(), Vec::new(), Some(false)),
            };
            let mut msg = serde_json::json!({
                "id": message.id.0,
                "role": "tool",
                "source": {"kind": "tool", "callId": call_id},
                "toolCallId": call_id,
                "content": blocks_to_web(&content),
            });
            if let Some(err) = is_error {
                msg["isError"] = serde_json::json!(err);
            }
            let mut envelope = serde_json::json!({
                "turn": turn,
                "step": step,
                "message": msg,
            });
            if let Some(meta) = presentation {
                envelope["presentation"] = meta.clone();
            }
            Some(row("tool/result", envelope))
        }
        SessionEvent::StepEnd { turn, step } => Some(row(
            "step/end",
            serde_json::json!({"turn": turn, "step": step}),
        )),
        // v2 富形（RELEASED_V2 必填七件套）
        SessionEvent::Compaction {
            before_seq,
            summary,
            compaction_id,
            shadowed_start,
            shadowed_seqs,
            shadowed_token_count,
            provider,
            model,
        } => Some(row(
            "compaction/summary",
            serde_json::json!({
                "compactionId": compaction_id,
                "summary": summary,
                "shadowedRange": {"start": shadowed_start, "end": before_seq},
                "shadowedSeqs": shadowed_seqs,
                "shadowedTokenCount": shadowed_token_count,
                "provider": provider,
                "model": model,
            }),
        )),
        SessionEvent::SessionTitle { title, source } => Some(row(
            "session/title",
            serde_json::json!({"title": title, "messageSeqs": [], "source": {"kind": source.kind()}}),
        )),
        // 权限三旋钮（web 行形：data.{preset|mode|policy}）
        SessionEvent::PermissionPreset { preset } => Some(row(
            "permission/preset",
            serde_json::json!({"preset": preset}),
        )),
        SessionEvent::SandboxModeSwitch { mode } => Some(row(
            "sandbox/mode",
            serde_json::json!({"mode": mode}),
        )),
        SessionEvent::ApprovalPolicy { policy } => Some(row(
            "approval/policy",
            serde_json::json!({"policy": policy}),
        )),
        SessionEvent::WorkspaceChanges { turn, files } => Some(row(
            "workspace/changes",
            serde_json::json!({"turn": turn, "files": files}),
        )),
        SessionEvent::ApprovalAsked { id, tool_name, call_id, reason } => Some(row(
            "approval/asked",
            serde_json::json!({
                "id": id,
                "toolName": tool_name,
                "callId": call_id,
                "reason": reason,
            }),
        )),
        SessionEvent::ApprovalDecided { id, outcome } => Some(row(
            "approval/decided",
            serde_json::json!({"id": id, "outcome": outcome}),
        )),
        SessionEvent::TodoWrite { todos } => Some(row(
            "todo/write",
            serde_json::json!({
                "todos": todos
                    .iter()
                    .map(|t| serde_json::json!({"content": t.content, "status": t.status}))
                    .collect::<Vec<_>>(),
            }),
        )),
        SessionEvent::JobRegistered(record) => Some(row("job/registered", job_record_wire(record, None))),
        SessionEvent::JobSettled(settlement) => Some(row(
            "job/settled",
            job_record_wire(
                &settlement.record,
                Some(&serde_json::json!({
                    "finishedAt": settlement.finished_at,
                    "status": settlement.status,
                    "cause": settlement.cause,
                    "producerReported": settlement.producer_reported,
                    "detail": settlement.detail,
                })),
            ),
        )),
        // 重试链（web llm/retry：normal 模式带 maxRetries，always 模式不带）
        SessionEvent::LlmRetry {
            retry_id,
            turn,
            step,
            provider,
            mode,
            policy_key,
            retry,
            max_retries,
            delay_ms,
            failure,
        } => {
            let mut data = serde_json::json!({
                "retryId": retry_id,
                "turn": turn,
                "step": step,
                "provider": provider,
                "mode": mode,
                "policyKey": policy_key,
                "retry": retry,
                "delayMs": delay_ms,
                "failure": serde_json::to_value(failure).unwrap_or(serde_json::Value::Null),
            });
            if let (Some(max), Some(obj)) = (max_retries, data.as_object_mut()) {
                obj.insert("maxRetries".into(), serde_json::json!(max));
            }
            Some(row("llm/retry", data))
        }
        SessionEvent::LlmRetryStarted {
            retry_id,
            turn,
            step,
            retry,
        } => Some(row(
            "llm/retry-started",
            serde_json::json!({"retryId": retry_id, "turn": turn, "step": step, "retry": retry}),
        )),
        // 请求纪元（web request/header：config + 实际下发的 system/tools +
        // 变更原因）——审计「模型实际看到什么」的唯一凭据，必须落盘；
        // 首次 Initial、此后仅变更时追加，体量有界
        SessionEvent::RequestHeader { header, reason } => {
            // v3：system prompt 经 system/message 行持久化，request/header 不再
            // 携带 system；canonical 同时省略空 tools / 空 adapterDefaults
            // （上游 canonicalizeTransformedEvent）。
            let mut header_value =
                serde_json::to_value(header).unwrap_or(serde_json::Value::Null);
            if let Some(obj) = header_value.as_object_mut() {
                obj.remove("system");
                if obj.get("tools").and_then(|t| t.as_array()).is_some_and(|a| a.is_empty()) {
                    obj.remove("tools");
                }
                if obj
                    .get("adapterDefaults")
                    .and_then(|a| a.as_object())
                    .is_some_and(|o| o.is_empty())
                {
                    obj.remove("adapterDefaults");
                }
            }
            Some(row(
                "request/header",
                serde_json::json!({
                    "header": header_value,
                    "reason": serde_json::to_value(reason).unwrap_or(serde_json::Value::Null),
                }),
            ))
        }
        SessionEvent::PlanMode { active } => {
            Some(row("plan/mode", serde_json::json!({"active": active})))
        }
        SessionEvent::SubagentCatalog { child_id, child_created_at, mode, name, label } => {
            // 父所有的子 agent 发现事实（上游 establishCatalogChild，v3）：
            // {version:3, childId, childCreatedAt, mode, name, label?}
            let mut data = serde_json::json!({
                "version": dsh_session::SUBAGENT_CATALOG_VERSION,
                "childId": child_id,
                "childCreatedAt": child_created_at,
                "mode": mode,
                "name": name,
            });
            if let Some(l) = label {
                data["label"] = serde_json::json!(l);
            }
            Some(row("subagent/catalog", data))
        }
        SessionEvent::ToolCall { turn, step, call_id, name, arguments, .. } => {
            // v2 处置表：{turn, step, callId, name, arguments}（rustdsh 此前
            // 无写臂——事件被静默丢弃，web 读侧缺工具行、回放 stats_tools
            // 恒 0、toolMs 无配对源；本臂补齐）
            Some(row(
                "tool/call",
                serde_json::json!({
                    "turn": turn,
                    "step": step,
                    "callId": call_id.0,
                    "name": name,
                    "arguments": arguments,
                }),
            ))
        }
        SessionEvent::ImageOffload { targets } => {
            // 上游 image/offload：{targets:[{seq, imageIndexes}]}（选择器
            // 落盘，重放投影省略命中图片）
            let targets_json: Vec<serde_json::Value> = targets
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "seq": t.seq,
                        "imageIndexes": t.image_indexes,
                    })
                })
                .collect();
            Some(row(
                "image/offload",
                serde_json::json!({ "targets": targets_json }),
            ))
        }
        SessionEvent::PresentedFiles { turn, call_id, files } => {
            // 上游 tool-present：present 成功结果落盘（含嵌套调用）。
            // files = [{path, description?}]（PresentedFile，description 缺省省略）
            let files_json: Vec<serde_json::Value> = files
                .iter()
                .map(|f| {
                    let mut o = serde_json::json!({ "path": f.path });
                    if let Some(d) = &f.description {
                        o["description"] = serde_json::json!(d);
                    }
                    o
                })
                .collect();
            Some(row(
                "deliverables/presented",
                serde_json::json!({
                    "turn": turn,
                    "callId": call_id,
                    "files": files_json,
                }),
            ))
        }
        SessionEvent::SystemMessage { turn, step, message, replace } => {
            // v3 system prompt 面节点（上游 emitSystem）：固定 role=system 的
            // plugin source；replace 精确替换 head 并携带 sourceEventSeqs
            // （canonical 富形 startSeq/endSeq）。
            let mut envelope = row(
                "system/message",
                serde_json::json!({
                    "turn": turn,
                    "step": step,
                    "message": {
                        "id": message.id.0,
                        "role": "system",
                        // producer-owned kind：system-prompt（plugin 包装
                        // 同为 V4 retired syntax）
                        "source": match &message.source {
                            MessageSource::Plugin { plugin, form, summary, sections } => {
                                plugin_source_v4(plugin, form, summary, sections, Some("system"))
                            }
                            other => serde_json::to_value(other)
                                .unwrap_or(serde_json::json!({"kind": "system-prompt"})),
                        },
                        "content": blocks_to_web(&message.content),
                    },
                }),
            );
            match replace {
                None => envelope["surfaceOp"] = serde_json::json!("append"),
                Some(seq) => {
                    envelope["surfaceOp"] =
                        serde_json::json!({"op": "replace", "startSeq": seq, "endSeq": seq});
                    envelope["sourceEventSeqs"] = serde_json::json!([seq]);
                }
            }
            Some(envelope)
        }
        _ => None,
    }
}

/// 迁移词汇表判定：迁移器接受的行类型 = 当前词汇 ∪ v0 打包存储记录
/// （未知且非 ignorable 的类型拒绝迁移——fail-closed，与上游一致）。
pub fn is_migration_known_type(ty: &str) -> bool {
    KNOWN_SESSION_EVENT_TYPES.contains(&ty) || KNOWN_STORAGE_RECORD_TYPES.contains(&ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_source_round_trips_and_pins() {
        // web SessionTitleSource 三 kind 都走线形；user 源 = 置顶
        for (source, kind) in [
            (dsh_session::TitleSource::Fallback, "fallback"),
            (dsh_session::TitleSource::Provider, "provider"),
            (dsh_session::TitleSource::User, "user"),
        ] {
            let ev = SessionEvent::SessionTitle { title: "随动".into(), source };
            let row = event_to_web_line(&ev, 7, 100).unwrap();
            assert_eq!(row["type"], "session/title");
            assert_eq!(row["data"]["title"], "随动");
            assert_eq!(row["data"]["source"]["kind"], kind);
            let parsed = web_line_to_event(&row).unwrap();
            match parsed {
                SessionEvent::SessionTitle { title, source } => {
                    assert_eq!(title, "随动");
                    assert_eq!(source.kind(), kind);
                }
                other => panic!("wrong event {other:?}"),
            }
        }
        // 旧行 / web 缺 source：回 fallback（旧文件双向兼容）
        let legacy = serde_json::json!({
            "type": "session/title", "seq": 3, "time": 1,
            "data": {"title": "旧标题"},
        });
        let parsed = web_line_to_event(&legacy).unwrap();
        match parsed {
            SessionEvent::SessionTitle { source, .. } => {
                assert_eq!(source, dsh_session::TitleSource::Fallback);
                assert!(!source.is_pinned());
            }
            other => panic!("wrong event {other:?}"),
        }
    }

    #[test]
    fn title_round_trips_to_web_artifacts() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 预置 per-record 树（web 已完成 bootstrap 的家）：写侧落主布局
        std::fs::create_dir_all(
            dir.join("storages")
                .join("session_projcache")
                .join("sessions"),
        )
        .unwrap();
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-test");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "你好".into(),
                source: dsh_session::TitleSource::Fallback,
            },
        )
        .unwrap();

        // 事件可读回（web session/title 行）
        assert_eq!(rec.title_of(&id, "/tmp/ws"), Some("你好".into()));
        // projcache 投影行落盘（web 侧栏冷启动来源）：主布局 per-record 文档
        let pr: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.join("storages")
                    .join("session_projcache")
                    .join("sessions")
                    .join("session-test.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(pr["version"], PROJCACHE_DOMAIN_VERSION);
        assert_eq!(pr["record"]["identity"]["cwd"], "/tmp/ws");
        assert_eq!(pr["record"]["rows"]["title"]["val"], "你好");
        assert_eq!(pr["record"]["rows"]["title"]["ver"], 1);
        // v2 起 seq 0 基（与内存位置对齐；上游 stage() 同）
        assert_eq!(pr["record"]["rows"]["title"]["seq"], 0);
        // 旧整档（无树家的 bootstrap 源）同步更新
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        let row = &pc["tables"]["sessions"]["session-test"];
        assert_eq!(row["identity"]["cwd"], "/tmp/ws");
        assert_eq!(row["rows"]["title"]["val"], "你好");
        assert_eq!(row["rows"]["title"]["ver"], 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v4_write_maps_plugin_wrappers_to_producer_owned_kinds() {
        // V4 admission 拒绝 kind:'plugin'（retired syntax，上游
        // assertV4SourceRowAdmission）：user/system 两处 plugin 包装源在
        // 落盘时必须重写为 producer-owned kind（producer-owned sources）
        let notice = dsh_llm::Message::new(
            dsh_llm::Role::User,
            vec![dsh_llm::ContentBlock::text("notice text")],
            MessageSource::Plugin {
                plugin: "plan-mode".into(),
                form: Some("notice".into()),
                summary: Some("notice text".into()),
                sections: None,
            },
        );
        let row = event_to_web_line(&SessionEvent::UserMessage(notice), 7, 100).unwrap();
        let source = &row["data"]["source"];
        assert_eq!(source["kind"], "plan-mode");
        assert_eq!(source["form"], "notice");
        assert_eq!(source["summary"], "notice text");
        assert!(source.get("plugin").is_none(), "plugin field must be dropped");

        let system = dsh_llm::Message::new(
            dsh_llm::Role::System,
            vec![dsh_llm::ContentBlock::text("system prompt")],
            MessageSource::Plugin {
                plugin: "@deepseek-ai/dsh-system-prompt".into(),
                form: None,
                summary: None,
                sections: None,
            },
        );
        let row = event_to_web_line(
            &SessionEvent::SystemMessage { turn: 1, step: 1, message: system, replace: None },
            8,
            100,
        )
        .unwrap();
        let source = &row["data"]["message"]["source"];
        assert_eq!(source["kind"], "system-prompt", "role-sensitive mapping");
        assert!(source.get("plugin").is_none());
    }

    #[test]
    fn lazy_materialization_writes_the_default_agent_preset() {
        // 懒物化会话头写宿主设置的用户默认预设（agent 预设数据面：新会话
        // 按 selectedDefault 解析并落头；上游 registry 同语义）
        let dir = std::env::temp_dir().join(format!("dsh-preset-hdr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        rec.set_default_agent_preset("minimal");
        let id = SessionId::new("session-preset-hdr");
        rec.append(&id, "/tmp/ws", &SessionEvent::PermissionPreset { preset: "workspace-write".into() })
            .expect("first append materializes with the default preset");
        let file = rec.session_file(&id, "/tmp/ws");
        assert_eq!(
            read_header_agent_preset(&file).as_deref(),
            Some("minimal"),
            "懒物化头应写用户默认预设"
        );
        // 未设置时（新 recorder）缺省 standard
        let rec2 = SessionRecorder::new(dir.join("sessions"));
        let id2 = SessionId::new("session-preset-hdr-default");
        rec2.append(&id2, "/tmp/ws", &SessionEvent::PermissionPreset { preset: "workspace-write".into() })
            .unwrap();
        assert_eq!(
            read_header_agent_preset(&rec2.session_file(&id2, "/tmp/ws")).as_deref(),
            Some("standard")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_without_create_materializes_the_log() {
        // 懒物化地基（新会话草稿纯内存、首事件落盘才成文件）：append 对
        // 未 create 的会话自动建文件（header + 行），load 可回读
        let dir = std::env::temp_dir().join(format!("dsh-persist-lazy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-lazy");
        // 无 create：直接追加首事件（pin 形）
        rec.append(&id, "/tmp/ws", &SessionEvent::PermissionPreset { preset: "workspace-write".into() })
            .expect("first append materializes");
        rec.append(&id, "/tmp/ws", &SessionEvent::SessionTitle { title: "t".into(), source: dsh_session::TitleSource::Fallback })
            .expect("second append");
        let (loaded, _) = rec.load(&id, Some("/tmp/ws")).expect("materialized session loads");
        let types: Vec<&str> = loaded
            .entries()
            .iter()
            .map(|e| match &e.event {
                SessionEvent::PermissionPreset { .. } => "permission/preset",
                SessionEvent::SessionTitle { .. } => "session/title",
                _ => "?",
            })
            .collect();
        assert_eq!(types, vec!["permission/preset", "session/title"], "row order preserved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn presented_files_round_trip_through_web_row() {
        // 上游 tool-present：present 成功结果落 deliverables/presented
        // {turn, callId, files:[{path, description?}]}（缺省 description 省略）
        let ev = SessionEvent::PresentedFiles {
            turn: 3,
            call_id: "call-9".into(),
            files: vec![
                dsh_session::PresentedFile {
                    path: "E:/ws/out.xlsx".into(),
                    description: Some("结果表格".into()),
                },
                dsh_session::PresentedFile { path: "E:/ws/report.md".into(), description: None },
            ],
        };
        let row = event_to_web_line(&ev, 11, 100).unwrap();
        assert_eq!(row["type"], "deliverables/presented");
        assert_eq!(row["data"]["turn"], 3);
        assert_eq!(row["data"]["callId"], "call-9");
        let files = row["data"]["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "E:/ws/out.xlsx");
        assert_eq!(files[0]["description"], "结果表格");
        assert!(files[1].get("description").is_none(), "absent description stays absent");

        let parsed = web_line_to_event(&row).expect("known type parses back");
        match parsed {
            SessionEvent::PresentedFiles { turn, call_id, files } => {
                assert_eq!((turn, call_id.as_str()), (3, "call-9"));
                assert_eq!(files.len(), 2);
                assert_eq!(files[0].path, "E:/ws/out.xlsx");
                assert_eq!(files[0].description.as_deref(), Some("结果表格"));
                assert_eq!(files[1].description, None);
            }
            other => panic!("expected PresentedFiles, got {other:?}"),
        }
    }

    #[test]
    fn image_offload_round_trips_through_web_row() {
        // 上游 image/offload：{targets:[{seq, imageIndexes}]}（卸载决定）
        let ev = SessionEvent::ImageOffload {
            targets: vec![
                dsh_session::ImageOffloadTarget { seq: 3, image_indexes: vec![0, 2] },
                dsh_session::ImageOffloadTarget { seq: 7, image_indexes: vec![1] },
            ],
        };
        let row = event_to_web_line(&ev, 12, 100).unwrap();
        assert_eq!(row["type"], "image/offload");
        let targets = row["data"]["targets"].as_array().unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0]["seq"], 3);
        assert_eq!(targets[0]["imageIndexes"], serde_json::json!([0, 2]));
        let parsed = web_line_to_event(&row).expect("known type parses back");
        match parsed {
            SessionEvent::ImageOffload { targets } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(targets[0].seq, 3);
                assert_eq!(targets[0].image_indexes, vec![0, 2]);
                assert_eq!(targets[1].seq, 7);
                assert_eq!(targets[1].image_indexes, vec![1]);
            }
            other => panic!("expected ImageOffload, got {other:?}"),
        }
    }

    #[test]
    fn job_lifecycle_round_trips_through_web_rows() {
        let record = dsh_session::JobRecord {
            job_id: "bash-7f3a1c02".into(),
            owner: "session-a".into(),
            kind: "bash".into(),
            label: "cargo test".into(),
            started_at: 1_762_700_000_000,
            tool_call: Some(("call-1".into(), "call-1".into())),
        };
        let row = event_to_web_line(&SessionEvent::JobRegistered(record.clone()), 20, 100).unwrap();
        assert_eq!(row["type"], "job/registered");
        assert_eq!(row["data"]["jobId"], "bash-7f3a1c02");
        assert_eq!(row["data"]["kind"], "bash");
        assert_eq!(row["data"]["startedAt"], 1_762_700_000_000u64);
        assert_eq!(row["data"]["toolCall"]["rootCallId"], "call-1");
        match web_line_to_event(&row).expect("parses back") {
            SessionEvent::JobRegistered(parsed) => {
                assert_eq!(parsed.job_id, "bash-7f3a1c02");
                assert_eq!(parsed.tool_call.as_ref().unwrap().0, "call-1");
            }
            other => panic!("expected JobRegistered, got {other:?}"),
        }

        let settled = dsh_session::JobSettlement {
            record,
            finished_at: 1_762_700_012_000,
            status: "killed".into(),
            cause: "kill".into(),
            producer_reported: true,
            detail: Some("signal: TERM; no longer needed".into()),
        };
        let row = event_to_web_line(&SessionEvent::JobSettled(settled), 21, 100).unwrap();
        assert_eq!(row["type"], "job/settled");
        assert_eq!(row["data"]["finishedAt"], 1_762_700_012_000u64);
        assert_eq!(row["data"]["cause"], "kill");
        assert_eq!(row["data"]["producerReported"], true);
        assert_eq!(row["data"]["detail"], "signal: TERM; no longer needed");
        match web_line_to_event(&row).expect("parses back") {
            SessionEvent::JobSettled(parsed) => {
                assert_eq!(parsed.status, "killed");
                assert_eq!(parsed.cause, "kill");
                assert!(parsed.producer_reported);
                assert_eq!(parsed.detail.as_deref(), Some("signal: TERM; no longer needed"));
            }
            other => panic!("expected JobSettled, got {other:?}"),
        }
    }

    #[test]
    fn todo_write_and_approval_round_trip_through_web_rows() {
        let ev = SessionEvent::TodoWrite {
            todos: vec![
                dsh_session::TodoEntry { content: "读代码".into(), status: "completed".into() },
                dsh_session::TodoEntry { content: "写实现".into(), status: "in_progress".into() },
            ],
        };
        let row = event_to_web_line(&ev, 11, 100).unwrap();
        assert_eq!(row["type"], "todo/write");
        assert_eq!(row["data"]["todos"][0]["content"], "读代码");
        assert_eq!(row["data"]["todos"][1]["status"], "in_progress");
        match web_line_to_event(&row).expect("known type parses back") {
            SessionEvent::TodoWrite { todos } => {
                assert_eq!(todos.len(), 2);
                assert_eq!(todos[0].status, "completed");
            }
            other => panic!("expected TodoWrite, got {other:?}"),
        }

        // approval/asked（log-only 审计，可选字段缺省不写）
        let ev = SessionEvent::ApprovalAsked {
            id: "call-1".into(),
            tool_name: "write".into(),
            call_id: Some("call-1".into()),
            reason: Some("escalate sandbox to danger-full-access: 需要清理".into()),
        };
        let row = event_to_web_line(&ev, 12, 100).unwrap();
        assert_eq!(row["type"], "approval/asked");
        assert_eq!(row["data"]["toolName"], "write");
        assert_eq!(row["data"]["callId"], "call-1");
        let parsed = web_line_to_event(&row).expect("known type parses back");
        match parsed {
            SessionEvent::ApprovalAsked { tool_name, call_id, .. } => {
                assert_eq!(tool_name, "write");
                assert_eq!(call_id.as_deref(), Some("call-1"));
            }
            other => panic!("expected ApprovalAsked, got {other:?}"),
        }
        // 无可选字段：写侧落 null、读侧回 None
        let ev = SessionEvent::ApprovalAsked {
            id: "call-2".into(),
            tool_name: "edit".into(),
            call_id: None,
            reason: None,
        };
        let row = event_to_web_line(&ev, 13, 100).unwrap();
        assert!(row["data"]["reason"].is_null());
        match web_line_to_event(&row).expect("parses back") {
            SessionEvent::ApprovalAsked { call_id, reason, .. } => {
                assert!(call_id.is_none());
                assert!(reason.is_none());
            }
            other => panic!("expected ApprovalAsked, got {other:?}"),
        }

        // approval/decided（闭集 outcome 原文）
        let ev = SessionEvent::ApprovalDecided { id: "call-1".into(), outcome: "allowed-once".into() };
        let row = event_to_web_line(&ev, 14, 100).unwrap();
        assert_eq!(row["type"], "approval/decided");
        assert_eq!(row["data"]["outcome"], "allowed-once");
        match web_line_to_event(&row).expect("parses back") {
            SessionEvent::ApprovalDecided { id, outcome } => {
                assert_eq!(id, "call-1");
                assert_eq!(outcome, "allowed-once");
            }
            other => panic!("expected ApprovalDecided, got {other:?}"),
        }
    }

    fn subagent_catalog_round_trips_through_web_row() {
        // 父所有的子 agent 发现事实：写形 {version:3, childId,
        // childCreatedAt, mode, name, label?}（label 缺省省略），读侧还原
        let ev = SessionEvent::SubagentCatalog {
            child_id: "session-child".into(),
            child_created_at: 1_724_000_000_000,
            mode: "continuable".into(),
            name: Some("researcher".into()),
            label: Some("demo task".into()),
        };
        let row = event_to_web_line(&ev, 9, 100).unwrap();
        assert_eq!(row["type"], "subagent/catalog");
        assert_eq!(row["data"]["version"], dsh_session::SUBAGENT_CATALOG_VERSION);
        assert_eq!(row["data"]["childId"], "session-child");
        assert_eq!(row["data"]["childCreatedAt"], 1_724_000_000_000u64);
        assert_eq!(row["data"]["mode"], "continuable");
        assert_eq!(row["data"]["name"], "researcher");
        assert_eq!(row["data"]["label"], "demo task");

        let parsed = web_line_to_event(&row).expect("known type parses back");
        match parsed {
            SessionEvent::SubagentCatalog { child_id, child_created_at, mode, name, label } => {
                assert_eq!(child_id, "session-child");
                assert_eq!(child_created_at, 1_724_000_000_000);
                assert_eq!(mode, "continuable");
                assert_eq!(name.as_deref(), Some("researcher"));
                assert_eq!(label.as_deref(), Some("demo task"));
            }
            other => panic!("expected SubagentCatalog, got {other:?}"),
        }

        // 历史行无 name（v0–v2）：读侧保留为 None，不回写键
        let ev = SessionEvent::SubagentCatalog {
            child_id: "session-child-2".into(),
            child_created_at: 7,
            mode: "one-shot".into(),
            name: None,
            label: None,
        };
        let row = event_to_web_line(&ev, 10, 100).unwrap();
        assert!(row["data"].get("label").is_none(), "absent label stays absent");
        assert!(row["data"]["name"].is_null(), "absent name stays null");

        let parsed = web_line_to_event(&row).expect("historical row parses back");
        match parsed {
            SessionEvent::SubagentCatalog { name, label, .. } => {
                assert_eq!(name, None, "historical row keeps no name");
                assert_eq!(label, None);
            }
            other => panic!("expected SubagentCatalog, got {other:?}"),
        }
    }

    #[test]
    fn subagent_catalog_legacy_v0_row_decodes_with_no_name() {
        // 跨端互通：web 写的历史 v0/v1 行（无 name 键）必须仍能读，不能因
        // 本次升级写 v3 而把旧日志判死
        let row = serde_json::json!({
            "type": "subagent/catalog",
            "data": {
                "version": 0,
                "childId": "session-legacy",
                "childCreatedAt": 123,
                "mode": "one-shot",
            },
        });
        let parsed = web_line_to_event(&row).expect("legacy v0 row parses");
        match parsed {
            SessionEvent::SubagentCatalog { child_id, mode, name, label, .. } => {
                assert_eq!(child_id, "session-legacy");
                assert_eq!(mode, "one-shot");
                assert_eq!(name, None, "legacy v0 has no name field");
                assert_eq!(label, None);
            }
            other => panic!("expected SubagentCatalog, got {other:?}"),
        }
    }

    #[test]
    fn touch_projcache_preserves_stamp_and_sibling_rows() {
        // 已存在的 per-record 文档（master web v6 形）：RMW 保留版本戳、
        // identity.createdAt 与兄弟投影行（sessionStats），只动 cwd 与 title
        let dir = std::env::temp_dir().join(format!("dsh-persist-pr-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-keep");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let path = dir
            .join("storages")
            .join("session_projcache")
            .join("sessions")
            .join("session-keep.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&serde_json::json!({
            "version": 6,
            "record": {
                "identity": {"createdAt": 12345, "cwd": "/tmp/old", "isSeeded": true, "inheritedEventCount": 7},
                "rows": {"sessionStats": {"ver": 1, "seq": 42, "val": {"turns": 3}}}
            }
        })).unwrap()).unwrap();

        rec.touch_projcache(&id, "/tmp/ws", 9, Some("新标题"));

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["version"], 6, "accepted stamp must be preserved");
        assert_eq!(doc["record"]["identity"]["createdAt"], 12345);
        assert_eq!(doc["record"]["identity"]["cwd"], "/tmp/ws");
        assert_eq!(
            doc["record"]["identity"]["isSeeded"], true,
            "lineage fields untouched"
        );
        assert_eq!(doc["record"]["rows"]["sessionStats"]["seq"], 42);
        assert_eq!(doc["record"]["rows"]["title"]["val"], "新标题");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touch_projcache_skips_unsafe_record_key() {
        // per-record 契约：key 必须是路径安全段；不安全 id 只走旧整档面
        let dir = std::env::temp_dir().join(format!("dsh-persist-pr-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session/bad\\id");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle { title: "t".into(), source: dsh_session::TitleSource::Fallback },
        )
        .unwrap();
        assert!(
            !dir.join("storages")
                .join("session_projcache")
                .join("sessions")
                .exists()
        );
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            pc["tables"]["sessions"]["session/bad\\id"]["rows"]["title"]["val"],
            "t"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touch_projcache_defers_to_bootstrap_until_tree_exists() {
        // 无树老家：只写旧整档（bootstrap 源），绝不抢先建树——树一旦有
        // 任何文档，web 的整档→树迁移就被永久抑制，整档里其余会话的缓存
        // 行全部丢失。web 完成迁移（树出现）后，写侧自动切 per-record。
        let dir = std::env::temp_dir().join(format!("dsh-persist-defer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-defer");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "旧面".into(),
                source: dsh_session::TitleSource::Fallback,
            },
        )
        .unwrap();
        assert!(
            !dir.join("storages").join("session_projcache").exists(),
            "tree must not be created"
        );
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            pc["tables"]["sessions"]["session-defer"]["rows"]["title"]["val"],
            "旧面"
        );

        // web bootstrap 落地（迁移整档行进树）：此后写侧切 per-record 面
        std::fs::create_dir_all(
            dir.join("storages")
                .join("session_projcache")
                .join("sessions"),
        )
        .unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::SessionTitle {
                title: "新面".into(),
                source: dsh_session::TitleSource::Fallback,
            },
        )
        .unwrap();
        let pr: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.join("storages")
                    .join("session_projcache")
                    .join("sessions")
                    .join("session-defer.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(pr["version"], PROJCACHE_DOMAIN_VERSION);
        assert_eq!(pr["record"]["rows"]["title"]["val"], "新面");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_projcache_titles_reads_both_layouts() {
        // 双布局标题兜底：per-record（v5/v6 戳）优先，foreign 戳/.bak 备份/
        // 残缺信封跳过，旧整档按 session 回退且不覆盖树上已有的 id
        let dir = std::env::temp_dir().join(format!("dsh-persist-titles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sessions = dir.join("session_projcache").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let write_doc = |name: &str, doc: serde_json::Value| {
            std::fs::write(
                sessions.join(name),
                serde_json::to_string_pretty(&doc).unwrap() + "\n",
            )
            .unwrap();
        };
        write_doc(
            "session-a.json",
            serde_json::json!({
                "version": 5,
                "record": {"identity": {"createdAt": 1, "cwd": "/w"}, "rows": {"title": {"ver": 1, "seq": 2, "val": "A"}}}
            }),
        );
        write_doc(
            "session-b.json",
            serde_json::json!({
                "version": 6,
                "record": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 2, "val": "B"}}}
            }),
        );
        // foreign 戳：web parseRecord 契约——视为 absent
        write_doc(
            "session-c.json",
            serde_json::json!({
                "version": 99,
                "record": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 2, "val": "C"}}}
            }),
        );
        // backupRecord 挪开的备份：不再以 .json 结尾，读侧忽略
        std::fs::write(
            sessions.join("session-d.json.bak.202609021548"),
            r#"{"version": 5, "record": {"rows": {"title": {"ver": 1, "seq": 2, "val": "D"}}}}"#,
        )
        .unwrap();
        // 残缺信封：跳过
        std::fs::write(sessions.join("session-e.json"), r#"{"record": {}}"#).unwrap();

        // 旧整档：session-a 已在树上（不覆盖），session-f 回退生效
        std::fs::write(
            dir.join("session_projcache.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "unit": {"name": "session_projcache", "version": 3},
                "global": null,
                "tables": {"sessions": {
                    "session-a": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "stale-A"}}},
                    "session-f": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "F"}}}
                }}
            })).unwrap(),
        ).unwrap();

        let titles = load_projcache_titles(&dir);
        let get = |id: &str| titles.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone());
        assert_eq!(get("session-a").as_deref(), Some("A"));
        assert_eq!(get("session-b").as_deref(), Some("B"));
        assert_eq!(get("session-c"), None, "foreign stamp reads as absent");
        assert_eq!(get("session-d"), None, "backup file is ignored");
        assert_eq!(get("session-e"), None, "malformed envelope reads as absent");
        assert_eq!(get("session-f").as_deref(), Some("F"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_projcache_titles_gates_legacy_stamp() {
        // 旧整档也要过版本门（web bootstrap 同款）：stamp 不在接受集内的
        // 整档不解释
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-titles-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session_projcache.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "unit": {"name": "session_projcache", "version": 99},
                "global": null,
                "tables": {"sessions": {
                    "session-x": {"identity": {"createdAt": 1}, "rows": {"title": {"ver": 1, "seq": 1, "val": "X"}}}
                }}
            })).unwrap(),
        ).unwrap();
        assert!(load_projcache_titles(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn request_header_round_trips_to_disk() {
        // 回归：event_to_web_line 曾把 RequestHeader 落进 `_ => None`，
        // 「模型实际看到的 system/tools/config」从未写盘——会话审计
        // （用-查-改-用的「查」）无从验证 prompt 修复是否生效。
        let dir = std::env::temp_dir().join(format!("dsh-persist-header-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-header");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let header = EpochHeader {
            config: serde_json::from_value(serde_json::json!({
                "provider": "deepseek", "model": "test-model", "maxTokens": 1024
            }))
            .unwrap(),
            system: Some("You are DeepSeek Harness (Rust).".into()),
            tools: None,
        };
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::RequestHeader {
                header,
                reason: HeaderReason::Initial,
            },
        )
        .unwrap();

        // 落盘行形：request/header（v3 起无 header.system——system prompt 经
        // system/message 行持久化）+ reason=initial
        let file = rec.session_file(&id, "/tmp/ws");
        let raw_bytes = zstd::stream::decode_all(std::fs::File::open(&file).unwrap()).unwrap();
        let raw = String::from_utf8(raw_bytes).unwrap();
        assert!(raw.contains("\"request/header\""), "{raw}");
        assert!(!raw.contains("You are DeepSeek Harness (Rust)."), "{raw}");
        assert!(raw.contains("\"initial\""), "{raw}");
        assert!(raw.contains("\"version\":4"), "{raw}");

        // 读回：typed 事件还原（header 无 system）
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        let restored = session
            .request_header()
            .expect("header should survive load");
        assert_eq!(restored.system, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_event_classification_loads_or_refuses() {
        // 回归：桌面端自己落盘的 tool/call、web 存储层的 chunk 打包记录
        // 都是构建已知类型——不得触发 fail-closed 拒绝（曾因此点击会话
        // 无反应：load Err 被静默吞掉）。
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-ignorable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-classes");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let file = rec.session_file(&id, "/tmp/ws");
        let row = |ty: &str, ignorable: bool| {
            let mut o = serde_json::json!({
                "type": ty,
                "seq": 99,
                "time": 1,
                "data": {"toolCallId": "c1", "name": "fs"},
            });
            if ignorable {
                o["ignorable"] = serde_json::json!(true);
            }
            o.to_string()
        };
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("tool/call", false)
            ),
        )
        .unwrap();
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("text-chunks", false)
            ),
        )
        .unwrap();
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("plugin/future-thing", true)
            ),
        )
        .unwrap();
        // ignorable 未知：跳过；tool/call 行此前被丢弃（无读臂——回放
        // stats_tools 恒 0、web 读侧缺工具行），现有读臂：恰好一条事件
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        let tool_calls = session
            .entries()
            .iter()
            .filter(|e| matches!(&e.event, SessionEvent::ToolCall { .. }))
            .count();
        assert_eq!(tool_calls, 1, "tool/call row now parses (was silently dropped)");

        // 未知且必读：拒绝解释整份日志
        rec.append_frame(
            &file,
            &format!(
                "{}
",
                row("plugin/required-thing", false)
            ),
        )
        .unwrap();
        assert!(rec.load(&id, Some("/tmp/ws")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn injection_sources_parse_and_round_trip() {
        // web 注入行（agent-instructions + changes[].path）解析为
        // MessageSource::Context 并经 zstd 往返保留
        let line = serde_json::json!({
            "type": "user/message",
            "data": {
                "id": "m1",
                "role": "user",
                "content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}],
                "source": {
                    "kind": "agent-instructions",
                    "form": "instructions",
                    "baseline": true,
                    "changes": [{"action": "set", "path": "packages/AGENTS.md"}]
                },
                "surfaceOp": "append"
            }
        });
        let ev = web_line_to_event(&line).expect("user message event");
        let SessionEvent::UserMessage(m) = &ev else {
            panic!("wrong event")
        };
        let MessageSource::Context {
            context_kind,
            changes_paths,
            form,
            ..
        } = &m.source
        else {
            panic!("expected Context source, got {:?}", m.source)
        };
        assert_eq!(context_kind, "agent-instructions");
        assert_eq!(changes_paths, &vec!["packages/AGENTS.md".to_string()]);
        assert_eq!(form, &Some("instructions".to_string()));

        let dir = std::env::temp_dir().join(format!("dsh-persist-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-ctx");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(&id, "/tmp/ws", &ev).unwrap();
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        let reloaded = session.entries().iter().find_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => Some(m.source.clone()),
            _ => None,
        });
        assert_eq!(reloaded, Some(m.source.clone()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_with_drifted_cwd_stays_in_canonical_bucket() {
        // 缺陷复现防护：会话建在桶 A，之后以桶 B 的 cwd 口径追加——
        // 必须落在桶 A 的原日志上，绝不产生桶 B 副本
        let dir =
            std::env::temp_dir().join(format!("dsh-persist-canonical-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-drift");
        rec.create(&id, "/tmp/ws-a", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws-a",
            &SessionEvent::UserMessage(Message::user_text("real")),
        )
        .unwrap();

        // cwd 口径漂移（应用侧把当前会话误标为另一个工作区）
        rec.append(
            &id,
            "/tmp/ws-b",
            &SessionEvent::SessionTitle { title: "t".into(), source: dsh_session::TitleSource::Fallback },
        )
        .unwrap();
        rec.append(
            &id,
            "/tmp/ws-b",
            &SessionEvent::UserMessage(Message::user_text("second")),
        )
        .unwrap();

        let a_file = dir
            .join("sessions")
            .join(project_key("/tmp/ws-a"))
            .join(id.as_str())
            .join("session.v4.jsonl.zstd");
        let b_file = dir
            .join("sessions")
            .join(project_key("/tmp/ws-b"))
            .join(id.as_str())
            .join("session.v4.jsonl.zstd");
        assert!(a_file.exists());
        assert!(!b_file.exists(), "cross-bucket copy created");
        let (session, _) = rec.load(&id, None).unwrap();
        let users: Vec<_> = session
            .entries()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::UserMessage(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2, "both appends landed on the canonical log");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_never_overwrites_existing_log() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-guard");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::UserMessage(Message::user_text("keep me")),
        )
        .unwrap();
        // 重复 create（如自愈路径误入）不得清掉已有日志
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
        assert!(session.entries().iter().any(|e| matches!(
            &e.event,
            SessionEvent::UserMessage(m) if m.content.iter().any(|b| matches!(b, dsh_llm::ContentBlock::Text { text } if text == "keep me"))
        )));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_scans_all_project_buckets() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let a = SessionId::new("session-a");
        let b = SessionId::new("session-b");
        rec.create(&a, "/tmp/ws-a", "standard").unwrap();
        rec.create(&b, "/tmp/ws-b", "standard").unwrap();
        let list = rec.list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|e| e.id == a && e.cwd.as_deref() == Some("/tmp/ws-a"))
        );
        assert!(
            list.iter()
                .any(|e| e.id == b && e.cwd.as_deref() == Some("/tmp/ws-b"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use dsh_session::SessionEvent;

    #[test]
    fn turn_end_error_reason_round_trips() {
        // 回归：turn/end 的 reason 曾被硬编码为 "completed"，失败轮次
        // （LLM 余额不足/超时/中止）在日志里永远"成功"——审计与历史
        // 回放都无法区分。真实 reason 必须双向保真。
        let failure = dsh_llm::LlmFailure {
            message: "Insufficient Balance".into(),
            code: "QUOTA".into(),
            status: Some(402),
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: None,
        };
        let event = SessionEvent::TurnEnd {
            turn: 3,
            reason: dsh_session::TurnEndReason::Error { failure },
        };
        let row = event_to_web_line(&event, 9, 5678).expect("turn/end must map");
        assert_eq!(row["data"]["reason"]["kind"], "error", "{row}");
        // v2 词汇：error 载荷键 = error（web 命名）
        assert_eq!(row["data"]["reason"]["error"]["code"], "QUOTA");
        match web_line_to_event(&row).expect("turn/end must parse back") {
            SessionEvent::TurnEnd { turn, reason } => {
                assert_eq!(turn, 3);
                match reason {
                    dsh_session::TurnEndReason::Error { failure } => {
                        assert_eq!(failure.code, "QUOTA");
                        assert_eq!(failure.message, "Insufficient Balance");
                    }
                    other => panic!("expected Error reason, got {other:?}"),
                }
            }
            other => panic!("expected TurnEnd, got {other:?}"),
        }
        // 旧格式（硬编码 completed）仍可读
        let legacy: serde_json::Value = serde_json::json!({
            "type": "turn/end", "seq": 1, "time": 1, "data": {"turn": 1, "reason": {"kind": "completed"}}
        });
        match web_line_to_event(&legacy).unwrap() {
            SessionEvent::TurnEnd {
                reason: dsh_session::TurnEndReason::Completed,
                ..
            } => {}
            other => panic!("expected Completed for legacy row, got {other:?}"),
        }
    }

    #[test]
    fn assistant_tool_call_blocks_survive_disk_round_trip() {
        // 回归：历史会话回放（gpui rebuild_from_session）依赖 assistant/
        // message 里的 tool-call 块渲染工具轨迹——落盘/读回任一侧丢块，
        // 打开历史会话就只剩用户气泡。
        let message = Message::assistant(
            vec![
                ContentBlock::text("让我看一下。"),
                ContentBlock::ToolCall {
                    id: CallId("call_1".into()),
                    name: "fs".into(),
                    arguments: r#"{"op":"list","path":"E:\x"}"#.into(),
                },
            ],
            "deepseek",
            "test-model",
        );
        let event = SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            message,
            interrupted: false,
            usage: None,
            time_ms: None,
        };
        let row = event_to_web_line(&event, 7, 1234).expect("assistant/message must map");
        assert_eq!(row["type"], "assistant/message");
        let back = web_line_to_event(&row).expect("assistant/message must parse back");
        match back {
            SessionEvent::AssistantMessage { message, .. } => {
                let kinds: Vec<&str> = message
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { .. } => "text",
                        ContentBlock::ToolCall { .. } => "tool-call",
                        _ => "other",
                    })
                    .collect();
                assert_eq!(kinds, vec!["text", "tool-call"]);
                match &message.content[1] {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(id.0, "call_1");
                        assert_eq!(name, "fs");
                        assert!(arguments.contains("\"op\":\"list\""));
                    }
                    other => panic!("expected tool-call, got {other:?}"),
                }
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }
    }

    /// 权限三旋钮事件 web 行形往返（data.{preset|mode|policy}，互通 web）。
    #[test]
    fn permission_knob_events_roundtrip() {
        let cases = [
            (
                SessionEvent::PermissionPreset { preset: "workspace-write".into() },
                "permission/preset",
                "preset",
                "workspace-write",
            ),
            (
                SessionEvent::SandboxModeSwitch { mode: "read-only".into() },
                "sandbox/mode",
                "mode",
                "read-only",
            ),
            (
                SessionEvent::ApprovalPolicy { policy: "never".into() },
                "approval/policy",
                "policy",
                "never",
            ),
        ];
        for (event, ty, field, value) in cases {
            let row = event_to_web_line(&event, 1, 1).expect("knob event must serialize");
            assert_eq!(row["type"], ty);
            assert_eq!(row["data"][field], serde_json::json!(value));
            let back = web_line_to_event(&row).expect("knob event must parse back");
            assert_eq!(back, event);
        }
    }
}

#[cfg(test)]
mod time_roundtrip_tests {
    use super::*;

    /// 信封 time 承载：写侧保留事件 time_ms（回放再落盘往返稳定），
    /// 读侧回传 Some——轨迹时长列的日志数据源。
    #[test]
    fn envelope_time_roundtrips() {
        let event = SessionEvent::AssistantMessage {
            turn: 1,
            step: 2,
            message: dsh_llm::Message::assistant(
                vec![dsh_llm::ContentBlock::text("hi")],
                "mock",
                "mock",
            ),
            interrupted: false,
            usage: None,
            time_ms: Some(1_788_672_421_453),
        };
        let row = event_to_web_line(&event, 7, 999).expect("must map");
        assert_eq!(row["time"], serde_json::json!(1_788_672_421_453u64));
        let back = web_line_to_event(&row).expect("must parse back");
        match back {
            SessionEvent::AssistantMessage { time_ms, .. } => {
                assert_eq!(time_ms, Some(1_788_672_421_453));
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }
        // 实时事件无承载 → 写侧用传入的落盘时刻
        let live = SessionEvent::StepStart { turn: 1, step: 1, time_ms: None };
        let row = event_to_web_line(&live, 8, 1234).expect("must map");
        assert_eq!(row["time"], serde_json::json!(1234u64));
        let back = web_line_to_event(&row).expect("must parse back");
        match back {
            SessionEvent::StepStart { time_ms, .. } => assert_eq!(time_ms, Some(1234)),
            other => panic!("expected StepStart, got {other:?}"),
        }
    }
}
