# Agent Native 本地看板 — 产品需求设计文档（PRD）

> 版本：v0.1（草案）
> 日期：2026-09-05
> 状态：待评审
> 产品名：**Baton**（2026-09-05 定名）

---

## 1. 产品定位与愿景

### 1.1 一句话定位

一个**完全本地运行**的看板协作工具：看板即"人机共用的任务中枢"（Task Bus），
AI Agent 和人类一样是看板的一等成员，多个 Agent 通过看板认领、执行、交接和协调任务，
人类通过简洁 GUI 参与管理、审批与监督。

### 1.2 与 Trello Desktop 的核心差异

| 维度 | Trello Desktop | 本产品 |
|---|---|---|
| 运行位置 | 云端服务 + 本地客户端 | **完全本地**，无云依赖，离线可用 |
| 成员 | 只有人 | **人 + AI Agent 同为一等公民** |
| 自动化 | Butler 规则（预设触发器） | Agent 直接读写看板，具备推理与决策能力 |
| Agent 接入 | 无（仅有通用 REST API） | **Agent 接入是核心协议**：MCP / 本地 API / CLI |
| 多 Agent 协作 | 不支持 | 任务认领、租约锁、依赖编排、Agent 间消息均为原生能力 |
| 数据归属 | 厂商云 | 用户本机，纯文件/SQLite，可随时备份迁移 |

### 1.3 设计哲学（黑板架构）

本产品本质是一个**黑板系统（Blackboard Architecture）**：
看板卡片 = 共享工作记忆；列 = 任务状态机；Agent = 监听黑板并作出贡献的知识源。
Agent 之间不直接点对点调用，而是**通过看板状态变化进行异步协调**——
这让协调过程天然可见、可审计、可被人介入。

### 1.4 目标用户

- **个人开发者 / 独立创作者**：用多个本地 Agent（编码、写作、调研）并行推进个人项目。
- **小团队技术负责人**：在人机混合团队里统一派活、看进度、做审批。
- **AI 重度用户 / Agent 编排爱好者**：把不同厂商、不同框架的 Agent 插到同一块板上协作。
- **隐私敏感用户**：任务内容、代码、文档不出本机。

---

## 2. 核心概念与角色模型

### 2.1 一等实体

| 实体 | 说明 |
|---|---|
| **Workspace（工作区）** | 顶层容器，对应一个本地目录/库；一个实例可含多个工作区 |
| **Project（项目）** | 工作区下的独立项目，拥有自己的看板、成员与配置 |
| **Board（看板）** | 一个项目可有多个看板（如：开发板、内容板、运维板） |
| **List/Column（列）** | 任务状态机的一个状态；可配置进入/离开规则（列策略） |
| **Card（卡片）** | 最小工作单元；承载描述、 checklist、附件、评论、执行日志 |
| **Member（成员）** | 统一抽象：人类成员 与 Agent 成员 都实现同一成员接口 |
| **Agent Profile（Agent 档案）** | Agent 的身份、能力标签、接入方式、权限范围、并发配额 |
| **Run（执行记录）** | 一次 Agent 对某卡片的执行会话，含输入、输出、产物、状态 |
| **Message（消息）** | 卡片评论 / Agent 间留言 / 系统通知的统一抽象 |
| **Artifact（产物）** | Agent 产出的文件（代码 diff、文档、图片），挂载在卡片上 |

### 2.2 角色与权限

| 角色 | 能力 |
|---|---|
| Owner（人） | 全部权限：项目配置、Agent 准入、权限分配、数据管理 |
| Member（人） | 建卡、移动卡片、评论、审批（按配置） |
| Agent - Worker | 认领/执行卡片、写评论、上传产物、请求审批；**不能修改项目配置** |
| Agent - Coordinator | 额外能力：创建/分解卡片、指派任务、调整优先级；**不能审批人才能批的节点** |
| Agent - Observer | 只读 + 可评论，用于监控、汇报类 Agent |

