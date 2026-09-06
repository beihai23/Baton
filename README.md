# Baton — Agent Native 本地看板（MVP 骨架 v0.1）

> 对齐 `../agent-native-kanban-prd.md`。本骨架实现最小闭环：
> **人建卡 → Agent claim 租约 → 评论/进度 → 乐观锁移列**。

## 目录结构

```
baton/
├── contract/        # 开发契约：schema.sql + types.ts（core 启动时内嵌建库）
├── core/            # Rust Core Server，HTTP API on 127.0.0.1:7700
├── src-tauri/       # Tauri 壳配置（包装 core + web；见下方说明）
├── web/             # GUI 前端（Vite + React + TS，dev 代理 /api → 7700）
└── screenshots/     # 浏览器验证截图
```

## 快速开始

```bash
# 1. 启动 core（会自动建库并播种演示项目）
cd core && cargo run

# 2. 另开终端，启动前端
cd web && npm install && npm run dev
# 打开 http://localhost:5173
```

### WebUI 模式（单进程，无 Tauri/Vite）

`baton-core` 可直接 serve 构建后的前端（GET 非 /api 路径 → 静态文件，SPA fallback）：

```bash
cd web && npm run build   # 产出 web/dist
cd core && cargo run      # 一个进程同时提供 UI + API
# 打开 http://127.0.0.1:7700
```

静态目录解析顺序：`BATON_WEB_DIR` → `./web/dist` → `../web/dist` → `<可执行文件目录>/web/dist`；
都找不到则退化为纯 API 模式。

## 已实现

- **数据层**：`contract/schema.sql` 全量建库（20 表 + 2 视图），WAL、外键、生成列、幂等建表
- **API**（`core/src/main.rs`，多线程，每请求一线程）：
  - `GET  /api/v1/board?board_id=` 整板状态（列 + 卡 + 租约快照 + 未结话题数；F-101 多看板）
  - `GET/POST /api/v1/projects`、`POST /api/v1/projects/{id}/boards` 多项目/看板（F-101）
  - `GET  /api/v1/events?since=<seq>` **长轮询事件流**（F-401 实时推送；新事件立即返回，否则挂起 25s）
  - `POST /api/v1/cards` 建卡（自动创建"主讨论"话题）
  - `POST /api/v1/cards/{id}/claim | /release` 认领（30min 租约）/ 释放
  - `POST /api/v1/cards/{id}/takeover` **强制接管**（F-405，仅人类：强制释放 Agent 租约）
  - `POST /api/v1/cards/{id}/comments` 评论（kind: chat/progress/system/...）
  - `POST /api/v1/cards/{id}/progress` 进度更新（写 ext.progress + progress 评论）
  - `POST /api/v1/cards/{id}/move` 移列（**乐观锁 + 租约校验 + 列策略引擎**）
  - `GET/POST /api/v1/approvals…` **审批中心**（列策略 require_approval 触发，批准后强制移列）
  - `POST /api/v1/cards/{id}/links` 需求来源/文档链接（Jira/MeeGo/url/本地文件）
  - `POST /api/v1/cards/{id}/git/attach | /refresh` git 声明 + **真实探测**（staged/unstaged/ahead/behind/last_commit 快照）
  - `POST /api/v1/cards/{id}/worksite/nodes` 工作现场节点（main + worktree 拓扑）
  - `POST /api/v1/cards/{id}/handoff/{prepare|ready|accept|cancel}` **移交状态机**（含移交包、时间线、租约交接）
  - `POST /api/v1/cards/{id}/artifacts`、`GET /api/v1/artifacts/{id}` **产物上传/查看**（F-108，文件存 `artifacts/<card_id>/`，文本 ≤256KB 内联预览）
  - `POST /api/v1/cards/{id}/deps[/remove]` **卡片依赖**（F-106/305：blocked_by 未完成禁入 Done 列；依赖完成自动通知下游）
  - `POST /api/v1/cards/{id}/assign` 指派 / 放入抢单池（F-105/303；claim 为原子 SQL，并发抢单先到先得）
  - `GET  /api/v1/notifications?member=` **通知中心**（F-404：审批请求/@提及/依赖解除/接管/移交，从事件日志派生）
  - `GET  /api/v1/members` 成员列表（指派用）
  - `GET/POST /api/v1/agents`、`POST /api/v1/agents/{id}/{token|revoke|heartbeat}` **Agent 注册/Token 轮换/吊销/心跳**（F-211~213）
