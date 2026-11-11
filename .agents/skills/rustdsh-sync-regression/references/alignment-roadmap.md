# 与官方 deepseek-harness 的差距盘点 × 对齐路线（2026-11-09）

> 用途：`feature-backlog.md` 是「逐项补缺」的账本，本文是「先看清全局再决定补什么」的地图。
> 上游基准：E:\aiproject\deepseek-harness HEAD `fcc0ad4621`（2026-11-04，0.2.2-rc.1 合并进 master）。
> 本仓基准：`1fee7be`（2026-11-09，433 测全绿、regression-core ALL GREEN、vendor 基线可重放）。
> 维护：每轮对齐后回填「状态」列；新发现未登记缺口追加到 §5。

## 0. 一页结论

**规模差**：上游 379 个 npm 包（307 非客户端 + 72 客户端）、53 个包组、5 个 app；rustdsh 18 个 crate。
但这个数字没有意义——上游一个"能力"通常拆成 5-10 个包（provider/service/tool/client/settings）。

**能力差**（有意义的口径）：官方 web 产品的组合清单 `bundle/web-app/cordis.patch.yml` 有 121 行、
base 层 99 行；**标准预设 `presets/standard.patch.yml` 有 27 个启用的能力行，rustdsh 等价覆盖 11 行（41%）**。
缺的 16 行全部集中在三类：**任务面**（todo/jobs/schedule/goal）、**扩展面**（skill/mcp/lsp/ask-user）、
**上下文经济**（result-pruner/spill/checkpoint/time-context）。

**结论一句话**：rustdsh 现在已经是一个"能聊、能读能改代码、能跑工具、存储与 web 互通"的 agent；
它缺的不是界面细节（那部分反而做得最细），而是**让 agent 能长期干活、能接外部世界、能被人管住的那一层**。
所以对齐的重心应该从"UI 像素走查"切到"能力面"。

## 1. 怎么量：两张权威清单

| 清单 | 位置 | 含义 |
|---|---|---|
| 产品面 | `packages/bundle/web-app/cordis.patch.yml`（121 行）+ `bundle/base/cordis.patch.yml`（99 行） | 官方 web/desktop 产品实际挂载的全部插件行——"官方有什么"的权威定义 |
| agent 能力面 | `packages/bundle/web-app/presets/standard.patch.yml`（28 行，27 启用） | 一个标准 agent 拿到的工具/提示词/后端——"官方 agent 能干什么"的权威定义 |

### 1.1 标准预设逐行对照

| # | 官方行 | 作用 | rustdsh | 备注 |
|---|---|---|---|---|
| 1 | persona | 人设前缀 | ✅ | `agent_presets::persona_prefix`（minimal 换句） |
| 2 | agent-instructions | AGENTS.md 注入 | ✅ | `dsh-system-prompt` agent_instructions |
| 3 | time-context | 当前时间/时区注入 | ❌ | 无对应 section |
| 4 | tool-bash | bash 执行 | ✅ | `dsh-shell`（Windows 优先 Git Bash） |
| 5 | tool-pwsh | Windows 下改用 pwsh | ❌ | **偏差而非缺口**：本地用 Git Bash 换取模型熟悉的 bash 语法 |
| 6 | tool-fs | read/write/edit/read_image/present | ✅ | `dsh-fs` 全家 + 上游名形别名 |
| 7 | tool-fs-search | grep/glob | ✅ | `dsh-search` |
| 8 | tool-jobs | 后台任务：run_in_background/读/等/取消 | ❌ | 无后台作业面 |
| 9 | tool-schedule | 定时任务工具 | ❌ | 无 schedule 子系统 |
| 10 | skill-filesystem | 技能发现（项目/用户/自定义目录） | ❌ | 无 skill 子系统 |
| 11 | tool-skill | 技能加载工具 + `/name` 直呼 | ❌ | 同上 |
| 12 | command-goal | `/goal` 人命令 | ❌ | 无 goal 面 |
| 13 | tool-goal | 目标读写工具 | ❌ | 同上 |
| 14 | plan-mode | plan 模式 + 渐进披露 | ✅ | 部分：无 plan-mode 完整状态机（审阅流已单进程等价） |
| 15 | compaction-basic | 上下文压缩 | ✅ | `dsh-compaction` |
| 16 | command-compact | `/compact` | ✅ | 命令菜单 |
| 17 | tool-result-pruner | 旧工具结果修剪 | ❌ | 无 |
| 18 | tool-subagent-control | 子代理列表/中断 | ❌ | 无 |
| 19 | tool-subagent | 子代理委派 | ✅ | `dsh-subagent`（one-shot，无 fork） |
| 20 | tool-subagent-fork | 子代理 fork | ❌ | 无 |
| 21 | workflow-ptc | PTC 执行器（Node 脚本） | ❌ | 与 25/26 同属 Node 运行时依赖 |
| 22 | tool-workflow | 编排脚本扇出子代理 | ❌ | 同上 |
| 23 | tool-ask-user | 通用提问（模型问人） | ❌ | 仅 plan 审阅有问答通道 |
| 24 | tool-todo | 任务清单 | ❌ | 无（`todo_write` 只在活动分类里做过名字映射） |
| 25 | tool-web | web_fetch/web_search | ✅ | `dsh-web`（DeepSeek 搜索 provider） |
| 26 | present | 交付文件声明 | ✅ | `PresentTool` + 交付卡 |
| 27 | tool-plugin-manager | 插件管理工具 | ❌ | 设置页开关已有，工具面没有 |

**覆盖 11/27（41%）**，其中 1 项（tool-pwsh）按平台偏差不计。

## 2. 差距分域矩阵

状态记号：`✅已对齐` / `🟡部分` / `⬜未做-可本地实现` / `🚫面外` / `🔒硬约束`

### A. 任务与自治（最大的结构性缺口）

| 能力 | 官方实现 | rustdsh | 状态 | 价值 |
|---|---|---|---|---|
| 任务清单 | `todo/tool-todo`：会话级清单，每轮整表替换，UI 常显计划 | 无 | ⬜ | 高：多步任务的"可见执行计划"，也是长任务不跑偏的锚 |
| 后台作业 | `jobs/`：run_in_background、读/等/取消、完成后回注会话 | 无（bash 前台 120s 超时） | ⬜ | 高：长命令/构建/测试不再阻塞一轮 |
| 定时任务 | `schedule/`：RFC5545 任务 + 侧栏自动化页 + 到期作 follow-up 投递 | 无 | ⬜ | 中高：定时复盘/日报/巡检 |
| 目标 | `goal/`：每会话一个持久目标 + 自动续跑（round driver） | 无 | ⬜ | 高：把"一问一答"变成"推进目标" |
| 团队协作 | `team/`：子代理树共享任务板 + 依赖 | 无 | ⬜ | 中：多子代理并行时才有意义 |
| 编排脚本 | `workflow/`：模型写脚本扇出子代理，返回结构化结果 | 无 | ⬜（依赖 Node） | 高：批量化审计/迁移的标准姿势 |
| plan 模式 | `plan/`：模式状态机 + 审阅 | 🟡 有审阅流与 `/plan` | 🟡 | — |
| 子代理 fork | `tool-subagent-fork` / `-control` | 🟡 one-shot 委派 | 🟡 | 中：并行分支探索 |

