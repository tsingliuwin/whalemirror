# rustdsh 功能点清单（回归遍历用）

每项标注验证方式：`[单测]` cargo test 覆盖、`[语料]` examples 真实 ~/.dsh 数据、`[UI]` 实机点击+截图。
UI 项给出入口路径与通过判据。回归时按层推进：A 全绿才进 B-H（UI 层失败先回查 A 层）。

## A. 核心链路（无 UI，regression-core.sh 一键跑）

| # | 功能点 | 验证 | 通过判据 |
|---|--------|------|----------|
| A1 | 全 workspace 单测（adapter 流式身份/4xx 分类、projcache 双布局/戳保留/版本门、session 投影等 34 组） | 单测 | 0 failed |
| A2 | 真实 ~/.dsh/sessions 全量加载（zstd 多帧 read_across_frames） | 语料 | load_web 列出全部会话无错误 |
| A3 | projcache 双布局标题互通（per-record 优先、旧整档回退、版本门） | 语料 | share_check 标题数 >0 |
| A4 | 按步条件工具准入（补缺 #41）：通用原语（ToolAdmission 四事实 + register_when/list_admitted/admitted）与 agent-loop 接入；plan 渐进披露（进入前不声明/进入后出现/离开仍声明/未准入 unknown tool/离开后 unavailable）；路由条件随模型切换生效 | `cargo test -p dsh-tools registry_admission` + `cargo test -p dsh-agent-loop --test plan_tool_admission` | 3 + 4 测全绿 |
| A5 | DeepSeek /models 列表核心（补缺 #39 子批 a+b+c）：URL/解析/事实分层/版本门/时序；fetch（redirect 拒绝/错误码/超时/不可达）；凭据作用域缓存（首次等待/过期后台刷新/失败保留且只报首条/invalidate/close/代际作废） | `cargo test -p dsh-llm-deepseek listing` | 22 测全绿（listing 15 + listing_cache 7） |
| A6 | 轨迹 run_code 记录事实解析（补缺 #54）：声明式前缀优先校验/参数提示 ASCII 单词边界/并集冲突不着色/description 回退首行（CRLF） | `cargo test -p dsh-gpui code_program` | 10 测全绿 |

## B. 聊天流（chat.rs / widgets.rs）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| B1 | 会话加载渲染：点侧栏会话行 → 历史消息气泡（角色/对齐） | 点任一历史会话 | 消息完整、无缺块 |
| B2 | markdown：标题/列表/表格/引用/行内代码/代码卡（语言标注+复制钮） | 发送带 markdown 的消息 | 各元素样式与 web 一致 |
| B3 | 工具卡分派：shell→terminal、fs read→read（行号 gutter、8 行折叠、复制）、fs write→diff（footer +A -R）、web_search→search（分组折叠）、web_fetch→网页获取卡、grep/glob→结果卡、未知→io 兜底 | 触发对应工具的会话 | 卡类型正确、错误行走 io 兜底 |
| B6 | shell 退出胶囊（补缺 #35①）：bash 结果尾标记 `[exit code: N]`/`[exit code: null]`/`[killed by signal: X]` 解析——行尾中性胶囊（4px 红点+「退出码 N/未正常退出/信号 X」）、卡内 banner 红字状态 pill、gutter 点转红、输出剥标记无双重渲染；干净退出（0/无标记）无胶囊；胶囊在场时 failureLine 红摘要与 fileLink 分支抑制（历史无标记行渲染不变） | 冒烟会话（bash 结果尾带 `[exit code: 7]`）+ 历史无标记会话 | 失败行见胶囊与红点、卡内 pill 同文案、输出无标记尾行；干净行无胶囊；旧会话渲染不变 |
| B7 | 轮次失败行 + QUOTA toast（补缺 #38）：LLM 失败轮显示红点+「本轮运行失败」+本地化消息+右贴 code chip（QUOTA→「当前请求的额度已用尽」）；live QUOTA 失败同时弹全窗 toast（自动消失）；切走再切回仅剩行、不弹 toast | 余额不足时发消息（或合成 turn/end error 会话回放） | 行文案本地化、chip 右侧 mono；toast 只 live 弹；回放会话不弹；turn/end reason=max-tokens 的会话在该轮末显示 amber 提示行「已达到输出 token 上限 + 截断说明」（合成日志可验），普通完成轮无此行 |
| B4 | 折叠/展开点击（折叠后列表高度重测，无残影） | 点 read/搜索卡折叠钮 | 展开完整、下方内容随动 |
| B5 | 流式输出：发送 → delta 渐进上屏 → 结束停止；发送钮变停止钮 | 发一条真实消息 | 逐字出现、结束后恢复 |
| B6 | 操作常显规则：最新轮尾部与最新用户行操作常显，更早轮整行悬停显现 | 悬停历史消息 | 与 web 一致 |
| B7 | 消息流顶部 16px 内边距（首条气泡不贴 header 分隔线） | 打开任一会话 | 目测间距 |
| B8 | 用户气泡字号 14/22 | 目测 | 与 web .bubble 一致 |
| B9 | 错误行渲染（RATE_LIMIT 等）：红色 + 出错位置 | 触发限流的会话 | 文案完整可复制 |

