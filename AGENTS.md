# AGENTS.md — Baton

> 本文件面向 AI 编码 Agent。读者对本项目一无所知，请从这里获取全貌。
> 项目内文档与代码注释以中文为主，本文件同样使用中文。

## 1. 项目概述

**Baton** 是一个 **Agent Native 的本地优先（local-first）看板工具**（MVP 骨架 v0.1）。
看板作为"人机共用的任务中枢"（黑板架构）：AI Agent 与人类一样是看板的一等成员，
可以认领（claim）、执行、交接（handoff）和协调任务；人类通过 GUI 管理、审批与监督。
产品不内置 LLM、不绑定任何模型厂商；Agent 自带智能，通过 MCP / HTTP / CLI 三种
语义一致的接入方式读写看板。

最小闭环：**人建卡 → Agent claim 租约 → 评论/进度 → 乐观锁移列**。

- 产品定义与路线图见 `docs/prd.md`（PRD v0.1，特性编号如 F-201/F-304/F-401 出自该文档）。
- `README.md` 面向第一次打开仓库的访客（是什么/为什么/怎么跑）；功能与验证清单的
  权威参考是本文件（§2 结构、§4 机制、§6 测试策略）。

## 2. 仓库结构

```
contract/        # 开发契约：schema.sql（20 表 + 2 视图）+ types.ts（TS 类型定义）
core/            # Rust Core：数据层 + HTTP API + CLI + MCP server（1 lib + 3 bin）
src-tauri/       # Tauri 2 桌面壳（内嵌 core server + 加载 web/dist）
web/             # GUI 前端（Vite 6 + React 18 + TypeScript 5.6）
docs/prd.md      # 产品需求文档
screenshots/     # 浏览器验证截图
```

三个可构建单元通过 path 依赖关联（**无 Cargo workspace**，`core/` 与 `src-tauri/`
各有独立 `Cargo.lock`）：

| 单元 | 清单文件 | 技术栈 |
|---|---|---|
| `core/` | `core/Cargo.toml` | Rust 2021，rusqlite 0.32 (bundled)、tiny_http 0.12、serde/serde_json、sha2 |
| `src-tauri/` | `src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` | Tauri 2，path 依赖 `baton-core` |
| `web/` | `web/package.json` | React 18 + TS 5.6 + Vite 6，无状态库/无路由库/无 UI 框架 |

### core 的四个产物

`core/Cargo.toml` 定义了 1 个 lib + 3 个 bin：

- **lib `baton_core`**（`core/src/lib.rs`，约 1400 行）：数据层 `Db` + 进程内事件总线
  `EventBus` + 全部业务逻辑（卡片 CRUD、claim 租约、评论、乐观锁移列、列策略引擎、
  审批、links/git/worksite/handoff、Agent 注册与 Token 鉴权、心跳在线状态、幂等键、
  产物上传、强制接管、多项目/看板、项目导出导入、事件日志）。schema 通过
  `include_str!("../../contract/schema.sql")` 内嵌，启动即建库（幂等，
  `CREATE ... IF NOT EXISTS`），首次启动自动播种演示数据。
- **lib 子模块 `server`**（`core/src/server.rs`）：HTTP 路由层 + 鉴权 + 幂等写接线 +
  WebUI 静态文件服务（GET 非 /api 路径 → `web/dist`，SPA fallback），
  `serve(addr, db_path)` 供独立二进制与 Tauri 壳共用。
