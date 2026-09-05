//! baton-mcp — MCP Server（stdio，newline-delimited JSON-RPC 2.0）
//!
//! 让任何支持 MCP 的 Agent（Claude Code / Kimi / 自研）零适配接入 Baton 看板。
//! 直接内嵌 Db（SQLite WAL 支持 core server 与本进程并发读写同一库文件）。
//!
//! 注册示例（Claude Code）：
//!   claude mcp add baton -- /path/to/baton-mcp
//! 环境变量：
//!   BATON_DB        数据库路径（默认 data/baton.db）
//!   BATON_AGENT_ID  本进程扮演的 Agent 成员 id（默认 a-code）

use baton_core::{ApiErr, Db};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

fn tool_defs() -> Value {
    json!([
        {"name": "board_get", "description": "获取整板状态：列 + 卡片 + 租约快照 + 未结话题数",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "card_list", "description": "列出卡片，可选按列过滤",
         "inputSchema": {"type": "object", "properties": {
             "list_id": {"type": "string", "description": "列 id，缺省返回所有列的卡片"}}}},
        {"name": "card_get", "description": "卡片详情：描述、ext、话题与评论、当前租约",
         "inputSchema": {"type": "object", "required": ["card_id"], "properties": {
             "card_id": {"type": "string"}}}},
        {"name": "card_create", "description": "创建卡片（进入 Ready 列，自动建主讨论话题）",
         "inputSchema": {"type": "object", "required": ["title"], "properties": {
             "title": {"type": "string"},
             "description": {"type": "string", "description": "Markdown 描述"}}}},
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
        {"name": "card_comment", "description": "在卡片话题下写评论（chat/progress/system/handoff/approval）",
         "inputSchema": {"type": "object", "required": ["card_id", "body"], "properties": {
             "card_id": {"type": "string"},
             "body": {"type": "string", "description": "Markdown"},
             "kind": {"type": "string", "enum": ["chat","progress","system","handoff","approval"]}}}},
        {"name": "progress_update", "description": "更新工作进度：percent(0-100) + 一句话 summary；同步写入进度评论",
         "inputSchema": {"type": "object", "required": ["card_id","percent","summary"], "properties": {
             "card_id": {"type": "string"},
             "percent": {"type": "integer", "minimum": 0, "maximum": 100},
             "summary": {"type": "string"}}}}
    ])
}

fn call_tool(db: &Db, agent: &str, name: &str, args: &Value) -> Result<Value, ApiErr> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    match name {
        "board_get" => Ok(db.board_state()?),
        "card_list" => Ok(db.list_cards(args.get("list_id").and_then(Value::as_str))?),
        "card_get" => Ok(db.card_detail(s("card_id"))?),
        "card_create" => Ok(db.create_card(agent, s("title"), s("description"))?),
        "card_claim" => db.claim_card(s("card_id"), agent),
        "card_release" => db.release_card(s("card_id"), agent),
        "card_move" => db.move_card(
            s("card_id"), agent, s("list_id"),
            args.get("rev").and_then(Value::as_i64).unwrap_or(0),
        ),
        "card_comment" => Ok(db.add_comment(
            s("card_id"), agent, s("body"),
            if s("kind").is_empty() { "chat" } else { s("kind") },
        )?),
        "progress_update" => db.update_progress(
            s("card_id"), agent,
            args.get("percent").and_then(Value::as_i64).unwrap_or(0),
            s("summary"),
        ),
        _ => Err(ApiErr::bad_request("unknown tool")),
    }
}

fn main() {
    let db_path = std::env::var("BATON_DB").unwrap_or_else(|_| "data/baton.db".into());
    let agent = std::env::var("BATON_AGENT_ID").unwrap_or_else(|_| "a-code".into());
    let db = Db::open(&db_path).expect("failed to open database");
    eprintln!("baton-mcp ready (db: {}, agent: {})", db_path, agent);

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

        let result: Result<Value, Value> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "baton", "version": "0.1.0"}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_defs()})),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(&db, &agent, name, &args) {
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
}
