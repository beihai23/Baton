//! Baton Core Server — 本地 HTTP API（127.0.0.1:7700）
//! 最小闭环路由：
//!   GET  /api/v1/board                     整板状态
//!   GET  /api/v1/cards/{id}                卡片详情（含话题+评论+租约）
//!   POST /api/v1/cards                     {title, description?, actor?}
//!   POST /api/v1/cards/{id}/claim          {holder}
//!   POST /api/v1/cards/{id}/release        {actor}
//!   POST /api/v1/cards/{id}/comments       {author, body, kind?}
//!   POST /api/v1/cards/{id}/progress       {actor, percent, summary}
//!   POST /api/v1/cards/{id}/move           {actor, list_id, rev}

use baton_core::Db;
use serde_json::{json, Value};
use std::sync::Mutex;
use tiny_http::{Method, Request, Response, Server};

fn main() {
    let db_path = std::env::var("BATON_DB").unwrap_or_else(|_| "data/baton.db".into());
    let addr = std::env::var("BATON_ADDR").unwrap_or_else(|_| "127.0.0.1:7700".into());

    let db = Db::open(&db_path).expect("failed to open database");
    let db = Mutex::new(db);
    let server = Server::http(&addr).expect("failed to bind");
    println!("baton-core listening on http://{}  (db: {})", addr, db_path);

    for mut req in server.incoming_requests() {
        let resp = handle(&db, &mut req);
        let _ = req.respond(resp);
    }
}

fn handle(db: &Mutex<Db>, req: &mut Request) -> Response<std::io::Cursor<Vec<u8>>> {
    let method = req.method().clone();
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/");
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    // seg: ["api","v1", ...]

    let body = read_json(req);
    let db = db.lock().unwrap();

    let result: Result<Value, (u16, Value)> = match (&method, seg.as_slice()) {
        (&Method::Get, ["api", "v1", "board"]) => db.board_state().map_err(|e| (500, err(e))),

        (&Method::Get, ["api", "v1", "cards", id]) => db.card_detail(id).map_err(|e| (500, err(e))),

        (&Method::Post, ["api", "v1", "cards"]) => {
            let title = body.get("title").and_then(Value::as_str).unwrap_or("未命名卡片");
            let desc = body.get("description").and_then(Value::as_str).unwrap_or("");
            let actor = body.get("actor").and_then(Value::as_str).unwrap_or("u-owner");
            db.create_card(actor, title, desc).map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "cards", id, "claim"]) => {
            let holder = body.get("holder").and_then(Value::as_str).unwrap_or("a-code");
            db.claim_card(id, holder).map_err(|e| (e.status, e.body))
        }

        (&Method::Post, ["api", "v1", "cards", id, "release"]) => {
            let actor = body.get("actor").and_then(Value::as_str).unwrap_or("u-owner");
            db.release_card(id, actor).map_err(|e| (e.status, e.body))
        }

        (&Method::Post, ["api", "v1", "cards", id, "comments"]) => {
            let author = body.get("author").and_then(Value::as_str).unwrap_or("u-owner");
            let text = body.get("body").and_then(Value::as_str).unwrap_or("");
            let kind = body.get("kind").and_then(Value::as_str).unwrap_or("chat");
            db.add_comment(id, author, text, kind).map_err(|e| (500, err(e)))
        }

        (&Method::Post, ["api", "v1", "cards", id, "progress"]) => {
            let actor = body.get("actor").and_then(Value::as_str).unwrap_or("a-code");
            let percent = body.get("percent").and_then(Value::as_i64).unwrap_or(0);
            let summary = body.get("summary").and_then(Value::as_str).unwrap_or("");
            db.update_progress(id, actor, percent, summary).map_err(|e| (e.status, e.body))
        }

        (&Method::Post, ["api", "v1", "cards", id, "move"]) => {
            let actor = body.get("actor").and_then(Value::as_str).unwrap_or("u-owner");
            let list = body.get("list_id").and_then(Value::as_str).unwrap_or("");
            let rev = body.get("rev").and_then(Value::as_i64).unwrap_or(0);
            db.move_card(id, actor, list, rev).map_err(|e| (e.status, e.body))
        }

        _ => Err((404, json!({"error": "not found", "path": path}))),
    };

    respond(result)
}

fn read_json(req: &mut Request) -> Value {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(json!({}))
}

fn err(e: rusqlite::Error) -> Value {
    json!({"error": e.to_string()})
}

fn respond(result: Result<Value, (u16, Value)>) -> Response<std::io::Cursor<Vec<u8>>> {
    let (status, body) = match result {
        Ok(v) => (200, v),
        Err((s, v)) => (s, v),
    };
    let data = body.to_string().into_bytes();
    Response::from_data(data)
        .with_status_code(status)
        .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
        .with_header("Access-Control-Allow-Origin: *".parse::<tiny_http::Header>().unwrap())
}
