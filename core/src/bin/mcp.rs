//! baton-mcp — MCP Server（stdio，newline-delimited JSON-RPC 2.0）
//!
//! 让任何支持 MCP 的 Agent（Claude Code / Kimi / 自研）零适配接入 Baton 看板。
//! 直接内嵌 Db（SQLite WAL 支持 core server 与本进程并发读写同一库文件）。
//!
//! 双时代协议支持：
//!   - legacy `2024-11-05`：客户端发 initialize → 按旧握手语义应答
//!   - modern `2026-07-28`（无状态核心）：无 initialize 要求；每请求可携带
//!     `_meta["io.modelcontextprotocol/protocolVersion"]`，不支持则回 -32022 并附支持列表；
//!     提供 `server/discover` 探针（规范建议 stdio 客户端先探测）
//!
//! Session（看板业务会话，非协议会话）：进程启动即自动 session_start（cwd/git 自动探测），
//! stdin 断开（进程退出）自动 session_end；每次工具调用以此 session 归属，也可用
//! 参数 session_id 显式覆盖（对齐 2026-07-28"跨请求状态必须由客户端显式携带"）。
//!
//! 注册示例（Claude Code）：
//!   claude mcp add baton -- /path/to/baton-mcp
//! 环境变量：
//!   BATON_DB        数据库路径（默认 ~/.baton/baton.db）
//!   BATON_AGENT_ID  本进程扮演的 Agent 成员 id（默认 a-code）

use baton_core::{ApiErr, Db};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const LEGACY_VERSION: &str = "2024-11-05";
const MODERN_VERSION: &str = "2026-07-28";
const SUPPORTED: [&str; 2] = [MODERN_VERSION, LEGACY_VERSION];