## C. Composer

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| C1 | inert 态：空会话且未绑定工作区 → 编辑器不挂载、hero 引导选工作区 | 新建纯草稿 | 占位引导文案，无输入框 |
| C2 | 多行输入：Enter 发送（按 E3 设置）、Shift+Enter 换行 | 输入 | 行为符合设置 |
| C3 | 模型选择菜单：打开/切换模型/点外关闭（adopt 下拉同款窗口级 mousedown） | 点 composer 右侧模型名 | 菜单开合正常、点外即关 |
| C4 | 权限 chip 显示（Workspace Write） | 目测 | 文案与 web 一致 |
| C5 | @文件引用注入 | 输入 @path 后发送 | 发送时解析已键入的 `@path`、每个存在的文件注入一条 context 行（上游 file-reference 的等价子集）。**候选弹层未实现**（上游 `ui-input-trigger` 的 `@` 源；见路线图 N23），不得据此判「引用完整」 |
| C6 | 繁忙时 Enter：按 E3 设置排队或打断 | 流式中按 Enter | 行为符合设置 |
| C7 | 模型切换公告（0.1.3-alpha.2）：会话中经 C3 切换模型后，下一轮消息批尾追加 user/plugin `model-selection` notice（summary「旧 → 新」），随日志派生进入模型历史；请求头 reason=change | C3 切换模型后发消息 | 日志新增 user/message 行（source 带 form=notice/summary）+ request/header reason=change；聊天流出现公告文本（显示形态为既有用户消息渲染面，上游折叠行不可复刻见偏差表） |
| C8 | v3 落盘序与 system 面节点（0.1.5-alpha.1）：step/start → system/message（head append，变化 replace）→ user 批；request/header 无 system 字段 | 跑一轮会话后查日志 | 日志行序正确、session.v3.jsonl.zstd、request/header data.header 无 system、system/message 行 source=plugin @dsh-system-prompt |
| C9 | 统计双 pill + 对话框（0.1.5-alpha.1）：composer 下仪表 pill（轮步+TPS）与数据 pill（总 token+缓存命中），点击开互斥对话框（会话统计 / Token 用量），再点关闭 | 跑一轮后看 composer 下方 | pill 数值正确、对话框行与数据一致（模型用时/TTFT/TPS；输入/缓存读/缓存写/输出，缓存写为 0 时该行省略）、互斥开合 |
| C10 | 附件卡文件类型图标（0.1.5-alpha.2）：回形针选文件/气泡附件卡按扩展名出类色文件底+白 mark 图标（pdf 红/word 蓝/excel 绿/图片紫/代码 deepseek 蓝/其他灰）| 附件 TXT/PDF/PNG/RS 各一张 | 图标随类型变化、未知扩展回落灰 other、双层渲染无错位 |
| C11 | 计划卡（0.2.1-alpha.1 补缺）：模型调 exit_plan_mode 提交计划后，轮尾出现计划卡（markdown 图标座+标题「计划 · Markdown」描述+打开钮），点击弹 Popover 展示计划全文（等宽块） | 让模型写计划提交 | 卡片出现、标题正确、Popover 展示全文 |
| C12 | 图片粘贴捕获（0.2.1-alpha.1 补缺子批 3）：剪贴板复制图片后 Ctrl+V → 图片作为附件 tile 出现在 composer 草稿（同回形针流终点），输入框不插入乱码文本；PNG/JPEG 外格式走文本粘贴 | 复制图片→composer 粘贴 | 草稿图片 tile 出现、发送含 image 块 |
| C13 | 非人类消息 context 行（10-09 补缺）：plan-mode 切换后下一轮轮头出现「上下文注入」行（标签 plan-mode，notice 摘要），不再是用户气泡；model-switch 公告同形（标签 model-selection） | /plan 切换→发消息开轮 | 轮头 context 行出现、标签正确、展开可见通知文本、落盘 source 为 producer-owned kind（plan-mode/model-selection） |
| C14 | subagent 子会话耐久化 + 运行中实时查看（0.1.6-alpha.2 补缺 #4）：模型调 subagent 委派任务后，子会话落盘（标题=description）+ 父日志 subagent/catalog 行；侧栏即时收录（活动即入列、免轮终免重启）；点开子会话后转录随写随刷（250ms 节流重放） | 让模型委派 subagent 任务→侧栏即现子会话→点开观察执行过程 | 工具结果含子报告；侧栏免重启出现子会话且运行中即可点开；查看中转录持续增长、轮次展开态不跳动；父会话日志含 subagent/catalog（dump_lines 可见） |
| C15 | composer 草稿随会话存取（补缺 #26）：会话 A 的 composer 打字/附件切到会话 B 后不带入 B（B 显示自己的存档草稿或空），切回 A 文本/附件恢复；空白草稿切走即弃（用户定向）；在途附件上传完成后回写不因切换丢失 | 会话 A 输入框打字→点侧栏会话 B→看 B 的 composer→点回 A | B 的 composer 无 A 的文本/附件；切回 A 文本恢复；发送 B 消息不含 A 草稿内容 |
| C16 | busy-Enter 投递语义（补缺 #27）：运行中 plain Enter/发送按钮=设置偏好（排队发送/打断）；Ctrl+Enter=按次反相；运行中+有稿主钮仍可发送（tooltip 排队发送/打断发送）；停止钮只在运行中+空稿出现且对准被查看会话（含 runtime）；打断取消的是被查看会话的轮 | 设置 busyEnter=打断→运行中发消息→消息立即打断重启；改排队→运行中发消息→入队待轮终；Ctrl+Enter 与设置相反；查看 runtime B 运行中点停止→B 停止 | 打断即时收束旧轮新消息开新轮；排队消息轮终被消费；Ctrl+Enter 与 Enter 行为互反；停止不影响主槽 |
| C17 | 排队条带（补缺 #28 首批）：busy+排队投递的消息不在转录立即回显，改在输入卡上方条带呈现（单条直出/多条「N 条排队消息」计数头可折叠）；消费时刻（下轮开始）转入转录为用户气泡；打断取消后条带与幽灵气泡都消失 | 设置排队→运行中发两条消息→看输入卡上方条带 | 两条以计数头+可折叠呈现；当前轮结束后下轮开始时条带消失、消息以用户气泡入列；打断（Ctrl+Enter 反相或停止钮）后条带消失无残留 |
| C18 | 排队插话 steer（补缺 #28 子批二）：条带行「插话发送」钮把该消息提升进当前轮（当前轮继续吃掉它而非等下一轮）；空稿 Ctrl+Enter 整条带插话；非运行中行钮不可点（弱化+「仅运行中可插话发送」） | 运行中发两条排队→点其中一条的插话钮→观察 | 被插话消息在本轮后续步中被消费（转录立即出现该用户气泡）；剩余排队条待下一轮；空稿 Ctrl+Enter 后全部入列；停止后行钮弱化 |
| C19 | 排队行编辑/删除（补缺 #28 子批三）：纯文本行可「编辑排队消息」（行内输入+保存/取消，保存后条带预览与 inbox 内容同步更新）；含附件行编辑弱化+「包含非文本内容，暂不支持编辑」；「删除排队消息」移除该行；均仅运行中可操作 | 运行中发纯文本排队→点铅笔→改文本保存→看预览与下轮实际入列内容；再发一条带附件的→看编辑钮；点删除 | 保存后下轮消费的是新文本；附件行编辑钮弱化；删除后该消息不再入列；打断后条带清空无残留 |
| C21 | 排队行附件 chips（补缺 #28 收口）：带附件的排队行在预览文本后缀附件 chips（图片→「图片」、文件→文件名，surface_2 底圆角块）；纯文本行无 chips | 运行中发带附件消息→看排队条带 | 附件 chip 出现在预览文本后；纯文本排队行无 chip；消费后随行消失 |
| C20 | 官方 Toast 通知（补缺 #31）：附件未就绪发消息→右上/右下浮出「文件还在上传，请等待上传完成后发送」toast（自动消失），行内红条退役；发送仍被拦截、草稿保留 | 附件上传中发消息 | toast 浮出且自动消失；消息未发送、草稿与附件卡保留；重试路径正常 |
| C22 | 用户文本凭据掩码（补缺 #32，ui-secret-mask）：用户气泡文本命中已发布凭据格式（sk- 含数字≥20、ghp_ 等 28 前缀、JWT 三段、私钥块、`scheme://user:password`）时显示 `*` 掩码——前缀键保留前 8 字符、私钥块保留 BEGIN/END 行、地址段整掩码、换行保留；无凭据/assistant 消息原样走 markdown 管线 | 发含 `sk-abc123…` 的消息→看气泡；发普通消息；看 assistant 复述 key 的消息 | 用户气泡内凭据呈 `sk-abc12****` 形；普通消息渲染不变；assistant 消息不掩码；存储/发送原文不变（点编辑可见原值） |
| C23 | 任务清单面板 todo_write（补缺 A1.1）：模型调 `todo_write` 后，composer 上方出现「任务」面板（清单图标 + 标题 13/24/500 + 进度摘要「N 已完成 · N 进行中 · N 待处理」13/20 三级 + chevron），默认折叠；点标题行展开 → 逐条任务（16px 状态点：已完成 / 进行中 / 待处理 + 内容 13/20 二级、单行省略、最大高 180）；下一次 turn/start 清空（轮末保留清单可见）；切走再切回从日志 `todo/write` 恢复 | 让模型提交多步计划 | 面板出现且文案/几何如上；展开后逐条正确；新轮开始后清空；重载会话恢复剩余清单 |
| D9 | 删除会话（用户定向本地功能，上游仅归档）：会话行 … 菜单「删除会话」→ 确认弹窗（整份日志移除不可恢复 + 归档导引）→ 确认后目录/名册/列表行全清；删除当前会话自动切到最近剩余会话 | 会话行 … → 删除 → 确认 | 磁盘目录消失（~/.dsh/sessions/<proj>/<id>/）、列表行消失、当前者被删后视图切走、取消弹窗无副作用 |