### B. 上下文经济（长会话能不能活下去）

| 能力 | 官方实现 | rustdsh | 状态 |
|---|---|---|---|
| 工具结果修剪 | `compaction/tool-result-pruner` | 无 | ⬜ |
| 大输出落盘 | `spill/`：超阈值文本落盘 + 取回指引 | 无（超长输出直接进上下文） | ⬜ |
| 检查点 | `session-checkpoint-policy` | 无 | ⬜ |
| 时间上下文 | `context/time-context` | 无 | ⬜ |
| 会话引用 | `context/session-reference`：把别的会话作为有界快照引用 | 无 | ⬜ |
| 占用表/容量 | `token-meter` + context-meter | 🟡 环形占用 + 目录兜底容量 | 🟡 计费/构成仅近似 |
| 压缩 | `compaction-basic` + 检查点帧 | ✅ | ✅ |

### C. 扩展与集成（能不能接外部世界）

| 能力 | 官方实现 | rustdsh | 状态 | 备注 |
|---|---|---|---|---|
| 技能 | `skill/`：SKILL.md 发现 + 按需加载 + `/name` 直呼 | 无 | ⬜ | 高价值、纯本地、无运行时依赖 |
| MCP | `mcp/`：设置驱动的 server 集群 + 工具/资源导入 | 无 | ⬜ | 把生态工具接进来的唯一标准通道 |
| LSP | `lsp/` + `lsp-stdio` + `tool-lsp`：定义/引用/实现/hover | 无 | ⬜ | 代码导航比 grep 强一个量级 |
| ACP | `acp/`：编辑器/外部客户端的 agent 协议 | 无 | 🚫 | 需要 client 生态 |
| Office→PDF | `document/`（LibreOffice kit） | 无 | ⬜ | 低优先 |
| SSH 远程 | `ssh/` | 无 | ⬜ | 低优先 |
| 插件市场 | `boot/plugin-market` + 远程索引 | 无 | 🚫 | 远程协议 + JS 运行时 |
| 插件运行时 | cordis JS bundle / 动态加载 | 🟡 静态清单 + 开关（session-titles 已真实移植） | 🚫 1:1 | JS 运行时缺失 |

### D. 安全与授权（现在只有"文件写沙箱"）

| 能力 | 官方实现 | rustdsh | 状态 |
|---|---|---|---|
| 一次性审批 | `interaction/approval`：敏感操作 ask → 允许一次/拒绝 | 无（`approval_policy` 字段仅互通保留） | ⬜ |
| 权限预设 | `interaction/permission`：沙箱+审批组合 | 🟡 UI 三旋钮会落 `sandbox/mode`+`approval/policy` 事件，但**没有审批管线，「询问」档实际不拦任何操作**（`main.rs:1199` 自注） | 🟡→实质 ⬜ |
| 沙箱 | `sandbox/`：只读/工作区写/全权 + 平台后端 + 一次升级 | 🟡 `WorkspaceContainment` 只管写路径 | 🟡 |
| 读前编辑硬门 | `fs-observation-policy` | 仅提示词引导 | ⬜（已在偏差表登记） |
| 重复调用提醒 | `guard/repeat-tool-reminder` | 无 | ⬜ |
| 工具超时策略 | `guard/timeout-policy` | 🟡 bash 有超时，其余无 | 🟡 |

### E. 交互与人机通道

| 能力 | 官方 | rustdsh | 状态 |
|---|---|---|---|
| 模型提问 | `tool-ask-user` + `ui-user-questions`（选择/多选/自定义/已回答批次气泡） | 🟡 只有 plan 审阅面板 | 🟡 |
| 消息反馈 | `message-feedback` + `ui-message-feedback`（赞/踩+原因+`/feedback`） | 无 | ⬜ |
| 引用 | `message-quotes`（引用某条消息继续对话） | 无 | ⬜ |
| 模型选择 | `ui-model-selection`：`/model` 弹层 + composer 座位 | 🟡 菜单 + `/model` | 🟡 |
| 命令面 | `/compact /plan /permission /model /export /feedback /goal` | 🟡 6 条硬编码（缺 `/feedback` `/goal`，无命令目录/模糊搜索） | 🟡 |
| @引用 | `ui-input-trigger` + `ui-reference` + `file-reference-local` | 🟡 **只有发送时解析已键入的 `@path`，没有候选弹层**（checklist C5 写"文件候选"与代码不符，见 §10） | 🟡 |

### F. 呈现与复盘

| 能力 | 官方 | rustdsh | 状态 |
|---|---|---|---|
| 交付卡 | `deliverables` + `ui-deliverables` | ✅ present 卡 | ✅ |
| 轮改动审阅 | `workspace-changes`（git 快照 ±行数）+ 整文件 diff 面板 | 🟡 单工具 diff 卡（backlog #1/#52 待定向） | 🟡 |
| 轨迹 | `ui-trajectory` | ✅ 台账 + 导航栏 | ✅ |
| 作业/定时/目标/技能/团队/工作流 UI | `ui-jobs` `ui-schedule` `ui-goal` `ui-skill` `ui-agent-team` `ui-workflow-run` | 无（无数据面无 UI） | ⬜ |
| 侧栏终端 / 内嵌浏览器 | `ui-sidebar-terminal` / `ui-sidebar-browser` | 无 | 🔒 终端模拟器 / webview |

### G. UI 面包级盘点（61 个 `ui-*` + 12 个基建包）

按包粒度复核（每包读 package.json + src 导出，再对本仓 grep 核对）：

| 判定 | 数量 | 代表 |
|---|---|---|
| 已复刻 | 9 | ui-chat、ui-plan、ui-model-selection、ui-permission-presets、ui-secret-mask、ui-sidebar、ui-sidebar-files、ui-layout、ui-settings-models |
| 部分 | 25 | ui-conversation（缺 activity 槽/视图固定）、ui-tool（缺提问卡）、ui-trajectory（run_code 高亮未接）、ui-input-trigger（@ 无候选）、ui-commands（6 行硬编码）、ui-theme（无字体族/字号）、ui-deliverables（缺整文件 review）、ui-workspace（无浏览页/pin 序）、ui-dockkit（无 split/float） |
| 缺失-可本地实现 | 9 | **ui-approval、ui-jobs、ui-message-feedback、ui-skill、ui-context-menu（无右键）、ui-shortcuts、ui-file-actions、ui-settings-shell/subagent/agent-loop（超时/并行/递归值硬编码）** |
| 缺失-面外 / 需先有数据面 | 10 | ui-goal、ui-workflow-run、ui-schedule、ui-sidebar-browser、ui-sidebar-terminal、ui-settings-account、ui-open-in-app、ui-entry-customization、ui-brand-official、基建 12 包 |