关键原则：
- **任何 Agent 权限都是 Owner 显式授予的，按项目隔离，可随时吊销。**
- **Coordinator 是一种"开放的角色能力"，不是内置功能**：本产品不内置 LLM、也不自带任何 Coordinator 实现。Owner 把 Coordinator 权限授予任何一个已接入的 Agent（如 Claude Code、Kimi 或自研编排 Agent），该 Agent 即可通过标准 MCP/CLI/API 工具面（`card.create`、`card.update`、`card.move`、依赖编排等）承担拆分任务、派活、盯进度的角色。对系统而言，Coordinator 与 Worker 走的是**同一套 API**，区别仅在授权范围。

---

## 3. 核心用户场景

### 场景 A：人派活，Agent 干活
1. 用户在 GUI 创建卡片"给登录页加上表单校验"，拖到 `Ready` 列，指派给 `code-agent`（或留空由 Agent 抢单）。
2. 本地运行的 code-agent 通过 MCP/API 监听到卡片进入 `Ready`，**认领（claim）**该卡片，系统自动加租约锁并移到 `In Progress`。
3. Agent 执行过程中把进度日志、中间结论写入卡片评论；完成后上传 diff 产物，把卡片移到 `Review`。
4. 用户收到通知，在 GUI 里查看 diff 与执行摘要，点"通过"→ 卡片进 `Done`；或点"打回"并留言 → 卡片回 `In Progress`，Agent 继续。

### 场景 B：多 Agent 流水线协作
1. Coordinator Agent 把"发布 v2.0"大卡片**分解**为 5 张子卡片，建立依赖关系（卡片 3 blocked-by 卡片 1、2）。
2. `research-agent` 完成调研卡并上传调研报告；`code-agent` 引用该产物开始编码；`test-agent` 在代码卡完成后被依赖触发自动激活。
3. 全程无需人工操作，但用户随时打开看板就能看到每张卡的实时状态、当前持有者、最新日志。

### 场景 C：人机混合审批
- 看板配置：`Deploy` 列的进入规则为 **必须人类 Owner 手动拖动或点击批准**。
- Agent 完成一切前置工作后卡片停在 `Awaiting Deploy Approval`，人类一键批准后才进入部署列，由 `ops-agent` 接手。

### 场景 D：Agent 间"留言"协调
- `code-agent` 发现需求描述有歧义，在卡片上 @ `pm-agent` 留言："第 2 条验收标准与第 4 条冲突，请澄清"，并将卡片标记为 `Blocked`。
- `pm-agent`（或人类）回复澄清后，卡片自动解除 Blocked，通知 code-agent 继续。

### 场景 E：多项目并行
- 用户同时开着"产品 A"、"副业博客"、"家庭事务"三个项目看板；
- 不同项目挂载不同 Agent 组合（博客项目挂 writer-agent + seo-agent）；
- Agent 之间跨项目隔离，互不可见。

---

## 4. 功能需求

### 4.1 看板基础能力（对标并精简 Trello）

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-101 | 多工作区 / 多项目 / 多看板，层级导航 | P0 |
| F-102 | 列的增删改、拖拽排序、WIP（在制品）上限设置 | P0 |
| F-103 | 卡片 CRUD：标题、富文本描述、checklist、标签、截止日期、优先级 | P0 |
| F-104 | 卡片拖拽移动、排序、批量操作 | P0 |
| F-105 | 卡片指派：可指派给人、Agent、或"空闲可抢"池 | P0 |
| F-106 | 卡片依赖：blocks / blocked-by / relates-to；被依赖未完成的卡片不可进入 Done | P1 |
| F-107 | 子任务/卡片分解：父子卡片树，父卡进度自动聚合 | P1 |
| F-108 | 附件与产物管理：文件本地存储，卡片内预览 | P0 |
| F-109 | 评论与 @提及（可 @人或 @Agent） | P0 |
| F-110 | 全局搜索与筛选（按标签/成员/状态/项目） | P1 |
| F-111 | 活动流（Activity Feed）：谁在何时对哪张卡做了什么 | P0 |
| F-112 | 看板模板：预置"软件开发 / 内容生产 / 通用 GTD"模板 | P2 |