## D. 侧栏（sidebar.rs）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| D1 | 工作区分组列表（cwd 分组、目录名、折叠箭头） | 打开侧栏 | 分组正确 |
| D2 | 会话行：标题（projcache 优先、web 标题回退、新会话兜底）、时间标签（刚刚/N分钟/N天/N个月）、blank 态 | 目测 | 与真实语料一致 |
| D3 | 新建会话：+ → 当前工作区已存 blank 会话复用/新建 | 点 + | 列表/标题正确 |
| D4 | 会话搜索过滤 | 输入搜索词 | 列表过滤 |
| D5 | 工作区/会话重命名（行菜单 → 输入框） | 右键/… 菜单 | 改名生效并持久化 |
| D6 | 会话归档（菜单 → 归档后从列表消失、不入启动选择） | 行菜单 | 列表更新 |
| D7 | 菜单点外关闭（窗口级 mousedown 外关 + 触发器只开不关防回弹） | 开菜单后点空白 | 菜单关闭 |
| D8 | 零工作区 hero 引导（不落盘、不进列表） | 无 ~/.dsh 时启动 | 引导态 |
| D9 | 侧栏折叠（« ）与列宽拖拽 | 拖中栏分隔 | 宽度记忆 |

## E. 设置-通用

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| E1 | 外观：浅色/深色/跟随系统（即时切换，分段控件） | 设置→通用→外观 | 主题即时生效 |
| E2 | 界面语言：中文/English | 同上 | 文案切换 |
| E3 | 繁忙时 Enter 行为：排队发送/打断 | 同上 | 写入 settings.yaml |
| E4 | 对话视图：常规/紧凑 | 同上 | 气泡密度切换 |
| E5 | 打开配置文件（settings.yaml 用系统关联程序） | 设置头右上 | 文件打开 |