**日常体感最强的 6 个 UI 缺口**：工具审批卡 → 后台作业列表 → 消息反馈（赞/踩）→ 技能引用与技能行 → 文件另存/图片复制 → 快捷键参考与自定义。
其中前 3 个与 §2 的 A/D/E 域是同一件事的两面（先有数据面，UI 才有内容可画）。

### H. 平台面（不做）

web server + API gateway + Typert Remote、账号体系（`deepseek-account`）、遥测（`otel`/product-analytics）、
remote-access / liveshare / 多客户端 `connection`/`store`、plugin market、desktop 打包与 CI。
这些是"把这套东西做成一个服务"的架构，rustdsh 的产品形态是**单机单进程桌面应用**，不上这一层。

## 3. 差距的性质：只决定「先后与前置」，不决定「做不做」

> **2026-11-09 用户定向：目标改为全量 1:1，取消「面外 / 永不做」类目。**
> 与官方保持一致是本仓第一原则；下面的分类只是排期工具，任何一项都必须有对应批次编号。

1. **1:1 直接复刻**（纯本地、无外部依赖）：todo、jobs、skill、spill、result-pruner、time-context、
   fs-observation-policy、fs-patch、approval、ask-user、repeat-tool-reminder、timeout-policy、
   session-query、subagent-fork/control、message-feedback/quotes、workspace-changes、worktree…
2. **等价承载**（GPUI 无对应组件/机制，但**语义、数值、文案必须与上游一致**）：审批弹层、jobs 任务表、
   schedule 进程内调度器、skill 目录扫描、@会话引用快照、右键菜单、dockkit 分栏…
3. **平台地基**（不先建则整片阻塞）：JS/Node 运行时（PTC/workflow/插件/extensions）、
   本地 HTTP+SSE 与 RPC 层（api/gateway/remotes/多端）、统一 subprocess + PTY（terminal/jobs/lsp/ssh）、
   webview 与终端模拟器（侧栏浏览器 / 侧栏终端）、PDF canvas。
4. **依赖官方服务**（客户端代码可 1:1，联调需服务与凭据）：账号（PKCE）、遥测（OTLP）、
   liveshare relay、插件市场索引、forge（GitHub）、webhook。**行为按上游同款：有服务时一致，无服务时报同款错误/降级。**

规模要按这个重新理解：这不是"补几个功能"，而是**把一个完整 harness 的宿主面也造出来**。
按单人节奏，这是以季度计的计划（总量估算见 §7.5）。

## 4. 已经对齐、不要重做的部分

- 会话存储 v4 全链（读写/迁移/未知事件门/附件 CAS/projcache 双布局）——这是最值钱的资产。
- agent loop（turn/step、多步工具、退避重试、压缩、收件箱/排队/插话/回显退役）。
- LLM adapter（DeepSeek SSE、reasoning_content、tool args、缓存 token、/models 发现与凭据作用域缓存）。
- 工具面：fs 全家（read/write/edit/read_image/present）、bash、grep/glob、web_fetch/web_search、subagent、exit_plan_mode。
- UI 主面：三栏 shell、流式 markdown、工具卡家族、轨迹台账、轮次导航、composer 全套（统计/占用/排队/草稿/权限）、设置页、dock 文件树与预览、亮暗主题。
- 交互细节：凭据掩码、退出码胶囊、失败行/QUOTA toast、max-tokens 提示、计划卡与审阅、预设四模式、插件开关 + session-titles。

## 5. 本轮新发现（backlog 尚未登记的缺口）

| # | 缺口 | 官方依据 | 备注 |
|---|---|---|---|
| N1 | `time-context`（当前时间/时区注入） | `context/time-context`，standard 预设第 3 行 | 极小改动，模型对"今天几号"全靠猜 |
| N2 | `repeat-tool-reminder`（重复调用提醒） | `guard/repeat-tool-reminder`（base 默认启用） | 省钱省时，纯 loop 层钩子 |
| N3 | `timeout-policy`（通用工具超时） | `guard/timeout-policy` | 现在只有 bash 有超时 |
| N4 | `tool-result-pruner`（旧工具结果修剪） | `compaction/`，standard 第 17 行 | 长会话上下文成本 |
| N5 | `spill`（大输出落盘 + 取回指引） | `spill/spill-local/spill-policy` | 超长输出直接爆上下文 |
| N6 | `session-checkpoint-policy` | base 第 30 行附近 | 压缩前后的一致性锚 |
| N7 | `fs-observation-policy`（读前编辑硬门） | base；偏差表已登记"未实装" | 安全/正确性 |
| N8 | `approval` 一次性审批 + 沙箱升级 | `interaction/approval` + `sandbox` escalation | 现在全权/工作区写是粗粒度开关 |
| N9 | `tool-working-directory`（cd 工具） | `working-directory/tool-working-directory` | 模型改 cwd 目前只能靠 bash 前导 cd |
| N10 | `tool-subagent-fork` / `tool-subagent-control` | standard 第 18/20 行 | 并行探索 + 子任务列表/中断 |
| N11 | `tool-ask-user`（通用提问） | standard 第 23 行 | 只有 plan 审阅有问答 |
| N12 | `session-query` 全文搜索 | `session-query/session-query-sqlite` | 侧栏现在只有标题/时间；跨会话检索靠人工翻 |
| N13 | `message-feedback` / `message-quotes` / `/feedback` | web-app 121 行内 | 反馈闭环与引用追问 |
| N14 | `ui-jobs`/`ui-schedule`/`ui-goal`/`ui-skill`/`ui-agent-team`/`ui-workflow-run` | web-app 组合 | 依赖 A 域数据面 |
| N15 | `entry-customization`（插件槽位/启动器） | web-app 组合 | 依赖插件架构 |
| N16 | `office-to-pdf`（Office 预览转换） | web-app 组合 `office-to-pdf` | dock 预览的 Office 分支 |
| N17 | 文件另存 / 图片进剪贴板 | `ui-file-actions` | 现在只有代码/文本复制钮 |
| N18 | 快捷键参考页 + 录制 + 本地偏好 | `ui-shortcuts` | 键位全硬编码（Esc Esc、Ctrl+Enter 用户无从发现） |
| N19 | shell / subagent / agent-loop 参数设置页 | `ui-settings-shell` `-subagent` `-agent-loop` | 命令超时、输出上限、并行工具数、子代理递归深度目前是写死常量 |
| N20 | 右键菜单体系（类型化 + 目标捕获） | `ui-context-menu` | 全仓 `MouseButton::Right` 零命中，各处手搓菜单 |
| N21 | 字体族 / 字号设置行 | `ui-theme` 的 FontFamilyRow/FontSizeRow | 只有亮/暗/跟随系统 |
| N22 | 应用内目录浏览（vs 系统选择器） | `ui-directory-picker-browse` | 添加工作区走 `rfd` 原生对话框 |
| N23 | `@` 引用候选弹层 | `ui-input-trigger` 的 `@` 源 | 见 §10 勘误：现仅发送时解析键入路径 |
| N24 | `token-meter`（重放感知计量） | `llm/token-meter` | 现用 chars/4 启发式估算，缓存/重放口径与官方有差 |
| N25 | `tool-worktree`（建并进入 git worktree） | `experimental/tool-worktree` | 并行改同一仓库时的隔离手段 |
| N26 | `action-cards`（答案下方动作卡 + 提案工具） | `experimental/action-cards` | 把"下一步可选动作"变成可点控件 |
| N27 | 消息引用（选中文本/整条引用后追问） | `session/message-quotes` | 与 N13 的 feedback 同属"消息操作条"家族 |
| N28 | 统一 `subprocess` 缝（进程组/受控输出/杀灭） | `subprocess/` | 现在 tokio Command 散在 shell 内，jobs/terminal/lsp 都要复用它 |

