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

- **数据层**：`contract/schema.sql` 全量建库（20 表 + 2 视图），WAL、外键、生成列
- **API**（`core/src/main.rs`）：
  - `GET  /api/v1/board` 整板状态（列 + 卡 + 租约快照 + 未结话题数）
  - `GET  /api/v1/cards/{id}` 卡片详情（话题 + 评论 + ext）
  - `POST /api/v1/cards` 建卡（自动创建"主讨论"话题）
  - `POST /api/v1/cards/{id}/claim | /release` 认领（30min 租约）/ 释放
  - `POST /api/v1/cards/{id}/comments` 评论（kind: chat/progress/system/...）
  - `POST /api/v1/cards/{id}/progress` 进度更新（写 ext.progress + progress 评论）
  - `POST /api/v1/cards/{id}/move` 移列（**乐观锁 rev 校验 + 租约持有者校验**，冲突 409）
- **GUI**：四列看板、建卡、卡片抽屉（认领/释放/移列/进度/评论）、2s 轮询刷新、
  租约与进度的视觉标识

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

工具面（9 个）：`board_get` `card_list` `card_get` `card_create` `card_claim`
`card_release` `card_move` `card_comment` `progress_update` —— 与 HTTP API 同语义，
乐观锁/租约规则一致（409 以 `isError` 返回）。

## 已验证（冒烟 + 浏览器全流程 + MCP 会话）

- 重复 claim → 409 `card already claimed`
- 错误 rev 移列 → 409 `rev conflict`（返回 current_rev）
- 非持有者移列 → 409 `card is claimed by another member`
- 持有者正确 rev 移列 → 200，rev 递增
- 进度更新 → ext.progress 落库 + `kind=progress` 评论进统一时间线
- 浏览器端到端：建卡 → 认领 → 进度 → 移列 → 评论，无 console 报错
- MCP 会话：initialize / tools/list / 9 个工具调用全部正常
- **MCP 与 HTTP core 并发共享同一库**：MCP 侧 claim + progress，HTTP 侧立即可见
- 已存在的库二次启动正常（schema 幂等，`CREATE ... IF NOT EXISTS`）

## Tauri 壳说明

`src-tauri/` 配置已就绪（窗口、CSP 仅放行 127.0.0.1:7700、构建命令已接线）。
首次 `cargo build` 需编译 tauri 全量依赖（较久）；骨架阶段用
`cargo run`（core）+ `npm run dev`（web）开发调试即可。
后续在 `src-tauri/src/main.rs` 的 `setup` 里 spawn 内嵌 core server。

## 下一步（按 PRD 路线图）

1. ~~MCP Server 接入层（F-201）~~ ✅ 已完成（`baton-mcp`）
2. WebSocket 事件推送替代轮询（F-401）
3. 列策略执行引擎（require_approval / require_progress_summary 已落库，待执行）
4. worktree / handoff / links 的 API 与 GUI Tab