### 4.2 Agent Native 接入层（核心差异化）

#### 4.2.1 三种接入方式（同时提供，语义一致）

| 编号 | 方式 | 说明 | 优先级 |
|---|---|---|---|
| F-201 | **MCP Server** | 本应用内置 MCP server，任何支持 MCP 的 Agent（Claude Code、Kimi、自研 Agent…）零适配接入；提供 `board.*` 工具集 | P0 |
| F-202 | **本地 HTTP/WebSocket API** | `http://127.0.0.1:<port>/api/v1`；REST 做读写，WebSocket 做事件订阅；Token 鉴权 | P0 |
| F-203 | **CLI** | `baton card move <id> --to done` 等全功能命令，方便脚本型/终端型 Agent | P1 |

#### 4.2.2 Agent 可用操作（工具面）

- `board.list / board.get` — 读取看板结构
- `card.list / card.get` — 读取卡片（含描述、评论、产物清单）
- `card.create / card.update` — 建卡、改卡（受权限约束）
- `card.claim / card.release` — 认领 / 释放任务（见 4.3）
- `card.move` — 移动卡片（受列策略约束）
- `card.comment` — 评论、@他人
- `artifact.upload / artifact.list` — 产物上传与引用
- `event.subscribe` — 订阅事件流（"项目 X 中进入 Ready 列的卡片"）
- `approval.request` — 请求人类审批
- `agent.heartbeat` — 心跳，用于在线状态与租约续期

#### 4.2.3 Agent 注册与管理

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-211 | GUI 中"添加 Agent"向导：命名、能力标签（coding/writing/research…）、接入方式、权限范围、并发上限 | P0 |
| F-212 | 每个 Agent 签发独立本地 Token，可随时吊销/轮换 | P0 |
| F-213 | Agent 在线状态面板：在线/离线、当前持有的卡片、最近心跳 | P0 |
| F-214 | Agent 粒度审计：每个 Agent 的全部操作可追溯 | P0 |

### 4.3 多 Agent 并发与协调机制（核心难点）

| 编号 | 机制 | 说明 | 优先级 |
|---|---|---|---|
| F-301 | **认领锁（Claim + Lease）** | 卡片被 claim 后获得排他租约（默认 30 min，可续期）；其他 Agent 只读；租约过期自动释放并告警 | P0 |
| F-302 | **乐观并发控制** | 所有写操作携带版本号（ETag/rev），冲突时返回 409，由调用方重试 | P0 |
| F-303 | **抢单模式** | 卡片放入"可抢"池后，多个空闲 Agent 按原子 claim 先到先得 | P0 |
| F-304 | **列策略（Column Policy）** | 每列可配置：允许谁进入、进入需审批（人/指定 Agent）、自动触发动作 | P0 |
| F-305 | **依赖编排** | 依赖满足时自动通知下游卡片的 assignee（或放回抢单池） | P1 |
| F-306 | **冲突提示与仲裁** | 两个 Agent 同时改一卡时，后到者收到差异信息；僵局时升级给人类 | P1 |
| F-307 | **幂等写** | 所有写 API 支持 Idempotency-Key，防止 Agent 重试造成重复操作 | P0 |
| F-308 | **速率限制与配额** | 按 Agent 限流（如 60 req/min），防止失控 Agent 打爆看板 | P1 |

### 4.4 人类参与与监督（GUI）

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-401 | 桌面 GUI（本地应用），实时同步看板状态（WebSocket 推送，秒级） | P0 |
| F-402 | **审批中心**：待审批事项聚合页，一键通过/打回，可附留言 | P0 |
| F-403 | **Agent 视角透视**：点开卡片可看到 Agent 的完整执行日志与产物 diff | P0 |
| F-404 | 通知中心：被 @、审批请求、租约异常、Agent 掉线等 | P1 |
| F-405 | "接管"按钮：人类可随时强制释放 Agent 租约，把卡拿回自己手里 | P0 |
| F-406 | 仪表盘：各项目卡片吞吐、Agent 工作量分布、平均交付时长 | P2 |
| F-407 | 键盘快捷键 + 命令面板（Cmd/Ctrl+K） | P2 |

