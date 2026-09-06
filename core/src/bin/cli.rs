//! baton — CLI（PRD F-203）
//! 直接内嵌 Db（与 core / MCP 共享同一 SQLite，WAL 并发安全）。
//!
//! 用法：
//!   baton board [--board <board_id>]         整板概览（缺省第一个看板）
//!   baton projects                           项目 + 看板列表
//!   baton project create --name N [--desc D] [--template software|content|gtd]  新建项目（F-112）
//!   baton card list [--list <list_id>] [--board <board_id>]
//!   baton card show <id>
//!   baton card create --title T [--desc D] [--board <board_id>]
//!   baton card claim <id> [--as <member>]    默认 --as a-code
//!   baton card release <id> [--as M]
//!   baton card move <id> --to <list_id> [--rev N]   缺省自动取当前 rev
//!   baton card comment <id> <body> [--kind chat|progress]
//!   baton card progress <id> --percent N --summary S
//!   baton card takeover <id> --as u-owner    强制接管（F-405，仅人类）
//!   baton card upload <id> --name N [--path P | --content C] [--kind file|diff|doc|image|log]
//!   baton card artifacts <id>                列出卡片产物
//!   baton card dep <id> [--add <other> | --remove <other>] [--rel blocked_by|blocks|relates_to]
//!   baton card assign <id> [--to <member>]   指派；不带 --to = 放入抢单池（F-303）
//!   baton approvals [--status pending]
//!   baton approve <approval_id> [--note N] / baton reject <id> [--note N]
//!   baton agents                             Agent 面板（在线状态/持有卡片）
//!   baton agent add --name N [--role worker] [--cap a,b]   注册 Agent 并签发 Token（仅人类）
//!   baton agent token <id>                   轮换 Token（仅人类）
//!   baton agent revoke <id>                  吊销（仅人类）
//!   baton heartbeat [--as a-code]            上报心跳（F-213）
//!   baton sessions [--agent <id>]            Session 资源视图
//!   baton session end <id>                   结束会话
//!   baton notifications [--as u-owner] [--since N] [--limit N]   通知中心（F-404）
//!   baton backup [--keep N]                  快照备份到 <工作区>/backups/（F-504，默认留 10 份）
//!   baton export [--project <id>] --out <dir>   导出项目：project.json + cards/*.md + 附件（F-503）
//!   baton import <dir>                       从导出目录导入（幂等，按原 id INSERT OR REPLACE）
//!   baton doctor                             自检
//! 输出均为 JSON（pretty），退出码 0 成功 / 1 失败。

use baton_core::Db;
use serde_json::{json, Value};
use std::process::exit;

