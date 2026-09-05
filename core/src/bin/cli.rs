//! baton — CLI（PRD F-203）
//! 直接内嵌 Db（与 core / MCP 共享同一 SQLite，WAL 并发安全）。
//!
//! 用法：
//!   baton board                              整板概览
//!   baton card list [--list <list_id>]
//!   baton card show <id>
//!   baton card create --title T [--desc D]
//!   baton card claim <id> [--as <member>]    默认 --as a-code
//!   baton card release <id> [--as M]
//!   baton card move <id> --to <list_id> [--rev N]   缺省自动取当前 rev
//!   baton card comment <id> <body> [--kind chat|progress]
//!   baton card progress <id> --percent N --summary S
//!   baton approvals [--status pending]
//!   baton approve <approval_id> [--note N] / baton reject <id> [--note N]
//!   baton doctor                             自检
//! 输出均为 JSON（pretty），退出码 0 成功 / 1 失败。

use baton_core::Db;
use serde_json::{json, Value};
use std::process::exit;

fn main() {
    let db_path = std::env::var("BATON_DB").unwrap_or_else(|_| "data/baton.db".into());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", include_str!("cli.rs").lines().take(20).collect::<Vec<_>>().join("\n"));
        exit(2);
    }
    let db = Db::open(&db_path).unwrap_or_else(|e| fail(json!({"error": e.to_string()})));
    let actor = flag(&args, "--as").unwrap_or_else(|| "a-code".into());

    let result: Result<Value, Value> = match args[0].as_str() {
        "doctor" => db.board_state().map(|b| json!({
            "db": db_path, "board_ok": true,
            "lists": b["lists"].as_array().map(|l| l.len()).unwrap_or(0),
        })).map_err(|e| json!({"error": e.to_string()})),

        "board" => db.board_state().map_err(|e| json!({"error": e.to_string()})),

        "approvals" => db.list_approvals(flag(&args, "--status").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),

        "approve" | "reject" => {
            let id = need(&args, 1, "<approval_id>");
            db.decide_approval(id, "u-owner",
                if args[0] == "approve" { "approved" } else { "rejected" },
                &flag(&args, "--note").unwrap_or_default())
                .map_err(|e| e.body)
        }

        "card" => card_cmd(&db, &args, &actor),
        other => Err(json!({"error": format!("unknown command: {}", other)})),
    };

    match result {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => fail(e),
    }
}

fn card_cmd(db: &Db, args: &[String], actor: &str) -> Result<Value, Value> {
    match args.get(1).map(String::as_str) {
        Some("list") => db.list_cards(flag(args, "--list").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),
        Some("show") => db.card_detail(need(args, 2, "<id>"))
            .map_err(|e| json!({"error": e.to_string()})),
        Some("create") => {
            let title = flag(args, "--title").unwrap_or_else(|| "未命名卡片".into());
            let desc = flag(args, "--desc").unwrap_or_default();
            db.create_card(actor, &title, &desc).map_err(|e| json!({"error": e.to_string()}))
        }
        Some("claim") => db.claim_card(need(args, 2, "<id>"), actor).map_err(|e| e.body),
        Some("release") => db.release_card(need(args, 2, "<id>"), actor).map_err(|e| e.body),
        Some("move") => {
            let id = need(args, 2, "<id>");
            let to = flag(args, "--to").ok_or_else(|| json!({"error": "missing --to <list_id>"}))?;
            let rev = match flag(args, "--rev").and_then(|s| s.parse().ok()) {
                Some(r) => r,
                None => db.card_detail(id).ok()
                    .and_then(|c| c.get("rev").and_then(Value::as_i64))
                    .unwrap_or(0),
            };
            db.move_card(id, actor, &to, rev).map_err(|e| e.body)
        }
        Some("comment") => {
            let id = need(args, 2, "<id>");
            let body = need(args, 3, "<body>");
            let kind = flag(args, "--kind").unwrap_or_else(|| "chat".into());
            db.add_comment(id, actor, body, &kind).map_err(|e| json!({"error": e.to_string()}))
        }
        Some("progress") => {
            let id = need(args, 2, "<id>");
            let percent = flag(args, "--percent").and_then(|s| s.parse().ok()).unwrap_or(0);
            let summary = flag(args, "--summary").unwrap_or_default();
            db.update_progress(id, actor, percent, &summary).map_err(|e| e.body)
        }
        _ => Err(json!({"error": "unknown card subcommand"})),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn need<'a>(args: &'a [String], i: usize, what: &str) -> &'a str {
    args.get(i).map(String::as_str).unwrap_or_else(|| fail(json!({"error": format!("missing {}", what)})))
}

fn fail(v: Value) -> ! {
    eprintln!("{}", serde_json::to_string_pretty(&v).unwrap());
    exit(1)
}