fn tool_defs() -> Value {
    json!([
        {"name": "board_get", "description": "获取整板状态：列 + 卡片 + 租约快照 + 未结话题数",
         "inputSchema": {"type": "object", "properties": {
             "board_id": {"type": "string", "description": "看板 id，缺省第一个看板"}}}},
        {"name": "project_list", "description": "列出所有项目及其看板",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "card_list", "description": "列出卡片，可选按列/看板过滤",
         "inputSchema": {"type": "object", "properties": {
             "list_id": {"type": "string", "description": "列 id，缺省返回所有列的卡片"},
             "board_id": {"type": "string", "description": "看板 id"}}}},
        {"name": "card_get", "description": "卡片详情：描述、ext、话题与评论、当前租约",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "card_create", "description": "创建卡片（进入看板第一列，自动建主讨论话题）；parent_id 可建子任务",
         "inputSchema": {"type": "object", "required": ["title"], "properties": {
             "title": {"type": "string"},
             "description": {"type": "string", "description": "Markdown 描述"},
             "board_id": {"type": "string", "description": "目标看板 id，缺省第一个看板"},
             "parent_id": {"type": "string", "description": "父卡 id（子任务继承父卡看板）"}}}},
        {"name": "card_claim", "description": "认领卡片，获得 30 分钟排他租约；已被认领返回冲突",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "card_release", "description": "释放卡片租约",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "card_move", "description": "移动卡片到其他列。必须携带当前 rev（乐观锁）；有活跃租约时仅持有者可移动",
         "inputSchema": {"type": "object", "required": ["card_id", "list_id", "rev"], "properties": {
             "card_id": {"type": "string"},
             "list_id": {"type": "string", "description": "目标列 id，如 l-doing / l-review / l-done"},
             "rev": {"type": "integer"}}}},
        {"name": "card_comment", "description": "在卡片话题下写评论（chat/progress/system/handoff/approval）；reply_to 可直接回复某条评论，thread_id 可指定话题",
         "inputSchema": {"type": "object", "required": ["card_id", "body"], "properties": {
             "card_id": {"type": "string"},
             "body": {"type": "string", "description": "Markdown"},
             "reply_to": {"type": "string", "description": "被回复的评论 id（可选）"},
             "thread_id": {"type": "string", "description": "目标话题 id（可选，缺省进主讨论）"},
             "kind": {"type": "string", "enum": ["chat","progress","system","handoff","approval"]}}}},
        {"name": "thread_create", "description": "在卡片讨论区新建话题（thread）",
         "inputSchema": {"type": "object", "required": ["card_id", "title"], "properties": {
             "card_id": {"type": "string"},
             "title": {"type": "string"}}}},
        {"name": "progress_update", "description": "更新工作进度：percent(0-100) + 一句话 summary；同步写入进度评论",
         "inputSchema": {"type": "object", "required": ["card_id","percent","summary"], "properties": {
             "card_id": {"type": "string"},
             "percent": {"type": "integer", "minimum": 0, "maximum": 100},
             "summary": {"type": "string"}}}},
        {"name": "link_add", "description": "给卡片添加需求来源/文档链接（Jira/MeeGo/url/本地文件）",
         "inputSchema": {"type": "object", "required": ["card_id","category","title"], "properties": {
             "card_id": {"type": "string"},
             "category": {"type": "string", "enum": ["source","doc"]},
             "system": {"type": "string", "enum": ["jira","meego","github_issue","url","file"]},
             "url": {"type": "string"}, "key": {"type": "string"}, "title": {"type": "string"},
             "relation": {"type": "string", "enum": ["origin","related"]},
             "kind": {"type": "string", "enum": ["url","local_file","artifact"]},
             "path": {"type": "string"}}}},
        {"name": "git_attach", "description": "声明卡片关联的仓库/分支（declared 层）",
         "inputSchema": {"type": "object", "required": ["card_id","repo_path","branch"], "properties": {
             "card_id": {"type": "string"}, "repo_path": {"type": "string"},
             "branch": {"type": "string"}, "base_branch": {"type": "string"}}}},
        {"name": "git_refresh", "description": "探测关联仓库的真实 git 状态（staged/unstaged/ahead/behind/last_commit），写入 observed 快照",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "worksite_add_node", "description": "登记工作现场节点（main 主目录或 worktree），可绑定子卡形成拓扑",
         "inputSchema": {"type": "object", "required": ["card_id","kind","path","branch"], "properties": {
             "card_id": {"type": "string"},
             "kind": {"type": "string", "enum": ["main","worktree"]},
             "path": {"type": "string"}, "branch": {"type": "string"},
             "purpose": {"type": "string"}, "owner": {"type": "string"},
             "bound_card_id": {"type": "string"}}}},
        {"name": "handoff_prepare", "description": "开始移交：整理移交包（上下文笔记 + 工作现场快照 + 未结话题）",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}, "reason": {"type": "string"},
             "to": {"type": "string", "description": "指定接手方成员 id，缺省公开可认领"},
             "context_note": {"type": "string"}, "env_notes": {"type": "string"}}}},
        {"name": "handoff_ready", "description": "移交包就绪：释放租约，卡片标记待接手",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "handoff_accept", "description": "接受移交：claim 卡片并继承工作现场",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "handoff_cancel", "description": "取消移交",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "agent_heartbeat", "description": "上报心跳：更新本 Agent 的在线状态（GUI Agent 面板可见）",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "artifact_upload", "description": "给卡片上传产物（diff/文档/日志等）：path（本机文件）或 content（文本）二选一",
         "inputSchema": {"type": "object", "required": ["card_id","name"], "properties": {
             "card_id": {"type": "string"},
             "name": {"type": "string", "description": "产物文件名"},
             "path": {"type": "string", "description": "本机文件路径，内容会被复制进工作区"},
             "content": {"type": "string", "description": "文本内容（与 path 二选一）"},
             "kind": {"type": "string", "enum": ["file","diff","doc","image","log"]},
             "mime": {"type": "string"}}}},
        {"name": "artifact_list", "description": "列出卡片上的产物",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "card_dep_add", "description": "添加卡片依赖（F-106）：blocked_by 未完成的卡片不能进入 Done 列，依赖完成时自动通知下游（F-305）",
         "inputSchema": {"type": "object", "required": ["card_id","other_card_id"], "properties": {
             "card_id": {"type": "string"},
             "other_card_id": {"type": "string"},
             "relation": {"type": "string", "enum": ["blocked_by","blocks","relates_to"],
                 "description": "从 card_id 视角的关系，默认 blocked_by"}}}},
        {"name": "card_dep_remove", "description": "移除卡片依赖",
         "inputSchema": {"type": "object", "required": ["card_id","other_card_id"], "properties": {
             "card_id": {"type": "string"},
             "other_card_id": {"type": "string"},
             "relation": {"type": "string", "enum": ["blocked_by","blocks","relates_to"]}}}},
        {"name": "notification_list", "description": "查看本 Agent 的通知：@提及、审批请求、依赖解除、移交待接手等（F-404）",
         "inputSchema": {"type": "object", "properties": {
             "since": {"type": "integer", "description": "事件 seq 游标，缺省 0"},
             "limit": {"type": "integer", "description": "最多返回条数，缺省 50"}}}},
        {"name": "card_assign", "description": "指派卡片给成员；assignee 缺省 = 放入抢单池（F-105/303）",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"},
             "assignee": {"type": "string", "description": "成员 id；缺省 = 抢单池"}}}},
        {"name": "session_start", "description": "显式进板：声明本会话的 scope/工作现场，返回进板简报（在手卡片/待接手移交/@提及）。stdio 进程会自动进板，通常无需调用",
         "inputSchema": {"type": "object", "properties": {
             "project_id": {"type": "string"}, "board_id": {"type": "string"},
             "cwd": {"type": "string"}, "repo_path": {"type": "string"}, "branch": {"type": "string"},
             "parent_session_id": {"type": "string", "description": "resume 链：上一个会话 id"}}}},
        {"name": "session_end", "description": "显式离场：结束指定（或当前）会话；持有租约进入自然到期",
         "inputSchema": {"type": "object", "properties": {
             "session_id": {"type": "string", "description": "缺省 = 当前进程会话"}}}},
        {"name": "session_list", "description": "列出会话（资源视图）：在线/stale/ended、工作现场、持有卡片",
         "inputSchema": {"type": "object", "properties": {
             "agent_id": {"type": "string", "description": "缺省 = 本进程 Agent"}}}},
        {"name": "approval_list", "description": "审批单列表（status 过滤：pending/approved/rejected，缺省全部）",
         "inputSchema": {"type": "object", "properties": {
             "status": {"type": "string"}}}},
        {"name": "approval_decide", "description": "裁决审批单。不能审批自己的申请（职责分离）；human 模式仅人类可裁决，peer 模式任何同伴成员（含 Agent）均可",
         "inputSchema": {"type": "object", "required": ["approval_id", "decision"], "properties": {
             "approval_id": {"type": "string"},
             "decision": {"type": "string", "enum": ["approved", "rejected"]},
             "note": {"type": "string"}}}},
        {"name": "card_join", "description": "参与协同：加入卡片协同者列表（多 Agent 共同完成同一任务）。协同者可评论/汇报/传产物/移列，但租约主责不变",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"},
             "session_id": {"type": "string", "description": "缺省 = 当前进程会话"}}}},
        {"name": "card_leave", "description": "退出协同",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}}
    ])
}

