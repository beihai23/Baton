//! Baton Core — 数据层（SQLite）
//! 对齐 contract/schema.sql 与 PRD §4.6。最小闭环范围：卡片 CRUD、claim 租约、
//! 评论（多话题）、乐观锁移列、事件日志。

use rusqlite::{params, Connection, Result as SqlResult};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = include_str!("../../contract/schema.sql");

/// 生成简洁唯一 id：时间戳(hex) + 自增(hex)，骨架阶段替代 ULID。
fn new_id(prefix: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{}-{:x}{:04x}", prefix, ts, SEQ.fetch_add(1, Ordering::Relaxed))
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &str) -> SqlResult<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        let db = Db { conn };
        db.seed()?;
        Ok(db)
    }

    /// 首次启动播种：演示项目/看板/四列 + 人类 Owner + code-agent
    fn seed(&self) -> SqlResult<()> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(());
        }
        self.conn.execute_batch(
            "INSERT INTO members(id,kind,name,role) VALUES
                ('u-owner','human','Lance','owner'),
                ('a-code','agent','code-agent','worker'),
                ('a-review','agent','review-agent','observer');
             INSERT INTO projects(id,name,description) VALUES
                ('p-demo','演示项目','MVP 最小闭环演示');
             INSERT INTO boards(id,project_id,name) VALUES('b-main','p-demo','开发板');
             INSERT INTO lists(id,board_id,name,position,policy_json) VALUES
                ('l-ready','b-main','Ready',0,'{}'),
                ('l-doing','b-main','In Progress',1,'{\"require_progress_summary\":true}'),
                ('l-review','b-main','Review',2,'{\"require_approval\":\"human\"}'),
                ('l-done','b-main','Done',3,'{}');",
        )?;
        Ok(())
    }

    fn log_event(&self, actor: &str, entity: &str, entity_id: &str, action: &str, payload: Value) {
        let _ = self.conn.execute(
            "INSERT INTO events(actor_id,entity,entity_id,action,payload_json) VALUES(?1,?2,?3,?4,?5)",
            params![actor, entity, entity_id, action, payload.to_string()],
        );
    }

    // ------------------------------------------------------------------ board

    /// 整板状态：列 + 卡（含租约快照、未结话题数、进度）
    pub fn board_state(&self) -> SqlResult<Value> {
        let mut lists = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT id,name,position,wip_limit,policy_json FROM lists
             WHERE archived_at IS NULL ORDER BY position",
        )?;
        let list_rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for lr in list_rows {
            let (lid, lname, lpos, wip, policy) = lr?;
            let cards = self.cards_in_list(&lid)?;
            lists.push(json!({
                "id": lid, "name": lname, "position": lpos,
                "wip_limit": wip, "policy": serde_json::from_str::<Value>(&policy).unwrap_or(json!({})),
                "cards": cards,
            }));
        }
        Ok(json!({ "board_id": "b-main", "lists": lists }))
    }

    fn cards_in_list(&self, list_id: &str) -> SqlResult<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id,c.title,c.priority,c.assignee_id,c.rev,c.progress_percent,
                    c.handoff_state, cl.holder_id, cl.lease_until,
                    (SELECT COUNT(*) FROM threads t WHERE t.card_id=c.id AND t.status='open')
             FROM cards c LEFT JOIN claims cl ON cl.card_id=c.id
             WHERE c.list_id=?1 AND c.archived_at IS NULL
             ORDER BY c.position, c.created_at",
        )?;
        let rows = stmt.query_map(params![list_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "priority": r.get::<_, String>(2)?,
                "assignee_id": r.get::<_, Option<String>>(3)?,
                "rev": r.get::<_, i64>(4)?,
                "progress_percent": r.get::<_, Option<i64>>(5)?,
                "handoff_state": r.get::<_, Option<String>>(6)?,
                "claim": match r.get::<_, Option<String>>(7)? {
                    Some(h) => json!({"holder_id": h, "lease_until": r.get::<_, String>(8)?}),
                    None => Value::Null,
                },
                "open_threads": r.get::<_, i64>(9)?,
            }))
        })?;
        rows.collect()
    }

    // ------------------------------------------------------------------ card

    pub fn create_card(&self, actor: &str, title: &str, description: &str) -> SqlResult<Value> {
        let id = new_id("c");
        let ext = json!({"schema_rev":1});
        self.conn.execute(
            "INSERT INTO cards(id,project_id,board_id,list_id,title,description,created_by,ext_json)
             VALUES(?1,'p-demo','b-main','l-ready',?2,?3,?4,?5)",
            params![id, title, description, actor, ext.to_string()],
        )?;
        // 每张卡自带一个主讨论话题
        self.conn.execute(
            "INSERT INTO threads(id,card_id,title,created_by) VALUES(?1,?2,'主讨论',?3)",
            params![new_id("t"), id, actor],
        )?;
        self.log_event(actor, "card", &id, "create", json!({"title": title}));
        self.card_detail(&id)
    }

    pub fn card_detail(&self, id: &str) -> SqlResult<Value> {
        let card = self.conn.query_row(
            "SELECT id,list_id,title,description,rev,priority,assignee_id,ext_json,
                    created_by,created_at,updated_at
             FROM cards WHERE id=?1",
            params![id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "list_id": r.get::<_, String>(1)?,
                    "title": r.get::<_, String>(2)?,
                    "description": r.get::<_, String>(3)?,
                    "rev": r.get::<_, i64>(4)?,
                    "priority": r.get::<_, String>(5)?,
                    "assignee_id": r.get::<_, Option<String>>(6)?,
                    "ext": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(json!({})),
                    "created_by": r.get::<_, String>(8)?,
                    "created_at": r.get::<_, String>(9)?,
                    "updated_at": r.get::<_, String>(10)?,
                }))
            },
        )?;
        let mut obj = card.as_object().unwrap().clone();
        obj.insert("claim".into(), self.active_claim(id)?);
        obj.insert("threads".into(), self.threads_with_comments(id)?);
        Ok(Value::Object(obj))
    }

    fn active_claim(&self, card_id: &str) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT holder_id, lease_until FROM claims
             WHERE card_id=?1 AND lease_until > datetime('now')",
        )?;
        let mut rows = stmt.query(params![card_id])?;
        if let Some(r) = rows.next()? {
            Ok(json!({"holder_id": r.get::<_, String>(0)?, "lease_until": r.get::<_, String>(1)?}))
        } else {
            Ok(Value::Null)
        }
    }

    fn threads_with_comments(&self, card_id: &str) -> SqlResult<Value> {
        let mut ts = self.conn.prepare(
            "SELECT id,title,status FROM threads WHERE card_id=?1 ORDER BY created_at",
        )?;
        let threads: Vec<(String, Option<String>, String)> = ts
            .query_map(params![card_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<SqlResult<_>>()?;
        let mut out = Vec::new();
        for (tid, title, status) in threads {
            let mut cs = self.conn.prepare(
                "SELECT id,author_id,kind,body,created_at FROM comments
                 WHERE thread_id=?1 ORDER BY created_at",
            )?;
            let comments: Vec<Value> = cs
                .query_map(params![tid], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "author_id": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "body": r.get::<_, String>(3)?,
                        "created_at": r.get::<_, String>(4)?,
                    }))
                })?
                .collect::<SqlResult<_>>()?;
            out.push(json!({"id": tid, "title": title, "status": status, "comments": comments}));
        }
        Ok(Value::Array(out))
    }

    // ------------------------------------------------------------------ claim

    /// 认领：已有未过期租约 → 409；成功写入 30min 租约
    pub fn claim_card(&self, card_id: &str, holder: &str) -> Result<Value, ApiErr> {
        let existing = self.active_claim(card_id)?;
        if let Value::Object(c) = &existing {
            return Err(ApiErr::conflict(json!({
                "error": "card already claimed",
                "claim": Value::Object(c.clone()),
            })));
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO claims(card_id,holder_id,lease_until)
             VALUES(?1,?2,datetime('now','+30 minutes'))",
            params![card_id, holder],
        )?;
        self.conn.execute(
            "INSERT INTO comments(id,card_id,thread_id,author_id,kind,body)
             SELECT ?1,?2,t.id,?3,'system',?4 FROM threads t
             WHERE t.card_id=?2 ORDER BY t.created_at LIMIT 1",
            params![new_id("cm"), card_id, holder, format!("{} 认领了此卡片（租约 30 分钟）", holder)],
        )?;
        self.log_event(holder, "card", card_id, "claim", json!({"holder": holder}));
        Ok(json!({"ok": true, "claim": self.active_claim(card_id)?}))
    }

    pub fn release_card(&self, card_id: &str, actor: &str) -> Result<Value, ApiErr> {
        self.conn
            .execute("DELETE FROM claims WHERE card_id=?1", params![card_id])?;
        self.log_event(actor, "card", card_id, "release", json!({}));
        Ok(json!({"ok": true}))
    }

    // ---------------------------------------------------------------- comment

    pub fn add_comment(&self, card_id: &str, author: &str, body: &str, kind: &str) -> SqlResult<Value> {
        let id = new_id("cm");
        // 未指定话题时写入卡片第一个话题（骨架简化）
        self.conn.execute(
            "INSERT INTO comments(id,card_id,thread_id,author_id,kind,body)
             SELECT ?1,?2,t.id,?3,?4,?5 FROM threads t
             WHERE t.card_id=?2 ORDER BY t.created_at LIMIT 1",
            params![id, card_id, author, kind, body],
        )?;
        self.log_event(author, "comment", &id, "create", json!({"card_id": card_id, "kind": kind}));
        self.card_detail(card_id)
    }

    // ------------------------------------------------------------------ move

    /// 乐观锁移列：rev 不匹配 → 409；有活跃租约时仅 holder 可移动
    pub fn move_card(&self, card_id: &str, actor: &str, list_id: &str, rev: i64) -> Result<Value, ApiErr> {
        if let Value::Object(c) = self.active_claim(card_id)? {
            if c.get("holder_id").and_then(Value::as_str) != Some(actor) {
                return Err(ApiErr::conflict(json!({
                    "error": "card is claimed by another member",
                    "claim": Value::Object(c),
                })));
            }
        }
        let n = self.conn.execute(
            "UPDATE cards SET list_id=?1, rev=rev+1,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?2 AND rev=?3",
            params![list_id, card_id, rev],
        )?;
        if n == 0 {
            let cur: i64 = self.conn.query_row(
                "SELECT rev FROM cards WHERE id=?1", params![card_id], |r| r.get(0),
            )?;
            return Err(ApiErr::conflict(json!({
                "error": "rev conflict",
                "current_rev": cur,
            })));
        }
        self.log_event(actor, "card", card_id, "move", json!({"to": list_id, "rev": rev}));
        Ok(self.card_detail(card_id)?)
    }

    // ------------------------------------------------------------------ ext

    /// 更新进度（ext.progress 局部更新）
    pub fn update_progress(&self, card_id: &str, actor: &str, percent: i64, summary: &str) -> Result<Value, ApiErr> {
        let ext_patch = json!({
            "percent": percent, "summary": summary,
            "updated_by": {"kind":"agent","id":actor,"name":actor},
            "updated_at": "now",
        });
        self.conn.execute(
            "UPDATE cards SET ext_json=json_set(ext_json,'$.progress',json(?1)),
                rev=rev+1, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?2",
            params![ext_patch.to_string(), card_id],
        )?;
        self.add_comment(card_id, actor, summary, "progress")?;
        self.log_event(actor, "card", card_id, "progress", json!({"percent": percent}));
        Ok(self.card_detail(card_id)?)
    }
}

