//! Baton Core Server — 本地 HTTP API + 可选 WebUI 静态服务（127.0.0.1:7700）
//! 以库模块形式提供，供 baton-core 二进制与 Tauri 壳内嵌启动。
//!
//! 除 /api 路由外，GET 非 /api 路径时从 WebUI 目录 serve 静态文件（SPA fallback
//! 到 index.html），即"单进程 WebUI 模式"：`baton-core` 一个进程同时提供 UI + API。
//! WebUI 目录解析顺序：环境变量 BATON_WEB_DIR → ./web/dist → ../web/dist →
//! <可执行文件目录>/web/dist（取第一个含 index.html 者）；都找不到则纯 API 模式。
//!
//! 路由：
//!   GET  /api/v1/board?board_id=             整板状态（缺省第一个看板）
//!   GET  /api/v1/projects                    项目 + 看板列表（F-101）
//!   GET  /api/v1/install-info                MCP 接入指引信息（baton-mcp 路径探测）//!   POST /api/v1/projects                    {actor(人类), name, description?} 新建项目（带默认看板）
//!   POST /api/v1/projects/{id}/boards        {actor(人类), name} 新建看板（带默认四列）
//!   POST /api/v1/projects/{id}/rename        {actor(人类), name} 重命名项目
//!   POST /api/v1/projects/{id}/delete        {actor(人类)} 删除项目（级联清除卡片数据）
//!   GET  /api/v1/events?since=<seq>          长轮询事件流（看板实时推送）
//!   GET  /api/v1/cards/{id}                  卡片详情（话题/评论/链接/工作现场/移交/产物）
//!   POST /api/v1/cards                       {title, description?, actor?}
//!   POST /api/v1/cards/{id}/claim|release    认领 / 释放租约
//!   POST /api/v1/cards/{id}/join|leave       参与 / 退出协同（多 Agent 同卡协作）
//!   POST /api/v1/cards/{id}/comments         {author, body, kind?, reply_to?, thread_id?}
//!   POST /api/v1/cards/{id}/threads          {actor, title} 新建话题
//!   POST /api/v1/cards/{id}/progress         {actor, percent, summary}
//!   POST /api/v1/cards/{id}/move             {actor, list_id, rev}（列策略引擎）
//!   GET  /api/v1/approvals?status=pending    审批列表
//!   GET  /api/v1/notifications?member=&since=&limit=   通知中心（F-404，事件派生）
//!   POST /api/v1/approvals/{id}/decide       {actor, decision: approved|rejected, note?}
//!   POST /api/v1/cards/{id}/links            {category, system?, url?, title?, ...}
//!   DELETE /api/v1/links/{id}
//!   POST /api/v1/cards/{id}/git/attach       {repo_path, branch, base_branch?}
//!   POST /api/v1/cards/{id}/git/refresh      探测真实 git 状态写 observed 快照
//!   POST /api/v1/cards/{id}/worksite/nodes   {kind, path, branch, purpose?, ...}
//!   DELETE /api/v1/work_nodes/{id}
//!   POST /api/v1/cards/{id}/handoff/{prepare|ready|accept|cancel}
//!   POST /api/v1/cards/{id}/takeover       {actor(人类)} 强制接管：释放 Agent 租约（F-405）
//!   POST /api/v1/cards/{id}/artifacts      {actor, name, kind?, mime?, content?|path?} 上传产物（F-108）
//!   GET  /api/v1/artifacts/{id}            产物元数据 + 文本内容（≤256KB 内联）
//!   POST /api/v1/cards/{id}/deps           {actor, other_card_id, relation: blocked_by|blocks|relates_to}
//!   POST /api/v1/cards/{id}/deps/remove    {actor, other_card_id, relation}
//!   GET  /api/v1/agents                      Agent 面板（在线状态/持有卡片/最近心跳）
//!   POST /api/v1/agents                      {actor(人类), name, role?, capabilities?} 注册并签发 Token
//!   POST /api/v1/agents/{id}/token           {actor(人类)} 轮换 Token（旧 Token 立即失效）
//!   POST /api/v1/agents/{id}/revoke          {actor(人类)} 吊销
//!   POST /api/v1/agents/{id}/heartbeat       Agent 心跳（F-213）
//!   GET  /api/v1/sessions?agent_id=          Session 资源视图（在线/stale/ended + 工作现场）
//!   POST /api/v1/sessions                    {agent_id, project_id?, board_id?, ...} 进板（含简报）
//!   POST /api/v1/sessions/{id}/heartbeat     Session 心跳 = 续命 + 租约续期
//!   POST /api/v1/sessions/{id}/end           离场
//!
//! 鉴权（F-212）：写操作中 actor 为已签发 Token 的 Agent 时，必须携带
//! `X-Baton-Token: bt-...`（或 `Authorization: Bearer`）；未签发 Token 的 Agent 与
//! 人类成员不校验（本地单机信任模型）。已吊销成员一律 403。

