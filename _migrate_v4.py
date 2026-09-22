import io

p = 'crates/dsh-persist/src/v2.rs'
src = io.open(p, encoding='utf-8').read()

# 1) 版本常量 4
old = '''/// 当前会话格式版本（web `SESSION_FORMAT_VERSION`）。0.1.5-alpha.1 起为 3：
/// system prompt 从 request/header 晋升为 `system/message` 面节点、PTC 词汇
/// 改名（code→ptc）、canonical 信封（replace 富形 startSeq/endSeq）。
pub const SESSION_FORMAT_VERSION: u64 = 3;'''
new = '''/// 当前会话格式版本（web `SESSION_FORMAT_VERSION`）。0.1.7-alpha.1 起为 4：
/// tool/result 升一等 tool 角色消息（剥 wrapper、role:'tool' 平铺）、plugin
/// source 转 producer kind（kind 即生产者名）、缺失 turn/end 补齐。
/// 0.1.5-alpha.1 的 3：system prompt 晋升 system/message + PTC 改名。
pub const SESSION_FORMAT_VERSION: u64 = 4;'''
assert old in src, 'version block'
src = src.replace(old, new, 1)

# 2) 迁移级联加 v4 段
old2 = '''            let version = v2::header_version(&v2::read_header(&file).unwrap_or_default());
            if version < v2::SESSION_FORMAT_VERSION {
                if version < 2 {
                    file = v2::migrate_to_v2(&file, id)?;
                }
                file = v2::migrate_v2_to_v3(&file, id)?;'''
new2 = '''            let version = v2::header_version(&v2::read_header(&file).unwrap_or_default());
            if version < v2::SESSION_FORMAT_VERSION {
                if version < 2 {
                    file = v2::migrate_to_v2(&file, id)?;
                }
                if version < 3 || v2::header_version(&v2::read_header(&file).unwrap_or_default()) < 3 {
                    file = v2::migrate_v2_to_v3(&file, id)?;
                }
                file = v2::migrate_v3_to_v4(&file, id)?;'''
assert old2 in src, 'cascade'
src = src.replace(old2, new2, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
print('v2.rs cascade ok')

# 3) lib.rs：迁移级联注释 + KNOWN 表扩充
p = 'crates/dsh-persist/src/lib.rs'
src = io.open(p, encoding='utf-8').read()
old3 = '''        // 写打开旧代 → 级联迁移（v0/v1 先到 v2，再统一 v2→v3；源文件保持
        // 原样；此后追加全在当前代）——上游 `ensureCurrentLog` 语义'''
new3 = '''        // 写打开旧代 → 级联迁移（v0/v1→v2→v3→v4；源文件保持原样；
        // 此后追加全在当前代）——上游 `ensureCurrentLog` 语义'''
if old3 in src:
    src = src.replace(old3, new3, 1)

# KNOWN 表追加（V3 词汇全集差集 + V4 新类型）
known_adds = [
    'deliverables/presented',
    'developer/message',
    'feedback/message-put',
    'feedback/message-delete',
    'image/offload',
    'subagent/catalog',
    'workspace/changes',
]
for k in known_adds:
    marker = f'    "{k}",\n'
    if marker not in src:
        # 插在 KNOWN 表合适位置（字母序附近锚点）——统一插在 "system/message", 行后
        anchor = '    "system/message",\n'
        assert anchor in src, k
        src = src.replace(anchor, anchor + marker, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
print('lib.rs known ok')