## 6. 对齐路线：双轨并行

关键判断：**缺口里约 60% 不依赖平台地基**，所以不能"等地基做完再动"。
两条轨并行——轨 A 立刻可做（能力面），轨 B 是长周期地基；轨 A 中途按需接入轨 B 的产出。

### 轨 A：能力面（现在就能做，按价值排序）

| 阶段 | 内容 | 为什么在这个位置 |
|---|---|---|
| A0 语义一致性 | 询问档接真审批、checklist C5 修正、PTC 文案如实 | 半天；先让"1:1"从口号变成可验证基线 |
| A1 任务面 | todo → jobs → goal(+round driver) → schedule → team | 决定"能不能独立完成 10 步以上的事" |
| A2 上下文经济 | spill → tool-result-pruner → checkpoint → time-context → token-meter | 决定长会话成本与稳定性 |
| A3 安全与守卫 | fs-observation-policy → approval 全链 → sandbox 策略/升级 → fs-patch → reminder/timeout | 上游默认开启，缺了就是语义不一致 |
| A4 扩展面 | skill → mcp → lsp → worktree → session-query | 从"改代码"到"接工作流" |
| A5 交互面 | ask-user → message-feedback/quotes → `@`候选 → 命令目录 → 快捷键 → 右键菜单 → 参数设置页 | 人机通道补全 |
| A6 呈现面 | workspace-changes 轮改动卡 + 整文件 diff 审阅 → jobs/schedule/goal/skill/team 的 UI | 依赖 A1/A4 数据面 |

### 轨 B：平台地基（长周期，越早开工越好）

| 阶段 | 内容 | 解锁什么 |
|---|---|---|
| B1 运行时 | Node sidecar（优先）/ 内嵌 JS 引擎；PTC 执行器、workflow 引擎、插件 JS 宿主、extensions | ptc 预设、workflow、team、cordis 预设、插件安装、experimental 大半 |
| B2 传输 | 本地 loopback HTTP + SSE + Remote 语义（gateway / 各 controller / remotes / 多端 store） | web server、api 层、liveshare、ACP 客户端、sdk |
| B3 进程 | 统一 subprocess 缝（进程组 / 输出上限 / 杀灭）+ PTY（conpty）+ terminal UI | terminal 工具与侧栏终端、jobs 后台执行、lsp、ssh |
| B4 渲染宿主 | webview（侧栏浏览器）、PDF canvas、sticky / 逐 span / inline icon 等 GPUI 缺口 | ui-sidebar-browser、pdf 富渲染、若干视觉 1:1 细节 |

### 依赖关系（谁挡住谁）

```
B1 ──→ ptc / workflow / team / 插件安装 / extensions / experimental 大半
B2 ──→ api / host/* / 多端 / 账号 / liveshare / acp / sdk
B3 ──→ terminal / jobs 的后台执行 / lsp / ssh
B4 ──→ browser / pdf / 部分 UI 细节
A1 ──→ A6 的任务类 UI（jobs/schedule/goal 页）
A4 ──→ A6 的 skill/team UI
```

## 7. 批次与总量

### 7.1 A0 语义一致性（✅ 2026-11-09 已落）

| 项 | 内容 | 状态 |
|---|---|---|
| A0-a | 「询问」档接真审批：`dsh-tools` 审批缝（`ApprovalOutcome` 闭集 / `ApprovalSlot`）+ `dsh-fs` 沙箱升级（`sandbox_permissions`/`justification`、上游逐字文案、拒绝标记 + 升级提示）+ 审计事件 `approval/asked`/`approval/decided`（dsh-session 变体 + persist 读写臂）+ 宿主审批卡片（等待审批 / 工具 X 请求越权执行 / 拒绝 / 允许一次） | ✅ 已落；单测 7 条（validation/档位/文案三态/批准生效） |
| A0-b | checklist C5 修正（`@` 候选弹层是缺口） | ✅ 已改 |
| A0-c | PTC 预设卡不再诈称已有执行器（卡内如实标注） | ✅ 已改 |

**A0 遗留（登记到 A3.2 / A5）**：① bash 族的 escalation（上游 `command` 面）随 B3 的真实沙箱落地；
② 审批卡片键盘 Enter/Esc（上游 ApprovalCard 支持）与「始终允许」三钮形态；③ 运行中多会话 runtime 的
工具面尚未接审批槽（当前 fail-closed 为 `unavailable`）；④ 审批审计写主 agent 会话
（runtime 会话的逐会话审计随 A1 多 runtime 改造）。

### 7.2 轨 A 批次（能力面）

