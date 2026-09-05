//! Baton Core Server — 本地 HTTP API（127.0.0.1:7700）
//!
//! 路由：
//!   GET  /api/v1/board                       整板状态
//!   GET  /api/v1/events?since=<seq>          长轮询事件流（看板实时推送）
//!   GET  /api/v1/cards/{id}                  卡片详情（话题/评论/链接/工作现场/移交）
//!   POST /api/v1/cards                       {title, description?, actor?}
//!   POST /api/v1/cards/{id}/claim|release    认领 / 释放租约
//!   POST /api/v1/cards/{id}/comments         {author, body, kind?}
//!   POST /api/v1/cards/{id}/progress         {actor, percent, summary}
//!   POST /api/v1/cards/{id}/move             {actor, list_id, rev}（列策略引擎）
//!   GET  /api/v1/approvals?status=pending    审批列表
//!   POST /api/v1/approvals/{id}/decide       {actor, decision: approved|rejected, note?}
//!   POST /api/v1/cards/{id}/links            {category, system?, url?, title?, ...}
//!   DELETE /api/v1/links/{id}
//!   POST /api/v1/cards/{id}/git/attach       {repo_path, branch, base_branch?}
//!   POST /api/v1/cards/{id}/git/refresh      探测真实 git 状态写 observed 快照
//!   POST /api/v1/cards/{id}/worksite/nodes   {kind, path, branch, purpose?, ...}
//!   DELETE /api/v1/work_nodes/{id}
//!   POST /api/v1/cards/{id}/handoff/{prepare|ready|accept|cancel}

use baton_core::Db;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Method, Request, Response, Server};

fn main() {
    let db_path = std::env::var("BATON_DB").unwrap_or_else(|_| "data/baton.db".into());
    let addr = std::env::var("BATON_ADDR").unwrap_or_else(|_| "127.0.0.1:7700".into());

    let db = Arc::new(Mutex::new(Db::open(&db_path).expect("failed to open database")));
    let server = Arc::new(Server::http(&addr).expect("failed to bind"));
    println!("baton-core listening on http://{}  (db: {})", addr, db_path);

    // 每请求一个线程：长轮询挂起不会阻塞其他请求
    for req in server.incoming_requests() {
        let db = db.clone();
        thread::spawn(move || {
            let _ = handle(&db, req);
        });
    }
}

fn handle(db: &Arc<Mutex<Db>>, mut req: Request) -> Result<(), std::io::Error> {
    let method = req.method().clone();
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    let query = url.split('?').nth(1).unwrap_or("").to_string();
    let seg: Vec<String> = path.trim_start_matches('/').split('/').map(String::from).collect();

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

    let body = read_json(&mut req);
    let db = db.lock().unwrap();
    let segs: Vec<&str> = seg.iter().map(String::as_str).collect();

    let result: Result<Value, (u16, Value)> = match (&method, segs.as_slice()) {
        (&Method::Get, ["api", "v1", "board"]) => db.board_state().map_err(|e| (500, err(e))),

        (&Method::Get, ["api", "v1", "cards", id]) => db.card_detail(id).map_err(|e| (500, err(e))),

        (&Method::Post, ["api", "v1", "cards"]) => {
            let title = s(&body, "title", "未命名卡片");
            db.create_card(s(&body, "actor", "u-owner"), title, s(&body, "description", ""))
                .map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "cards", id, "claim"]) => {
            db.claim_card(id, s(&body, "holder", "a-code")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "release"]) => {
            db.release_card(id, s(&body, "actor", "u-owner")).map_err(api_err)
        }

        (&Method::Post, ["api", "v1", "cards", id, "comments"]) => {
            db.add_comment(id, s(&body, "author", "u-owner"), s(&body, "body", ""), s(&body, "kind", "chat"))
                .map_err(|e| (500, err(e)))
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

        _ => Err((404, json!({"error": "not found", "path": path}))),
    };

    drop(db);
    req.respond(respond(result))
}

fn s<'a>(body: &'a Value, key: &str, default: &'a str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn read_json(req: &mut Request) -> Value {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(json!({}))
}

fn err(e: rusqlite::Error) -> Value {
    json!({"error": e.to_string()})
}

fn hdr(s: &str) -> tiny_http::Header {
    s.parse().unwrap()
}

fn api_err(e: baton_core::ApiErr) -> (u16, Value) {
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