use crate::Db;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Method, Request, Response, Server};

/// F-308 速率限制：进程内滑动窗口（每 Agent 每分钟 N 次写请求，默认 60）。
/// 仅约束 HTTP 层；CLI/MCP 直连 Db 属本地信任模型，不受限。
struct RateLimiter {
    hits: HashMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    fn new() -> Self {
        RateLimiter { hits: HashMap::new() }
    }
    /// 记录一次请求；超过 per_min 返回 false
    fn check(&mut self, key: &str, per_min: i64) -> bool {
        let now = Instant::now();
        let q = self.hits.entry(key.to_string()).or_default();
        while q.front().map(|t| now.duration_since(*t) > Duration::from_secs(60)).unwrap_or(false) {
            q.pop_front();
        }
        if q.len() as i64 >= per_min {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// 启动 HTTP 服务并阻塞当前线程（Tauri 壳中在后台线程调用）
pub fn serve(addr: &str, db_path: &str) {
    let db = Arc::new(Mutex::new(Db::open(db_path).expect("failed to open database")));
    let limiter = Arc::new(Mutex::new(RateLimiter::new()));
    let web_dir = Arc::new(resolve_web_dir());
    let server = Arc::new(Server::http(addr).expect("failed to bind"));
    println!("baton-core listening on http://{}  (db: {})", addr, db_path);
    match web_dir.as_ref() {
        Some(d) => println!("WebUI 模式：静态目录 {}", d),
        None => println!("纯 API 模式（未找到 web/dist，可设 BATON_WEB_DIR 指定）"),
    }

    // 每请求一个线程：长轮询挂起不会阻塞其他请求
    for req in server.incoming_requests() {
        let db = db.clone();
        let limiter = limiter.clone();
        let web_dir = web_dir.clone();
        thread::spawn(move || {
            let _ = handle(&db, &limiter, &web_dir, req);
        });
    }
}

/// 解析 WebUI 静态目录：BATON_WEB_DIR → ./web/dist → ../web/dist → <exe>/web/dist
fn resolve_web_dir() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("BATON_WEB_DIR") {
        candidates.push(PathBuf::from(d));
    }
    candidates.push(PathBuf::from("web/dist"));
    candidates.push(PathBuf::from("../web/dist"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("web/dist"));
        }
    }
    candidates
        .into_iter()
        .find(|p| p.join("index.html").is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

fn handle(db: &Arc<Mutex<Db>>, limiter: &Arc<Mutex<RateLimiter>>, web_dir: &Option<String>, mut req: Request) -> Result<(), std::io::Error> {
    let method = req.method().clone();
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    let query = url.split('?').nth(1).unwrap_or("").to_string();
    let seg: Vec<String> = path.trim_start_matches('/').split('/').map(String::from).collect();

    // WebUI 静态服务：GET 非 /api 路径 → dist 静态文件，未命中 fallback 到 index.html（SPA）
    if method == Method::Get && seg.first().map(String::as_str) != Some("api") {
        return serve_static(req, web_dir, &path);
    }

    // 长轮询事件流：有新事件立即返回；否则挂起最多 25s。
    // tiny_http 的 respond 在 body EOF 后才 flush，无法做真正的 SSE 无限流，
    // 长轮询达到同等实时效果且更简单可靠。
    if method == Method::Get && seg.as_slice() == ["api", "v1", "events"] {
        let since: i64 = query.split('&')
            .find_map(|kv| kv.strip_prefix("since="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let bus = db.lock().unwrap().bus();
        let (events, last_seq) = bus.poll(since, std::time::Duration::from_secs(25));
        let data = json!({"events": events, "last_seq": last_seq}).to_string().into_bytes();
        return req.respond(
            Response::from_data(data)
                .with_status_code(200)
                .with_header(hdr("Content-Type: application/json"))
                .with_header(hdr("Cache-Control: no-cache"))
                .with_header(hdr("Access-Control-Allow-Origin: *")),
        );
    }

    let token = extract_token(&req);
    let idem_key = extract_header(&req, "idempotency-key");
    let (raw_body, body) = read_json(&mut req);
    let db = db.lock().unwrap();
    let segs: Vec<&str> = seg.iter().map(String::as_str).collect();

    // 写操作的 actor：取自 body 的 actor/holder/author/agent_id；heartbeat 路由的身份在路径中
    fn actor_of<'a>(body: &'a Value, segs: &'a [&'a str]) -> Option<&'a str> {
        ["actor", "holder", "author", "agent_id"]
            .iter()
            .find_map(|k| body.get(k).and_then(Value::as_str))
            .or_else(|| match segs {
                ["api", "v1", "agents", id, "heartbeat"] => Some(*id),
                _ => None,
            })
    }

    // F-212 鉴权：写操作中 actor 为已签发 Token 的 Agent 时必须携带匹配 Token。
    if method != Method::Get {
        // 例外：Agent 注册是自举入口——新 Agent 尚未入库也没有 Token，
        // 无法在 auth_check 通过；人类校验/自注册开关由 create_agent 自己执行。
        let is_register_route = segs.as_slice() == ["api", "v1", "agents"];
        // session 路由的身份从库中解析（session → agent）
        let actor: Option<String> = actor_of(&body, &segs).map(String::from)
            .or_else(|| match segs.as_slice() {
                ["api", "v1", "sessions", sid, "heartbeat"] | ["api", "v1", "sessions", sid, "end"] => {
                    db.session_agent(sid).ok().flatten()
                }
                _ => None,
            });
        if let Some(a) = actor.as_deref() {
            if !is_register_route {
                if let Err(e) = db.auth_check(a, token.as_deref()) {
                    let status = e.status;
                    let body = e.body;
                    drop(db);
                    return req.respond(respond(Err((status, body))));
                }
            }
            // F-308：Agent 写请求速率限制（人类与读请求不限）
            if let Ok((kind, _)) = db.member_kind_role(a) {
                if kind == "agent" {
                    let per_min = db.agent_rate_limit(a).unwrap_or(60);
                    let allowed = limiter.lock().unwrap().check(a, per_min);
                    if !allowed {
                        drop(db);
                        return req.respond(respond(Err((429, json!({
                            "error": format!("rate limit exceeded: {} req/min", per_min),
                        })))));
                    }
                }
            }
        }
    }

    // F-307 幂等写：携带 Idempotency-Key 的 POST 先查重
    if method == Method::Post {
        if let Some(key) = &idem_key {
            let hash = crate::sha256_hex(&format!("{:?} {} {}", method, path, raw_body));
            match db.idempotency_lookup(key) {
                Ok(Some((h, Some(resp)))) if h == hash => {
                    // 重放：直接返回首个响应，不产生副作用
                    drop(db);
                    let data = resp.into_bytes();
                    return req.respond(
                        Response::from_data(data)
                            .with_status_code(200)
                            .with_header(hdr("Content-Type: application/json"))
                            .with_header(hdr("Idempotency-Replayed: true"))
                            .with_header(hdr("Access-Control-Allow-Origin: *")),
                    );
                }
                Ok(Some((h, _))) if h != hash => {
                    drop(db);
                    return req.respond(respond(Err((409, json!({
                        "error": "idempotency key reused with a different request",
                    })))));
                }
                Ok(_) => {} // 未命中：继续执行
                Err(e) => {
                    let body = err(e);
                    drop(db);
                    return req.respond(respond(Err((500, body))));
                }
            }
        }
    }

    let result: Result<Value, (u16, Value)> = match (&method, segs.as_slice()) {
        (&Method::Get, ["api", "v1", "board"]) => {
            let board_id = query.split('&')
                .find_map(|kv| kv.strip_prefix("board_id="))
                .map(String::from);
            db.board_state(board_id.as_deref()).map_err(|e| (500, err(e)))
        }

        (&Method::Get, ["api", "v1", "projects"]) => db.list_projects().map_err(|e| (500, err(e))),

        // 接入指引：探测本机 baton-mcp 二进制路径（与当前进程同目录），供 GUI 展示安装命令
        (&Method::Get, ["api", "v1", "install-info"]) => {
            let mcp_bin = std::env::current_exe().ok()
                .and_then(|exe| exe.parent().map(|d| d.join("baton-mcp")))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "baton-mcp（cargo build 后位于 core/target/debug/）".into());
            Ok(json!({"mcp_bin": mcp_bin}))
        }

        (&Method::Post, ["api", "v1", "projects"]) => {
            db.create_project(s(&body, "actor", "u-owner"), s(&body, "name", "未命名项目"),
                s(&body, "description", ""), s(&body, "template", "software")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "projects", pid, "boards"]) => {
            db.create_board(s(&body, "actor", "u-owner"), pid, s(&body, "name", "未命名看板"),
                s(&body, "template", "software"))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "projects", pid, "rename"]) => {
            db.rename_project(s(&body, "actor", "u-owner"), pid, s(&body, "name", "")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "projects", pid, "delete"]) => {
            db.delete_project(s(&body, "actor", "u-owner"), pid).map_err(api_err)
        }

        (&Method::Get, ["api", "v1", "cards", id]) => db.card_detail(id).map_err(|e| (500, err(e))),

        (&Method::Post, ["api", "v1", "cards"]) => {
            let title = s(&body, "title", "未命名卡片");
            db.create_card(s(&body, "actor", "u-owner"), title, s(&body, "description", ""),
                body.get("board_id").and_then(Value::as_str),
                body.get("parent_id").and_then(Value::as_str))
                .map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "cards", id, "claim"]) => {
            db.claim_card(id, s(&body, "holder", "a-code"),
                body.get("session_id").and_then(Value::as_str)).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "release"]) => {
            db.release_card(id, s(&body, "actor", "u-owner")).map_err(api_err)
        }

        // 协同参与：加入/退出卡片的协同者列表（多 Agent 共同完成同一任务）
        (&Method::Post, ["api", "v1", "cards", id, "join"]) => {
            db.join_card(id, s(&body, "actor", "a-code"),
                body.get("session_id").and_then(Value::as_str)).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "leave"]) => {
            db.leave_card(id, s(&body, "actor", "a-code")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "comments"]) => {
            db.add_comment(id, s(&body, "author", "u-owner"), s(&body, "body", ""), s(&body, "kind", "chat"),
                body.get("reply_to").and_then(Value::as_str),
                body.get("thread_id").and_then(Value::as_str))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "threads"]) => {
            db.create_thread(id, s(&body, "actor", "u-owner"), s(&body, "title", "新话题"))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "progress"]) => {
            db.update_progress(id, s(&body, "actor", "a-code"),
                body.get("percent").and_then(Value::as_i64).unwrap_or(0),
                s(&body, "summary", ""))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "move"]) => {
            db.move_card(id, s(&body, "actor", "u-owner"), s(&body, "list_id", ""),
                body.get("rev").and_then(Value::as_i64).unwrap_or(0))
                .map_err(api_err)
        }

        (&Method::Get, ["api", "v1", "approvals"]) => {
            let status = query.split('&')
                .find_map(|kv| kv.strip_prefix("status="))
                .map(String::from);
            db.list_approvals(status.as_deref()).map_err(|e| (500, err(e)))
        }

        (&Method::Get, ["api", "v1", "notifications"]) => {
            // F-404 通知中心：从事件日志派生（审批/@提及/依赖解除/接管/移交）
            let qv = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{}=", k)));
            db.notifications(
                qv("member").unwrap_or("u-owner"),
                qv("since").and_then(|v| v.parse().ok()).unwrap_or(0),
                qv("limit").and_then(|v| v.parse().ok()).unwrap_or(50),
            ).map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "approvals", id, "decide"]) => {
            db.decide_approval(id, s(&body, "actor", "u-owner"), s(&body, "decision", ""), s(&body, "note", ""))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "links"]) => {
            db.add_link(id, s(&body, "actor", "u-owner"), &body).map_err(api_err)
        }

        (&Method::Delete, ["api", "v1", "links", id]) => {
            db.delete_link(id, s(&body, "actor", "u-owner")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "git", "attach"]) => {
            db.git_attach(id, s(&body, "actor", "a-code"), s(&body, "repo_path", ""),
                s(&body, "branch", ""), body.get("base_branch").and_then(Value::as_str))
                .map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "git", "refresh"]) => {
            db.git_refresh(id, s(&body, "actor", "a-code")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "worksite", "nodes"]) => {
            db.worksite_add_node(id, s(&body, "actor", "a-code"), &body).map_err(api_err)
        }

        (&Method::Delete, ["api", "v1", "work_nodes", id]) => {
            db.worksite_remove_node(id, s(&body, "actor", "u-owner")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "handoff", action]) => {
            db.handoff_action(id, s(&body, "actor", "a-code"), action, &body).map_err(api_err)
        }

        (&Method::Get, ["api", "v1", "agents"]) => db.list_agents().map_err(|e| (500, err(e))),

        (&Method::Get, ["api", "v1", "members"]) => db.list_members().map_err(|e| (500, err(e))),

        (&Method::Post, ["api", "v1", "cards", id, "assign"]) => {
            // F-105：assignee 缺省/null = 放入抢单池（F-303）
            let assignee = body.get("assignee").and_then(Value::as_str);
            db.assign_card(id, s(&body, "actor", "u-owner"), assignee).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "agents"]) => {
            let caps = body.get("capabilities").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default();
            db.create_agent(s(&body, "actor", "u-owner"), s(&body, "name", "agent"),
                s(&body, "role", "worker"), caps).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "agents", id, "token"]) => {
            db.rotate_agent_token(s(&body, "actor", "u-owner"), id).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "agents", id, "revoke"]) => {
            db.revoke_agent(s(&body, "actor", "u-owner"), id).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "agents", id, "heartbeat"]) => {
            db.heartbeat(id).map_err(api_err)
        }

        (&Method::Get, ["api", "v1", "sessions"]) => {
            let agent_id = query.split('&')
                .find_map(|kv| kv.strip_prefix("agent_id="))
                .map(String::from);
            db.list_sessions(agent_id.as_deref()).map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "sessions"]) => {
            db.session_start(s(&body, "agent_id", "a-code"), &body).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "sessions", id, "heartbeat"]) => {
            db.session_heartbeat(id).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "sessions", id, "end"]) => {
            db.session_end(id).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "takeover"]) => {
            db.takeover_card(id, s(&body, "actor", "u-owner")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "artifacts"]) => {
            db.upload_artifact(id, s(&body, "actor", "a-code"), &body).map_err(api_err)
        }

        (&Method::Get, ["api", "v1", "artifacts", id]) => {
            db.get_artifact(id).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "deps"]) => {
            db.add_dep(id, s(&body, "other_card_id", ""), s(&body, "relation", "blocked_by"),
                s(&body, "actor", "u-owner")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "deps", "remove"]) => {
            db.remove_dep(id, s(&body, "other_card_id", ""), s(&body, "relation", "blocked_by"),
                s(&body, "actor", "u-owner")).map_err(api_err)
        }

        _ => Err((404, json!({"error": "not found", "path": path}))),
    };

    // F-307：写成功时记录幂等键（供重放）
    if let (Some(key), Ok(v)) = (&idem_key, &result) {
        let hash = crate::sha256_hex(&format!("{:?} {} {}", method, path, raw_body));
        let actor = actor_of(&body, &segs).unwrap_or("u-owner");
        let _ = db.idempotency_store(key, actor, &hash, &v.to_string());
    }
    drop(db);
    req.respond(respond(result))
}