- **速率限制（F-308）**：Agent 的 HTTP 写请求按 `agent_json.rate_limit_per_min`（默认 60 次/分钟）
  进程内滑动窗口限流，超限 429；人类与读请求不限；CLI/MCP 直连 Db 属本地信任模型不受限。
- **Agent Token 鉴权（F-212）**：HTTP 写操作中 actor 为已签发 Token 的 Agent 时，必须携带
  `X-Baton-Token`（或 `Authorization: Bearer`），不匹配 401、已吊销 403、Agent 冒充人类管理操作 403；
  未签发 Token 的 Agent 与人类成员不校验（本地单机信任模型，演示数据即如此）。
- **幂等写（F-307）**：写请求携带 `Idempotency-Key` 头；同 key 同请求体重放返回首个响应
  （带 `Idempotency-Replayed: true`），同 key 不同请求体 → 409。
- **CLI**（`core/src/bin/cli.rs`，F-203）：`baton board / projects / project create [--template] /
  card list|show|create|claim|release|move|comment|progress|takeover|upload|artifacts|dep|assign /
  agents / agent add|token|revoke / heartbeat / notifications / backup / export / import /
  approvals / approve|reject / doctor`
- **快照备份（F-504）**：`baton backup [--keep N]` —— `VACUUM INTO` 在线快照到
  `<工作区>/backups/`，保留最近 N 份（默认 10）。
- **看板模板（F-112）**：建项目/看板可选模板：`software`（默认四列）/ `content`
  （选题→写作中→待审核→已发布）/ `gtd`（Inbox→Next→Doing→Done）。
- **导出/导入（F-503）**：`baton export --out <dir>` 生成 `project.json`（全量事实源）+
  `cards/*.md`（人读）+ `artifacts/`（附件复制）；`baton import <dir>` 幂等落库（INSERT OR REPLACE）。
- **GUI**：左侧多项目/看板导航 + 模板选择（F-101/112）+ 四列看板、卡片拖拽移动、
  审批中心、**通知中心**（F-404，未读角标 + 点击跳卡）、**Agent 管理面板**（F-211：
  注册/轮换 Token/吊销，Token 一次性展示）、Agent 在线徽标（F-213）、卡片抽屉六 Tab
  （讨论/需求/Git/现场/移交/产物）+ 依赖展示与管理（F-106）+ 指派下拉/可抢标识（F-105/303）+
  接管按钮、长轮询实时刷新

## 列策略引擎（F-304，已生效）

- `require_progress_summary`：无进度摘要的卡片禁止进入该列（400）
- `require_approval: "human"`：非人类 Owner 申请进入时自动生成审批单（不移动），
  Owner 批准后授权强制移列；审批记录进统一时间线

## Agent Session（会话实例）—— 任务分配的真实对象

看板里的 Agent 是"编制"（members 表：持久身份 + Token + 能力标签），而实际干活的是
**某工具的一次对话/进程**——即用即焚。因此资源分配的对象是 Session：

- **进板**：`POST /api/v1/sessions`（或 `baton-mcp` 进程启动时自动）声明 scope
  （project/board）与工作现场——cwd 所在的 git 仓库/分支自动探测，零配置。
  返回**进板简报**：本 profile 的在手卡片、可接手的移交、未读 @提及。
- **在线与租约**：`POST /api/v1/sessions/{id}/heartbeat` 续命，并**自动续期本 session
  持有的全部租约**；180s 无心跳 → 面板标记 stale；`session_end` 显式离场
  （MCP 进程退出自动触发）。租约不强制回收，进入自然到期，可被人类接管。
- **归属**：claim 记录 session_id；时间线评论带 session 标识，谁干的可追溯。
- **resume 链**：`parent_session_id` 记录"这个会话接了谁的班"。
- **GUI**：Agent 管理面板按 profile 分组展示各 session 的状态/工作现场/持卡数。

## MCP Server（Agent 接入，`core/src/bin/mcp.rs`）

`baton-mcp` 是双时代 MCP stdio server，同时支持：

- **legacy `2024-11-05`**：客户端发 `initialize` 即按旧握手语义应答（向后兼容）；
- **modern `2026-07-28`（无状态核心）**：无握手要求，每个请求可携带
  `_meta["io.modelcontextprotocol/protocolVersion"]`；不支持的版本回 `-32022` 并附
  `supported` 列表；提供 `server/discover` 探针（返回 supportedVersions + 进板简报）。