- **bin `baton-core`**（`core/src/main.rs`）：独立 HTTP server，监听 `127.0.0.1:7700`。
- **bin `baton-mcp`**（`core/src/bin/mcp.rs`）：MCP stdio server（newline-delimited
  JSON-RPC 2.0），**双时代协议**：legacy `2024-11-05`（initialize 握手）与 modern
  `2026-07-28` 无状态核心（`server/discover` 探针、per-request
  `_meta["io.modelcontextprotocol/protocolVersion"]` 校验、不支持回 -32022）。
  进程启动自动 `session_start`（进板），stdin 断开自动 `session_end`（离场）。
  33 个工具（board_get/project_list/card_list/card_get/card_create/card_claim/
  card_release/card_move/card_comment/progress_update/thread_create/link_add/git_attach/git_refresh/
  worksite_add_node/handoff_prepare/handoff_ready/handoff_accept/handoff_cancel/
  agent_heartbeat/artifact_upload/artifact_list/card_dep_add/card_dep_remove/
  notification_list/card_assign/session_start/session_end/session_list/
  approval_list/approval_decide/card_join/card_leave），
  直接内嵌 `Db`，供 Claude Code 等 Agent 接入。
- **bin `baton`**（`core/src/bin/cli.rs`）：CLI（`baton board / projects / project create /
  card list|show|create|claim|release|move|comment|progress|takeover|upload|artifacts|dep|assign /
  agents / agent add|token|revoke / heartbeat / sessions / session end / notifications /
  backup / export / import / approvals / approve|reject / doctor`），
  同样直接内嵌 `Db`，输出 pretty JSON。

**关键架构事实**：HTTP server / CLI / MCP 三个进程各自内嵌 `Db`，可并发共享同一个
SQLite 库文件（WAL 模式）。三者语义一致（乐观锁/租约/列策略规则相同，409 冲突语义一致）。

## 3. 构建与运行

### 开发模式（前端 + core 分离）

```bash
# 终端 1：启动 core（自动建库并播种演示数据）
cd core && cargo run
# 数据库默认在 core/data/baton.db；监听 127.0.0.1:7700

# 终端 2：启动前端（Vite dev，/api 代理到 7700，见 web/vite.config.ts）
cd web && npm install && npm run dev
# 打开 http://localhost:5173
```

### WebUI 模式（单进程，无 Tauri/Vite）

```bash
cd web && npm run build   # 产出 web/dist
cd core && cargo run      # baton-core 同时 serve 静态 UI + /api
# 打开 http://127.0.0.1:7700
```

GET 非 /api 路径时 server 从 WebUI 目录 serve 静态文件（未命中 SPA fallback 到
index.html）。目录解析顺序：`BATON_WEB_DIR` → `./web/dist` → `../web/dist` →
`<可执行文件目录>/web/dist`（取第一个含 index.html 者）；都找不到则纯 API 模式。

### 桌面应用（Tauri）

```bash
cd web && npm run build && cd ..   # 先构建前端
cd src-tauri && cargo run          # 窗口内嵌 core，setup 中 spawn 线程自动启动
```

Tauri 生产环境数据库在系统应用数据目录（macOS: `~/Library/Application Support/dev.baton.app/baton.db`）；
前端经 `API_BASE`（dev 走 Vite 代理为空串；WebUI 模式同源为空串；仅 Tauri webview
直连 `http://127.0.0.1:7700`，见 `web/src/api.ts`）访问内嵌 core；CSP `connect-src`
仅放行该地址。

### 构建检查

```bash
cd core && cargo build            # 编译 lib + 3 个 bin
cd web && npm run build           # tsc --noEmit + vite build（含类型检查）
cd src-tauri && cargo build       # 首次编译 Tauri 全量依赖较久
```

### CI / Release

`.github/workflows/release.yml`：发布 GitHub Release 时触发，macOS 双 target
（aarch64 + x86_64）构建 Tauri 应用并把 .dmg/.app.tar.gz 附加到该 Release。
web/dist 在 workflow 里显式构建（`npm ci && npm run build`，绕过 beforeBuildCommand
的工作目录坑），tauri-action 以 `--config '{"build":{"beforeBuildCommand":"true"}}'` 跳过。
**未签名/未公证**：用户首次打开需右键 → 打开。

### 环境变量