fn s<'a>(body: &'a Value, key: &str, default: &'a str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or(default)
}

/// WebUI 静态文件服务：命中文件直接返回，否则 SPA fallback 到 index.html。
/// 路径含 `..` 段时视为未命中（防目录穿越），同样落到 index.html。
fn serve_static(req: Request, web_dir: &Option<String>, path: &str) -> Result<(), std::io::Error> {
    let Some(dir) = web_dir else {
        return req.respond(respond(Err((404, json!({
            "error": "web ui not available (set BATON_WEB_DIR or build web/dist)",
        })))));
    };
    let rel = path.trim_start_matches('/');
    let mut file_path = PathBuf::from(dir);
    if !rel.is_empty() && !rel.split('/').any(|seg| seg == "..") {
        let candidate = file_path.join(rel);
        if candidate.is_file() {
            file_path = candidate;
        } else {
            file_path = file_path.join("index.html");
        }
    } else {
        file_path = file_path.join("index.html");
    }
    match File::open(&file_path) {
        Ok(f) => {
            let mime = match file_path.extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("json") | Some("map") => "application/json",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                Some("jpg") | Some("jpeg") => "image/jpeg",
                Some("ico") => "image/x-icon",
                Some("woff") => "font/woff",
                Some("woff2") => "font/woff2",
                Some("txt") => "text/plain; charset=utf-8",
                Some("webmanifest") => "application/manifest+json",
                _ => "application/octet-stream",
            };
            let mut resp = Response::from_file(f).with_header(hdr(&format!("Content-Type: {}", mime)));
            if mime.starts_with("text/html") {
                // 入口 HTML 不缓存，保证重新构建后立即生效（带 hash 的 assets 可放心缓存）
                resp = resp.with_header(hdr("Cache-Control: no-cache"));
            }
            req.respond(resp)
        }
        Err(_) => req.respond(respond(Err((404, json!({"error": "not found", "path": path}))))),
    }
}