## F. 设置-模型（settings.rs models_page）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| F1 | DeepSeek 卡：env 锁定只读态（DEEPSEEK_API_KEY 由启动环境提供）；编辑卡 key 替换 | 设置→模型 | 锁定文案/替换生效 |
| F2 | 自定义行卡：凭据绿点（有 key）、编辑/删除按钮、自定义 tag（目录外 route 才打） | 目测 | 与 yaml 一致 |
| F3 | 删除确认弹层：取消/确认（删除后 settings.yaml providers 移除、凭据保留） | 删除 | 二次确认、持久化 |
| F4 | 编辑卡：API 密钥（已配置 placeholder）、自定义设置折叠（API 地址/模型目录只读行/其余字段 hint）、保存（换 key 重注册 adapter）| 编辑 | 保存后路由生效 |
| F5 | adopt 卡：目录下拉（候选=目录未 adopt 项）、点外关闭防回弹、高级自定义折叠（API 地址）、保存 | 添加提供方 | 注册+持久化 |
| F6 | declare 卡：Provider ID 校验（格式错误/重复→error 行，空→hint）、显示名称、API 地址必填、协议恒 openai、API 密钥、模型行增删（查重）、至少一模型、创建 | 添加自定义提供方 | 错误行逐条正确、创建后注册 |
| F7 | 获取可用模型（declare）：空地址禁用置灰、填地址可点、busy「正在询问提供方…」、失败行（401/403 带 check the API key、无模型文案、UNSUPPORTED）、成功→候选弹层 | 填地址+key 后点 | 各分支正确 |
| F8 | 候选弹层：标题「选择要添加的模型」/描述/搜索过滤（id+name 子串）/全选-取消全选（rc.1 语义：过滤态取消全选清空整个集合）/单行勾选/取消（清候选与搜索词）/添加所选（已配置行胜出、容量 K/M 保留、去重） | 弹层内操作 | 与 web ModelListEditor 一致 |
| F9 | 获取可用模型（编辑卡）：恒可点（web askable 恒真）、key/地址回退链（表单键入→已存→目录 base_url）、空 base → 完整诊断 `pi-ai ships no catalog for provider "X", ...`、采纳写 display_name/容量进已存提供方 | 编辑卡折叠区内 | 各分支正确 |

## G. 轨迹视图

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| G1 | 对话/轨迹 tab 切换 | 顶部 tab | 视图切换、状态保持 |
| G2 | 轨迹事件列表渲染 | 切到轨迹 | 事件序列完整 |

## H. 框架/窗口

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| H1 | 自定义标题栏：最小化/最大化/关闭 | 窗口钮 | 正常 |
| H2 | 中栏列宽拖拽与记忆 | 拖分隔条 | 重启保持 |
| H3 | 标准模式 chip（composer 左侧） | 目测 | 与 web 一致 |

## 排障日志源

- `cargo test --workspace` 输出（断言失败即回归点）
- `cargo run --example load_web/share_check -p dsh-persist`（持久化互通）
- 会话原始日志：`~/.dsh/sessions/<dir>/<id>/session.jsonl.zstd`（zstd 多帧，用 `dump_lines`/`inspect_kinds` examples 解）
- 配置：`~/.dsh/settings.yaml`（llm-pi-ai.providers 段）、`~/.dsh/settings.json`、`~/.dsh/.credentials.yaml`、`~/.dsh/storages/`（projcache 双布局）
- UI 问题：CUA 截图 vs 上游 web 源码数值（packages/client/ui-*/src 与 *.module.css）

## I. 界面几何与样式数值（界面走查）