| 变量 | 作用 | 默认值 |
|---|---|---|
| `BATON_DB` | 数据库路径（core server / CLI / MCP 共用） | `data/baton.db` |
| `BATON_ADDR` | HTTP 监听地址（仅 `baton-core` bin） | `127.0.0.1:7700` |
| `BATON_WEB_DIR` | WebUI 静态目录（WebUI 模式） | 自动探测 `web/dist` |
| `BATON_AGENT_SELF_REGISTER` | 是否允许 Agent 自注册（`0`/`false` 关闭） | 开（本机信任模型） |
| `BATON_AGENT_ID` | MCP 进程扮演的成员 id | `a-code` |

### MCP 注册示例

```bash
claude mcp add baton -- /path/to/baton/core/target/debug/baton-mcp
```

## 4. 代码组织与核心机制

### HTTP 路由（`core/src/server.rs`）

tiny_http，每请求一个线程（长轮询挂起不阻塞其他请求），`Arc<Mutex<Db>>` 共享数据层。
路由清单见该文件头部注释，包括：`/api/v1/board?board_id=`、`/api/v1/projects`（多项目/
看板/模板）、`/api/v1/events?since=`（长轮询，挂起最多 25s；tiny_http 的 respond 在
body EOF 后才 flush，不支持 SSE 无限流，长轮询为等效替代）、`/api/v1/cards/{id}`、卡片
claim/release/takeover/assign/comments/progress/move/artifacts/deps、审批 `approvals`、
`links`、`git attach/refresh`、`worksite/nodes`、`handoff/{prepare|ready|accept|cancel}`、
`/api/v1/agents`（注册/Token 轮换/吊销/心跳）、`/api/v1/sessions`（进板/心跳续租/离场/
资源视图）、`/api/v1/members`、`/api/v1/notifications`（通知中心，从 events 派生）。

### 关键不变量（改动时不要破坏）

- **乐观锁**：卡片 `rev` 字段；移列必须携带当前 `rev`，不匹配返回 409 `rev conflict`
  （响应体附 `current_rev`）。
- **租约**：claim 写入 30 分钟租约（`claims` 表，一卡一条，主驾）；有活跃租约时
  持有者或在场协同者可移动卡片（rev 乐观锁兜底并发），否则 409
  `card is claimed by another member`；重复 claim → 409 `card already claimed`。
  release 仅持有者本人或人类（协调者）可操作（他人 403、无活跃租约 409）；
  takeover（收回租约）仅人类，强制释放并留系统评论审计痕迹。
- **协同参与（多 Agent 同卡协作）**：`card_participants` 表记录在场协同者（副驾）。
  `join`/`leave` 显式到场离场；协同者可评论/汇报/传产物/移列，但主责（租约）不变。
  区别于移交（handoff，串行交接）与子任务（parent_id，分解）——协同是并行共做。
- **Token 鉴权（F-212）**：HTTP 写操作中 actor 为**已签发 Token** 的 Agent 时必须携带
  `X-Baton-Token`（sha256 比对 `agent_json.token_hash`），不匹配 401；已吊销成员一律 403；
  Agent 管理/建项目/强制接管等操作仅人类（`require_human`）。未签发 Token 的 Agent
  （如种子演示数据）与人类成员不校验 —— 本地单机信任模型。
- **幂等写（F-307）**：HTTP 写请求携带 `Idempotency-Key` 头时，同 key 同请求体（sha256
  of method+path+body）重放直接返回首个响应，同 key 不同体 409；记录存
  `idempotency_keys` 表。
