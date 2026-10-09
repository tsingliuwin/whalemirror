# 上游更新分析法（模式 A 详细步骤）

上游仓库：`E:\aiproject\deepseek-harness`（git）。rustdsh 是其 **web 前端（packages/client/*）+ 存储行为（storage/session）+ llm-deepseek 适配层** 的 Rust/GPUI 1:1 复刻。
> **当前同步点：dsh-v0.2.1-alpha.1（5badb15009）**——2026-09-23 同步（发布点=master HEAD，无增量；跨 0.1.7 系列与 0.2.x，1552 文件 +74303/-11195）。此前 00102833df（0.1.7-alpha.2，2026-09-23 同步）。
> 0.1.5-alpha.1 主面：**会话格式 v3**（system prompt 晋升 system/message 行 + request/header 去 system + PTC 改名 + canonical 信封）、composer 统计行改双图标 pill + 互斥统计对话框、SystemPromptRow（系统提示词折叠行）；Sidebar 工作区文件树/dockkit/textpreview/remotes 全链面外。
> **最近检查：2026-09-14（文件浏览器面重判 + 实施轮）**——上游无需新拉（本地 master c291e7961a 已含 ui-sidebar-files/documentpreview 全链；网络面 GitHub SSH/HTTPS 双断、系统代理 7897 出口坏，SSH443 握手可成但传输被掐，改用本地既有树分析）； **最近检查：2026-09-11（定时轮 #7，零更新轮）**——上游 pull 经仓库局部代理（http.proxy=127.0.0.1:7897，SSH/HTTPS 直连被墙后的固定修复）成功，Already up to date（HEAD=master=rc.2 发布点 fb2c4b9e69），五段零差异，无动作。上轮 #6 同步结论不变。
> **最近检查：2026-09-25（定时轮 #20，补缺轮）**——上游零更新；补缺实施归档前停运行（5f 小接线，archive_session 运行态改「先 cancel 再归档」）。#17/#18 轮结论不变。

## 1. 一键差异分析

```bash
.agents/skills/rustdsh-sync-regression/scripts/check-upstream.sh [from] [to]
```

`from` 取上次同步点（查项目记忆 `rustdsh-sync-*-scope` 或 git log 里的「同步 web x.x.x」提交）。脚本输出五段：发布树差异 / 提交清单 / 同步面 stat / 词汇表 diff / 存储行为 diff。

**首个关键判定——发布是否有实质变更**：
`git diff tagA tagB --name-only | grep -v package.json` 为空 = 两发布点之间零功能变更（只有版本号批量 bump）。此时还要看 **master HEAD 相对发布标签的增量**（功能可能在打标签后才合入 master，用户的工作区拉的是 HEAD）。

## 2. 同步面判定表

> **面外处置原则（2026-09-18 起）**：判「面外」= 登记进 `feature-backlog.md` 差距清单并排批次，
> 不再以「不处理」终结。上游无更新的轮次按 backlog 补缺。硬约束项标观察并定期重估。

| 上游改动 | 判定 | 理由 |
|----------|------|------|
| packages/client/ui-*（chat/conversation/settings*/sidebar/primitives/theme/tool）| **面内** | UI 1:1 镜像 |
| packages/client/*/locale(s).ts 词汇 | **面内** | 词汇表逐项核对 |
| packages/storage、session-projection-cache 的**行为**变更 | **面内** | projcache/存储格式互通 |
| packages/llm/llm-deepseek（translate/types/error）| **面内** | dsh-llm-deepseek 镜像 |
| ui-settings-models 的 fetch/discovery | **面内**（已补齐，见 fetch-models 功能） | 获取可用模型 |
| packages/llm/llm-pi-ai discovery/catalog | **半面内** | discovery 语义移植；富目录（catalogModels）不移植 |
| session-persistence 内部重构（coordinator/handle/storage-contract）| 面外，**除非**动磁盘格式 | host 内部 seam；jsonl 格式稳定性用回归 A2/A3 兜底 |
| read_image 图片卡 / 新工具渲染 | 面外（除非 rustdsh 新增对应工具） | rustdsh 工具集无 read_image |
| proxy/http-proxy/app-boot/python 打包 | 面外 | Node 网络层，web 壳层无涉 |
| issue-management（.github）、CI、docs、notes | 面外 | 仓库自动化 |
| experimental/*（agent-team、cordis）| 面外 | rustdsh 无镜像 |
| message-edit 这类 feat→revert 对 | 净抵消，净零 | 与基准 diff 即可确认 |
| session 日志格式 v2（0.1.3-alpha.1：代数文件名 session.vN.jsonl、header isSeeded、chunk 内嵌 settlement.stream、assistant/attempt、tool/result message 形、compaction 富形） | **面内** | 磁盘格式互通；rustdsh 写 v2 + 读全代 + 写打开旧代先迁移（v2::migrate_to_v2） |
| session-persistence-jsonl lease（跨进程写锁） | 面外 | 读侧从不碰锁；rustdsh 单写者；仅需容忍会话目录里的 session.lock |
| file-upload 双端 RPC/Worker 传输面 | 面外 | rustdsh 本地存储一次完成，无传输；错误码/进度/断点无对应面 |
| 命令附件准入（CommandSubmitAttachment/registry） | 面外 | rustdsh 无命令系统 |
| QueueDock/Trajectory 文件计数/WorkspaceBrowser reveal/goal 命令芯片 | 面外 | rustdsh 无对应 UI 面 |
| token-meter（sourceEventSeqs → 内嵌 stream 重算）| 面外 | rustdsh 无 token-meter 镜像（TurnUsage 走实时 usage） |
| 链接语言（--dsw-alias-link + LinkIcon + hover 点状下划线）| 半面内 | 色值已同（deepseek-400/500）；LinkIcon 内联图标与逐 span hover 态 vendor TextView 不可复刻（偏差） |
| system-prompt 序位表（alpha.2：HARNESS_SOURCE 10000 / WEB_SURFACE 10100，persona 拆 prefix(0)/suffix(10200)）| **面内**（已对齐）| dsh-system-prompt SECTION_ORDERS 逐值镜像；rustdsh 无 persona 配置消费面，Config.persona→personaPrefix/Suffix 拆分不落码 |
| 模型切换公告（48cc1cf1d6：pre-step 比较上一 request/header，路由变化在消息批尾追加 user/plugin model-selection notice，notice form + summary；request/header reason=change 由既有 EpochHeader 比较自然成立）| **面内**（已实施）| dsh-agent-loop run_turn 消息批 + MessageSource::Plugin 扩 form/summary/sections（上游 plugin & ContextFormed）|
| MessageSource::Plugin 形扩展（plugin & ContextFormed：form instructions/catalog/snapshot(sections)/notice(summary)/relay/recall 可选字段）| **面内**（已实施）| serde default + skip None 向后兼容旧形；新形落盘与上游逐字节对齐 |
| open-in-app（9292dd8a2d feat workspace：web UI 经 URL scheme 唤起本地编辑器/终端/git 客户端 + 30 个 app.* 词汇 + ui-primitives Menu 依附改动）| 面外 | web→本地桥接功能；rustdsh 即本地应用，无对应面 |
| queue.sending 词汇（a3ccbd4d99 队列提交发送中态）| 面外 | QueueDock/排队系统 rustdsh 无对应面（既有判定）|
| chat 滚动 fix ×2（fa6bf62a98 settle pinned scroll before layout growth、9ef426d729 near-floor gestures pending）| 面外 | DOM 滚动/布局增长异步性专属；GPUI 列表滚动语义不同 |
| conversation-nodes perf（84c11c7243 avoid replaying settled assistant streams）+ llm stream readers（isTokenDelta/isVisibleChunk/assistantStreamFirstTokenTime 导出）| 面外 | web 重放优化；readers 无 rustdsh 消费面（session-stats/token-meter 无镜像）|
| resume 系列（6ad01cbd8d 重复 system prompt、477ff9d5e3 resumed series boundaries 等）| 面外 | rustdsh system 经 request/header 字段下发，无历史消息注入面，重复问题不存在 |
| connection 层（1bd26370cc handshake recovery、1e04ff35 retry factors）| 面外 | rustdsh 无 connection 层（本地直连）|
| subprocess native runner 批量（~80 提交：Windows Job/句柄/隔离/生命周期）| 面外 | host 进程管理内部实现，不动磁盘格式与工具可观察行为 |
| session 流式迁移（a84a8da9e1 stages、ec2f63dbdb publication、46196d6f95 v0→v2 streaming、migration-verifier Worker 校验）| 面外 | 内部性能重构，磁盘格式逐字不变（README 物理编码段仅加流式细节）；撕裂尾帧部分恢复语义 rustdsh 逐帧独立解压天然等价（未确认批次本就不可靠）|
| StateDot 第五态 idle/unloading 动画（452b2a816a + a6ef8dc97a）| 面外 | 消费面是 ui-settings-plugin-inventory 插件状态点，rustdsh 无插件库存页；state_dot 按调用方传色无枚举可扩 |
| Switch/Pill/Tag 胶囊原语重构（452b2a816a、c279350e03）| 面外 | rustdsh 无这三原语的镜像 |
| str_replace_editor 默认工具移除（36a4665144、965adbb5cf）| 面外 | rustdsh 工具集本就无 str_replace_editor |
| ui-agent-preset / ui-settings-plugin-inventory / ui-settings-plugins / SubagentModelSelectionCard / ui-trajectory / ui-skill | 面外 | rustdsh 无对应 UI 面（既有判定）|
| token-meter / session-stats / telemetry-otel / session-controller / api contract | 面外 | host 统计与遥测内部面，无镜像 |
| 会话格式 v3（0.1.5-alpha.1：header version 3 + agentPreset code→ptc + system prompt 晋升 `system/message` 行（head 保护/精确 replace/startSeq,endSeq 富形）+ request/header 去 system/空 tools/空 adapterDefaults + tool/code-dispatch→tool/ptc-dispatch + tools-code-mode→tools-ptc）| **面内**（已实施）| 磁盘互通；rustdsh 写 v3 + 读 v2/v3 + 写打开 v0/v1→v2→v3 级联迁移（v2.rs migrate_v2_to_v3：system promotion/PTC 改名/canonical/seq 重映射，id=v2-to-v3-system+sha256）；dsh-agent-loop 落盘序 step/start→system→user，head 归一化 replace（非 in-history 路线）|
| SystemPromptRow（0.1.5-alpha.1：system prompt 折叠行「系统提示词」/更新行「系统提示词更新」，展开体 141px 代码块）| **面内**（已实施）| chat.rs 渲染 system/message 为 Role::Context 折叠行；上游 surface replace 语义 rustdsh 线性显示每条（偏差）|
| composer 统计条改双图标 pill + 互斥统计对话框（3997f36999：gauge pill=轮步+TPS 开「会话统计」，database pill=总 token+缓存命中开「Token 用量」；stats.counts 去 ·；stats.dialog.* 词汇）| **面内**（已实施）| SessionStats 窗口累计（上游 deriveStats fallback fold 语义）；TTFT 会话平均；TPS 以 llm−ttft 近似 decode 窗口 |
| Sidebar 工作区文件树（ui-sidebar-files 树面 + 文本预览）| **面内**（2026-09-14 重判并实施，用户显式要求）| 曾两次判面外（rustdsh 无文件浏览器面）；用户指向 web 实装后重判；同日二次重判布局（用户指与 web 出入大）：按 web 实形落为右栏独立 dock（440px、中栏扣宽）+ 38px tab 条（28px 胶囊 chips：类型图标+标题+活动/悬停 ×、+ 钮、末端折叠钮）+ 活动 tab 体（文件树 / 带行号 gutter 代码预览 + 换行/重载/加载更多）；胶囊置中栏头部右侧（⋯/详情钮之左，web 同位）；左栏恢复纯会话列表、详情面板恢复工具/消息两变体。文件树规格：懒加载层级/目录优先自然序/filetype 图标/路径头 directory 灰+name 全墨/重载/空/截断/失败行/noWorkspace。数据面本地 fs 直接实现（canonicalize 围栏 outside-workspace、条目上限 2000、页字节 2MB、整读 32MB、NUL/非 UTF-8 判非文本），词汇照 ui-sidebar-files/ui-sidebar-documentpreview locales。仍面外：dockkit split/float/拖拽与 guide 多 tab 语义（+ 本地承载为开/激活文件树 tab）、documentpreview 富渲染器（html/pdf/markdown/image；预览为纯文本+行号，语法高亮面外）、fs watch 变更通告（changed/reloadNow 条）、openResource 地址体系、deliverables 产出文件、聊天点击文件改侧栏打开（rustdsh 维持既有打开行为）|
| Send busy 态系列（9a5ed6fb60 跟随 busy-Enter、2f630626b8 纯文本草稿才显模式名、8935c3d725 上传 pending 保 Send）| 面外 | rustdsh 发送钮无 busy 文案形态 |
| 附件卡文件类型图标（0.1.5-alpha.2：4ee9e055d5 shared file type icons——FileCard 的 DocumentFileIcon → FileTypeIcon，扩展名/文件名分类 12+ 类，类色文件底 + 白 mark 双层 glyph）| **面内**（已实施）| 分类逻辑与色值照抄（传统类 + code 归并单类）；glyph 以 body/mark 双 svg 分层 tint 复刻（assets/filetype/），fold 与 body 同色（单 tint 白折角对比损失）、48 语言 CodeFileIcon 细分收敛通用 code glyph（资产不可得）|
| transcript 设置文案中文化（'Normal'→'标准'、'Compact'→'紧凑'）| **面内**（已实施）| rustdsh 原为「常规」，改「标准」对齐 |
| deepseek 模型目录（0.1.5-rc.1/rc.2：DEFAULT_MODELS 头部插 deepseek-flash/DeepSeek-V41-Flash（vision+in-history），目录净态 V41 Flash/V4 Flash/V4 Pro/V4 Flash Vision Exp；默认 Chat Completions → V41 Flash）| **面内**（已实施）| rustdsh 落点：设置页 deepseek 内置模型行 4 条 + 启动/AppSettings 默认模型 deepseek-chat→deepseek-flash；description 文案与 image 字段无显示面不落（CatalogModel 结构差异既有偏差）|
| Usage 对话框 cacheWrite 为 0 省行（3435dbd690 stats review 修正）| **面内**（已实施）| 统计对话框 Token 用量分支条件化 |
| 时长格式化小时段（0.1.6-alpha.1 duration.hours：≥1h 出现「X小时X分X秒」，分秒补零）| **面内**（已实施）| format_run_duration 补小时段（统计对话框/轨迹时长列消费）；顺手清理 pill 化后的死代码链（stats_line/stats_line_text/fmt_duration）|
| fix(chat) restore collapsed thinking by default + revert historical timing recovery（#3880 系）| 面外净零 | 两笔均 revert 上游 #3880 引入的行为；rustdsh 从未引入（思考行恒折叠、时间列走自有 time_ms 承载），净态一致 |
| Think/compaction 头滚动吸顶（67271a921b）| 面外 | 聊天流无 sticky 渲染机制（与轨迹吸顶的固定行高模型不同构）|
| browser-use 实验后端/MCP、boot 桌面打包（pkg/asar/runtime）、terminal-controller 新包 + 终端 launcher/shell 记忆、guide 终端菜单、draft editor 隔离、session-log 上传、Mermaid docs viewer | 面外 | 各无对应面（browser-use 工具集无/Node 打包层/终端与 guide 面/docs 站 viewer）|
| markdown 表格 hover 高度稳定（55a17d2e57）| 面外 | vendor TextView 表格渲染面 |
| Sidebar Browser（9fed351d3d 等一批：聊天链接改侧栏内嵌浏览器打开/导航/安全文档）| 面外 | GPUI 无 webview 组件；rustdsh 链接维持系统浏览器打开（7f0a613 守卫）|
| plan 卡片系列（提交/审阅/预览/artifact 卡/自动打开）| 面外 | rustdsh 无 plan 审阅面（既有判定）|
| subagent 侧栏聊天（e62587c163）/ d9a55c7c0d desktop 系列（asar 更新/DPAPI/插件管理器移除）| 面外 | subagent 无 UI 面（既有判定）/ Node 桌面打包层 |
| b6726fe79d move context usage into composer stats | 面外（行为等价）| 实质为 pill 容器 css 宽度约束微调 + 注释清理；rustdsh 自绘 strip 布局行为已等价 |
| error.sessionInUse（其他 DSH 实例占用会话报错）| 面外 | lease 检测既有判定：rustdsh 单写者约定 + 仅容忍 session.lock |
| changes.*/review.* 轮改动审阅卡大批词汇（已编辑 N 文件/±计数/侧栏查看/split-unified diff）| 面外（待定向）| 依赖 workspace-files 的 turn 改动收集与 diff 数据面 + review 卡 UI；与 09-14/09-15 实装的文件树 dock 联动，交互形态待用户定夺后单独立项 |
| stepProcess 过程组类目标题（0.1.7-alpha.1：message.stepProcess.* 24 条，activity() 11 类分类、distinct call 去重排序、前 3 类 done 文案组合 joinTwo/sharedPrefix/comma/more）| **面内**（已实施）| process_activity/process_title/process_activity_counts 纯函数（dsh-gpui lib 5 测）+ TurnFold 计数改造 + 控制行 label 接线；rustdsh 工具名适配（fs 按 op 细分）|
| 存储格式 v4（session-format-v3-to-v4：observeRestart 缺失 turn/end 补齐 reason=interrupted、subagent/catalog 新事件 + finish 目录补齐、消息源重写/内容迁移/retired syntax 等 15 子模块）| **面内**（已实施）| SESSION_FORMAT_VERSION 4（写 v4 文件名、读 v3/v4、写打开级联 v0/v1→v2→v3→v4）；写侧 tool/result 平铺（role:tool+toolCallId+裸块+isError）、plugin source producer kind（system-prompt/model-selection/plugin:X）；读侧 v3 wrapper 与 v4 平铺双兼容；迁移链 migrate_v3_to_v4（observeRestart 补 interrupted、liftToolResult、producerKind、内容块 plugin: 前缀、seq 重映射）|
| projcache 列表读面身份放宽（去 inheritedEventCount，lifecycle identity）| 面外（无缺口）| rustdsh 标题读面本按文件名定位、不校验该字段，行为已等价 |
| turn triggers（e17e1ac801 轮触发标注 + message.trigger.* 词汇）| 面外→backlog #21 | 轮头触发标注（收到执行请求/继续执行目标）——rustdsh 轮头有承载位 |
| steering 系列修复（preserve steering order/cross-client pending order/retire confirmed echoes）| 面外→backlog #22 | rustdsh inbox 有 steer/send 双通道，顺序语义需对照上游核对 |
| voice-input 语音输入插件 / account-controller 账号登录 / Team panel / shortcuts 快捷键系统（净态）/ CLI JSON Schema / tool-cordis inspect | 面外→backlog 登记 | 各无对应面或实验性（语音/账号/Team/快捷键为独立子系统）|
| workspace 归档前停运行（cbae324bfa）/ XLSX 预览 / 文档缩放统一 / deliverables diff 行背景 | 面外→backlog 登记 | 归档接线小项；预览体系归 #7；diff 卡归 #1 |
| stepProcess 拆分与文案更新（0.2.1-alpha.1：readImage/write 独立类目、subagents 改「子智能体」、prepare.* 系列准备中文案）| **面内**（已实施）| ProcessActivity 拆 Write/ReadImage + done 文案表更新；prepare.*（运行中行形态）rustdsh 折叠仅闭合轮无消费面，不落 |
| message.turnProcess 文案（worked「已完成工作」→「已完成」、took「用时 X」→「已完成，用时 X」）| **面内**（已实施）| 轮尾用时行文案对齐 |
| storage-json 数组判守（tables 为数组时拒绝——防把不可见记录当空视图覆写丢弃）| 面外→backlog #23 | rustdsh 读 web projcache 单元时的防御性对齐（登记小项）|
| storage-domain invariant 删除 / mods 桥（Claude Code mods→DSH 插件实验面）/ DevTools 打包 / LibreOffice Kit / Team 后续 | 面外 | Host 自检伴生/实验 mods 桥/Electron 打包/Office 渲染/协作面各无对应面 |
| category.service-stability 文案改「稳定性和速度」/ guide.description | 面外 | feedback 分类与 sidebar guide 面无镜像 |
| CodeBlock contentRef/display:contents、TurnTailNodeView actions margin-top 4px | 面外 | web DOM 缝（ref/滚动端口挂载）与 DOM 流间距补偿，vendor TextView 布局模型无对应结构 |
| sidebar guide 起始页/documentpreview 精修/preview scrollports | 面外 | Sidebar 全链面外（既有判定）|
| Mermaid/Graphviz/SVG/HTML 围栏预览（ee35e40bc8 等 feat 系列 → 58956c1a8a Revert）| 面外净零 | feat→revert 对，以 HEAD 净态为准 |
| Sidebar 图片文件预览（8870a13aa9）/ ui-deliverables 交付文件呈现（presented.* 词汇大批）/ workspace-files subagent roots / webworker file handle | 面外 | Sidebar 文件树与 deliverables 无对应面（既有判定）；api/webworker 层无镜像 |
| think 摘要去粗体（b08310ac1d）/ markdown 图片失败显原文本（88ab8e9133）| 面外 | rustdsh think 行固定标题无摘要文本面；vendor TextView 无图片加载 |
| session-controller 拒空 prompt（ae19d9383b）/ api file 边界修复系列 | 面外 | rustdsh 无 session-controller RPC 层 |
| open-in-app SSH 检测共享 / visualizer feat+revert 净零 / native node-addon-system（flock/Landlock）/ agent-instructions root marker 修复 | 面外 | 无对应面/净零/Node 原生层 |