| 批次 | 内容 | 依赖 | 预估 | DoD |
|---|---|---|---|---|
| A1.1 | `todo_write` + 计划常显 + 重启恢复 | 无 | 2-3 天 | ✅ **已落 11-09**（backlog #106）：工具 + 整表 `todo/write` 事件 + composer 上方面板（聚焦/折叠/进度摘要）+ turn/start 清空 + 重载 fold；端到端 2 条 + 单测 3 条 + persist 往返 1 条 |
| A1.2 | `jobs`：后台执行 / 读 / 等 / 取消 / 完成回注 + 会话头 roster | 工具层钩子 | 3-5 天 | ✅ **数据+工具面已落 11-09**（backlog #107）：`dsh-jobs` crate（输出环绝对偏移/淘汰、注册表 read/wait/kill/remove/subscribe、三工具逐字对齐、完成通知）+ bash `run_in_background` + `job/*` 审计事件 + prompt section；剩 UI roster（A6） |
| A1.3 | `goal`：投影 + 工具 + `/goal` + round driver + turn-trigger 行 | A1.1 | 5-7 天 | ✅ **能力面全落 11-10**（backlog #119，批①②③+宿主接线）：纯面（`dsh-session::goal`）+ `dsh-goal` crate（store/authority/tools/wrapup/driver/prompt）+ `dsh-gpui` 接线（三工具注册 + `tool:goal` 节 + store 耐久缝接 recorder 落 `goal/change` + 事件 sink 承认轮推进/轮末驱动 + driver followup/remove 缝接 agent + probe 缝（打开轮次消息快照）+ defer 缝（wrapup 以 `tool-goal` context 通知注入）+ `/goal` 命令（六命令逐字文案）+ 命令菜单 goal 行）；DoD：跨 3 轮自动推进到完成（测试绿）。**剩**：GoalBar UI（A6）、goal 附件提交面、probe 缝的 runtime 会话路由 |
| A1.4 | `schedule`：任务存储 + 到期投递 + 侧栏自动化页 + 工具 | A1.3 的 follow-up 缝 | 5-7 天 | **批① 已落 11-10**（backlog #120）：新 crate `dsh-schedule` 纯面——七种 timing 记录类型（after/at/every/daily/weekly/cron/rrule 的 serde 形）+ 输入错误码闭集 + `schedule_title`/`decode_stored_title`/`decode_id`/`canonicalize_time_zone`/`decode_instant`/`future_instant`/`schedule_prompt`/`local_clock_time`/`normalize_weekdays` 校验函数（文案逐字）+ `render_reminder_framing`/`render_recurring_reminder_batch_framing`（逐字）+ `format_instant`（canonical 恒 3 位毫秒——jiff `to_string` 省零毫秒不可用，手写 formatter）+ `format_schedule_local_time`；**新依赖 jiff 0.2**（IANA tzdb，首个时区库）；10 条单测。**批② ① 已落 11-10**：occurrence 评估——`next_matching_target`/`latest_due_occurrence`/`resolve_daily_occurrence`/`resolve_weekly_occurrence`/`resolve_every_occurrence`/`next_daily_target`/`next_weekly_target`（DST gap 跳过 + overlap 取早，jiff `to_ambiguous_zoned().disambiguate(Earlier)` + 墙钟回环比对；tzdata 回卷不低于已承诺目标）；9 条单测。**批② ② 已落 11-10**：五构造器（`create_after/at/every/daily/weekly_schedule_record`，校验顺序与文案逐字）+ 逐形态严格解码器（`decode_after/at/every/daily/weekly_record`：title 先行 → 键闭集 → 逐字段，`rethrowLogError` 语义对齐）+ 严格解析词表 `parse.rs`（`parse_local_calendar`/`parse_offset_instant`/`civil_to_epoch_ms`——Hinnant 算术绕开 jiff `Timestamp` 范围）+ `canonicalize_time_zone` 升级（空/带白/形前置 + jiff `iana_name` 规范大小写）+ `decode_instant` 三分支拒绝（形不合法 canonical 诊断 / 2 月 30 `not a real UTC calendar instant` / 9999 尾段 canonical 诊断）+ `normalize_weekdays` 改收原始 `&Value`；17 条新测；**登记偏差**：①时区别名不回 tzdb 主名（jiff-tzdb 无 link→primary 表，上游 `Intl` 会 Calcutta→Kolkata；功能等价仅落盘串形）；②9999-12-31 当天 occurrence 的「下一次」耗尽回答 None（上游 `PlainDate.add` 会 throw——防御性改进）；③weekdays JSON 浮点数上游接受、本面拒（手改耐久文件才可见）；附带修 `occurrence.rs` 日期推进 `Date::MAX` panic 与 `max_four_digit_year_ms` 纳秒尾量不一致。**批② ③ 已落 11-10**（选型经用户拍板：cron 自写 + rrule 借库，与上游结构同构）：新模块 `recurrence.rs`（拒绝闭集 syntax/time_zone/density + `recurrence_decision` + `four_digit_year_epoch`）+ `cron.rs` 自写引擎（五字段方言值集与逐字诊断、Vixie DOM/DOW 星语义 AND/OR、canonical 文本再生最短编码、400 年地平线 next/latest 走查、DST gap 跳过）+ `rrule.rs` 适配层（rust `rrule` crate 0.14 + 严格校验/canonical/决策全自写逐字；库恒 UTC 形枚举墙钟、时区换算走本层 jiff；COUNT/UNTIL 在 instant 层施加；DTSTART gap 拒绝；成本上限 10080/BYSETPOS 全套）+ `decode_schedule_record` 七形态闭集包装 + cron/rrule 构造器与解码器接入 `record.rs`；**修批① bug**：`ScheduleRecord` 缺 `#[serde(rename_all="lowercase")]`（kind 标签曾为 PascalCase，与上游 wire 不一致）；**登记偏差**：①库适配差异——rust crate 校验要求 DAILY+ 显式 BYHOUR、MINUTELY/HOURLY 显式时间部件，本层显式注入 DTSTART 派生同值（RFC 5545 默认继承语义不变）；②rust crate 无 npm 库的 hasCandidate 等价面外，has_candidate 用 crate 迭代器实现；新依赖 `rrule 0.14`（借库）+ `chrono 0.4`（rrule 库的类型面，仅 UTC 模式）；12 条新测（dsh-schedule 共 58）。**批② ④ 已落 11-10**：新模块 `task.rs`（上游 `storage.ts` 的 `scheduleTaskSchema` 全量——`ScheduleTask` 规范化值形（record/target/involvedSessionIds/status/createdAt/updatedAt/lastDelivery/deliveryHistory/pendingDelivery）、v2 解码兼容读 v1 行（target 缺失用旧 sessionId 绑定或稳定 create-once 目的地 `session-schedule-<sha256(id)>`、new_once 缺 sessionId 填充、回执补目的地、`inactive`→`ended`、involved 缺省、lastDelivery=最新历史回执 refine、messageId 任务内唯一、自定义诊断逐字；zod 结构性诊断以等义英文近似——登记偏差）+ `ScheduleTarget` 三形态/`DeliveryReceipt`/`DeliveryHistory` serde wire 形）+ 新模块 `update.rs`（上游 `update.ts`：`validateTiming` 键闭集、`timingOf`（一次性保留已承诺绝对目标）、`normalizedTiming` 等价归一、`updateScheduleRecord`（未变 timing 保留承诺目标；变更/resume 走创建同款构造器；resume 无未来换逐字文案）+ `createScheduleRecord` 七形态分派 + rrule 专用更新（title 先验、描述变更保目标、recompute 重算））+ 新模块 `store.rs`（上游 `storage-json` per-record 布局等价：`~/.dsh/schedule/tasks/<id>.json` = `{"version": 2, "record": <任务>}` pretty+尾换行、v1 版本戳兼容、FOREIGN（畸形/版本不接受）读作不存在不拖垮域、值损坏报 `Corrupt` 清单化、临时文件+rename 原子写、id 路径安全防御）；17 条新测（dsh-schedule 共 75）。**批② ⑤-a 已落 11-10**：新模块 `service.rs`（runtime 纯面 + 服务词表——`append_delivery` 保留窗口/条数唯一写路径裁剪、`delivered`（new_once 固化 session 目的地/involved 去重追加/updatedAt 单调 +1/回执缺目的地由任务目的地补齐）、`advanced`（循环规则前进五形态分派，无下一目标即 ended）、`next_admission_at` 投递地板（60s / new 目的地独立间隔，后继守住间隔以 occurrence 锚定）、目录条目形与 `compare_entries` 排序、列表分页 limit 1-500、`render_delivery_text` 单条/批框架、`revised_task` 原子编辑 + `ScheduleEditRequest` 词表、身份生成 `schedule-<uuid>`——uuid 依赖落位、`format_task_local_time`）；lib 补 `ScheduleRecord` 访问器（time_zone/set_scheduled_at/set_title_prompt）；5 条新测（dsh-schedule 共 80；另勘误：批②③ 实为 +12 非 22、批②④ 实为 +17 非 16——前两批文档数字已更正）。**批② ⑤-b/c 已落 11-11**（宿主接线 + 四工具，A1.4 能力面收口）：新模块 `dsh-gpui::schedule_host`（ScheduleState 共享态 + 服务操作 + `ScheduleTool` 四壳——定义文案逐字含 RRULE_HELP/targetHelp 间距插值、声明错误码白名单 runForCaller 同款、子代理拒投按本仓 id 前缀约定、JSON 文本输出 renderValue 同款）+ main.rs 接线（四工具注册 actor 探针读 live agent、`tool:schedule` 提示词节 MANAGEMENT_AUTHORIZATION 逐字 + 新增 `ToolSchedule` 位次 1650、AppView `schedule` 字段 + 1s 轮询驱动循环 + `schedule_drive` 到期扫描分组 + `schedule_deliver_group`：投递地板过滤 → 目的地解析（session 复用 live runtime / 未开会话 headless 构造 recorder.load 恢复+建档 / new 保留身份 pendingDelivery）→ inject/send 分派（Running 态）→ advanced/delivered 提交 → watchExecution 持槽至 when_idle 再驱动）+ **`MessageSource::Schedule` 正式变体**（wire 裸 `kind:"schedule"` 与上游一致；persist 读臂补映射，写臂 serde 透传）+ 修批① bug（format_schedule_local_time 曾用 jiff 冗长 to_string——改 strtime 秒精度+数字偏移）+ 修投递 bug（单次规则统一走 advanced：无下一目标即 ended，否则每地板间隔重投）。**实机 E2E 实证**（restart-host + 手写任务文件冒烟）：驱动循环拾取 → new_once 建档+headless runtime → 投递延迟 852ms → 会话日志 `USER src=Schedule` + 框架文案逐字 + 模型轮完成 → 任务 ended + 回执历史落盘。**登记偏差**：①驱动循环 1s 轮询（上游精确 timer，投递抖动 ≤1s）；②重启后未消费提醒重排队（recoverOutstandingExecution）与 involved 会话变更通知（notify）defer 后续批登记 backlog；③子代理识别 id 前缀约定（上游 delegationDepth）。**A1.4 剩余**：侧栏自动化页（归 A6）。4 条新测（schedule_host；dsh-schedule 80 测 + workspace 93 套件零失败）。**defer 两件已落 11-11**（5de81d0）：恢复面（`schedule_recover_outstanding`：按收据 (message_id, 目标会话) 分组——收据行在日志且无后继 turn/end（重启掐死的认领轮/中途轮）→ 空闲目的地同 id 消息重排队开轮 + 整组持槽看护，运行中只看护；归档会话 load 失败放行；偏差：rustdsh 收件箱易失，崩溃前未认领消息随下轮驱动自然重投——上游经耐久收件箱重排队同 id）+ 变更通知（ScheduleNotice{created/updated/deleted} + ScheduleNotifySink 后置接线 + `schedule_change_notice_text` 上游 notify 逐字 + main.rs 通知泵派发到 involved 的 live runtime inject，actor 除外；偏差：inject 对空闲会话不落耐久）；schedule_host +2 测 |
| A1.5 | `team`：任务板 + `tool-agent-team` + `ui-agent-team` | B1 / A1.2 | 1-2 周 | 两个子代理共享任务板、不重复劳动 |
| A2.1 | `spill` + `tool-result-pruner` | 无 | 2-3 天 | ✅ **已落 11-09**（backlog #108）：`dsh-spill` crate（本机后端/保留策略/剪枝）+ 宿主 `maxInlineTokens=12500` + 压缩触发后先剪枝；剩 shadow 价事件与 spill 清理 |
| A2.2 | `checkpoint` + `time-context` + `token-meter` | 无 | 3-4 天 | ✅ **前两件已落 11-09**（backlog #109）：时间注入 + `sync_all` 检查点；剩 token-meter 口径（沿用 chars/4，未登记） |
| A3.1 | `fs-observation-policy` + `fs-patch`(apply_patch) | 无 | 3-4 天 | ✅ **观察策略已落 11-09**（backlog #110）：改前必读门 + 版本 CAS（默认关闭，同官方发货形态）；`apply_patch` 待做 |
| A3.2 | `approval` 全链 + `sandbox` 策略/升级 + reminder/timeout | A0-a | 1-2 周 | ✅ **已落 11-11**（批①②③；审批链本体 A0-a 已落 backlog #105）：**批①** policy 语境——`dsh-agent-loop::policy`（sandbox 三档 + `approval:policy` ask/never 句逐字，JSON.stringify 同形转义；`set_session` 折叠三类权限旋钮事件恢复，权限预设切换/物化 pin/新草稿三处宿主同步）+ 审批策略切换通知（`setPolicy` inject 模板逐字，user/plugin user-approval 裸源）；**批②** repeat-tool-reminder（`repeat_guard` 模块：阈值 [3,5,8]、深键 canonicalize、post-execute 计数拒绝也计数、温和→详述逐字、pre-step 用户插话重置、include/exclude 通配手写实现）；**批③** timeout 判定面外（definition 级守卫上游零消费方，backlog #122）+ bash 前台超时转后台语义差登记（backlog #121）。测试 +26（policy 10+3 / repeat_guard 12+1）。**剩**：bash 族 escalation 随 B3；审批卡 Enter/Esc 快捷键与第三钮（#105 遗留②） |
| A4.1 | `skill`：发现 + `tool-skill` + `/name` + `ui-skill` | 无 | 3-5 天 | ✅ **已落 11-09**（backlog #111）：`dsh-skill` crate（五根秩发现 / frontmatter / 目录与正文渲染逐字）+ `skill` 工具按预设门注册 + 首个请求前落 durable `skill-catalog` 消息（按 producer-owned kind 去重）+ `/name` 直接调用；11 条测试，实机发现 85 条技能。剩 fs 监听 / custom 根 |
| A4.2 | `mcp`：客户端 + manager + resources + 设置页集群 | 设置页扩展 | 1-2 周 | ✅ **已落 11-09**（backlog #112）：`dsh-mcp` crate（stdio + JSON-RPC 2.0、initialize 四版协商、`tools/list` 分页与重名整表拒绝、`tools/call` 60s、`mcp__<server>__<raw>` 桥两阶段换代）+ 宿主按 `settings.yaml mcp.servers[]` 拉起；13 条测试。剩 Streamable HTTP / 重连退避 / mcp-resources / 设置页编辑 |
| A4.3 | `lsp`：stdio + `tool-lsp` + 设置 | B3 | 1-2 周 | 跳定义 / 引用 / hover；✅ **纯面已落 11-09**（backlog #114）：`dsh-lsp` 缝（四操作闭集、`final_extension`、全有或全无注册、错误码文案）、帧协议（Content-Length、64KiB 头上限、五类失败文案）、渲染（一基↔零基换算、`path:line:character`、两类截断标记、`file:` URI 相对化、空结果文案、present 卡）；30 条测试。**剩批②**：stdio client + provider + `lsp` 工具 + `settings.yaml servers` 表与宿主接线 |
| A4.4 | `worktree` + `session-query`（工具 + 侧栏检索） | 无 | 4-6 天 | 建 worktree 进入；跨会话关键词检索并跳转 |
| A5.1 | `ask-user` 通用提问 + 已回答批次气泡 + 提问卡 | A0-a | 3-5 天 | ✅ **已落 11-11**（批①②③ + 实机 E2E）：批① `dsh-tools::AskUserTool`（legacy 阻塞形，文案/schema 逐字 + AskUserSlot fail-closed + 子代理拒答探针）；批② 宿主提问卡（`question_card` 纯态：分页单题/单多选/自定义/跳过/推荐剥离预选 + 泵 + 答案通道 + 停止弃卡）；批③ 转录面（`question_reply`：工具行「提问 · n/m 已回答」状态摘要 + 已回答批次气泡右对齐——ToolResult 答案批合成 user 条目，回放/实时两路同形）。**实机 E2E**：卡弹出（推荐预选✓+徽标+分页+自定义输入）→ 提交 → 答案批 JSON 回流（会话日志 seq 4521 tool/call → tool/result answers）→ 模型续跑；工具行「2/2 已回答」+ 气泡两题齐全。**剩**（backlog #123）：timed 变体（timeout/pending/projection/late-reply）、X 钮 hide+工具行重开（现走取消）、气泡时间/复制钮 |
| A5.2 | feedback / quotes / 命令目录 / 快捷键 / 右键 / 参数页 / `@`候选 / 文件另存 | 无 | 1-2 周 | 逐项 UI 判据 |
| A6 | `workspace-changes` 轮改动卡 + 整文件 diff + 任务类页面 | A1/A4 | 2-3 周 | 轮尾 ±行数卡、可点开单文件 diff |