fn main() {
    let db_path = baton_core::default_db_path();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", include_str!("cli.rs").lines().take(20).collect::<Vec<_>>().join("\n"));
        exit(2);
    }
    let db = Db::open(&db_path).unwrap_or_else(|e| fail(json!({"error": e.to_string()})));
    let actor = flag(&args, "--as").unwrap_or_else(|| "a-code".into());

    let result: Result<Value, Value> = match args[0].as_str() {
        "doctor" => db.board_state(None).map(|b| json!({
            "db": db_path, "board_ok": true,
            "lists": b["lists"].as_array().map(|l| l.len()).unwrap_or(0),
        })).map_err(|e| json!({"error": e.to_string()})),

        "board" => db.board_state(flag(&args, "--board").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),

        "projects" => db.list_projects().map_err(|e| json!({"error": e.to_string()})),

        "project" => match args.get(1).map(String::as_str) {
            Some("create") => {
                let name = flag(&args, "--name").unwrap_or_else(|| "未命名项目".into());
                let desc = flag(&args, "--desc").unwrap_or_default();
                let tpl = flag(&args, "--template").unwrap_or_else(|| "software".into());
                db.create_project("u-owner", &name, &desc, &tpl).map_err(|e| e.body)
            }
            _ => Err(json!({"error": "unknown project subcommand (create)"})),
        },

        "approvals" => db.list_approvals(flag(&args, "--status").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),

        "approve" | "reject" => {
            let id = need(&args, 1, "<approval_id>");
            db.decide_approval(id, &flag(&args, "--as").unwrap_or_else(|| "u-owner".into()),
                if args[0] == "approve" { "approved" } else { "rejected" },
                &flag(&args, "--note").unwrap_or_default())
                .map_err(|e| e.body)
        }

        "card" => card_cmd(&db, &args, &actor),

        "agents" => db.list_agents().map_err(|e| json!({"error": e.to_string()})),

        "agent" => match args.get(1).map(String::as_str) {
            Some("add") => {
                let name = flag(&args, "--name").unwrap_or_else(|| "agent".into());
                let role = flag(&args, "--role").unwrap_or_else(|| "worker".into());
                let caps: Vec<String> = flag(&args, "--cap")
                    .map(|c| c.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                    .unwrap_or_default();
                // agent 管理仅人类可操作，固定以 u-owner 身份执行
                db.create_agent("u-owner", &name, &role, caps).map_err(|e| e.body)
            }
            Some("token") => db.rotate_agent_token("u-owner", need(&args, 2, "<id>")).map_err(|e| e.body),
            Some("revoke") => db.revoke_agent("u-owner", need(&args, 2, "<id>")).map_err(|e| e.body),
            _ => Err(json!({"error": "unknown agent subcommand (add|token|revoke)"})),
        },

        "heartbeat" => db.heartbeat(&actor).map_err(|e| e.body),

        "sessions" => db.list_sessions(flag(&args, "--agent").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),

        "session" => match args.get(1).map(String::as_str) {
            Some("end") => db.session_end(need(&args, 2, "<id>")).map_err(|e| e.body),
            _ => Err(json!({"error": "unknown session subcommand (end)"})),
        },

        "notifications" => db.notifications(
            &actor,
            flag(&args, "--since").and_then(|s| s.parse().ok()).unwrap_or(0),
            flag(&args, "--limit").and_then(|s| s.parse().ok()).unwrap_or(50),
        ).map_err(|e| json!({"error": e.to_string()})),

        "backup" => db.backup(
            flag(&args, "--keep").and_then(|s| s.parse().ok()).unwrap_or(10),
        ).map_err(|e| e.body),

        "export" => export_cmd(&db, &args),

        "import" => import_cmd(&db, &args),

        other => Err(json!({"error": format!("unknown command: {}", other)})),
    };

    match result {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => fail(e),
    }
}