它直接内嵌 Db（SQLite WAL，可与运行中的 core server 并发读写同一库）。
进程启动自动 `session_start`（cwd/git 自动探测），stdin 断开自动 `session_end`。

```bash
# 注册到 Claude Code
claude mcp add baton -- /path/to/baton/core/target/debug/baton-mcp

# 环境变量
BATON_DB=data/baton.db     # 数据库路径（默认同上）
BATON_AGENT_ID=a-code      # 本进程扮演的 Agent 成员 id（默认 a-code）
```

工具面（28 个）：`board_get` `project_list` `card_list` `card_get` `card_create`
`card_claim` `card_release` `card_move` `card_comment` `progress_update` `link_add`
`git_attach` `git_refresh` `worksite_add_node` `handoff_prepare` `handoff_ready`
`handoff_accept` `handoff_cancel` `agent_heartbeat` `artifact_upload` `artifact_list`
`card_dep_add` `card_dep_remove` `notification_list` `card_assign`
`session_start` `session_end` `session_list`
—— 与 HTTP API 同语义，乐观锁/租约/列策略规则一致（409 以 `isError` 返回）。

## 已验证（冒烟 + 浏览器全流程 + MCP 会话）

- 重复 claim → 409 `card already claimed`
- 错误 rev 移列 → 409 `rev conflict`（返回 current_rev）
- 非持有者移列 → 409 `card is claimed by another member`
- 无进度摘要进 In Progress → 400 列策略拒绝；补进度后 200
- Agent 进 Review → 自动转审批单（不移动）；Owner 批准后强制移列 ✅
- 进度更新 → ext.progress 落库 + `kind=progress` 评论进统一时间线
- 长轮询推送：页面无刷新实时出现新卡片（浏览器实测 1→2）
- git_refresh 真实探测本机仓库（staged/unstaged/untracked/clean/ahead + 快照时间）
- handoff 全状态机：prepare → ready（释放租约）→ accept（接手方自动 claim，时间线留痕）；
  非法迁移 409
- 浏览器端到端：建卡 → 认领 → 进度 → 移列 → 评论 → 审批，无 console 报错
- MCP 会话：initialize / tools/list / 工具调用全部正常
- **MCP / CLI / HTTP core 三方并发共享同一库**（SQLite WAL），状态互见
- 已存在的库二次启动正常（schema 幂等，`CREATE ... IF NOT EXISTS`）
- **Token 鉴权（F-212）**：已签发 Token 的 Agent 无/错 Token → 401；吊销后 → 403；
  Agent 调用人类专属管理接口（注册/轮换/吊销/接管/建项目）→ 403；
  未签发 Token 的 Agent 与人类成员不校验（本地单机信任模型）
- **Agent 面板（F-213）**：心跳后 `online: true`（120s 窗口），`holding_cards` 实时反映租约，
  GUI 顶栏显示在线徽标
- **幂等写（F-307）**：同 Idempotency-Key 重放返回首个响应（不重复建卡，带
  `Idempotency-Replayed: true`）；同 key 不同请求体 → 409
- **产物（F-108）**：文本 content / 本机文件 path 两种方式上传，落盘 `artifacts/<card_id>/`，
  sha256 落库，`GET /artifacts/{id}` 内联文本预览，上传动作写系统评论
- **强制接管（F-405）**：人类 takeover 强制释放 Agent 租约（系统评论留痕）；无租约时 409；
  Agent 尝试 → 403
- **多项目（F-101）**：新建项目自动带默认看板 + 四列策略；看板间卡片隔离；
  GUI 侧栏切换/新建项目已浏览器验证（见 screenshots/baton-v3-multiproject.png）