**A4.3 进度（分批）**

| 小批 | 内容 | 状态 |
|---|---|---|
| ① | 缝 + 帧协议 + 渲染纯面：`dsh-lsp`（四操作闭集、`final_extension`、全有或全无注册、错误码文案）、Content-Length 帧（64KiB 头上限、五类失败文案）、渲染（一基↔零基 UTF-16、`path:line:character`、两类截断标记、`file:` URI 相对化、present 卡） | **已落 11-09**（6618693，backlog #114），30 条测试 |
| ② | 连接层：`dsh-lsp::client`（Content-Length 分帧端点、id 关联、出站请求/通知、入站服务端→客户端请求应答与 -32601 兜底、`$/cancelRequest`、stderr 有界尾部与退出文案、解码失败判死并终止受管范围、关闭后请求立即失败） | **已落 11-10**（backlog #114），8 条假服务端联调 |
| ③ | 翻译层 + 宿主层：`translate`（能力判定、`utf-16` 协商、`Location`/`LocationLink`/`Hover` 归一与逐字错误文案）与 `host`（工作区规范化 + `file:` URI、源文件包含校验、字节上限读取） | **已落 11-10**（backlog #114），18 条单测 |
| ④ | instance + provider：`LspInstance`（握手后台化、串行队列、瞬态文档生命周期、取消竞速 + `$/cancelRequest` 宽限 + 升级拆卸、优雅 shutdown/exit → 强杀升级、拆卸事务 watch 化）与 `StdioProvider`（按规范化 workspace 池化、队列内读源、死实例透明换新重试一次、dispose 后拒绝查询） | **已落 11-10**（backlog #114），12 条假服务端联调；取消信号经缝的 `LspQueryRequest.signal` 通道 |
| ⑤ | `lsp` 工具（`LSP_PROMPT_TEXT`、一基↔零基换算、60s 预算、maxLocations=100/maxResultChars=16000）+ `settings.yaml lsp.servers` 表（id/command/args/env/extensionToLanguage/enabled/initializationOptions/configuration）+ `dsh-gpui` 宿主接线（加载期解析可执行文件、provider 注册进缝、工具按会话工作区注册） | **已落 11-10**（backlog #114），5 条工具联调；批② 至此收口 |