## 3. 实施顺序

1. 词汇表先行：新增 key 落到 rustdsh 对应界面文案；删除的 key 检查 rustdsh 是否残留。
2. UI 数值变更：照抄上游 `ui-theme` 包 / `*.module.css` 的数值（px/色阶/圆角/字号行高）。
3. 存储行为变更：先读上游 spec/测试（`*/tests/*.spec.ts`）确认语义，再改 dsh-persist，配等价 Rust 单测。
4. llm-deepseek 变更：对照 translate.ts 逐语义核对 adapter.rs（注意上游可能先引入又撤销，**以 HEAD 净态为准**）。
5. 每完成一块即 `cargo test --workspace`；全部完成后跑完整回归（模式 B）。
6. 提交信息格式参照项目历史：`同步 web <版本>：<一句话主旨>（<存储/UI 细目>）；<偏差与原因>；回归：<测数> + 实机验证点`。

## 4. 已知固化的偏差（勿重复推导）

- 无 pi-ai 富目录短路（PROVIDER_CATALOG 只有单默认模型）→ 目录型提供方改走端点探测。
- 获取可用模型：编辑卡 askable 恒真；空 base 报上游完整 `pi-ai ships no catalog...` 文案；UA `dsh-rust/*`；60s 总超时；禁用态无 tooltip。
- read_image 图片卡无渲染面（rustdsh 工具集无 read_image）。
- serde_json 开 `preserve_order`（端点顺序 = JS Object.entries）。
- composer 图片捕获不复刻（0.1.3 之前既有的面，维持）：无粘贴/拖放图片、无图片规范化流水线；聊天里的图片块仅展示 web 写入日志中的（64px tile，字节反查 attachments/v1/objects）。
- 附件（0.1.3-alpha.1 同步）：本地存储瞬时完成，无上传进度/取消/断点（FILE_NOT_STAGED 等 RPC 错误码无对应面）；外部文件拖放不复刻，回形针走文件对话框；移除钮常显（web hover 显形）。
- v2 写形：迁移对 rustdsh 旧形合成 `legacy-message:{sid}:{seq}` 消息 id 与 `{kind:"model",provider:"legacy",model:"legacy"}` source（上游迁移只认自家旧形）；compaction legacy `{beforeSeq,summary}` → 富形时 shadowedTokenCount=0、provider/model="legacy"。
- 链接/内联码样式：TextView 无逐 span hover 态 → 链接保持常显下划线（上游默认无下划线 + hover 点状）；LinkIcon 前置图标不可复刻（文本流无内联图标）；inline code 0.5px 描边不可复刻，底色已对齐 neutral-50/neutral-800。
- turn-metrics contract 本区间净零（仅 import 换源）。
- open-in-app（0.1.3-alpha.2）：web→本地应用 URL-scheme 桥接，rustdsh 本体即本地应用，无对应面；30 个 app.* 词汇随之不落。
- MessageSource::Plugin 旧形消费（Message::system 等仅 plugin 字段）不变；新形 form/summary/sections 仅在 producer 显式供给时落盘（当前唯 model-selection notice）。
- 撕裂尾帧：上游部分解码恢复完整记录+写侧截断重写；rustdsh 逐帧独立解压，损坏尾帧整帧不返回——两者在"未确认 durable 批次不交付"语义上等价，边缘崩溃场景不另行实施。
- StateDot idle/unloading 五态目录：消费面（插件库存页）rustdsh 无镜像；state_dot 色值由调用方供给，不扩枚举。
- 模型切换公告显示形态（10-09 更新）：上游在转录里渲染为折叠摘要行；rustdsh 现以 context 行渲染（非 user source 的 user/message 一律 context 行——内存 Plugin 形经 context_entry_from 同映射，与重载回放同形；标签=上游 contextProducer default 的 kind 原文）。
- 非 user source 落盘形（10-09 修复）：V4 admission 拒绝 plugin 包装源（assertV4SourceRowAdmission：retired syntax）。写侧 user/message 与 system/message 的 Plugin source 重写为 producer-owned kind（producer_kind：'@deepseek-ai/dsh-system-prompt'→'system-prompt'（role 敏感）、'plan-mode'/'model-selection' 等同名直映、其余 'plugin:<name>'）。0.1.7 同步轮只在迁移器（v2.rs）做了该重写、live 写路径漏掉——本轮接线（原 plugin_source_v4 死代码转正）。读侧非 user kind 一律还原 Context{context_kind} 开放词汇。
- plan-mode notice 投递（10-09 对齐 inject）：set_plan_mode 的 narration 走收件箱（next-step，无唤醒），下一次认领（空闲期=下一轮 step 1，先于用户文本）随批落盘；原直写日志会让 notice 游离轮间、被记到上一轮名下（上游日志形不符）。偏差：mid-turn 切换的 plan/mode 事件 rustdsh 立即落盘，上游挂 pendingIntents 等下一个被接受的 in-turn pre-step（可观察面极小，暂不追）。
- agent/inbox/spliced 持久收件箱事件 rustdsh 未落（收件箱纯内存）：上游 waking/turn-trigger 判定与轨迹收件箱投影依赖它——独立面，等 goal/followup 等非人类唤醒面一并评估（见 backlog 5d）。
- dsh-shell 端到端 cmd 编码测试（cmd_non_utf8_stderr_is_readable）弱化为编码无关断言：cmd 子进程输出字节随父链 console 输出代码页漂移且偶发尾字节截断（65001/936 间歇），强语义由 GBK 纯字节解码单测固定覆盖——2026-09-08 定时轮 #3 发现并修复（曾致 cargo test 中止后续 suite、总数波动 80/88/93）。