- **列策略引擎**（`lists.policy_json`，在 `Db::move_card` 内执行）：
  - `require_progress_summary`：无进度摘要（`ext.progress.summary`）的卡片禁止进入该列（400，
    **只约束 Agent**——进度摘要是 Agent 的行为规范；人是协调者，移列豁免）。
  - `require_approval: "human"`：非人类 Owner 移入时自动生成审批单而不移动（返回
    `{"approval_pending": <id>}`）；审批批准后强制移列（`do_move(rev=None)`，
    跳过乐观锁与租约校验）。
  - `require_approval: "peer"`：**同伴验收（职责分离）**——任何人移入都生成审批单，
    不直通；裁决人不能是申请者自己（403），Agent 同伴也可裁决。
    典型场景：执行 Agent 完成任务后，由另一个 Agent/人验收。
  - 审批裁决规则（`decide_approval`）：申请者一律不能自审（403）；
    `human` 模式的审批单仅人类可裁决，`peer` 模式任何其他成员均可。
- **事件日志**：所有状态变更通过 `Db::log_event` 写 `events` 表（append-only，
  `seq` 自增）并广播到 `EventBus`（进程内历史容量 500），驱动长轮询实时刷新。
- **移交状态机**：`none → preparing → ready →(accept)→ none`，任意非 none 状态可
  cancel 回 `none`；非法迁移返回 409。`ready` 时移交方自动释放租约，`accept` 时
  接手方自动 claim。
- **统一时间线**：claim、审批、接管、产物上传、指派、依赖解除等动作通过 `sys_comment`
  写入 `kind='system'` 评论；进度更新写 `kind='progress'` 评论，与聊天共用一条历史。
  评论支持 `reply_to` 直接回复（自动落到父评论所在话题；目标不存在或跨卡 400），
  HTTP/CLI(`--reply-to`)/MCP(`reply_to`) 三端语义一致。评论归属话题的优先级：
  `reply_to` > `thread_id`（须属于本卡，否则 400）> 卡片第一个 thread（主讨论）；
  话题通过 `POST /cards/{id}/threads`（CLI 无对应子命令）/ MCP `thread_create` 创建。
- **依赖门禁（F-106/305）**：列策略 `is_done: true` 标记完成列；进入完成列时
  `blocked_by` 依赖必须全部已在完成列，否则 409（附 blocking 列表）；**子任务（F-107
  `cards.parent_id`）也必须全部完成**，否则 409（附 unfinished_children 列表）；
  卡片进入完成列后
  `notify_dependents` 给依赖全部满足的下游卡写系统评论 + `dep_resolved` 事件。
- **抢单原子性（F-303）**：`claim_card` 是单条原子 SQL
  （`INSERT ... ON CONFLICT(card_id) DO UPDATE ... WHERE 旧租约已过期`：无行插入、
  过期行替换、活跃行不命中返回 409），不要改回"先查后插"（多进程并发会双占）。
- **通知中心（F-404）**：不建表，从 `events` 派生（审批请求/@提及/依赖解除/接管/移交）；
  @提及在 `add_comment` 时按成员名/id 解析写入 `mentions_json`；已读游标在客户端
  （localStorage `baton.last_read_seq`）。
- **速率限制（F-308）**：仅 HTTP 层对 Agent 写请求生效（`RateLimiter` 进程内滑动窗口，
  默认 60 次/分钟，可由 `agent_json.rate_limit_per_min` 覆盖）；人类/读请求/CLI/MCP 不限。
- **Session 模型（Agent 资源管理）**：Agent Profile（`members`）是编制，Session
  （`sessions` 表）是出勤——任务分配与归属的真实对象。进板 `session_start` 声明
  scope/工作现场（cwd/git 自动探测）并返回简报（在手卡/待接手移交/@提及）；
  心跳续命并**自动续期本 session 持有的租约**；180s 无心跳展示为 stale（计算状态，
  不落库）；claim 记录 session_id，时间线署名到会话。与 MCP 协议层的"会话"无关：
  2026-07-28 无状态核心下，session_id 是业务层显式标识，由调用方逐请求携带
  （MCP stdio 下由进程级自动 session 代劳）。