// ------------------------------------------------------------------ errors

pub struct ApiErr {
    pub status: u16,
    pub body: Value,
}

impl ApiErr {
    pub fn conflict(body: Value) -> Self {
        ApiErr { status: 409, body }
    }
    #[allow(dead_code)]
    pub fn bad_request(msg: &str) -> Self {
        ApiErr { status: 400, body: json!({"error": msg}) }
    }
}

impl From<rusqlite::Error> for ApiErr {
    fn from(e: rusqlite::Error) -> Self {
        ApiErr { status: 500, body: json!({"error": e.to_string()}) }
    }
}

pub fn empty_ext() -> Map<String, Value> {
    Map::new()
}

impl Db {
    /// 列出卡片：可选按列过滤（供 MCP / API 的 card_list）
    pub fn list_cards(&self, list_id: Option<&str>) -> SqlResult<Value> {
        match list_id {
            Some(l) => Ok(Value::Array(self.cards_in_list(l)?)),
            None => {
                let mut all = Vec::new();
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM lists WHERE archived_at IS NULL ORDER BY position",
                )?;
                let ids: Vec<String> = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<SqlResult<_>>()?;
                for lid in ids {
                    all.extend(self.cards_in_list(&lid)?);
                }
                Ok(Value::Array(all))
            }
        }
    }
}