- V3 system prompt 面：上游 in-history 路线（provider 适配器声明能力）变化时 append 新节点；rustdsh 路线走请求 system 参数（非 in-history）→ 归一化 replace head。读取多节点 v3 日志（in-history 写形）时 rustdsh 线性显示每条 system/message，不做 surface 替换折叠。
- 统计 pill：IconGaugeOutline16/IconDatabaseOutline16 不可得（gpui-component-assets 86 枚无 gauge/database），近似用 LayoutDashboard/ChartPie；TPS 的 decode 窗口以 llm−ttft 近似（rustdsh 无独立解码计时）；对话框无点外关闭（strip 在文档流，无法全窗捕获；pill 再点/互斥切换关闭）+ 面板固定于 pill 行上方居中（上游逐 pill 锚定 + viewport clamp）。
- subagent 子会话耐久化（10-09 补缺 #4 数据面）：宿主经 SubagentTool::set_child_sink 注入 recorder+cwd 闭包（dsh-subagent 不反向依赖 dsh-persist），子会话事件含 SessionTitle（=description）落同一 sessions 根；父日志 subagent/catalog 由 set_parent_link(Weak) + append_session_event 落（mode 'one-shot'，上游 establishCatalogChild 语义）。偏差：catalog append 失败为尽力而为（上游失败则处置 run 并向调用者抛错）；运行中侧栏列表不自动刷新（重启后可见）；子 agent 的 childCreatedAt 取 spawn 时刻（上游取 child.header.createdAt，差值为同毫秒级）。
- 运行中实时查看子会话（10-09 补缺 #4）：上游 SidebarChat 为侧栏独立 tab；rustdsh 等价承载=宿主单视图（点击侧栏子会话行 → 运行中查看式切换 peek）+ 子活动推送刷新。宿主 child_sink 闭包同缝持久化 + tokio mpsc 转发子活动，第二事件泵（与 agent 事件泵同 cx.spawn 形）驱动 AppView::on_child_activity：新 id 即时入侧栏（absorb_new_sessions），被查看时按 250ms 节流整体重放磁盘快照（peek_refresh_due 纯函数；快照句柄原地换内容保读面同源，turn_expanded 跨重放保留）。节流重放而非增量应用：rebuild_from 的轮次状态机（cur_turn/unfinished/step_starts）抽出成本高，整体重放 O(日志) 且子日志小，先取行为等价——若子日志增长到千行级再评估增量缝。实机验证留日常使用（需真实模型委派 subagent）。
- 5e steering/echo 三笔核对（10-09）：①认领序一致（dsh-agent claim：next-step FIFO 全取 + 一个 next-turn，同上游 inbox.claim mutate 序）；②820824edd7 echo 所有权/cross-client pending order 为 web 传输 plumbing（rpcId/QueueDock/Inbox 投影水位/重连基线）——rustdsh 单进程无此面，不适用；③809e0942b9 的行为 analogue 已修：ChatEntry.echo_id + 轮终 retire_unclaimed_echoes（unclaimed_echoes 纯函数 + 单测）——取消时未认领回显退役，实时态=重载态。c7b9c91497 steering 渲染序由日志序自然成立。
- image offload 纯函数层（10-09 补缺 #3 子批 4a）：ContentBlock::Image 增 offloaded 标记（serde 仅 true 序列化，上游 ImageBlock.offloaded 可选 true 同形；日志读侧接受 wire 携带、写侧条件携带）；dsh-llm 新模块 image_offload（offloaded_image_text 含 imageIdentity 引号名/normalizedAccessText 恢复路径文案、project_offloaded_images 仅顶替消息顶层 offloaded 出现、required_image_offload + offloaded_image_prefix_count：count/byte 超出按 quantum 圆整、byte quantum>1 用严格 > 判定、quantum=1 用 >=；base64 表示按 ceil(bytes/3)*4 计长）。deviation：嵌套 tool-result 内的 offloaded 图片不由投影顶替（上游投影同只走消息顶层）。后续：4b 适配器预算+IMAGE_OFFLOAD_REQUIRED、4c 恢复循环+image/offload 耐久事件。
- 实机使用反馈修复（10-09，miaocr 会话 b99477fd 取证）：①shell 工具被 WSL 劫持——shell_program 在 PATH 命中 bash 时返回裸名 "bash"，而 CreateProcess 搜索序是「应用目录→当前目录→System32→PATH」，System32\bash.exe 是 WSL 启动器必然先命中：cargo/rustup 不可用、/c 路径不存在、每条命令附 wsl.exe UTF-16 代理警告噪声；修复=返回 canonicalize 后的完整路径（相对 PATH 项同样堵死）+ 不裸名单测。GUI 冷启动（PATH 无 bash 走 Program Files 显式路径）反而是对的——restart-host 经 bash 启动时才触发。②no tool "read"——上游 tool-fs 注册独立名 read/write/edit/read_image（参数 file_path/content/old_string…），rustdsh 只注册 fs op 形而提示词 section 以「read 工具」名引用，模型直呼名吃错；修复=dsh-fs 增 ReadTool/WriteTool 名形别名（file_path 为准+path 容错+行窗 offset/limit；委托 fs 原面共享沙箱/workdir/diff presentation）+ process_activity 归类映射。偏差登记：shell 工具名 shell≠上游 bash；edit（str-replace）与 read_image 未实装；read 输出为裸文本（上游行号+行窗 JSON 形）——见 backlog 工具名对齐项。
- 新建会话工作区同步（10-09 用户实机反馈）：current_workspace 原语义只覆盖 hero 显式选择（选择/新建工作区时赋值），侧栏选中会话与新建会话从不回写——选中某工作区会话后新建会话，会话虽落对工作区（resolve_target_workspace 会按当前会话归属解析 cwd），但 hero chip 停在「选择工作区」、composer 保持锁定态（3056/4745 的 empty&&none 门），需重选工作区才能输入。修复=同步规则统一为「当前会话的归属」：switch_session_inner/new_session 同步、enter_blank_draft 清空、AppView::new 启动即按初始会话同步（workspace_of_session 由预留死代码转正）；view_only_switch（运行中查看）不动 chip（agent 会话未变）。
- edit 工具（10-09 补缺 5g）：上游 tool-fs/edit.ts 全语义落地——parseEditArgs 校验（file_path/old_string 非空、old≠new、replace_all 默认 false）、applyLiteralEdit（内容与 pattern 双向 CRLF→LF 归一化匹配，写回 restoreLineEndings 保留原行尾；0 处 not found / 多处且未 replace_all 报 matched N times，文案逐字对齐 fsio.ts）、成功文案 formatEditOutput（All occurrences 变体）、presentation=diff 卡（oldText=编辑前全文/newText=全文，LCS hunk 渲染侧算，同 write 面）。偏差：read-before-edit 硬门（fs-observation-policy 默认 requires）未实装——仅 tool:edit 提示词引导（与上游 section 文案同文），模型未 read 直接 edit 不会被拒；后续 observation-policy 面落地时补。
- 新建会话跳 A 空白修复（10-09 用户实机反馈）：归属判定双口径——侧栏分组按 cwd（「目录即真相」project_key 匹配），而 workspace_of_session/resolve_target_workspace 按名册 session_ids 查；名册缺录的会话（web 端创建/漂移）切换后把 current_workspace 清空，新建回退按列表序（insert(0) 最近优先）命中 A 空白所属工作区 → blank_session_in 复用跳 A。修复=统一 cwd 口径：workspace_id_of_cwd 纯函数（project_key 匹配，单测；大小写/尾分隔符不归一与 project_key 语义一致）+ workspace_of_session/resolve 两级回退全部改用；名册 session_ids 保留手动排序职责（侧栏 order_manual 已如此用）。
- 新会话懒物化（10-09 用户定向偏差）：上游 connectWorkspace 点击即建持久空白会话（草稿锚+复用）；rustdsh 改纯内存草稿——new_session 不建文件/不落 pin 三事件/不写名册（draft_session 标记，权限三旋钮仅内存态），首条消息发送时 materialize（pin 三事件先行落盘——recorder append 首事件自动建文件、行序 pin→turn/start 与上游同；写名册；列表行 blank 翻 false），切走即弃（drop_current_draft 摘行，无磁盘足迹）。hero 选工作区在草稿上=重定向（换 cwd/行目录，零文件操作）。旧磁盘空白会话仍可被 blank_session_in 复用（存量兼容）。回退锚：append_without_create_materializes_the_log 单测。
- 会话删除（10-09 用户定向本地功能）：上游 web 无删除动作（仅归档——隐藏留日志，配 archivedSessionIds）；rustdsh 增侧栏行菜单「删除会话」（红字破坏性 → 确认弹窗（cancel_session_delete 态 + Modal 卡，文案指明整份日志移除不可恢复并导引归档替代）→ delete_session：当前者先 cancel、recorder.delete 移除会话目录（多代文件）、名册/归档集/列表行同步清理、删除当前会话切到最近剩余（无则空白草稿）。删除未物化草稿=纯摘行。
- read_image 工具（10-09 补缺 5g）：上游 read-image.ts 全语义最小等价——扩展名表（png/jpg/jpeg/webp/gif 声明媒体类型；非图片扩展名拒）/魔数嗅探（PNG/JPEG/GIF87a·89a/RIFF+WEBP）/先持久再返回（内容寻址 file-objects）/结果双块（上游 imageReadContent：envelope 文本 <path><type><content> 形 + image 块）/错误文案逐字。适配器关键补面：tool-result 嵌套图片原被 blocks_to_text 跳过（图片到不了模型）——拆为紧随工具消息组的合成 user 消息（OpenAI wire tool 角色无 image_url part；上游 anthropic-messages wire 的 tool 结果走 user 角色图片原生可达，两者行为等价）。偏差：无 route 能力门（上游 assertImageCapableRoute 对非 vision 提前拒——rustdsh 由提供方报错兜底）；无 imageLimits 字节/像素上限与 16-bit PNG 规范化（上游 attachment service 面）；WEBP 基础 VP8/VP8L 无画布块报不支持；转录中 tool-result 图片 tile 未渲染（envelope 文本可见）。顺手修 chat.rs 图片 tile 反查路径（objects/ → file-objects/，与 AttachmentStore 同布局）。