fn card_cmd(db: &Db, args: &[String], actor: &str) -> Result<Value, Value> {
    match args.get(1).map(String::as_str) {
        Some("list") => db.list_cards(flag(args, "--list").as_deref(), flag(args, "--board").as_deref())
            .map_err(|e| json!({"error": e.to_string()})),
        Some("show") => db.card_detail(need(args, 2, "<id>"))
            .map_err(|e| json!({"error": e.to_string()})),
        Some("create") => {
            let title = flag(args, "--title").unwrap_or_else(|| "未命名卡片".into());
            let desc = flag(args, "--desc").unwrap_or_default();
            db.create_card(actor, &title, &desc, flag(args, "--board").as_deref(),
                flag(args, "--parent").as_deref())
                .map_err(|e| json!({"error": e.to_string()}))
        }
        Some("claim") => db.claim_card(need(args, 2, "<id>"), actor,
            flag(args, "--session").as_deref()).map_err(|e| e.body),
        Some("release") => db.release_card(need(args, 2, "<id>"), actor).map_err(|e| e.body),
        Some("join") => db.join_card(need(args, 2, "<id>"), actor,
            flag(args, "--session").as_deref()).map_err(|e| e.body),
        Some("leave") => db.leave_card(need(args, 2, "<id>"), actor).map_err(|e| e.body),
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
            let reply_to = flag(args, "--reply-to");
            let thread_id = flag(args, "--thread");
            db.add_comment(id, actor, body, &kind, reply_to.as_deref(), thread_id.as_deref()).map_err(|e| e.body)
        }
        Some("progress") => {
            let id = need(args, 2, "<id>");
            let percent = flag(args, "--percent").and_then(|s| s.parse().ok()).unwrap_or(0);
            let summary = flag(args, "--summary").unwrap_or_default();
            db.update_progress(id, actor, percent, &summary).map_err(|e| e.body)
        }
        Some("takeover") => {
            // 强制接管（F-405）仅人类可操作：baton card takeover <id> --as u-owner
            db.takeover_card(need(args, 2, "<id>"), actor).map_err(|e| e.body)
        }
        Some("upload") => {
            let id = need(args, 2, "<id>");
            let a = json!({
                "name": flag(args, "--name").unwrap_or_else(|| "artifact".into()),
                "kind": flag(args, "--kind"),
                "path": flag(args, "--path"),
                "content": flag(args, "--content"),
            });
            db.upload_artifact(id, actor, &a).map_err(|e| e.body)
        }
        Some("artifacts") => db.card_detail(need(args, 2, "<id>"))
            .map(|d| d.get("artifacts").cloned().unwrap_or(json!([])))
            .map_err(|e| json!({"error": e.to_string()})),
        Some("dep") => {
            let id = need(args, 2, "<id>");
            let rel = flag(args, "--rel").unwrap_or_else(|| "blocked_by".into());
            if let Some(other) = flag(args, "--add") {
                db.add_dep(id, &other, &rel, actor).map_err(|e| e.body)
            } else if let Some(other) = flag(args, "--remove") {
                db.remove_dep(id, &other, &rel, actor).map_err(|e| e.body)
            } else {
                db.card_detail(id)
                    .map(|d| d.get("deps").cloned().unwrap_or(json!([])))
                    .map_err(|e| json!({"error": e.to_string()}))
            }
        }
        // 指派：--to <member>；不带 --to = 放入抢单池（F-303）
        Some("assign") => db.assign_card(need(args, 2, "<id>"), actor, flag(args, "--to").as_deref())
            .map_err(|e| e.body),
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

// ------------------------------------------------------------------ export / import (F-503)

/// 导出：project.json（事实源）+ cards/<id>-<title>.md（人读）+ artifacts/（附件复制）
fn export_cmd(db: &Db, args: &[String]) -> Result<Value, Value> {
    let out = flag(args, "--out").unwrap_or_else(|| "baton-export".into());
    let data = db.export_project(flag(args, "--project").as_deref())
        .map_err(|e| json!({"error": e.to_string()}))?;
    let dir = std::path::Path::new(&out);
    let io = |e: std::io::Error| json!({"error": e.to_string()});
    std::fs::create_dir_all(dir.join("cards")).map_err(io)?;
    std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&data).unwrap())
        .map_err(io)?;
    // Markdown 渲染每张卡
    let pid = data.pointer("/projects/0/id").and_then(Value::as_str).unwrap_or("");
    let mut md_count = 0usize;
    for (cid, title) in db.project_card_ids(pid).map_err(|e| json!({"error": e.to_string()}))? {
        let md = db.card_markdown(&cid).map_err(|e| json!({"error": e.to_string()}))?;
        let slug: String = title.chars()
            .map(|c| if c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) { c } else { '-' })
            .take(30).collect();
        std::fs::write(dir.join("cards").join(format!("{}-{}.md", cid, slug)), md).map_err(io)?;
        md_count += 1;
    }
    // 附件复制（保持相对路径 artifacts/<card_id>/<file>）
    let mut art_count = 0usize;
    if let Some(arts) = data.get("artifacts").and_then(Value::as_array) {
        for a in arts {
            let Some(rel) = a.get("path").and_then(Value::as_str) else { continue };
            let src = db.workspace_dir().join(rel);
            if src.exists() {
                let dst = dir.join(rel);
                if let Some(p) = dst.parent() { std::fs::create_dir_all(p).map_err(io)?; }
                std::fs::copy(&src, &dst).map_err(io)?;
                art_count += 1;
            }
        }
    }
    Ok(json!({"ok": true, "dir": out, "cards_md": md_count, "artifact_files": art_count}))
}

/// 导入：读 project.json 幂等落库 + 附件复制回工作区
fn import_cmd(db: &Db, args: &[String]) -> Result<Value, Value> {
    let dir = std::path::Path::new(need(args, 1, "<dir>"));
    let io = |e: std::io::Error| json!({"error": e.to_string()});
    let raw = std::fs::read_to_string(dir.join("project.json")).map_err(io)?;
    let data: Value = serde_json::from_str(&raw)
        .map_err(|e| json!({"error": format!("project.json 解析失败: {}", e)}))?;
    // 附件复制回工作区（按相对路径）
    let mut art_count = 0usize;
    if let Some(arts) = data.get("artifacts").and_then(Value::as_array) {
        for a in arts {
            let Some(rel) = a.get("path").and_then(Value::as_str) else { continue };
            let src = dir.join(rel);
            if src.exists() {
                let dst = db.workspace_dir().join(rel);
                if let Some(p) = dst.parent() { std::fs::create_dir_all(p).map_err(io)?; }
                std::fs::copy(&src, &dst).map_err(io)?;
                art_count += 1;
            }
        }
    }
    let mut r = db.import_project(&data).map_err(|e| e.body)?;
    r.as_object_mut().unwrap().insert("artifact_files".into(), json!(art_count));
    Ok(r)
}