fn call_tool(db: &Db, agent: &str, session: Option<&str>, name: &str, args: &Value) -> Result<Value, ApiErr> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    match name {
        "board_get" => Ok(db.board_state(args.get("board_id").and_then(Value::as_str))?),
        "project_list" => Ok(db.list_projects()?),
        "card_list" => Ok(db.list_cards(
            args.get("list_id").and_then(Value::as_str),
            args.get("board_id").and_then(Value::as_str),
        )?),
        "card_get" => Ok(db.card_detail(s("card_id"))?),
        "card_create" => Ok(db.create_card(agent, s("title"), s("description"),
            args.get("board_id").and_then(Value::as_str),
            args.get("parent_id").and_then(Value::as_str))?),
        "card_claim" => db.claim_card(s("card_id"), agent,
            args.get("session_id").and_then(Value::as_str).or(session)),
        "card_release" => db.release_card(s("card_id"), agent),
        "card_move" => db.move_card(
            s("card_id"), agent, s("list_id"),
            args.get("rev").and_then(Value::as_i64).unwrap_or(0),
        ),
        "card_comment" => db.add_comment(
            s("card_id"), agent, s("body"),
            if s("kind").is_empty() { "chat" } else { s("kind") },
            args.get("reply_to").and_then(Value::as_str),
            args.get("thread_id").and_then(Value::as_str),
        ),
        "thread_create" => db.create_thread(s("card_id"), agent, s("title")),
        "progress_update" => db.update_progress(
            s("card_id"), agent,
            args.get("percent").and_then(Value::as_i64).unwrap_or(0),
            s("summary"),
        ),
        "link_add" => db.add_link(s("card_id"), agent, args),
        "git_attach" => db.git_attach(
            s("card_id"), agent, s("repo_path"), s("branch"),
            args.get("base_branch").and_then(Value::as_str),
        ),
        "git_refresh" => db.git_refresh(s("card_id"), agent),
        "worksite_add_node" => db.worksite_add_node(s("card_id"), agent, args),
        "handoff_prepare" | "handoff_ready" | "handoff_accept" | "handoff_cancel" => {
            db.handoff_action(s("card_id"), agent, name.trim_start_matches("handoff_"), args)
        }
        "agent_heartbeat" => match session {
            Some(sid) => db.session_heartbeat(sid),  // 心跳即续租约
            None => db.heartbeat(agent),
        },
        "artifact_upload" => db.upload_artifact(s("card_id"), agent, args),
        "artifact_list" => db.card_detail(s("card_id"))
            .map(|d| d.get("artifacts").cloned().unwrap_or(json!([])))
            .map_err(ApiErr::from),
        "card_dep_add" => db.add_dep(
            s("card_id"), s("other_card_id"),
            if s("relation").is_empty() { "blocked_by" } else { s("relation") },
            agent,
        ),
        "card_dep_remove" => db.remove_dep(
            s("card_id"), s("other_card_id"),
            if s("relation").is_empty() { "blocked_by" } else { s("relation") },
            agent,
        ),
        "notification_list" => Ok(db.notifications(
            agent,
            args.get("since").and_then(Value::as_i64).unwrap_or(0),
            args.get("limit").and_then(Value::as_i64).unwrap_or(50),
        )?),
        "card_assign" => db.assign_card(
            s("card_id"), agent,
            args.get("assignee").and_then(Value::as_str),
        ),
        "session_start" => db.session_start(agent, args),
        "session_end" => db.session_end(
            args.get("session_id").and_then(Value::as_str).or(session)
                .ok_or_else(|| ApiErr::bad_request("no session to end"))?,
        ),
        "session_list" => Ok(db.list_sessions(
            args.get("agent_id").and_then(Value::as_str).or(Some(agent)),
        )?),
        "approval_list" => Ok(db.list_approvals(
            args.get("status").and_then(Value::as_str),
        )?),
        "approval_decide" => db.decide_approval(
            s("approval_id"), agent, s("decision"), s("note"),
        ),
        "card_join" => db.join_card(s("card_id"), agent,
            args.get("session_id").and_then(Value::as_str).or(session)),
        "card_leave" => db.leave_card(s("card_id"), agent),
        _ => Err(ApiErr::bad_request("unknown tool")),
    }
}