GUI 设计原则：
- **三栏心智**：左（项目导航）/ 中（看板）/ 右（卡片详情或审批中心），不做复杂嵌套。
- Agent 的活动在 UI 上有**统一视觉标识**（头像样式、机器人角标、操作着色），一眼区分"这是 Agent 干的"。
- 默认展示足够信息：卡片上直接显示当前持有者、租约倒计时、最新一条进展。

### 4.5 数据与存储

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-501 | 全本地存储：SQLite（结构化）+ 文件目录（附件/产物），单工作区一个目录 | P0 |
| F-502 | 完整操作日志（append-only event log），支持审计与回放 | P0 |
| F-503 | 一键导出：整个项目导出为目录包（JSON + Markdown + 附件），可导入 | P0 |
| F-504 | 自动快照备份（每日 + 手动），保留最近 N 份 | P1 |
| F-505 | （未来）可选的端到端加密同步，仅作为插件，不在核心 | P3 |

### 4.6 卡片数据模型（详细设计）

卡片是全系统的核心聚合体。设计目标：**既要让人在 GUI 里一眼看懂，又要让 Agent 可以结构化地读写每一个字段**。

#### 4.6.1 总体分层

```
Card（卡片）
├── core        核心字段（标题、描述、状态、指派、租约…）—— 所有卡都有
├── discussion  讨论区：多话题（Thread）× 多轮（Comment）
├── ext         结构化扩展信息（requirements / progress / git / worksite / handoff / custom）
└── artifacts   产物与附件（文件实体，卡片持有引用）
```

原则：
- **可查询的进表，易演进的进 JSON**：讨论、链接、工作现场节点等需要筛选/索引/关联的数据建独立表；`ext` 主体为带 `schema_rev` 的 JSON，向前兼容。
- **易变数据只存快照**：git 状态（staged/unstaged 等）随时变化，卡片里只存"某时刻的快照 + 刷新时间戳"，并显式区分 **declared（声明的）** 与 **observed（探测到的）**。
- **一切写操作走 event log**，天然获得审计与回放。

#### 4.6.2 核心字段（core）

```ts
interface Card {
  id: string;                    // ULID
  project_id: string;
  board_id: string;
  list_id: string;               // 当前所在列 = 状态机当前状态
  title: string;
  description: string;           // Markdown；frontmatter 可放验收标准等结构化块
  rev: number;                   // 乐观并发版本号，所有写操作必须携带
  priority: 'urgent' | 'high' | 'medium' | 'low';
  labels: string[];
  due_at?: string;               // ISO 8601
  assignee?: MemberRef;          // 人 或 Agent；null = 抢单池
  claim?: {                      // 当前租约（无租约则缺省）
    holder: MemberRef;
    lease_until: string;         // 到期自动释放
    run_id?: string;             // 关联的执行记录
  };
  parent_id?: string;            // 父子卡片树
  blocked_by: string[];          // 依赖（卡片 id）
  created_by: MemberRef;
  created_at: string;
  updated_at: string;
  archived_at?: string;
  ext: CardExt;                  // 见 4.6.4
}

type MemberRef = { kind: 'human' | 'agent'; id: string; name: string };
```

#### 4.6.3 讨论模型：多话题 × 多轮

讨论不是一条平铺评论流，而是**话题（Thread）为一等实体**：

```ts
interface Thread {                 // 子话题
  id: string;
  card_id: string;
  title?: string;                  // 话题名，如 "验收标准歧义"
  status: 'open' | 'resolved' | 'wontfix';
  anchor?: {                       // 话题可锚定到具体内容
    kind: 'description' | 'comment' | 'checklist_item' | 'artifact';
    ref_id: string;
    quote?: string;
  };
  created_by: MemberRef;
  created_at: string;
  resolved_by?: MemberRef;
  resolved_at?: string;
}

interface Comment {                // 话题内多轮消息
  id: string;
  card_id: string;
  thread_id: string;
  reply_to?: string;               // 话题内的直接回复
  author: MemberRef;
  kind: 'chat' | 'progress' | 'system' | 'handoff' | 'approval';
  body: string;                    // Markdown
  mentions: MemberRef[];           // 可 @人或 @Agent，触发通知
  created_at: string;
  edited_at?: string;
}
```

