# 功能差距清单（backlog）——向 web 端全面靠齐的作战地图

> **原则（2026-09-18 起）**：发现 web 端已有而 rustdsh 没有的功能，不再以「面外」终结，而是登记进本清单，
> 做好计划一点一点靠齐。上游有更新就更新；上游没有更新的轮次就从本清单按优先级取项实施。
> 排序总纲：**一切围绕「做出一款真正好用的 agent 工具」**——能提升日常 agent 使用体验的靠前，
> 纯展示/低频的靠后，技术硬约束的如实标注并定期重估。
>
> 维护规则：定时轮每轮结尾回看本清单——新增缺口登记、已实施项移入「已完成」、优先级按近期使用感受调整。
> 状态：`待办` / `进行中` / `已完成` / `观察`（依赖硬约束或上游实验性，定期重估）/ `不适用`（确认永不做的，附因）。

## 批次一（agent 核心体验，优先）

| # | 缺口 | 引入点 | 缺口描述 | 依赖/约束 | 状态 |
|---|---|---|---|---|---|
| 1 | 轮改动审阅卡 + diff 数据面 | 0.1.6-alpha.2 | changes/review 系列词汇：本轮「已编辑 N 个文件」卡（±行数、二进制标记）、点开单文件 diff、split/unified 切换、wrap 开关、「在侧边栏打开整个文件」联动文件树 dock。**数据面切片已落（09-19）**：dsh-tools 增 presentation 元数据缝（上游 output.presentationMeta 投影等价）+ 行 diff 纯函数（LCS hunk、±3 行上下文、上下文不计统计——上游 DiffBlock 同语义）+ dsh-fs write 捕获 before（FileDiff oldText/newText）+ tool/result 落盘读回往返（presentation 落 data 内，上游 merge-extensible 宽容）；剩 UI 层（review 卡/文件树 dock diff tab）**待用户定交互形态** | 上游 spec 已读：changes=fs/observed 观测流（version token，非 diff 内容）；diff=工具结果 presentationMeta（FileDiff oldText/newText，presentation.ts） | 进行中（UI 待定向） |
| 2 | plan 审阅面 | 0.1.6-alpha.2 多笔 | plan 卡片（提交/审阅状态点/预览 tab + plan icon/artifact 卡自动打开/narrow 屏自动预览/审阅摘要卡）；上游 plan 生命周期（提交→审阅→采纳）在会话流的呈现 | plan 数据经 session-reference/事件流承载，需先读上游 plan 相关事件与 spec | 待办 |
| 3 | 图片输入全链 | 0.1.3-alpha.1 起累积 | composer 图片捕获/粘贴/拖放 + 规范化流水线（上游 image offload：resize/编码）；vision 模型的图片发送；durable image offload（0.1.6-alpha.1 note：附件落盘与消息引用）。**请求侧切片已落（09-20）**：adapter 增 image_fetcher 注入（attachments 存储反查字节），user 消息含 Image 时 content 转 parts 数组（文本句柄 + image_url data URL——上游 serialize.ts base64 路径同形），反查失败降级占位文本；宿主 5 处 DeepSeekAdapter 构造全部接线。**捕获切片已落（09-21）**：回形针选 PNG/JPEG → dsh-persist image_dimensions 测尺寸（IHDR/SOF 扫描 3 测）→ save_file_verbatim 入存（与文件同 id 空间）→ composer 草稿图片 tile（object_path 直读）→ 发送 ContentBlock::Image（走请求侧）| Files API file 通道未实现；粘贴/拖放捕获待做；image-tokens 计价/预算待做；durable offload（ImageBlock.offloaded 标记+surfaceOp replace+IMAGE_OFFLOAD_REQUIRED 恢复循环）待做；实机视觉冒烟受 Temp 写入拦截未完成（数据面 6 测覆盖），留日常使用验证 | 进行中（子批 3：粘贴/拖放捕获；子批 4：durable offload 循环） |
| 4 | subagent 侧栏聊天 | 0.1.6-alpha.2 | 子 agent 会话在侧栏 tab 打开、实时查看子任务过程、settlement notice 文本化呈现（b86b89da94/29debb8b24 的 notice 语义） | dsh-subagent 已有驱动；缺 UI 承载（dock tab 或独立面板）+ 子会话日志读取面 | 待办 |
| 5 | 每会话独立 runtime（真并发） | 既有 | 运行中在其它会话发消息（上游 per-session runtime 真并发）；rustdsh 现为单 runtime 槽 + 查看式切换（e10050b） | 重构：按会话运行表 + 事件按会话路由 + 持久化按会话 id；大工程，拆独立批次 | 待办（大） |

## 批次二（工作台补全）