| # | 功能点 | 验证 | 通过判据 |
|---|--------|------|----------|
| I1 | 圆角标尺与 composer 统计 pill 几何（补缺 #60）：R4/R8/R12/R16/R20/R28 六档 + pill 半径 R8、字号 12、行高 20、padding 1/8、gap 6、图标 14 | `cargo test -p dsh-gpui ui_metrics` | 2 测全绿（标尺单调、pill 落 R8 档且总高 22px） |
| I2 | 实机比对：composer 统计 pill 观感 | 宿主内发一轮消息 → 看输入框上方两个统计 pill | 圆角为 8px 方角感（非全圆胶囊）、hover 有底色、与图标/数字基线对齐 |
| I3 | 侧栏会话行/菜单几何（补缺 #62）：行 H32/R12/左 9（12−3）/右 8/gap 6；菜单条目 pad 4×7、R12、字号 12、行高 20；小节标题左缘与条目对齐 | `cargo test -p dsh-gpui ui_metrics` | 4 测全绿（令牌换算 12−3=9、行高落 R12 档、条目自然高 28、菜单 pad 4/min 96） |
| I4 | 实机比对：侧栏行与弹出菜单观感 | 宿主内点会话行 → 看选中底色与标题左缘；点行尾「…」→ 看菜单条目间距/圆角/左缘对齐 | 会话标题与工作区图标左缘对齐、选中底色比内容窄 3px 且圆角 12；菜单条目更紧凑（约 28px 高）、圆角 12、小节标题与条目左缘齐 |
| I5 | 会话头标签条按压区（补缺 #63）：盒高 35 = 上 10 + 行 16 + 下 9、左右外扩 10、激活条 2px·贴底·左右内缩 10、容器 gap 20/pad-left 8/margin-top 10 | `cargo test -p dsh-gpui ui_metrics` | 5 测全绿（含 10+16+9=35 与热区>可视行） |
| I7 | 侧栏搜索药丸（补缺 #64）：展开态高 32/R12/右内距 4/搜索钮 28×32/输入 -2·13·18/清除钮 24·R8；**展开即聚焦** | `cargo test -p dsh-gpui ui_metrics` + 实机 | 6 测全绿；实机点放大镜后直接键入即过滤（无需再点输入框），点 ✕ 清空并收起 |
| I8 | markdown 代码块几何（补缺 #65）：R16（radius-lg）、代码体内距 16、横幅 9×14/gap12/11·18 | `cargo test -p dsh-gpui ui_metrics` + 实机像素实测 | 7 测全绿；11-01 实测（miaocr「常用命令」代码块）：块高 97px、角部曲线横向跨度约 16px（旧 6px），底部角半径与顶部同值；横幅「复制 bash」不裁切 |
| I9 | 统计 pill 读数等宽数字（补缺 #68）：数字段走 mono、文字/单位保持比例字体；小数点夹在数字间才并入；句末孤立点不算数值 | `cargo test -p dsh-gpui stat_reading` | 5 测全绿；实机「1 轮 22 步 / 864k / 缓存命中 0%」数字为等宽字形，读数变化时 pill 宽度不抖 |
| I10 | 设置页行几何（补缺 #69）：`.field` padding 12 上下、gap 6、标签 13/19.5、说明 12/19.2、行间 0.5px border-top | `cargo test -p dsh-gpui ui_metrics`；侧栏 ⚙ 设置打开通用设置页 | 单测锁常量；实机正文列分隔线间距 ≈69px（上游 68.7） |
| I11 | 三栏列宽契约（补缺 #70）：CENTER_MIN 400、右栏 ≤ 视口×0.7、右栏先让步、侧栏 264–420 | `cargo test -p dsh-gpui columns` | 7 测全绿；实机窗口缩到 900 宽时侧栏自动收成图标道、中栏满铺无横向溢出 |
| I12 | 设置面板外壳（补缺 #72）：宽 800、高 min(800, 视口高−48)、半径 R28 | `cargo test -p dsh-gpui ui_metrics`；侧栏 ⚙ 打开设置 | 单测锁高度换算；实机面板高 = 视口−48（800 视口 → 752）、圆角为 R28 面板档 |
| I13 | 设置导航选中格填充（补缺 #73）：亮 bluish-100 #EBEEF2 / 暗 bluish-750，且与 hover 档可分 | `cargo test -p dsh-gpui ui_metrics`；侧栏 ⚙ 打开设置 | 单测锁色值；实机选中格像素 = rgb(235,238,242) |
| I14 | 侧栏 dock 文件树行几何（补缺 #74）：行高 19.5+2×5≈29.5（不设固定高）、半径 R12、每级缩进 18、gap 6、note 12/3 | `cargo test -p dsh-gpui ui_metrics`；中栏头部 📁 胶囊打开 dock 文件树 | 单测锁常量；实机行距 29/28px、悬停底板高 29 且角落内缩 11/7/5/4px（R12 曲线）、`crates`→子项图标左移 18px |