关键规则：
- **统一时间线**：Agent 的进度日志、系统事件（移动卡片、租约变更）、审批记录都以 `kind` 区分的 Comment 写入，人和 Agent 看到的是同一条历史。
- **有未 resolved 话题 ≠ Blocked**：是否阻塞由卡片状态决定；但 GUI 会提示"3 个未结话题"，列策略可配置"存在未结话题时禁止进入 Done"。
- 进度类评论（`kind: 'progress'`）同时聚合进 `ext.progress.history`，形成结构化进度轨迹。

#### 4.6.4 ext：结构化扩展信息

```ts
interface CardExt {
  schema_rev: 1;                   // ext 结构版本，用于迁移
  requirements?: RequirementsExt;  // 需求来源与需求文档
  progress?: ProgressExt;          // 当前工作进度
  git?: GitExt;                    // git 上下文（含 worktree、脏状态）
  worksite?: WorksiteExt;          // 工作现场拓扑
  handoff?: HandoffExt;            // 移交状态
  custom?: Record<string, unknown>; // 命名空间化自定义字段，如 "x-acme.estimate"
}
```

##### (a) requirements — 任务需求来源

```ts
interface RequirementsExt {
  source_links: SourceLink[];      // 需求从哪来（项目管理工具关联）
  doc_links: DocLink[];            // 需求文档链接
  attachment_refs: string[];       // 需求附件 → artifact id
}

interface SourceLink {
  system: 'jira' | 'meego' | 'github_issue' | 'url' | 'file';
  url: string;                     // 如 https://jira.corp/browse/PROJ-1234
  key?: string;                    // 结构化键，如 "PROJ-1234"
  title?: string;                  // 抓取或手填的标题快照
  relation: 'origin' | 'related';  // origin = 需求来源；related = 仅关联
  synced_at?: string;              // （可选插件）外部标题/状态最近同步时间
}

interface DocLink {
  kind: 'url' | 'local_file' | 'artifact';
  url?: string;                    // kind=url
  path?: string;                   // kind=local_file（本机路径）
  artifact_id?: string;            // kind=artifact（卡内产物）
  title: string;
}
```

说明：MVP 只存链接与标题快照，**不做** Jira/MeeGo 实时双向同步；同步留给 v0.3 插件（`synced_at` 字段为此预留）。

##### (b) progress — 当前工作进度

```ts
interface ProgressExt {
  percent: number;                 // 0-100，由持有者自评
  summary: string;                 // 一句话现状："登录页校验已完成，在补单测"
  milestones: { title: string; done: boolean; done_at?: string }[];
  blockers?: string;               // 当前卡点描述
  updated_by: MemberRef;
  updated_at: string;
  // 历史轨迹不冗余存储：由 kind='progress' 的评论聚合而成
}
```

约束：卡片每次移动列时系统**强制要求**持有者更新 `summary`（可在列策略中关闭），保证看板上的每张活跃卡都有"最新人话进展"。

##### (c) git — 仓库上下文

```ts
interface GitExt {
  repos: RepoCtx[];                // 一张卡可关联多个仓库
}

interface RepoCtx {
  repo_path: string;               // 主工作目录（本机绝对路径）
  branch: string;                  // 主工作分支
  declared: {                      // 声明层：Agent 打算/应该在哪干活
    base_branch?: string;          // 从哪个分支切出
    pr_url?: string;
  };
  observed?: GitStatusSnapshot;    // 探测层：最近一次真实探测
}

interface GitStatusSnapshot {
  staged: number;                  // 已 stage 文件数
  unstaged: number;                // 已修改未 stage 文件数
  untracked: number;
  clean: boolean;                  // 三者全 0
  ahead: number;                   // 领先 remote/base 的提交数
  behind: number;
  last_commit?: { sha: string; message: string; at: string };
  snapshot_at: string;             // ⚠️ 快照时间，UI 必须显示"截至 xx:xx"
  snapshot_by: MemberRef;          // 谁触发的探测
}
```