## 同步点：dsh-v0.2.1-alpha.2（d743267388，2026-10-09）

发布 diff 5badb15009..d743267388：3226 文件 +114987/-28177（洪流为 docs/.agents notes/scripts/tests/lockfile——同步面实际小）。面内实施：**settings.collapse 工作步骤收起时机**（presentation-policy CollapseTiming：completion=完成即折 / next-input=保持展开至下一条消息发送；Browser-local 内存偏好不写 Host 设置；settings.collapse.* 四词照抄；ChatView collapse_timing + TurnEnded 折叠门 + push_user_entry 发送折回 + 设置行（对话视图行后）+ CollapseTiming 纯函数 2 判定 + 单测）。面外登记 backlog 5h：plan 审阅流两笔语义记档（#2 对齐用：dismissed review → {approved:false}+concludes turn、exit 仅 approval）、模型目录无 key 可见性守卫、Auto review 图标、语音术语、插件设置重组、ui-chat fold/scroll 七笔（web Flow 引擎专有）、composer 4931 光标、思考翻译按钮。词汇表：settings.collapse.* 4 键（已抄）。
- shell→bash 改名（10-09 补缺 5g 收口）：ShellTool name 'shell'→'bash'（上游 tool-bash name 'bash'，模型直呼习惯对齐同 read/write 系列）；描述 lead 对齐 "Execute a bash command and return its combined stdout and stderr"（保留本地 Git Bash/cmd 风味行——cmd 回退分支文案自带 NOT bash 警示）；提示词 section 名 tool:shell→tool:bash（order 槽位本就是 ToolBash）+ 文案自引用 The bash tool；名字锁定单测。旧会话日志 name="shell" 的 tool/call 回放归类由 process_activity 原有 "shell"|"bash"|"pwsh" 三名映射兼容。
- read 行号信封收口（10-09 补缺 5g 完结）：ReadTool 输出改上游 formatReadOutput 形——`<path>/<type>file</type>/<content>` 信封 + `N: text` 行号体 + 分页 footer（Showing lines X-Y of N. Use offset / End of file - total N lines）+ 默认 2000 行窗（READ_LIMIT）；dsh-gpui lib parse_read_envelope 纯解析（剥信封/footer/行号前缀，保留行前导空白与空行编号条目；footer 前空行分隔恰剥一个）+ 单测；chat.rs 增 read 卡臂（信封解析喂读卡、file_path/path 双容错、非信封回退原样）；widgets 名称映射补齐（bash|shell 双名 Bash 图标、read/write/edit/read_image 行标题与路径链接）。同轮修复 bash 改名渲染回归：terminal 卡/显示名/行文本原匹配旧名 shell——新调用 name="bash" 曾落泛型卡。偏差：无 maxLineLength/maxBytes 截断（上游 caps 面）；fs op=read 仍输出裸文本（rustdsh 聚合形内部用，模型面已由 read 承担）。
- present 工具与轮尾交付卡（10-09 补缺 #6）：上游 tool-present 全语义——files 数组（1-8 上限/建议至多 4 入 schema 文案）每项校验存在且常规文件（not a regular file / file not found…retry 文案逐字）、成功模型可见文本逐行 Presented {path}、经 tools/result 钩子落 deliverables/presented {turn, callId, files:[{path, description?}]}（rustdsh 等价=PresentTool 泛型交付缝（callId+已解析文件），宿主闭包算轮号（日志最后 turn/start 折叠）+ append_session_event——错误结果不落）。聊天：presented_by_turn 按轮归并（回放收集/实时轮终扫会话——事件走宿主 sink 不进 UI 事件流），轮尾卡（0.5 l1/r12/surface 同 plan 卡家族，每行 file_type_icon+basename+描述）。偏差：无 presented.* 打开动作（待 #11 openResource）；无嵌套 PTC 调用聚合（单层直呼）。
- presented 打开动作收口（10-09 #6 完结）：上游 presented.* open（资源管理器/Finder/默认应用经 openResource 地址体系）——rustdsh 等价承载=presented 卡行点击走宿主已注入的 try_relative_file_opener 钩子（存在性守卫 + AppView.open_file_preview dock 预览路由，同 markdown 路径链接点击的成熟缝，零新接线）；vendor text mod 补导出 try_relative_file_opener（原只导出 setter）。#11 openResource 体系（Session/绝对路径统一地址抽象）判面外——web 多端地址路由的内部抽象，rustdsh 单视图直连预览缝已覆盖其消费面。
- 会话统计整日志投影（10-09 补缺 #10）：上游 sessionStats fold 移植（dsh-session session_stats_fold + 2 单测）——turns 按上游口径「至少一个闭合 step/end 的轮」（空/拒绝轮不计；原回放按 turn/start 计数偏多）、steps=step/end 计数（含失败/取消/MaxTokens；原按 assistant/message 计数偏少）、llmMs=step/start→assistant/message、toolMs=tool/call→tool/result 按 callId 配对（轮终清 pending）；rebuild 换投影后跨重载一致。**顺带修实锤互通缺口**：SessionEvent::ToolCall 写臂缺失（agent-loop 追加的事件被 event_to_web_line 静默丢弃——真实 v4 日志无 tool/call 行【实测 miaocr 日志行型确认】、web 读 rustdsh 会话缺工具行事件、回放 stats_tools 恒 0、toolMs 无配对源）——补写臂 {turn,step,callId,name,arguments} + 读臂（envelope time 塞 ToolCall.time_ms——serde skip 内存态，wire 不变）。偏差：TTFT/decode 回放不可恢复（rustdsh 流块无时间戳承载，上游流块带时间）——live 窗口计时为准；live 窗口与投影口径微差（TurnStart 计数 vs 闭合步），重载后收敛。
- fs watch 变更通告（10-09 补缺 #9，轮询等价）：上游 workspace-files WorkspaceChangeFeed 为 fs/observed 事件驱动 watch（观察版本 + dirty 节点 + setAutomatic 后台暂停）+ reload 文案「重新读取」；rustdsh 等价承载=dock 文件树展开层签名轮询——dir_level_signature（名字序+kind 编码）/any_level_changed 纯函数（含载入层消失判定，+1 单测）、5s cx.spawn 轮询泵（dock 打开期间；面板关/已 stale 自空转）、变更条（警告图标+「目录已在外部变更」+「重新读取」点击整条即重载清位）、dock_reload_tree/dock_sync_root 同步清 stale。偏差：5s 粒度非即时；未展开子目录的外部改动不预检测（首次展开自然读新）。
- documentpreview image 格式（10-09 补缺 #7 首切片）：is_image_preview 纯函数（扩展名表与聊天附件 FileKind::Image 同源，+1 单测）；FilePreview 三处接线——open_file_preview 图片即置 eof（不启文本分页）、load_preview_page 守卫（reload 路径同样短路）、dock_preview 体渲染 gpui::img（居中 ObjectFit::Contain、max_w/h_full、p12 溢出隐藏）替代行号行。图片文件点开即见位图（此前文本分页吃出乱码/失败行）。剩 markdown 富渲染（预览面走 TextView 路由）、pdf/html 逐格式评估；#24 大图查看器的渲染底座由此就绪。
- 聊天图片大图查看器（10-09 补缺 #24 完结）：tile（render_chat_attachment ImageTile）点击 → AppView.image_viewer.open（对象路径非空才开——反查失败 tile 无动作）；根渲染全窗遮罩（absolute size_full、occlude、黑 65%、点击任意处 close）居中 gpui::img（max_w=viewport 90%、contain）。ImageViewer 状态机抽 dsh-gpui lib（+1 单测）。image.loading 骨架态不做（本地对象路径直读即时）。
- durable image offload 循环（10-09 补缺 #3 子批 4b+4c 一次成环）：①LlmFailure.offload_images typed 载荷（serde skip，上游 failure 的 offloadImages 等价）；②适配器 stream 入口 inline 预算检查（images.ts assertImagesFit base64 形：ceil(bytes/3)*4 计长、上游默认 20MB/600 张/byte quantum 10MB/count quantum 20——超限 IMAGE_OFFLOAD_REQUIRED 失败携数，不自行移除）；③build_body 头 offloaded 图片投影占位文本（4a project_offloaded_images→offloaded_image_text 无路径变体——上游 access 为规范化副本路径，rustdsh 无该面用降级文案）；④agent-loop run_step Err 拦截：offload_oldest_images 按会话日志序（=请求序）深度优先数图片（含嵌套 tool-result 递归、已卸载计数不选中）选最旧 count 个落 ImageOffload 耐久事件（targets {seq, imageIndexes}）→ continue 本步免费重试（无 provider retry 预算/无 llm/retry 事件——上游同）；无可卸载按普通错误收束；⑤dsh-session ImageOffload 事件 + derive_messages 派生应用（apply_offload_targets：命中消息深度优先索引镜像计数置 offloaded）；⑥dsh-persist 读写臂（KNOWN 已有 image/offload）。偏差：Files API raw 形态不做（无通道，base64-only）；预算常量固定无 config 面；测试 6+2+1+2（适配器预算/投影、agent-loop 恢复 e2e 双向、session 派生、persist 往返）。
- documentpreview markdown 格式（10-09 补缺 #7 次切片）：is_markdown_preview 纯函数（md/mdx/markdown 扩展名 + README/CHANGELOG/CONTRIBUTING/AUTHORS/LICENSE/NOTICE 无扩展名惯用文档，+1 单测）；预览面板接线——open_file_preview 与 preview_reload 对 md 全量加载（load_preview_page 循环到 eof），dock_preview 体渲染 MarkdownBlock（聊天流同一 TextView 富渲染管线：标题标尺/段落间距/代码卡），滚动体 p16。行号列不显示；头部 wrap 钮保留（对 md 无效但无害，同图片）。