- **导出/导入（F-503）**：export 生成 project.json + cards/*.md + artifacts/；
  import 到新库数据完整（评论/产物保留）、幂等可重复导入
- **依赖（F-106/305）**：blocked_by 未完成进 Done → 409（附 blocking 列表）；上游进 Done 后
  下游收到"解除阻塞"系统评论 + `dep_resolved` 事件；自依赖 400
- **抢单池（F-303）**：claim 为单条原子 SQL（`INSERT ... WHERE NOT EXISTS 活跃租约`），
  5 进程并发抢同一卡 = 1 成功 4 × 409；指派/回池 GUI 与 API 均通
- **通知中心（F-404）**：@提及（按成员名/id 解析）、审批请求、依赖解除等派生通知；
  自己产生的事件不通知自己；GUI 未读角标 + 点击跳卡已浏览器验证
- **Agent 管理 GUI（F-211）**：面板注册/轮换/吊销，Token 一次性展示框已浏览器验证
- **快照备份（F-504）**：连续 3 次备份 keep=2 正确清理；快照可直接 `baton doctor` 打开
- **看板模板（F-112）**：content 模板建出的列与策略（待审核需审批、已发布为完成列）已验证
- **速率限制（F-308）**：限额 3 次/分钟时第 4/5 次写 → 429；人类写请求不受限
- **Session（资源模型）**：HTTP 进板自动探测 git 仓库/分支并返回简报；claim 绑 session
  （他人 session 冒领 400）；心跳续租约（leases_renewed=1）；离场后心跳 409；
  旧库自动迁移补 `claims.session_id` 列
- **MCP 双时代**：legacy initialize → 2024-11-05 + `_meta.baton`（session + 简报）；
  modern 无握手 `server/discover` + 带 `_meta` 版本的 tools/call 直通；
  不支持版本 → -32022 附支持列表；进程退出自动 session_end
- **GUI 资源视图**：Agent 面板按 profile 分组展示 session（状态/分支/仓库/持卡），
  浏览器验证无报错（screenshots/baton-v4-sessions.png）

## Tauri 桌面应用

Tauri 壳已构建并验证通过：`src-tauri/src/main.rs` 在 `setup` 里 spawn 线程内嵌
`baton_core::server::serve("127.0.0.1:7700", db)`，数据库落在系统应用数据目录
（macOS: `~/Library/Application Support/dev.baton.app/baton.db`）。窗口加载
`web/dist` 生产包，前端经 `API_BASE`（生产环境为 `http://127.0.0.1:7700`）直连内嵌 core；
CSP `connect-src` 仅放行该地址。

```bash
# 开发调试
cd web && npm run build && cd ..
cd src-tauri && cargo run        # 打开桌面窗口，内嵌 core 自动启动

# 打安装包（web/dist 需先构建；tauri CLI 走 npm exec，无需全局安装）
cd src-tauri && npm exec --yes --package=@tauri-apps/cli -- tauri build
# 产物：src-tauri/target/release/bundle/macos/Baton.app
#       src-tauri/target/release/bundle/dmg/Baton_<version>_aarch64.dmg
```

已验证：窗口进程启动后 core 正常监听，`/api/v1/board`、建卡、`/api/v1/events`
长轮询均正常响应；`tauri build` 打出的 Baton.app 实测启动后内嵌 core 正常应答。

注意：`beforeBuildCommand`（`npm --prefix ../web run build`）在部分 npm exec 环境下
工作目录解析有问题，如遇 ENOENT 可先手动 `cd web && npm run build`，再用
`--config '{"build":{"beforeBuildCommand":"true"}}'` 跳过。

## 下一步（按 PRD 路线图）

1. ~~MCP Server 接入层（F-201）~~ ✅ `baton-mcp`（25 工具）
2. ~~实时推送（F-401）~~ ✅ 长轮询事件流（tiny_http 阻塞式 respond 不支持 SSE 无限流，长轮询等效且更稳）
3. ~~列策略执行引擎（F-304）~~ ✅ require_progress_summary + require_approval + is_done 已生效
4. ~~worktree / handoff / links 的 API 与 GUI Tab~~ ✅ 六 Tab 抽屉已上线
5. ~~CLI（F-203）~~ ✅ `baton` 命令
6. ~~Tauri 桌面壳 + 安装包~~ ✅ 已构建并实测（Baton.app / dmg）
7. ~~Agent Token/心跳/在线面板/注册管理 GUI（F-211~213）~~ ✅ 已生效
8. ~~幂等写（F-307）~~ ✅ Idempotency-Key
9. ~~产物上传（F-108）/ 强制接管（F-405）/ 多项目 UI（F-101）/ 导出导入（F-503）~~ ✅
10. ~~依赖编排（F-106/305）/ 通知中心（F-404）/ 抢单池（F-303）/ 快照备份（F-504）/
    看板模板（F-112）/ 速率限制（F-308）~~ ✅
11. 待做（v0.3+）：多工作区、仪表盘统计（F-406）、命令面板（F-407）、活动流页面（F-111）、
    全局搜索（F-110）、卡片标签/截止日期 UI（F-103）、插件系统、Jira/MeeGo 双向同步