关键设计：
- **declared vs observed 分离**：Agent 声明"我在 feature/login 分支工作"是意图；`observed` 是 Core Server 本地执行 `git status --porcelain` 探测到的真实状态（仅当 `repo_path` 在本机时可探测）。
- 探测通过 `git.refresh` 工具/API 触发，或由 Agent 在心跳/进度更新时主动上报；**卡片永不声称实时 git 状态**。
- GUI 在卡片角标显示脏状态：`●3 staged · 2 未提交 · ↑5`，hover 显示快照时间。

##### (d) worksite — 工作现场拓扑

多 Agent 协作时，一张卡的工作现场 = 主工作目录（主分支）+ 若干 worktree，每个 worktree 可能绑定不同子任务/Agent：

```ts
interface WorksiteExt {
  root: string;                    // → WorkNode.id，主工作目录节点
  nodes: WorkNode[];
}

interface WorkNode {
  id: string;
  kind: 'main' | 'worktree';
  path: string;                    // 本机绝对路径
  branch: string;
  purpose?: string;                // "并行开发支付模块"
  owner?: MemberRef;               // 当前在该现场干活的 Agent/人
  bound_card_id?: string;          // 绑定的（子）卡片，形成跨卡拓扑
  observed?: GitStatusSnapshot;    // 该节点的独立脏状态快照
  created_by: MemberRef;
  created_at: string;
  removed_at?: string;             // worktree 清理后保留记录
}
```

拓扑关系即 `root + nodes + bound_card_id`：GUI 渲染为"主分支 — 若干 worktree 分支"的星型图，节点上标注 owner、绑定子卡、脏状态；跨卡绑定时边指向另一张卡片。

##### (e) handoff — 工作现场移交

适用于：多 Agent 接力，或某 Agent 搞不定主动"脱手"。**移交的不是一句话，而是整个工作现场 + 上下文包**。

```ts
interface HandoffExt {
  state: 'none' | 'preparing' | 'ready' | 'accepted' | 'cancelled';
  from?: MemberRef;                // 移交方
  to?: MemberRef;                  // 指定接手方；null = 公开可认领
  reason?: string;                 // "超出能力范围，需要前端专家"
  package?: HandoffPackage;        // 移交包
  timeline: { at: string; by: MemberRef; action: string; note?: string }[];
}

interface HandoffPackage {
  context_note: string;            // Markdown：已完成什么、卡在哪、踩过的坑、建议下一步
  worksite_snapshot: WorksiteExt;  // 移交时的工作现场快照（路径/分支/脏状态）
  env_notes?: string;              // 环境说明：依赖安装、如何跑起来、密钥在哪（本机）
  open_threads: string[];          // 未 resolved 的话题 id 列表（接手方必读）
  artifact_refs: string[];         // 相关产物
  prepared_at: string;
}
```

状态机：

```
none ──handoff.prepare──▶ preparing ──handoff.ready──▶ ready ──handoff.accept──▶ accepted ─▶ none（新一轮）
                        ▲                 │                       │
                        └──cancel─────────┴───────cancel──────────┘
```

- `preparing`：移交方整理移交包，卡片保持其租约；
- `ready`：移交方租约释放，卡片打"待接手"标记，可配置自动回抢单池或通知指定接手方；
- `accept`：接手方 claim 成功即 accept，`handoff.timeline` 永久保留（谁在何时从谁手里接的）；
- 人类在 GUI 可发起"强制移交"（结合 F-405 接管）：把人作为 `to` 的特例。

#### 4.6.5 存储映射（SQLite）