| I15 | 预览/文件头路径标签（补缺 #75）：`pathPartsOf` 语义（目录含原生分隔符、去尾分隔符、纯分隔符全是名字）、头部行 38 / 左 16 / 右 6 / gap 4、路径 margin-right 12 | `cargo test -p dsh-gpui path_label` + `cargo test -p dsh-gpui ui_metrics`；dock 文件树点开 README.md | 6 测 + 1 测全绿；实机两处头部路径分隔符全为 `\`（无 `\`+`/` 混写），预览头路径右端到 wrap 钮 ≈16px |
| I16 | 设置页几何（补缺 #76）：导航 gap 4 / 格 40·R12·pad 9,12,16、行卡 R20·pad 12,14、卡间 8、标题块↔首卡 12、linkButton R8 | `cargo test -p dsh-gpui ui_metrics`；打开设置 → 模型页 | 单测全绿；实机导航行距 44（=40+4）、行卡左上圆角跨 ~8px（R20）、卡间 9px / 说明↔首卡 13px |
| I17 | 空会话 hero 工作区行（补缺 #77）：行内距 8、chip R8/gap4/高28/pad8、cap 360 + 标签省略号、展开态着色 | `cargo test -p dsh-gpui ui_metrics`；点「新建会话」→ 点工作区 chip | 单测全绿；实机 chip 左缘距卡面 ~9px、展开填充首行左内缩 6px（R8）、下拉与 chip 同左缘 |
| I18 | 28px 图标钮族（补缺 #78）：R8 方钮（非圆），composer「+」为 999px 圆 + selector 底，发送 34px 圆 | `cargo test -p dsh-gpui ui_metrics`；悬停侧栏搜索钮 | 单测全绿；实机搜索钮 hover 底板首行左内缩 5px（R8）、「+」为灰底正圆 |
| I19 | composer 输入卡（补缺 #79）：圆角 R28（面板档）、首行内距 8、行距 12 | `cargo test -p dsh-gpui ui_metrics`；任意会话底部输入卡 | 单测全绿；实机卡角曲线跨度 ~31px（R28）、改前改后差异只在两角与 2px 顶边带 |
| I20 | 用户消息气泡（补缺 #80）：圆角 R20、pad 10/16 | `cargo test -p dsh-gpui ui_metrics`；任意会话看自己的提问气泡 | 单测全绿；实机单行气泡 88×42、左缘圆角内缩序列 17/13/11/9/8…→0（R20） |
| I21 | 菜单行几何（补缺 #81）：min-h 34、左右内距 8（+菜单 4 = 12）、R12、13/20；与 dockkit 菜单家族区分 | `cargo test -p dsh-gpui ui_metrics`；点 composer 模型牌/权限钮/「+」命令菜单 | 2 测全绿；实机悬停行高 34px、左缘内缩 10/7/5/4/3/2/2/1（R12） |
| I22 | 会话头预设标签（补缺 #82）：静态 chrome——高 22、R4、gap 4、12-22、cap 180、图标 14，无 hover/指针 | `cargo test -p dsh-gpui ui_metrics`；任意会话头部看标题右侧「标准模式」 | 单测全绿；实机标签高 22px、左上角内缩 3/1/1/0（R4）、悬停无底色变化 |
| I23 | `subagent/catalog` v3 契约（补缺 #84）：写 `{version:3, mode:"continuable", name, label}`，`name` 满足 `/^[a-z][a-z0-9-]{0,31}$/` 且非 `lead`；读侧历史 v0–v2 行 `name=None` 仍可回读 | `cargo test -p dsh-session catalog_names`；`cargo test -p dsh-subagent`；`cargo test -p dsh-persist` | 11 + 7 测全绿；persist 往返保留 name、legacy v0 行解码不报错；`child_persistence` 断言 mode=continuable 且 name 合法非 `lead` |
| I24 | markdown 表格几何（补缺 #86）：无竖线/无外框/无表头底色，单元格 10×16 且首末列贴边、13/22、表头 500、行底 0.5px 发丝、列最小宽 100 | `cargo test -p dsh-gpui --test table_geometry`；任意含表格的会话（如「熟悉一下」） | 5 测全绿；实机表格区竖线列 = 0、表头像素 = 白、行底发丝含表头下沿且表头行高 22px、短列不再竖排折字 |
| I25 | 设置左轨纵向节奏（补缺 #88）：`.nav` 上内距 10/左右 12、标题行 24 高 + 8 下距（16px 字、文字左缘 24）、格子左缘 12、格间 4 | `cargo test -p dsh-gpui --lib ui_metrics`；侧栏 ⚙ 打开设置 | 单测全绿；实机选中格填充带相对面板顶 42（改前 64）、标题文字 y 170–183（改前 181–194）、格子左缘 x = 面板左 + 12 |
| I26 | GPUI 生态基线（补缺 #89）：gpui-pre/-platform 0.3.11 + gpui-base/component/assets 0.9.0，vendor 23 处 [dsh] 补丁重放（表格几何/排版/粘贴/opener/代码块 R16） | `cargo test --workspace`；`cargo check -p gpui-base`；打开含表格会话 | 360/0 全绿 + regression-core GREEN；实机表格竖线=0、表头白底、设置 nav 几何不变（y 195–234） |
| I27 | prompt 重试去重（补缺 #90）：同一 `MessageId` 只接纳一次——入列即记收据，认领/移除不撤销；日志 `user/message` 行是收据恢复源；收据按确切会话缓存 | `cargo test -p dsh-agent-loop --test prompt_dedup`；`cargo test -p dsh-agent contains` | 6 + 1 测全绿；重复 `send` 返回 false、收件箱与日志各只有一条同 id 输入 |
| I28 | 模型页提供方卡按钮两档 + 一次只开一张卡（补缺 #92）：md 36/R12/14-22/pad14、sm 28/R8/12-18/pad10、行操作 gap 4、删除为错误色无描边、添加卡与行编辑卡互斥 | `cargo test -p dsh-gpui --lib ui_metrics`；`cargo test -p dsh-gpui --lib provider_cards`；设置 → 模型 | 单测全绿；实机提供方卡高 54（改前 62）、编辑钮 28 高且顶边 R8、删除文字 rgb(236,19,19)、开添加卡后点编辑只剩一张表单 |
| I29 | hero/composer 间距（补缺 #93）：hero 栈 gap 8 + 行上距 4（标题↔行 12、行↔卡 8）、工作区行 pad 20/16 + gap 2、composer 工具栏 gap 12、草稿 pad 4/8/0/14、输入地板 36/52、附件栏 pad 2/10 + 底回拉 6 | `cargo test -p dsh-gpui --lib ui_metrics`；新建会话看 hero；会话内看停靠 composer | 单测全绿；实机工作区文件夹 x 459→471、预设标签 x 548→558、空态输入区 36→52 高、停靠态仍 36 |
| I30 | 压缩分隔行（补缺 #94）：24 高可展开行——注入图标↔chevron 组 hover 切换、标题 13/24、2px 方点、摘要「已压缩 N 条历史记录（约 T tokens）」、无摘要禁用展开、展开体 22px 缩进 + markdown | `cargo test -p dsh-gpui --lib compaction_row`；`cargo test -p dsh-gpui --test compaction_row` | 3 + 4 测全绿；实机会话触发压缩后出现可展开行（本机无历史压缩事件，待实机取证） |
| I31 | composer 上下文占用表（补缺 #95）：14px 环形 + `N%` 读数、点击展开 264 宽面板（头行 `~used / capacity`、4px 三段色条、图例 `~tokens`）；口径 4 字符/token、块 4、角色 4；缺容量或空会话不渲染；内置 DeepSeek 未声明容量时按官方 1_048_576 兜底 | `cargo test -p dsh-gpui --lib context_meter`；`cargo test -p dsh-gpui --lib capacity_parse`；`cargo test -p dsh-gpui --test context_meter`；任意会话底部统计行 | 4 + 2 + 3 测全绿；实机触发条件：当前路由模型声明了 contextWindow（本机为空白，待核） |
| I32 | vendor 补丁可重放基线（补缺 #89 后续）：`.vendor/` 存「被补丁覆盖的原始 0.9.0 源码 + 完整补丁」，升级 gpui-base/gpui-component 时用 `scripts/vendor-refresh.sh` 重放，不再靠人肉复原 | `bash scripts/vendor-verify.sh`（基线==registry 原始源 且 基线+补丁==vendor）；`bash scripts/vendor-verify-test.sh`（注入漂移必须被抓到）；`cargo test -p dsh-gpui --test vendor_baseline`；regression-core 第 4 段 | verify 输出 `VENDOR-BASELINE: OK`；自测输出 `clean=0, drift=1, restore=0`；3 测全绿；故意往 vendor 追加一行 → verify 退出 1 并点名文件，还原后回 0 |
| I33 | 上下文容量链（#95 收尾）：模型行声明 → 官方目录兜底（`deepseek*` = 1_048_576）→ 不渲染；探测采纳补空白字段不改已调值；落盘归一 `128K`/`131072`；模型行容量 chip 就地编辑 | `cargo test -p dsh-gpui --lib capacity`；`cargo test -p dsh-gpui --test context_meter`；设置→模型→编辑卡→自定义设置 | 5 + 3 + 4 测全绿；实机占用表出现（76%）、两张卡模型行显示 `1048576(目录)`；容量 chip 点击待修（#96） |
| I34 | web_fetch 对齐上游：URL 校验（长度/scheme/内嵌凭据）、同源重定向 ≤5、content-type 分类、HTML→文本、5MB/10 万字符截断、显式产品 UA | `cargo test -p dsh-web fetch_policy` | 7 测全绿（含 HTML 丢弃 script/style、表格 `A | B`、截断 footer、重定向相对解析、有损 UTF-8 解码） |
| I35 | 上下文容量手填（#96）：编辑卡首屏「上下文窗口」输入框（`field()`+`Input`），打开时预填路由默认模型容量，保存写回并落盘；接受 `128K`/`131072`，空值清除回转目录兜底 | `cargo test -p dsh-gpui --lib capacity`；设置→模型→任意提供方「编辑」 | 6 测全绿；实机填 `200K` 保存后 settings.yaml 写入 `contextWindow: 200000`，占用表 76%→25% |
| I36 | reasoning 档位声明（#42）：`CustomModel.reasoning_efforts` ⇄ settings.yaml `reasoningEfforts`（off 写 null、空声明不写）；校验照上游三条拒稿规则；编辑卡首屏单行输入（`off,low=low_v2` 语法），拒绝非法声明写盘 | `cargo test -p dsh-gpui --lib reasoning_efforts`；`cargo test -p dsh-gpui --test reasoning_efforts_yaml`；设置→模型→非 DeepSeek 路由「编辑」| 5 + 4 测全绿；实机填 `off,low=low_v2,medium,high` 保存后 yaml 出现四档（off 为 null），重启回填一致 |
| I37 | 轮次过程组表头（补缺 #97）：flex gap 6、14/24、tertiary（hover secondary）、16×16 leading 图标↔chevron 叠放、展开态 pb 8、无定高/无底边发丝 | `cargo test -p dsh-gpui --lib process_group_header`；任意含工具调用的会话（如「熟悉一下」）| 单测全绿；实机 leading 图标在左（x 460–471）、无横向发丝、hover 出 `▾`、展开出 `▴` 且 8px 下距 |
| I38 | composer 统计行槽位顺序（补缺 #98）：统计 pill（轮/步、token/缓存）在左，上下文占用表在尾 | `cargo test -p dsh-gpui --lib composer_dock_order`；任意有统计的会话底部读数行 | 单测全绿；实机读数顺序为 `N 轮 M 步 · xxxk 缓存命中 X% · XX%` |
| I39 | 插件管理面（用户定向）：`dsh-plugin` crate——内置插件目录（`plugins.enabled` 选择列表，上游 OPTIONAL_BUNDLES 语义：开=追加/关=移除）+ 会话标题域纯函数（清洗/OSC-CSI-ESC 剥除/UTF-8 字节截断/5 词 40 字节回退/整行强调剥除/all-prompts 请求计划 4096 字节拒绝式预算）；设置页「插件」区块（Switch 开关 + 实验性徽标），新 prompt 即时生效 | `cargo test -p dsh-plugin`；设置 → 插件 | 11 测全绿；实机开关 ON 后 yaml 落 `plugins.enabled: [session-titles]`，再点回 OFF 段清空 |
| I40 | session-titles 随动标题（首个真实插件）：`SessionEvent::SessionTitle` 补 `source`（fallback/provider/user，user=置顶停摆；读写双向兼容旧行）；发送合格 human prompt 触发——立即落 5 词 40 字节回退标题，插件启用则异步 LLM all-prompts 生成（provider 源标题作锚、4096 字节超限拒发、tool-calls/max-tokens 拒收、失败保留旧标题、60s 块间预算）；重命名落 user 源置顶；调用必须在前台 executor（tokio enter guard 仅主线程） | `cargo test -p dsh-plugin`；`cargo test -p dsh-persist title_source`；插件开启后新会话发送一条消息 | 11 + persist 46 全绿；实机日志序 = fallback(seq3) → turn → **provider「Rustdsh 项目简介」(seq9)**，会话头与侧栏实时改名，重启后从磁盘恢复 |
| I41 | Agent 预设数据面（用户定向）：`agent_presets` 目录（standard/ptc/minimal/cordis 四卡文案与徽标逐字，order 1-4）+ `ToolAdmission.preset` 准入缝 + `dsh_tools::preset_admits` 策略（minimal 只留 bash/read_image）+ agent-loop `set_agent_preset`/`set_system_prompt`；会话头 `agentPreset` 缝激活——懒物化头写用户默认（recorder.set_default_agent_preset）、fork 继承源会话预设、运行中会话换绑即生效 | `cargo test -p dsh-gpui --lib agent_presets`；`cargo test -p dsh-agent-loop --test agent_preset_gate`；`cargo test -p dsh-persist preset_hdr` | 4 + 3 + persist 32 测全绿；e2e 断言 minimal 请求只声明 bash/read_image、standard 全声明 |
| I42 | 设置页「Agent 预设」四卡（替换占位页）：2×2 卡格（名称+徽标+右上角 monospace id+描述，选中态描边+填充），点击设用户默认并落 `agent-preset-registry.selectedDefault`；通用页预设 chip 显示当前名并跳转本页 | `cargo test -p dsh-gpui --lib agent_presets`；设置 → Agent 预设 | 单测全绿；实机四卡渲染与上游同形，点极简模式 → yaml 落 `selectedDefault: minimal` → 点回标准模式恢复（selectedDefault: standard） |
| I43 | 纯上下文注入过程组（补缺上游 44ad857798「show context injections as tool-call rows」）：全 Context 成员的过程组报告单一 `context` 类目（计成员数），标题「已注入上下文」（不再回落「已完成分析」）；混合组 Context 不加计数；组头图标按 context 类目取（ArrowDown 近似）；系统提示词前置行不算成员 | `cargo test -p dsh-gpui --lib process_title`；/plan 切换后发「直接回复 ok」| 单测全绿（全 Context 组/混合组/空组三分支）；实机过程组头显示「↓ 已注入上下文」、展开见「上下文注入 · model-selection」行、答案 ok（14 秒/5.1k tok） |
| I44 | 轨迹时间条几何/配色（界面走查：轨迹 tab）：`.plot` 50 高 + grid `44px minmax(0,1fr)`；左泳道标签列「输入/模型/工具」10px 右对齐、top 7/21/35、右边 0.5px l1；轨道背景 bg-layer-2（surface）；span 高 8、圆角 1px、top = 7+lane×14；span 两侧间隙 min(宽×8%, 1px)、最小宽 2；user span = business-primary（accent）不透明度 0.78（曾误用语义绿）、message = 品牌×红、tool = warn-label；轮界刻度 0.5px border-l2 全高 | `cargo test -p dsh-gpui --lib ui_metrics`；会话 header 点「轨迹」 | 2 测全绿（几何常量 + span_box 间隙）；实机对比：改前全宽**亮绿**条且无标签列、背景深黑 → 改后左列「输入/模型/工具」可见、user span 为**品牌蓝**且在「输入」泳道、轨道背景 layer-2 |
| I45 | 台账上下文行独立类目（界面走查：轨迹 tab）：注入行标签「上下文」（上游 `TrajectoryCellKind.context` → `.tagContext` success-68% mix 前景 + success-tertiary 底），不再并入「用户」；行文本 = 注入正文首行（正文空回退 `标题 · 标签`）；时间条 context span 落输入泳道、底色 `tag_context_fg`（上游 `color-mix(success 68%, label-2)`）、不透明度同 user | `cargo test -p dsh-gpui --lib traj_`；会话 header 点「轨迹」 | 2 测全绿（标签四类目互异 + 文本回退链）；实机：注入行标签由绿「用户」变绿系「上下文」，时间条 span 落输入泳道 |
| I46 | 侧栏会话行 leading 状态槽 + 时间排版（界面走查：侧栏会话行）：`Rows.tsx` `<span className={css.slot}>` 16×20 定尺恒占位（运行/待交互状态点与插件槽位落此格），标题紧随其后；`.time { font-size:10px; line-height:16px; margin-inline-end:-1px }` | `cargo test -p dsh-gpui --lib session_row_leading`；任意含会话的侧栏 | 1 测全绿（槽 16×20 < 行高 32、时间 10/16/−1 且不再沿用 12/20）；实机：标题左缘 30 → 42（槽 16 + gap 6 占位），时间戳字号由 12 降到 10 且垂直居中 |
| I47 | 会话标题跑马灯策略（上游 `useTitleMarquee`，数据/策略层）：溢出 ≤8px 不动、0.03 px/ms 匀速、触屏不进入、减少动态直跳右缘、离开瞬回；两端 12px 渐隐判据（左遮罩=已离开起点、右遮罩=右侧仍有文字，到头撤右遮罩） | `cargo test -p dsh-gpui --lib title_marquee` | 5 测全绿（小溢出不动 / 触屏与减少动态 / range 非负 / 匀速与夹边 / 渐隐三态）；**UI 接线未随本切片提交**：GPUI `group_hover` 只返回 `StyleRefinement`、挂不了逐帧重绘，需 AppView 侧 hover 记账 + 行内 `with_animation` 驱动位移，并以同底色渐变覆层近似 `mask-image`；试接入的惰性控件会使静止态省略号失效（视觉回归）故回退，见 backlog #118 ① |
| I6 | 实机像素取证：激活条几何 | 截屏后按 accent 色扫描标签条下缘 | 激活条仅 2 行像素高、左右内缩 10px（条宽 = 文字宽，本机实测文字 629–653 / 条 628–653） |