- **产物存储（F-108）**：文件本体在 `<工作区目录>/artifacts/<card_id>/<id>-<name>`
  （工作区目录 = 库文件所在目录），元数据进 `artifacts` 表（含 sha256）。
- **导出格式（F-503）**：`baton-export/v1` —— project.json 全量表 dump（不含易变的
  claims），导入按原 id `INSERT OR REPLACE` 幂等落库，生成列经 `PRAGMA table_xinfo`
  自动剔除。

### 前端（`web/src/`）

- `App.tsx`：单文件应用，左侧扁平看板导航（一行一看板：看板名 + 项目名，悬浮出现
  项目行内重命名/两段式删除；新建项目为低频动作，默认收起到虚线入口）、
  模板选择（F-101/112）、
  四列看板 + 卡片拖拽（列策略预告：拒绝/进审批）、审批中心（待办 + 最近已处理）、通知中心（F-404 未读角标）、Agent 管理面板（F-211
  注册/轮换/吊销/会话）+ 在场汇总芯片（F-213，点击开面板）、
  接入指引独立面板（`InstallPanel`：一键复制 Claude Code 命令/通用 MCP 配置/给 Agent 的自包含安装指令，
  路径由 `GET /api/v1/install-info` 探测；入口在侧栏底部「⇄ 接入 Agent」+
  无 Agent 在岗时的看板引导横幅，onboarding 与管理分离）、卡片抽屉六 Tab（讨论/需求/Git/现场/移交/产物）+
  讨论区话题索引（chip 点击滚动定位）+ 新建话题 + 评论树（`reply_to` 嵌套渲染）+
  依赖展示与管理（F-106，添加依赖为标题搜索选择器，无需记卡片 id）+ 指派下拉/可抢标识（F-105/303）+ 收回租约按钮（F-405，
  协调者动作；claim/进度上报等 Agent 自主行为不在 GUI 出现）+
  长轮询实时刷新（100ms 防抖合并密集事件；长轮询在首次数据加载完成后才启动，
  避免与首屏请求争抢浏览器同域连接）。UI 文案为中文。
- `api.ts`：全部 TS 接口类型 + API 客户端，**与 `contract/types.ts` 对齐**；
  `API_BASE` 区分三种场景：dev 走 Vite 代理（空串）、WebUI 模式同源（空串）、
  Tauri webview 直连 7700。`MEMBER_NAMES`/`LIST_NAMES`
  硬编码演示数据。
- `styles.css`：设计系统（CSS 变量 tokens + `.btn` 按钮体系），视觉方向「调度室」，
  **色值/字体/组件纪律以 `DESIGN.md` 为准**。约定：**可交互元素
  必须有按钮/链接样式（`.btn`/`.btn-link`/focus ring），纯信息用 `.mini-badge`/
  `.tag`/`.muted`，二者不得混用**；人机分色：琥珀（--human）=人/主交互，
  青（--agent）=Agent 在场；success=正常，warn=注意，danger=危险操作。
  全局 `button` 是无样式 reset，新按钮必须显式加类。

### 契约（`contract/`）

- `schema.sql` 是**数据库结构的唯一来源**，core 启动时内嵌执行；改表结构只改这里
  （保持幂等，`CREATE ... IF NOT EXISTS`；需要 SQLite ≥ 3.31，用到生成列）。
- `types.ts` 与 `schema.sql` 一一对应，是前后端字段命名的对齐基准。
- 修改任一文件后需同步检查：`core/src/lib.rs` 的 SQL/JSON 组装、`web/src/api.ts`
  的接口类型。

### 演示数据（seed）

首次启动播种：人类 Owner `u-owner`（Lance）、Agent `a-code`、`a-review`；
演示项目 `p-demo` / 看板 `b-main` / 四列 `l-ready`、`l-doing`（require_progress_summary）、
`l-review`（require_approval=human）、`l-done`。

## 5. 代码风格约定

