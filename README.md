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

## 已实现

- **数据层**：`contract/schema.sql` 全量建库（20 表 + 2 视图），WAL、外键、生成列、幂等建表
- **API**（`core/src/main.rs`，多线程，每请求一线程）：
  - `GET  /api/v1/board` 整板状态（列 + 卡 + 租约快照 + 未结话题数）
  - `GET  /api/v1/events?since=<seq>` **长轮询事件流**（F-401 实时推送；新事件立即返回，否则挂起 25s）
  - `POST /api/v1/cards` 建卡（自动创建"主讨论"话题）
  - `POST /api/v1/cards/{id}/claim | /release` 认领（30min 租约）/ 释放
  - `POST /api/v1/cards/{id}/comments` 评论（kind: chat/progress/system/...）
  - `POST /api/v1/cards/{id}/progress` 进度更新（写 ext.progress + progress 评论）
  - `POST /api/v1/cards/{id}/move` 移列（**乐观锁 + 租约校验 + 列策略引擎**）
  - `GET/POST /api/v1/approvals…` **审批中心**（列策略 require_approval 触发，批准后强制移列）
  - `POST /api/v1/cards/{id}/links` 需求来源/文档链接（Jira/MeeGo/url/本地文件）
  - `POST /api/v1/cards/{id}/git/attach | /refresh` git 声明 + **真实探测**（staged/unstaged/ahead/behind/last_commit 快照）
  - `POST /api/v1/cards/{id}/worksite/nodes` 工作现场节点（main + worktree 拓扑）
  - `POST /api/v1/cards/{id}/handoff/{prepare|ready|accept|cancel}` **移交状态机**（含移交包、时间线、租约交接）
- **CLI**（`core/src/bin/cli.rs`，F-203）：`baton board / card list|show|create|claim|release|move|comment|progress / approvals / approve|reject / doctor`
- **GUI**：四列看板、卡片拖拽移动、审批中心（角标计数 + 一键通过/打回）、
  卡片抽屉五 Tab（讨论/需求/Git/现场/移交）、长轮询实时刷新、租约与进度视觉标识

## 列策略引擎（F-304，已生效）

- `require_progress_summary`：无进度摘要的卡片禁止进入该列（400）
- `require_approval: "human"`：非人类 Owner 申请进入时自动生成审批单（不移动），
  Owner 批准后授权强制移列；审批记录进统一时间线

## MCP Server（Agent 接入，`core/src/bin/mcp.rs`）

`baton-mcp` 是标准 MCP stdio server，任何支持 MCP 的 Agent 零适配接入。
它直接内嵌 Db（SQLite WAL，可与运行中的 core server 并发读写同一库）。

```bash
# 注册到 Claude Code
claude mcp add baton -- /path/to/baton/core/target/debug/baton-mcp

# 环境变量
BATON_DB=data/baton.db     # 数据库路径（默认同上）
BATON_AGENT_ID=a-code      # 本进程扮演的 Agent 成员 id（默认 a-code）
```

工具面（17 个）：`board_get` `card_list` `card_get` `card_create` `card_claim`
`card_release` `card_move` `card_comment` `progress_update` `link_add` `git_attach`
`git_refresh` `worksite_add_node` `handoff_prepare` `handoff_ready` `handoff_accept`
`handoff_cancel` —— 与 HTTP API 同语义，乐观锁/租约/列策略规则一致（409 以 `isError` 返回）。

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

## Tauri 壳说明

`src-tauri/` 配置已就绪（窗口、CSP 仅放行 127.0.0.1:7700、构建命令已接线）。
首次 `cargo build` 需编译 tauri 全量依赖（较久）；骨架阶段用
`cargo run`（core）+ `npm run dev`（web）开发调试即可。
后续在 `src-tauri/src/main.rs` 的 `setup` 里 spawn 内嵌 core server。

## 下一步（按 PRD 路线图）

1. ~~MCP Server 接入层（F-201）~~ ✅ `baton-mcp`（17 工具）
2. ~~实时推送（F-401）~~ ✅ 长轮询事件流（tiny_http 阻塞式 respond 不支持 SSE 无限流，长轮询等效且更稳）
3. ~~列策略执行引擎（F-304）~~ ✅ require_progress_summary + require_approval 已生效
4. ~~worktree / handoff / links 的 API 与 GUI Tab~~ ✅ 五 Tab 抽屉已上线
5. ~~CLI（F-203）~~ ✅ `baton` 命令
6. 待做：多项目切换 UI、依赖编排触发、通知中心、Agent 注册管理 GUI、Tauri 壳首次构建