### 7.3 轨 B 批次（平台地基）

| 批次 | 内容 | 依赖 | 预估 | DoD |
|---|---|---|---|---|
| B1 | Node sidecar + PTC 执行器 + workflow 引擎 + 插件 JS 宿主 + extensions | 无 | 2-3 个月 | 跑通上游 workflow 脚本；ptc 预设真实生效；插件可装可跑 |
| B2 | 本地 HTTP + SSE + gateway/controllers/remotes + 多端 store | 无 | 1.5-2.5 个月 | 本地端口起来，wire 形状与上游一致（可直接跑上游 spec） |
| B3 | 统一 subprocess 缝（进程组/输出上限/杀灭）+ PTY + terminal UI | 无 | 1-1.5 个月 | 侧栏终端可交互，环境跨调用存活 |
| B4 | webview + PDF canvas + GPUI 渲染缺口（sticky/逐 span/inline icon） | 生态或自绘 | 1-2 个月 | 侧栏浏览器可开页面；PDF 可看；细节判据通过 |

**B3 进度（分批）**

| 子批 | 内容 | 状态 |
|---|---|---|
| ① | 统一 subprocess 缝 `dsh-subprocess`：环境清洗 / 可执行文件解析 / `SpawnSpec`+`Stdio` / 有界 `Collected` / `ManagedProcess` / `spawn_piped` / `run`；`dsh-shell` 前台与后台泵、`dsh-mcp::spawn_stdio` 三处改道（顺带修掉 MCP 子进程 `mem::forget` 不回收、前台输出无界收集） | **已落 11-09**（f70fc0a，backlog #113） |
| ② | bash 族沙箱升级（A3.2 遗留的 `command` 面 escalation）——依赖缝的受管范围 | 待做 |
| ③ | PTY（portable-pty/conpty）+ terminal 工具 + 侧栏终端 UI + 跨调用环境存活 | 待做 |
| ④ | 后代回收：Linux scope owner / Windows Job Object（当前只保证直接子进程） | 待做 |