- **注释与文档语言：中文**（项目内代码注释、PRD、README 均为中文，沿用之）。
- Rust：零框架主义 —— 不用 async/tokio，不用 Web 框架；HTTP 用 tiny_http + 每请求
  一线程，DB 用 `Arc<Mutex<Db>>` 包裹。业务错误用 `ApiErr { status, body }`
  （409/400/500）。
- 文件头部有 `//!` 模块注释说明范围与路由/用法，修改模块时同步更新头部注释。
- id 生成：`new_id(prefix)` = 时间戳(hex) + 进程内自增(hex)，骨架阶段替代 ULID。
- 时间戳统一用 SQLite `strftime('%Y-%m-%dT%H:%M:%fZ','now')`（ISO 8601 UTC）。
- 前端：单文件大组件风格，无路由/无状态库；类型集中在 `api.ts`。
- JSON 字段（`ext_json`、`policy_json`、`package_json`）用 `serde_json::Value` 操作，
  常用 `json_extract`/`json_set` 做局部更新；`ext` 带 `schema_rev` 向前兼容。
- 卡片高频查询字段（`progress_percent`、`handoff_state`）是从 `ext_json` 提取的
  SQLite 生成列（VIRTUAL），改 ext 结构时注意保持一致。

## 6. 测试策略

**目前没有自动化测试**（无 `cargo test` 用例、无前端测试、无 CI 配置）。
验证方式是手工冒烟 + 浏览器端到端（无头浏览器驱动 WebUI）+ MCP 会话。

已验证的场景清单（从旧 README"已验证"一节迁移，作为回归参照）：

- 租约：重复 claim 409；rev 冲突 409（附 current_rev）；非持有者/非协同者移列 409；
  租约过期后可重新认领（原子 upsert）；release 权限（持有者/人类可，他人 403）；
  takeover 人类强制释放留系统评论；5 进程并发抢同一卡 = 1 成功 4 × 409
- 列策略：无进度摘要进 In Progress 400（Agent 受限/人类豁免）；Review 列自动转审批单、
  批准后强制移列；peer 验收（申请者自审 403、同伴 Agent 可裁决）；Done 列依赖门禁
  409（附 blocking）+ 子任务未完成 409（附 unfinished_children）+ 依赖解除通知下游
- 讨论区：reply_to 跨卡/不存在 400；thread_id 跨卡 400；回复归父评论所在话题；
  IME 组合中 Enter 不误发
- 协同参与：join 后副驾可移列/评论/汇报；leave 后移列 409；重复 leave 409
- handoff 全状态机（prepare→ready 释放租约→accept 自动 claim；非法迁移 409）
- 长轮询推送、断连恢复补 refresh、首屏不被长轮询阻塞
- Token 鉴权：已签发 Agent 无/错 Token 401、吊销 403、人类专属操作 403；
  自注册默认放开（BATON_AGENT_SELF_REGISTER=0 时 403）
- 幂等写：同 key 重放返回首个响应（Idempotency-Replayed）、异体 409；速率限制 429
- 产物上传（content/path）+ sha256 + 内联预览；git_refresh 真实探测；导出/导入幂等；
  备份 keep 清理；MCP 双时代握手；三方（MCP/CLI/HTTP）并发共享同一库
- GUI 全流程：建卡即开抽屉、拖拽预告、审批中心历史、扁平侧栏项目管理、接入指引复制

改动后请至少：

1. `cd core && cargo build` 编译通过；
2. `cd web && npm run build`（含 `tsc --noEmit`）类型检查通过；
3. 涉及核心逻辑时，按上面相应场景手工冒烟验证（可用 CLI `baton doctor`、
   `baton card ...` 快速复现）。

## 7. 安全与边界

- **纯本地运行**：core 只绑定 `127.0.0.1:7700`；CORS 响应头为 `*`（本地工具约定，
  不要引入到对外服务）。