| 表 | 说明 |
|---|---|
| `cards` | core 字段 + `ext_json` 列（TEXT，JSON）+ 高频查询字段冗余为生成列并建索引：`progress_percent`、`handoff_state`、`list_id`、`assignee` |
| `threads` / `comments` | 讨论区，`card_id` + `thread_id` 索引 |
| `links` | source_links / doc_links 统一存表，`kind/system` 可索引（"找出所有关联 Jira 的卡"） |
| `artifacts` | 产物元数据，文件本体存工作区目录 `artifacts/<card_id>/` |
| `work_nodes` | worksite 节点独立成表（支持跨卡绑定查询：`bound_card_id` 索引） |
| `handoffs` | 当前状态冗余到 `cards.ext_json`，timeline 全量存表 |
| `events` | append-only 事件日志（所有上述变更的统一事实源） |

演进策略：`ext.schema_rev` 单调递增，启动时执行迁移；插件自定义字段必须放 `custom` 且以 `x-<vendor>.` 前缀命名，核心升级永不触碰。

#### 4.6.6 新增 API / MCP 工具

在 4.2.2 基础上补充：

- `thread.create / thread.list / thread.resolve / thread.reopen`
- `comment.create`（增加 `thread_id`、`kind`、`reply_to`）
- `progress.update`（percent/summary/milestones/blockers 局部更新）
- `link.add / link.remove`（source_links / doc_links）
- `git.attach`（关联 repo/branch）、`git.refresh`（探测并写 observed 快照）
- `worksite.add_node / worksite.remove_node / worksite.bind_card`
- `handoff.prepare / handoff.ready / handoff.accept / handoff.cancel`

#### 4.6.7 GUI 呈现

卡片详情页右栏分 Tab：

| Tab | 内容 |
|---|---|
| 讨论 | 话题列表（未结置顶）+ 话题内多轮对话 + 统一时间线视图切换 |
| 进展 | percent 进度条、milestones checklist、blockers 高亮、进度历史 |
| 需求 | source_links（Jira/MeeGo 图标 + key + 标题快照）、doc_links、需求附件 |
| Git | 仓库列表、分支、脏状态角标（带快照时间）、PR 链接 |
| 现场 | worksite 星型拓扑图：主目录 + worktree 节点（owner/绑定子卡/脏状态） |
| 移交 | handoff 状态、移交包预览（接手前可查看）、timeline |

---

## 5. 非功能需求

| 类别 | 要求 |
|---|---|
| 离线可用 | 断网 100% 可用；不依赖任何云服务（LLM 调用是 Agent 自己的事，与本工具无关） |
| 性能 | 单板 5,000 卡片流畅滚动；API P95 < 50ms（本地）；GUI 状态推送延迟 < 1s |
| 并发 | 支持 ≥ 10 个 Agent + 3 个人同时对同一项目操作，无写丢失 |
| 可靠性 | Agent 进程崩溃 → 租约到期自动回收；应用崩溃 → 重启后状态完整恢复（WAL + 快照） |
| 安全 | API 仅监听 127.0.0.1（默认）；Token 鉴权；Agent 权限最小化；敏感操作（删项目、改权限）仅人类可做 |
| 可观测 | 每个 Run 有完整日志；提供 `baton doctor` 自检命令 |
| 可移植 | 工作区目录可直接拷贝到另一台机器打开 |
| 平台 | 首发 macOS + Windows + Linux 桌面。**已定：Tauri（Rust 壳 + Web 前端）**，见 §8 决策记录 |

---

## 6. 建议技术架构（参考，不强制）

