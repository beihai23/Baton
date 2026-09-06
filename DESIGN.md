# Design System — Baton

> 由 /design-consultation 于 2026-09-06 创建。所有视觉/UI 决策以本文件为准；
> 修改前先在团队中确认。预览页：`/tmp/baton-design-preview.html`（可重新生成）。

## Product Context

- **What this is:** Agent Native 的本地优先看板工具——看板是人机共用的任务中枢，
  人类是协调者/管理者，AI Agent 是干活的一等成员（MCP/CLI/HTTP 接入）
- **Who it's for:** 指挥多个 AI Agent 干活的开发者/技术管理者
- **Space/industry:** 开发者工具 / 任务管理（品类参照：Linear、Height、GitHub Projects）
- **Project type:** 桌面应用（Tauri）+ 同源 WebUI，暗色优先
- **Memorable thing:** 打开它的前三秒就应该感到"这是一间正在运转的人机协作调度室"——
  有东西活着，有人在替我干活，一切尽在掌控

## Aesthetic Direction

- **Direction:** Industrial/Utilitarian × Editorial（工业控制台 × 编辑部排版纪律）
- **Decoration level:** intentional（票据美学：等宽卡号、虚线撕缝线、移交印章；
  无渐变、无装饰性色块、无居中一切）
- **Mood:** 熄灯办公室里亮着的调度台。安静、密集、有生命体征
- **Signature:** **人机分色**——琥珀=人类的动作与主交互，青=Agent 的在场与活动。
  产品定位（Agent 是一等成员）直接变成色彩语言

## Color

- **Approach:** restrained——只有两种强调色（人/Agent），语义色仅表达状态
- **暗色主题（默认）:**

| Token | Hex | 用途 |
|---|---|---|
| `--bg` | `#12100d` | 页面底色（暖调石墨） |
| `--surface` | `#1a1712` | 面板/列/侧栏 |
| `--surface-2` | `#23201a` | 卡片/输入框/徽标底 |
| `--surface-3` | `#2e2921` | hover/抬升面 |
| `--border` | `#332d23` | 常规边框 |
| `--border-strong` | `#4a4232` | 强边框/虚线撕缝线 |
| `--text` | `#f0eadd` | 纸白正文 |
| `--text-2` | `#b7ad9c` | 次级文本 |
| `--muted` | `#847c6d` | 静默文本/占位 |
| `--human` | `#e8a33d` | 人类动作、主按钮、链接、选中态 |
| `--agent` | `#4fd8c8` | Agent 在场/活动/持有 |
| `--success` | `#6fc38a` | 正常/完成/审批通过 |
| `--warn` | `#e5c05c` | 注意/需审批列 |
| `--danger` | `#e5655e` | 危险操作/错误 |

- **soft 变体**（徽章/印章底色）：`--human-soft: rgba(232,163,61,.13)`，
  `--agent-soft: rgba(79,216,200,.12)`
- **浅色主题：** 存在（`[data-theme="light"]`，暖纸面），但不是主战场，优先级低
- **纪律：** 暖色底配错色容易显脏——token 必须成体系使用，禁止随手写死色值

## Typography

- **UI/标题（西文）:** IBM Plex Sans 400/500/600/700（工业血统，控制台气质）
- **UI（中文）:** PingFang SC（系统回落；本地工具不加载 CJK 网络字体）
- **数据/编号:** JetBrains Mono 400/500（一切 id、时间戳、rev、计数、Token）
- **字体栈:** `--font-ui: "IBM Plex Sans","PingFang SC","Microsoft YaHei",sans-serif`、
  `--font-mono: "JetBrains Mono","SF Mono",ui-monospace,monospace`
- **Loading:** Google Fonts `<link>`（仅两个西文字体）
- **Scale:** 11（辅助/mono 卡号）· 12（meta/badge）· 13（正文/按钮）· 14（卡片标题）
  · 15-18（抽屉标题）· 数字用 tabular-nums
- **禁用:** Inter/Roboto/Space Grotesk 及一切"AI 工具默认字体"

## Spacing

- **Base unit:** 8px；密度 comfortable-dense（看板是密度工具）
- **Scale:** 2 / 4 / 8 / 12 / 16 / 24 / 32 / 48

## Layout

- **Approach:** grid-disciplined（工具型产品，网格纪律优先）
- **骨架:** 顶栏（品牌/面包屑/Agent 电波条/动作区）+ 左侧项目导航（208px）+
  看板列（292px 固定宽，横向滚动）+ 右侧抽屉（480px + 遮罩）
- **Border radius:** sm 7px（输入/小件）、10px（卡片/面板）、999px（胶囊徽标）
- **票据美学细节：** 卡片顶部等宽卡号行 + 虚线分隔（`1px dashed var(--border-strong)`）；
  移交完成盖"已移交"印章（琥珀描边、旋转 -4°）

## Motion

- **Approach:** minimal-functional
- **Duration:** hover 120-150ms；面板/抽屉 200ms ease-out；无滚动编排
- **Signature:** Agent 在线指示点的 2s 呼吸脉冲（`pulse`）——全站唯一常驻动效，
  "有东西活着"的最低剂量表达

## 组件纪律（交互 vs 信息）

- **可交互元素必须像可交互：** 按钮（`.btn` 体系：primary=琥珀底 / 默认 / agent=青描边 /
  danger=红 / link=琥珀文字）、链接=琥珀色、focus-visible 环
- **纯信息绝不像按钮：** 状态徽标（badge/mini-badge）、tag、muted 文本
- **全局 `button` 是无样式 reset，** 新按钮必须显式加 `.btn*`

## 未来方向（记录，未实现）

- **Agent 电波条**（顶栏常驻 ticker：`a-code · 正在改 server.rs · 租约剩 23:41`）——
  让 Agent 的"在场"成为界面主角。需要 session 简报数据支撑，属功能迭代
- 宋体标题（Songti SC / Noto Serif SC）作为更强的编辑感实验——暂缓
  （CJK 网络字体成本 / 跨平台回落不确定）

## Decisions Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-09-06 | 初始设计系统「调度室」 | /design-consultation；方向综合自主提案与独立设计 subagent（"夜班调度室"），两者独立收敛到暖暗底 + 琥珀/青人机分色 |
| 2026-09-06 | 顶栏分区：左=context，右=次级开关→环境状态→主行动线；Agent 在场状态压缩为可点汇总芯片（点击打开 Agent 面板） | 平铺徽章列车导致信息层级丢失；视觉权重必须与使用频率/信息类型成正比 |
| 2026-09-06 | 抽屉加状态横幅（抢单池/已指派/谁在干/移交态）；指派下拉从元信息行归入操作区；审批/通知/Agent 面板改为下拉浮层（fixed + 遮罩点击关闭），不再挤压看板布局；每个 Tab 顶部一句话用途说明 | 状态要直接给答案而不是让用户推理；动作与元信息分家；弹层不应造成布局位移 |
| 2026-09-06 | 建卡即打开抽屉（引导指派/补描述）；审批中心加"最近已处理"闭环；拖拽经过目标列时预告策略结果（拒绝=红、进审批=琥珀），不等松手后才报错 | 每个动作都要有明确的"下一步"承接；反馈要在动作发生前可用 |
| 2026-09-06 | 放弃 AI  mockup，走 HTML 预览页 | gstack design 二进制缺 OpenAI key；HTML 预览页已目检通过 |
| 2026-09-06 | CJK 用系统黑体而非网络宋体 | 本地工具不背 MB 级字体加载；宋体标题记入未来方向 |