- **Token 鉴权（F-212，已实现）**：已签发 Token 的 Agent 写操作必须携带
  `X-Baton-Token`（sha256 存 `members.agent_json.token_hash`，明文仅在签发/轮换时
  返回一次）；吊销即 403。未签发 Token 的 Agent 与人类成员仍走本地信任模型
  （actor 自报）；若未来暴露网络，需把 Token 校验扩展为全员强制。
  **Agent 自注册**：`POST /api/v1/agents` 是自举入口（HTTP 层跳过 auth_check），
  默认允许 Agent 自注册并当场拿到 Token（本机信任模型）；
  `BATON_AGENT_SELF_REGISTER=0` 关闭后退回仅人类可注册。
- Token 随机源是 `/dev/urandom`：**必须 `File::open + read_exact(16)`，严禁
  `std::fs::read("/dev/urandom")`**（会读到 OOM 被系统 SIGKILL —— 踩过的坑）。
- `git_refresh` 会以 core 进程权限在本机执行 `git -C <path>`（路径来自 API 请求）——
  本地信任模型下可接受；改动时注意不要引入 shell 注入（当前用 `Command` 数组传参，
  无 shell）。
- Tauri CSP：`default-src 'self'; connect-src 'self' http://127.0.0.1:7700`，
  新增网络访问需同步放宽 CSP。
- 本地数据（`data/`、`core/data/`、`*.db`、`*.db-wal`、`*.db-shm`）已在 `.gitignore`，
  不要提交数据库文件。

## 8. 常见坑

- **改 `contract/schema.sql` 后对已有库不生效**：schema 幂等建表但不迁移旧表。
  **例外**：`Db::open` 里的 `migrate()` 做轻量列迁移（如 `claims.session_id`，
  通过 `PRAGMA table_info` 判存在后 `ALTER TABLE ADD COLUMN`）——给已有表加列时
  在这里登记；改列/改约束仍需删库重建（开发期删除 `core/data/baton.db` 即可）。
- **长轮询不是 SSE**：tiny_http 的 respond 在 body EOF 后才 flush，不要尝试改成 SSE
  无限流。
- **Tauri 首次 `cargo build` 很慢**（全量依赖）；骨架阶段可只用 `cd core && cargo run`
  + Vite 开发。注意没有 Cargo workspace，`cargo run -p baton-core` 在仓库根目录不可用。
- **CLI 默认 actor 是 `a-code`（Agent）**：用它移动 `l-review` 会触发审批而不是直接
  移动，属预期行为；`baton approve/reject` 则硬编码以人类 Owner `u-owner` 身份执行。
- **`EventBus` 是进程内的**：CLI/MCP 进程写库产生的事件不会推送到 HTTP server 进程的
  长轮询客户端（各自有独立的 bus）；跨进程感知依赖下次轮询/刷新。
- **`tauri build` 的 `beforeBuildCommand`**（`npm --prefix ../web run build`）在
  `npm exec --package=@tauri-apps/cli` 下工作目录解析有坑（会找到仓库外）；先手动
  `cd web && npm run build`，再用
  `tauri build --config '{"build":{"beforeBuildCommand":"true"}}'` 跳过。
  tauri CLI 无需全局安装：`npm exec --yes --package=@tauri-apps/cli -- tauri build`。
- **导出导入与生成列**：`cards.progress_percent/handoff_state` 是 SQLite 生成列，
  `SELECT *` 导出后不能直接 INSERT；`import_project` 已用 `PRAGMA table_xinfo`
  （hidden>0）自动剔除，改 schema 加生成列时无需改导入逻辑。

## 9. 设计系统

做视觉或 UI 决策前先读 `DESIGN.md`。字体、色彩、间距、组件纪律（可交互 vs 信息）
以该文件为准；偏离需用户明确同意。核心约定：琥珀=人/主交互、青=Agent、
语义色只表达状态；可交互元素必须带 `.btn*` 类，纯信息用 badge/muted。