```
┌─────────────────────────────────────────────────────────┐
│  Desktop GUI — Tauri（Rust 壳 + Web 前端） (人)           │
│  看板 / 审批中心 / Agent 面板 / 仪表盘                     │
└──────────────┬──────────────────────────────────────────┘
               │ WebSocket / REST（本地回环）
┌──────────────▼──────────────────────────────────────────┐
│  Core Server (本地常驻进程)                               │
│  ┌──────────┐ ┌───────────┐ ┌────────────┐ ┌─────────┐  │
│  │ Board    │ │ Claim &   │ │ Event Bus  │ │ Approval│  │
│  │ Service  │ │ Lease Mgr │ │ (pub/sub)  │ │ Engine  │  │
│  └──────────┘ └───────────┘ └────────────┘ └─────────┘  │
│  ┌──────────────────────────────────────────────────┐   │
│  │ Access Layer: MCP Server │ HTTP API │ CLI        │   │
│  └──────────────────────────────────────────────────┘   │
└──────────────┬──────────────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────────────┐
│  Storage: SQLite (WAL) + append-only Event Log + 文件目录 │
└──────────────────────────────────────────────────────────┘

外部：各种 Agent 进程（Claude Code / Kimi / 自研脚本…）
      通过 MCP / HTTP / CLI 接入，与 Core Server 对等通信
```

关键决策说明：
- **事件溯源友好**：所有变更先写 event log 再更新物化状态，天然获得审计、回放、实时推送能力。
- **Agent 无特权通道**：GUI 和 Agent 走同一套 API 语义，保证"人看到的"就是"Agent 操作的"。
- **不内置 LLM**：本工具是中立的任务中枢，Agent 自带智能；避免与任何模型厂商绑定。

---

## 7. MVP 范围与路线图

### MVP（v0.1，目标 6~8 周）
- 单工作区、多项目、多看板、卡片/列 CRUD、拖拽
- **卡片数据模型 §4.6 全量落地**：多话题讨论、requirements（Jira/MeeGo 链接）、progress、git（declared/observed 快照）、worksite 拓扑、handoff 状态机
- Agent 注册 + Token + HTTP/WebSocket API + **MCP Server**
- Claim/Lease 锁、乐观并发、列策略（含人工审批列）
- 评论、@提及、产物上传、活动流、审批中心
- SQLite 存储 + 导出/导入

### v0.2
- 依赖编排与子卡片树、CLI、抢单池
- 通知中心、Agent 在线面板、快照备份
- 看板模板

### v0.3+
- 多工作区、仪表盘统计、命令面板
- 插件系统（自定义列策略脚本、自定义 Agent 适配器、Jira/MeeGo 双向同步插件）
- （远期，可选）加密 P2P 同步

---

## 8. 决策记录

全部开放问题已于 2026-09-05 评审完毕，无遗留待定项。

- ✅ **GUI 技术栈：Tauri**（Rust 壳 + Web 前端，轻量、适合本地工具）。
- ✅ **卡片描述格式：Markdown 为主、结构化为辅**——验收标准等结构化块放 YAML frontmatter；需求链接、进度、git、工作现场、移交等全部进入 `ext` 结构化扩展（见 §4.6）。
- ✅ **Jira/MeeGo 只做链接关联，不做实时双向同步**（MVP）；同步留给 v0.3 插件，`synced_at` 字段已预留。
- ✅ **Coordinator Agent 不内置**：产品不带 LLM、不自带 Coordinator 实现；Coordinator 是一种开放的 Agent 角色权限（见 §2.2），由 Owner 授予用户接入的最强能力 Agent，通过标准 MCP/CLI/API 承担编排工作。
- ✅ **租约冲突仲裁：MVP 只做"升级给人"**；Agent 间自动协商协议推迟到 v0.3 再评估。
- ✅ **商业模式：开源核心 + 付费 Pro**（多工作区、仪表盘、团队协作同步为 Pro 功能），个人版免费/开源优先。
- ✅ **多实例/多机协作：严格单机**，仅列入远期愿景，当前架构不为此预留复杂度。

---

## 9. 成功度量（建议）

- Agent 完成的任务卡片占比（目标：上线 1 个月后个人用户 > 40%）
- 卡片从 Ready → Done 的中位时长（对比纯人工基线）
- 人工审批平均响应时长
- 租约冲突率 / 死锁升级率（衡量协调机制是否够用）
- 7 日留存：创建过 Agent 并让其完成 ≥ 3 张卡片的用户比例