### 7.4 平台与生态面（地基之上）

账号 PKCE、遥测 OTLP + product-analytics、liveshare、plugin market（索引+安装+market UI）、
forge、webhook、hooks（Claude Code / Codex 桥）、cua、browser-use、ssh 全套、office-to-pdf、
语音输入/转写、inspector、sdk/acp 服务端、桌面打包与 CI、8 语言 i18n、dockkit 分栏/浮窗/拖拽。

### 7.5 总量粗估（单人，保持现有节奏）

| 部分 | 规模 | 粗估 |
|---|---|---|
| 轨 A（能力面） | 约 45 个能力点 | 4-6 个月 |
| 轨 B（地基） | 4 项 | 3-5 个月（与 A 并行） |
| 平台/生态面 | 约 40 个面 | 4-6 个月（强依赖 B1/B2） |
| **合计** | — | **12-18 个月** |

若把目标收敛到「web-app 桌面产品面」（= 官方 desktop 等价，不含 sdk/acp/headless/多端/CI），
约 **8-12 个月**。这个量级需要你确认投入预期——它决定了我是按"一个季度冲透轨 A"还是"长期慢跑"的节奏排。

## 8. 依赖官方服务的面：客户端照做，联调需服务

| 面 | 官方依赖 | 对齐做法 |
|---|---|---|
| 账号（deepseek-account / authorization） | 官方 OAuth-PKCE 端点 | 客户端流程 1:1；无凭据时报与官方相同的失败文案 |
| 遥测（otel / product-analytics） | OTLP 端点、匿名 id | 通道 1:1，随 profile 开关；默认按官方 desktop 的取值 |
| liveshare | relay 服务 | 协议与 UI 1:1；连不上时报同款错误 |
| 插件市场 | 远程索引 | 索引协议 1:1；无网时降级同官方 |
| forge | GitHub API + gh CLI | 同上 |
| webhook | HTTP 服务端 | 本地起服务，语义 1:1 |
| ACP / SDK | 外部客户端 | 协议服务端 1:1；无客户端不影响桌面 |

## 9. 三个技术选型（不再问"做不做"，只问"用哪个"）

1. **B1 运行时**：① **Node sidecar（随包 node.exe）**——最 1:1，上游本来就是 Node，脚本语义零偏差；
   ② 内嵌 quickjs——单进程、无外部依赖，但 Node API 兼容层要大改；③ boa——纯 Rust，性能与兼容性最弱。
   推荐 ①。
2. **B2 传输**：① **真 HTTP + SSE**（axum/hyper）——wire 形状与上游一致，能直接复跑上游 spec；
   ② 进程内直连——快，但从此无法验证传输层 1:1。推荐 ①。
3. **B3 终端**：① **portable-pty（conpty）**——成熟、跨平台；② 自绘 PTY 层——成本高收益低。推荐 ①。

另有一个**行为对齐问题**必须你定：上游在 Windows 下是 `tool-bash disabled` + `tool-pwsh enabled`。
严格 1:1 意味着 rustdsh 要改成 **pwsh 为主、bash 不声明**（这会改变现有 Git Bash 体验，也影响已有会话的
工具名回放）。三个选项：
- 严格 1:1：Windows 只声明 pwsh（bash 工具保留但按平台不声明）
- 兼容 1:1 + 体验：pwsh 与 bash 都声明（偏离上游组合，但模型两套语法都能用）
- 维持现状：bash 优先（最偏上游，但当前实机体验最好）

## 10. 勘误与"看起来有、其实没有"

| 项 | 表面现象 | 实际 | 影响 |
|---|---|---|---|
| checklist C5 | 写着「输入 @ → 文件候选、选中后注入」 | 代码里 `@` **没有候选弹层**，只有发送时 `resolve_file_references` 解析已键入的 `@path`（`main.rs:4103`） | 回归会误判通过；实际缺口是 N23（**A0-b 已改清单措辞**） |
| 权限"询问"档 | 设置页可选「询问」，落 `approval/policy` 事件 | ~~无审批管线，选「询问」不拦任何操作~~ → **A0-a 已接真审批**：沙箱越权重试走审批卡片，`never` 确定性拒绝；审计落 `approval/asked`/`approval/decided` | 已修复（剩余 bash 族 escalation 随 B3） |
| PTC 预设 | 预设卡片可选「PTC 模式」 | 只有展示文案与 persona，**没有 ptc 执行器**（`agent_presets.rs:10` 自认） | 期望落差；与 §3 的 Node 分叉同源 |
| 工具面数量 | 文档/直觉上"工具挺全" | 常驻 12 个（fs 全家 6 + bash + grep/glob + subagent + exit_plan_mode）+ 有 key 时 web_search；官方标准预设是 17 个工具行 | 差距集中在任务面（todo/goal/jobs/schedule/skill/ask-user/workflow） |
| `session-query` | 侧栏有搜索框 | 只匹配**会话标题**（`sidebar.rs:246`），没有跨会话内容检索，也没有给模型的 `session_search` 工具 | 历史结论无法复用（N12） |

## 11. 执行不变式（2026-11-09 起生效）

1. **无面外**：上游存在的面一律 1:1 对齐。不再产出「不适用 / 永不做」类目；只有「已完成 / 进行中 / 待排期（有批次号）」。
2. **偏差只能是临时的**：任何与上游不一致的地方，必须同时给出 ① 差异描述 ② 排期批次号 ③ 消除条件。
   没有批次号的偏差视为缺陷，不再写进"固化偏差表"了事。
3. **一致性优先于本地方便**：例如 Windows 下上游用 pwsh、只用 pwsh——这条冲突时就按上游来（除非用户另行定向）。
4. **同步节奏不变**：每次上游更新照旧走「五段差异 → 面内实施 → 回归 → 提交」，只是把"面外登记"改成"排期登记"。
5. **验收口径**：每个切片必须过 `cargo test --workspace` + `regression-core.sh`；UI 项追加实机取证（像素/文案判据）。
6. **文档同步**：backlog 项完成后移入「已完成」；本文档 §7 的批次状态回填；`feature-checklist.md` 增加对应功能点。