/// 提取指定请求头（大小写不敏感）
fn extract_header(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// 提取 Agent Token：`X-Baton-Token` 头或 `Authorization: Bearer <token>`
fn extract_token(req: &Request) -> Option<String> {
    extract_header(req, "x-baton-token").or_else(|| {
        extract_header(req, "authorization")
            .and_then(|v| v.strip_prefix("Bearer ").map(String::from))
    })
}

/// 读取请求体：(原始字符串, 解析后的 JSON)
fn read_json(req: &mut Request) -> (String, Value) {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    let v = serde_json::from_str(&buf).unwrap_or(json!({}));
    (buf, v)
}

fn err(e: rusqlite::Error) -> Value {
    json!({"error": e.to_string()})
}

fn hdr(s: &str) -> tiny_http::Header {
    s.parse().unwrap()
}

fn api_err(e: crate::ApiErr) -> (u16, Value) {
    (e.status, e.body)
}

fn respond(result: Result<Value, (u16, Value)>) -> Response<std::io::Cursor<Vec<u8>>> {
    let (status, body) = match result {
        Ok(v) => (200, v),
        Err((st, v)) => (st, v),
    };
    Response::from_data(body.to_string().into_bytes())
        .with_status_code(status)
        .with_header(hdr("Content-Type: application/json"))
        .with_header(hdr("Access-Control-Allow-Origin: *"))
}