| # | 缺口 | 引入点 | 缺口描述 | 依赖/约束 | 状态 |
|---|---|---|---|---|---|
| 6 | deliverables 交付文件面 | 0.1.5-alpha.2/0.1.6-alpha.2 | 产出文件列表卡（本轮文件改动卡的兄弟面）、presented.* 打开动作（资源管理器/Finder/默认应用/更多操作）、与侧栏资源联动 | 依赖 #1 的改动收集数据面 | 待办（#1 后） |
| 7 | documentpreview 富渲染器 | 0.1.5-alpha.2 | 侧栏预览 html/pdf/markdown/image 体（当前纯文本+行号）；pdf 文本层旋转（afe85c1cfd） | pdf/html 渲染在 GPUI 需自绘/嵌方案，逐格式评估 | 待办（逐格式拆） |
| 8 | 语法高亮（代码块/预览） | 0.1.5-rc.1（Mermaid 系列 revert 后净态外的持续面） | vendor TextView 代码块无 shiki 级高亮；上游 CodeBlock 高亮 + code-file-icon 的语言着色 | vendor TextView 扩展或自绘代码块（8b 行渲染已有底座） | 待办 |
| 9 | fs watch 变更通告 | 0.1.5-alpha.2 | 文件树 changed/reloadNow 条（外部改动检测 + 一键重载） | 需 notify 类 crate 或轮询 mtime | 待办 |
| 10 | 会话统计持久投影 | 0.1.5-alpha.1 | session-stats/token-meter 投影：统计数值走整日志投影（跨重载/分页一致），替代窗口累计；TTFT/decode 精确计时 | 数据面迁移，UI 不变 | 待办 |
| 11 | openResource 资源地址体系 | 0.1.5-alpha.2 | Session/绝对路径文件资源地址统一（支撑 #1/#6/#7 的「在侧边栏打开」动作） | 纯内部缝 | 待办（随 #1 落） |

## 批次三（长尾/观察）

| # | 缺口 | 引入点 | 约束/备注 | 状态 |
|---|---|---|---|---|
| 12 | Think/compaction 头滚动吸顶 | 0.1.6-alpha.1 | 聊天流无 sticky 机制；轨迹页的固定行高模型不同构。低频体验项 | 观察 |
| 13 | markdown 表格 hover 高度稳定 | 0.1.6-alpha.1 | vendor TextView 表格面 | 观察 |
| 14 | 终端面（terminal-controller：launcher/shell 记忆/主题/PTY 会话） | 0.1.6-alpha.1 | GPUI 无终端模拟器组件；上游 PTY 也曾推迟。等 GPUI 生态或自绘 xterm 级成本评估 | 观察（硬约束） |
| 15 | Sidebar Browser（内嵌 webview） | 0.1.6-alpha.2 | GPUI 无 webview，完整内嵌浏览器不可行；**等价承接已落（09-22）**：聊天流 markdown 文件路径链接点击 → 侧栏 dock 文件预览路由（vendor gpui-component 相对文件打开器钩子 set_relative_file_opener，宿主注入 AppView.open_file_preview；inline/node 两点击点，http(s)/mailto 照常系统打开）；富渲染器（语法高亮/图片/pdf）仍待 #7 | 进行中（富渲染面留 #7） |
| 16 | guide 起始页/多 tab | 0.1.6-alpha.1 | 依赖 dock 多 tab 引擎 | 观察 |
| 17 | session-log 上传遥测 / OTel | 0.1.6-alpha.1 | 本地工具不上传遥测（目标定位不符） | 不适用 |
| 18 | browser-use 实验浏览器后端 | 0.1.6-alpha.1 | 上游自身实验性；观察其稳定度 | 观察 |
| 19 | LinkIcon 内联图标 / 逐 span hover / inline code 0.5px 描边 | 0.1.3-alpha.1 起 | vendor TextView 单色 tint + 无逐 span 事件；硬约束 | 观察（硬约束） |
| 20 | system/compaction 组头（轨迹 prompt-change 面） | 本地 7b72ca1 偏差 | rustdsh 无对应 cell 类型；随 #2 plan 面评估 | 观察 |

## 已完成（从清单结转）

- 侧栏工作区文件树 + 文本预览（09-15，66e25ca/5b38f40——原判面外，用户定向后面内实装）
- 统计双 pill + 互斥对话框（0.1.5-alpha.1）
- 附件文件类型图标（0.1.5-alpha.2）
- composer 命令菜单四行子集（本地 b3c9bc7；goal/plan/feedback/permission 四行随 #2 补）
- 权限预设三旋钮（本地 df9bf41）

## 排期备忘

- 每轮节奏：同步判定（有新面内 → 实施）→ 无面内则从批次一按序取一项推进（哪怕只完成数据面/半个 UI）→ 回归 → 记录进度。
- 大项（#3 图片输入、#5 真并发）拆子批，每轮只承诺一个可验证的切片。
- 硬约束项（14/15/19）每 2-4 周重估一次（GPUI/依赖演进可能解锁）。