fn main() {
    let db_path = baton_core::default_db_path();
    let agent = std::env::var("BATON_AGENT_ID").unwrap_or_else(|_| "a-code".into());
    let db = Db::open(&db_path).expect("failed to open database");

    // 进程启动即自动进板（stdio：进程生命周期 ≈ 会话生命周期）；
    // cwd/git 自动探测，失败不阻塞（返回 None 会话继续无 session 工作）
    let auto_session: Option<String> = db.session_start(&agent, &json!({
        "meta": {"via": "mcp-stdio", "pid": std::process::id()},
    })).ok()
        .and_then(|v| v.get("session_id").and_then(Value::as_str).map(String::from));
    eprintln!("baton-mcp ready (db: {}, agent: {}, session: {})",
        db_path, agent, auto_session.as_deref().unwrap_or("none"));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        let line = line.trim();
        if line.is_empty() { continue; }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let Some(id) = id else { continue }; // notification：无 id，不回包

        // 2026-07-28 无状态核心：请求若携带 per-request 版本声明则校验；
        // 不支持 → -32022 并附支持列表（客户端据此换版本重试）
        if let Some(v) = msg.pointer("/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
        {
            if !SUPPORTED.contains(&v) {
                let resp = json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32022,
                    "message": format!("unsupported protocol version: {}", v),
                    "data": {"supported": SUPPORTED},
                }});
                let mut out = stdout.lock();
                let _ = writeln!(out, "{}", resp);
                let _ = out.flush();
                continue;
            }
        }

        let result: Result<Value, Value> = match method {
            // initialize = 选择 legacy 语义（2024-11-05 握手）；响应附带进板简报
            "initialize" => {
                let mut r = json!({
                    "protocolVersion": LEGACY_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "baton", "version": "0.2.0"},
                    "instructions": "Baton 看板已自动为你创建会话（见 _meta.baton）。调用工具即在工作；建议先读 briefing（在手卡片/待接手移交/@提及）。定期调用 agent_heartbeat 续租约。",
                });
                if let Some(sid) = &auto_session {
                    let briefing = db.session_briefing(&agent).unwrap_or(json!({}));
                    r.as_object_mut().unwrap().insert("_meta".into(), json!({
                        "baton": {"session_id": sid, "agent_id": agent, "briefing": briefing},
                    }));
                }
                Ok(r)
            }
            // 2026-07-28：无握手探针（规范建议 stdio 客户端先探测）
            "server/discover" => {
                let mut r = json!({
                    "supportedVersions": SUPPORTED,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "baton", "version": "0.2.0"},
                });
                if let Some(sid) = &auto_session {
                    let briefing = db.session_briefing(&agent).unwrap_or(json!({}));
                    r.as_object_mut().unwrap().insert("_meta".into(), json!({
                        "baton": {"session_id": sid, "agent_id": agent, "briefing": briefing},
                    }));
                }
                Ok(r)
            }
            "ping" => Ok(json!({})), // 2026-07-28 已移除 ping；为 legacy 客户端保留
            "tools/list" => Ok(json!({"tools": tool_defs()})),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(&db, &agent, auto_session.as_deref(), name, &args) {
                    Ok(v) => Ok(json!({
                        "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap()}]
                    })),
                    Err(e) => Ok(json!({
                        "content": [{"type": "text", "text": e.body.to_string()}],
                        "isError": true
                    })),
                }
            }
            _ => Err(json!({"code": -32601, "message": format!("method not found: {}", method)})),
        };

        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", resp);
        let _ = out.flush();
    }

    // stdin 断开（宿主进程退出）→ 自动离场
    if let Some(sid) = &auto_session {
        let _ = db.session_end(sid);
        eprintln!("baton-mcp session ended: {}", sid);
    }
}
