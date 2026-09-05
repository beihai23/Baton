//! Baton Core — 数据层（SQLite）
//! 对齐 contract/schema.sql 与 PRD §4.6。
//! 范围：卡片 CRUD、claim 租约、评论（多话题）、乐观锁移列、列策略引擎、审批流、
//! links / git / worksite / handoff、事件日志 + 事件总线（长轮询推送）。

pub mod server;

use rusqlite::{params, Connection, Result as SqlResult};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
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

// ------------------------------------------------------------------ EventBus

/// 进程内事件广播：每次写事件日志同时推送给 SSE 订阅者。
pub struct EventBus {
    subs: Mutex<Vec<mpsc::Sender<String>>>,
    history: Mutex<std::collections::VecDeque<(i64, String)>>,
}

impl EventBus {
    pub fn new() -> Self {
        EventBus { subs: Mutex::new(Vec::new()), history: Mutex::new(Default::default()) }
    }
    pub fn subscribe(&self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        self.subs.lock().unwrap().push(tx);
        rx
    }
    pub fn publish(&self, msg: &str) {
        // msg 为 JSON，含 seq；入历史（容量 500）并唤醒长轮询订阅者
        let seq = serde_json::from_str::<Value>(msg).ok()
            .and_then(|v| v.get("seq").and_then(Value::as_i64))
            .unwrap_or(0);
        {
            let mut h = self.history.lock().unwrap();
            h.push_back((seq, msg.to_string()));
            while h.len() > 500 { h.pop_front(); }
        }
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|tx| tx.send(msg.to_string()).is_ok());
    }
    /// 长轮询：取 seq 大于 since 的事件；无则最多等待 timeout，有新事件即返回
    pub fn poll(&self, since: i64, timeout: std::time::Duration) -> (Vec<String>, i64) {
        let snapshot = |h: &std::collections::VecDeque<(i64, String)>| -> (Vec<String>, i64) {
            let evs: Vec<String> = h.iter().filter(|(s, _)| *s > since).map(|(_, m)| m.clone()).collect();
            let last = h.back().map(|(s, _)| *s).unwrap_or(since);
            (evs, last)
        };
        {
            let h = self.history.lock().unwrap();
            let (evs, last) = snapshot(&h);
            if !evs.is_empty() { return (evs, last); }
        }
        let rx = self.subscribe();
        let _ = rx.recv_timeout(timeout);   // 唤醒或超时皆可
        let h = self.history.lock().unwrap();
        snapshot(&h)
    }
}

// ------------------------------------------------------------------ Db

pub struct Db {
    conn: Connection,
    bus: Arc<EventBus>,
}

impl Db {
    pub fn open(path: &str) -> SqlResult<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        let db = Db { conn, bus: Arc::new(EventBus::new()) };
        db.seed()?;
        Ok(db)
    }

    pub fn bus(&self) -> Arc<EventBus> {
        self.bus.clone()
    }

    fn now_iso(&self) -> String {
        self.conn
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))
            .unwrap_or_default()
    }

    /// 首次启动播种：演示项目/看板/四列 + 人类 Owner + 两个 Agent
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

    /// 写事件日志（append-only）并广播给 SSE 订阅者
    fn log_event(&self, actor: &str, entity: &str, entity_id: &str, action: &str, payload: Value) {
        let ok = self.conn.execute(
            "INSERT INTO events(actor_id,entity,entity_id,action,payload_json) VALUES(?1,?2,?3,?4,?5)",
            params![actor, entity, entity_id, action, payload.to_string()],
        );
        if let Ok(_) = ok {
            let seq: i64 = self.conn.last_insert_rowid();
            self.bus.publish(
                &json!({
                    "seq": seq, "at": self.now_iso(), "actor_id": actor,
                    "entity": entity, "entity_id": entity_id, "action": action,
                })
                .to_string(),
            );
        }
    }

    fn member_kind_role(&self, id: &str) -> SqlResult<(String, String)> {
        self.conn.query_row(
            "SELECT kind, role FROM members WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
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

    /// 列出卡片：可选按列过滤
    pub fn list_cards(&self, list_id: Option<&str>) -> SqlResult<Value> {
        match list_id {
            Some(l) => Ok(Value::Array(self.cards_in_list(l)?)),
            None => {
                let mut all = Vec::new();
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM lists WHERE archived_at IS NULL ORDER BY position",
                )?;
                let ids: Vec<String> =
                    stmt.query_map([], |r| r.get(0))?.collect::<SqlResult<_>>()?;
                for lid in ids {
                    all.extend(self.cards_in_list(&lid)?);
                }
                Ok(Value::Array(all))
            }
        }
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
        obj.insert("links".into(), self.card_links(id)?);
        obj.insert("work_nodes".into(), self.card_work_nodes(id)?);
        obj.insert("handoff".into(), self.card_handoff(id)?);
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
        self.sys_comment(card_id, holder, &format!("{} 认领了此卡片（租约 30 分钟）", holder))?;
        self.log_event(holder, "card", card_id, "claim", json!({"holder": holder}));
        Ok(json!({"ok": true, "claim": self.active_claim(card_id)?}))
    }

    pub fn release_card(&self, card_id: &str, actor: &str) -> Result<Value, ApiErr> {
        self.conn
            .execute("DELETE FROM claims WHERE card_id=?1", params![card_id])?;
        self.log_event(actor, "card", card_id, "release", json!({}));
        Ok(json!({"ok": true}))
    }

    fn sys_comment(&self, card_id: &str, actor: &str, body: &str) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO comments(id,card_id,thread_id,author_id,kind,body)
             SELECT ?1,?2,t.id,?3,'system',?4 FROM threads t
             WHERE t.card_id=?2 ORDER BY t.created_at LIMIT 1",
            params![new_id("cm"), card_id, actor, body],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- comment

    pub fn add_comment(&self, card_id: &str, author: &str, body: &str, kind: &str) -> SqlResult<Value> {
        let id = new_id("cm");
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

    /// 乐观锁移列 + 列策略引擎：
    /// - rev 不匹配 → 409
    /// - 有活跃租约时仅 holder 可移动 → 409
    /// - 目标列 require_progress_summary → ext.progress.summary 必填，否则 400
    /// - 目标列 require_approval=human 且操作者非人类 Owner → 创建审批单，不移动（202 语义）
    pub fn move_card(&self, card_id: &str, actor: &str, list_id: &str, rev: i64) -> Result<Value, ApiErr> {
        if let Value::Object(c) = self.active_claim(card_id)? {
            if c.get("holder_id").and_then(Value::as_str) != Some(actor) {
                return Err(ApiErr::conflict(json!({
                    "error": "card is claimed by another member",
                    "claim": Value::Object(c),
                })));
            }
        }

        // 列策略
        let policy: Value = self.conn.query_row(
            "SELECT policy_json FROM lists WHERE id=?1", params![list_id],
            |r| r.get::<_, String>(0),
        ).map(|s| serde_json::from_str(&s).unwrap_or(json!({})))?;

        if policy.get("require_progress_summary").and_then(Value::as_bool) == Some(true) {
            let summary: Option<String> = self.conn.query_row(
                "SELECT json_extract(ext_json,'$.progress.summary') FROM cards WHERE id=?1",
                params![card_id], |r| r.get(0),
            )?;
            if summary.as_deref().unwrap_or("").trim().is_empty() {
                return Err(ApiErr::bad_request(
                    "column policy: progress.summary is required before entering this list",
                ));
            }
        }

        if policy.get("require_approval").and_then(Value::as_str) == Some("human") {
            let (kind, role) = self.member_kind_role(actor)?;
            if !(kind == "human" && role == "owner") {
                let aid = new_id("ap");
                self.conn.execute(
                    "INSERT INTO approvals(id,card_id,list_id,requested_by) VALUES(?1,?2,?3,?4)",
                    params![aid, card_id, list_id, actor],
                )?;
                self.sys_comment(card_id, actor, "请求进入需人工审批的列，已提交审批单")?;
                self.log_event(actor, "approval", &aid, "request",
                    json!({"card_id": card_id, "list_id": list_id}));
                return Ok(json!({"approval_pending": aid, "card_id": card_id}));
            }
        }

        self.do_move(card_id, actor, list_id, Some(rev))
    }

    /// 实际移列（rev=None 表示审批授权后的强制移动，跳过乐观锁）
    fn do_move(&self, card_id: &str, actor: &str, list_id: &str, rev: Option<i64>) -> Result<Value, ApiErr> {
        let n = match rev {
            Some(r) => self.conn.execute(
                "UPDATE cards SET list_id=?1, rev=rev+1,
                    updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id=?2 AND rev=?3",
                params![list_id, card_id, r],
            )?,
            None => self.conn.execute(
                "UPDATE cards SET list_id=?1, rev=rev+1,
                    updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id=?2",
                params![list_id, card_id],
            )?,
        };
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

    // ---------------------------------------------------------------- approval

    pub fn list_approvals(&self, status: Option<&str>) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id,a.card_id,a.list_id,a.requested_by,a.status,a.note,a.created_at,
                    c.title
             FROM approvals a JOIN cards c ON c.id=a.card_id
             WHERE (?1 IS NULL OR a.status=?1) ORDER BY a.created_at DESC",
        )?;
        let rows = stmt.query_map(params![status], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "card_id": r.get::<_, String>(1)?,
                "list_id": r.get::<_, String>(2)?,
                "requested_by": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?,
                "note": r.get::<_, Option<String>>(5)?,
                "created_at": r.get::<_, String>(6)?,
                "card_title": r.get::<_, String>(7)?,
            }))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    /// 审批决定：approved → 强制移列（授权跳过租约与乐观锁）；rejected → 记录
    pub fn decide_approval(&self, approval_id: &str, actor: &str, decision: &str, note: &str) -> Result<Value, ApiErr> {
        if decision != "approved" && decision != "rejected" {
            return Err(ApiErr::bad_request("decision must be approved|rejected"));
        }
        let (card_id, list_id, status): (String, String, String) = self.conn.query_row(
            "SELECT card_id,list_id,status FROM approvals WHERE id=?1",
            params![approval_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if status != "pending" {
            return Err(ApiErr::conflict(json!({"error": "approval already decided", "status": status})));
        }
        self.conn.execute(
            "UPDATE approvals SET status=?1, note=?2, decided_by=?3,
                decided_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?4",
            params![decision, note, actor, approval_id],
        )?;
        self.log_event(actor, "approval", approval_id, decision, json!({"card_id": card_id}));
        self.sys_comment(&card_id, actor,
            &format!("审批{}：{}", if decision == "approved" { "通过" } else { "打回" }, note))?;
        if decision == "approved" {
            self.do_move(&card_id, actor, &list_id, None)?;
        }
        Ok(json!({"ok": true, "decision": decision, "card_id": card_id}))
    }

    // ------------------------------------------------------------------ ext

    /// 更新进度（ext.progress 局部更新）
    pub fn update_progress(&self, card_id: &str, actor: &str, percent: i64, summary: &str) -> Result<Value, ApiErr> {
        let ext_patch = json!({
            "percent": percent, "summary": summary,
            "updated_by": {"kind":"agent","id":actor,"name":actor},
            "updated_at": self.now_iso(),
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

    /// 读-改-写 ext_json（rev 递增）
    fn patch_ext<F>(&self, card_id: &str, f: F) -> Result<Value, ApiErr>
    where F: FnOnce(&mut Value) {
        let raw: String = self.conn.query_row(
            "SELECT ext_json FROM cards WHERE id=?1", params![card_id], |r| r.get(0),
        )?;
        let mut ext: Value = serde_json::from_str(&raw).unwrap_or(json!({"schema_rev":1}));
        f(&mut ext);
        self.conn.execute(
            "UPDATE cards SET ext_json=?1, rev=rev+1,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
            params![ext.to_string(), card_id],
        )?;
        Ok(self.card_detail(card_id)?)
    }

    // ---- links ----

    pub fn add_link(&self, card_id: &str, actor: &str, args: &Value) -> Result<Value, ApiErr> {
        let id = new_id("lk");
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        self.conn.execute(
            "INSERT INTO links(id,card_id,category,system,key,relation,synced_at,kind,url,path,artifact_id,title,created_by)
             VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,?8,?9,NULL,?10,?11)",
            params![
                id, card_id,
                s("category").unwrap_or("doc"),
                s("system"), s("key"), s("relation"),
                s("kind"), s("url"), s("path"),
                s("title").unwrap_or(""), actor,
            ],
        )?;
        self.log_event(actor, "link", &id, "create", json!({"card_id": card_id}));
        Ok(self.card_detail(card_id)?)
    }

    pub fn delete_link(&self, link_id: &str, actor: &str) -> Result<Value, ApiErr> {
        self.conn.execute("DELETE FROM links WHERE id=?1", params![link_id])?;
        self.log_event(actor, "link", link_id, "delete", json!({}));
        Ok(json!({"ok": true}))
    }

    fn card_links(&self, card_id: &str) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,category,system,key,relation,kind,url,path,title
             FROM links WHERE card_id=?1 ORDER BY position, created_at",
        )?;
        let rows = stmt.query_map(params![card_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "category": r.get::<_, String>(1)?,
                "system": r.get::<_, Option<String>>(2)?,
                "key": r.get::<_, Option<String>>(3)?,
                "relation": r.get::<_, Option<String>>(4)?,
                "kind": r.get::<_, Option<String>>(5)?,
                "url": r.get::<_, Option<String>>(6)?,
                "path": r.get::<_, Option<String>>(7)?,
                "title": r.get::<_, String>(8)?,
            }))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    // ---- git ----

    pub fn git_attach(&self, card_id: &str, actor: &str, repo_path: &str, branch: &str, base_branch: Option<&str>) -> Result<Value, ApiErr> {
        self.patch_ext(card_id, |ext| {
            let repos = ext.pointer_mut("/git/repos")
                .and_then(Value::as_array_mut);
            let repo = json!({
                "repo_path": repo_path, "branch": branch,
                "declared": { "base_branch": base_branch },
            });
            match repos {
                Some(arr) => arr.push(repo),
                None => { ext["git"] = json!({"repos": [repo]}); }
            }
        })?;
        self.log_event(actor, "card", card_id, "git_attach", json!({"repo_path": repo_path, "branch": branch}));
        self.card_detail(card_id).map_err(ApiErr::from)
    }

    /// 探测真实 git 状态（git status --porcelain -b + git log -1），写入 observed 快照
    pub fn git_refresh(&self, card_id: &str, actor: &str) -> Result<Value, ApiErr> {
        let now = self.now_iso();
        let raw: String = self.conn.query_row(
            "SELECT ext_json FROM cards WHERE id=?1", params![card_id], |r| r.get(0),
        )?;
        let ext: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let paths: Vec<String> = ext
            .pointer("/git/repos").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|r| r.get("repo_path").and_then(Value::as_str).map(String::from)).collect())
            .unwrap_or_default();
        let mut snaps = Vec::new();
        for p in &paths {
            snaps.push(probe_git(p, actor, &now));
        }
        self.patch_ext(card_id, |ext| {
            if let Some(arr) = ext.pointer_mut("/git/repos").and_then(Value::as_array_mut) {
                for (i, snap) in snaps.iter().enumerate() {
                    if let Some(repo) = arr.get_mut(i) {
                        repo["observed"] = snap.clone();
                    }
                }
            }
        })?;
        self.log_event(actor, "card", card_id, "git_refresh", json!({"repos": paths}));
        self.card_detail(card_id).map_err(ApiErr::from)
    }

    // ---- worksite ----

    pub fn worksite_add_node(&self, card_id: &str, actor: &str, args: &Value) -> Result<Value, ApiErr> {
        let id = new_id("wn");
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        self.conn.execute(
            "INSERT INTO work_nodes(id,card_id,kind,path,branch,purpose,owner_id,bound_card_id,created_by)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id, card_id,
                s("kind").unwrap_or("worktree"),
                s("path"), s("branch"), s("purpose"),
                s("owner"), s("bound_card_id"), actor,
            ],
        )?;
        // 主节点同步 worksite.root 指针
        if s("kind") == Some("main") {
            let nid = id.clone();
            self.patch_ext(card_id, |ext| {
                ext["worksite"] = json!({"root": nid});
            })?;
        }
        self.log_event(actor, "work_node", &id, "create", json!({"card_id": card_id}));
        Ok(self.card_detail(card_id)?)
    }

    pub fn worksite_remove_node(&self, node_id: &str, actor: &str) -> Result<Value, ApiErr> {
        self.conn.execute(
            "UPDATE work_nodes SET removed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            params![node_id],
        )?;
        self.log_event(actor, "work_node", node_id, "remove", json!({}));
        Ok(json!({"ok": true}))
    }

    fn card_work_nodes(&self, card_id: &str) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,kind,path,branch,purpose,owner_id,bound_card_id,created_at
             FROM work_nodes WHERE card_id=?1 AND removed_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![card_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "path": r.get::<_, String>(2)?,
                "branch": r.get::<_, String>(3)?,
                "purpose": r.get::<_, Option<String>>(4)?,
                "owner_id": r.get::<_, Option<String>>(5)?,
                "bound_card_id": r.get::<_, Option<String>>(6)?,
                "created_at": r.get::<_, String>(7)?,
            }))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    // ---- handoff ----

    pub fn handoff_action(&self, card_id: &str, actor: &str, action: &str, args: &Value) -> Result<Value, ApiErr> {
        let cur_state: String = self.conn.query_row(
            "SELECT COALESCE(json_extract(ext_json,'$.handoff.state'),'none') FROM cards WHERE id=?1",
            params![card_id], |r| r.get(0),
        )?;

        match (cur_state.as_str(), action) {
            ("none", "prepare") | ("preparing", "prepare") => {
                let hid = new_id("hf");
                let s = |k: &str| args.get(k).and_then(Value::as_str);
                let nodes = self.card_work_nodes(card_id)?;
                let open_threads: Vec<String> = {
                    let mut st = self.conn.prepare(
                        "SELECT id FROM threads WHERE card_id=?1 AND status='open'")?;
                    let rows: Vec<String> = st.query_map(params![card_id], |r| r.get(0))?
                        .collect::<SqlResult<_>>()?;
                    rows
                };
                let pkg = json!({
                    "context_note": s("context_note").unwrap_or(""),
                    "worksite_snapshot": {"nodes": nodes},
                    "env_notes": s("env_notes"),
                    "open_threads": open_threads,
                    "artifact_refs": [],
                    "prepared_at": self.now_iso(),
                });
                self.conn.execute(
                    "INSERT INTO handoffs(id,card_id,state,from_id,to_id,reason,package_json)
                     VALUES(?1,?2,'preparing',?3,?4,?5,?6)",
                    params![hid, card_id, actor, s("to"), s("reason"), pkg.to_string()],
                )?;
                self.set_handoff_state(card_id, "preparing", actor, s("to"), s("reason"))?;
                self.handoff_tl(&hid, actor, "prepare", s("context_note"))?;
                self.log_event(actor, "handoff", &hid, "prepare", json!({"card_id": card_id}));
            }
            ("preparing", "ready") => {
                let hid = self.current_handoff_id(card_id)?;
                self.conn.execute("UPDATE handoffs SET state='ready', updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", params![hid])?;
                self.set_handoff_state(card_id, "ready", actor, None, None)?;
                self.release_card(card_id, actor)?;   // 移交方释放租约，卡片可被接手
                self.handoff_tl(&hid, actor, "ready", None)?;
                self.log_event(actor, "handoff", &hid, "ready", json!({"card_id": card_id}));
            }
            ("ready", "accept") => {
                let hid = self.current_handoff_id(card_id)?;
                self.claim_card(card_id, actor)?;     // 接手即认领
                self.conn.execute("UPDATE handoffs SET state='accepted', to_id=?2, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", params![hid, actor])?;
                self.set_handoff_state(card_id, "none", actor, None, None)?;  // 状态归零，新一轮
                self.handoff_tl(&hid, actor, "accept", None)?;
                self.log_event(actor, "handoff", &hid, "accept", json!({"card_id": card_id}));
            }
            (_, "cancel") if cur_state != "none" => {
                let hid = self.current_handoff_id(card_id)?;
                self.conn.execute("UPDATE handoffs SET state='cancelled', updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", params![hid])?;
                self.set_handoff_state(card_id, "none", actor, None, None)?;
                self.handoff_tl(&hid, actor, "cancel", None)?;
                self.log_event(actor, "handoff", &hid, "cancel", json!({"card_id": card_id}));
            }
            _ => return Err(ApiErr::conflict(json!({
                "error": format!("illegal handoff transition: {} --{}-->", cur_state, action),
            }))),
        }
        Ok(self.card_detail(card_id)?)
    }

    fn current_handoff_id(&self, card_id: &str) -> Result<String, ApiErr> {
        self.conn.query_row(
            "SELECT id FROM handoffs WHERE card_id=?1 ORDER BY created_at DESC LIMIT 1",
            params![card_id], |r| r.get(0),
        ).map_err(ApiErr::from)
    }

    fn set_handoff_state(&self, card_id: &str, state: &str, actor: &str, to: Option<&str>, reason: Option<&str>) -> Result<(), ApiErr> {
        let now = self.now_iso();
        self.patch_ext(card_id, |ext| {
            ext["handoff"] = json!({
                "state": state,
                "from": {"kind":"agent","id":actor,"name":actor},
                "to": to.map(|t| json!({"kind":"agent","id":t,"name":t})),
                "reason": reason,
                "updated_at": now,
            });
        })?;
        Ok(())
    }

    fn handoff_tl(&self, handoff_id: &str, actor: &str, action: &str, note: Option<&str>) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO handoff_timeline(id,handoff_id,by_id,action,note) VALUES(?1,?2,?3,?4,?5)",
            params![new_id("ht"), handoff_id, actor, action, note],
        )?;
        Ok(())
    }

    fn card_handoff(&self, card_id: &str) -> SqlResult<Value> {
        let row = self.conn.query_row(
            "SELECT id,state,from_id,to_id,reason,package_json FROM handoffs
             WHERE card_id=?1 ORDER BY created_at DESC LIMIT 1",
            params![card_id],
            |r| Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?,
            )),
        );
        let (hid, state, from, to, reason, pkg) = match row {
            Ok(v) => v,
            Err(_) => return Ok(Value::Null),
        };
        let mut tl = self.conn.prepare(
            "SELECT at,by_id,action,note FROM handoff_timeline WHERE handoff_id=?1 ORDER BY at")?;
        let timeline: Vec<Value> = tl.query_map(params![hid], |r| {
            Ok(json!({
                "at": r.get::<_, String>(0)?,
                "by_id": r.get::<_, String>(1)?,
                "action": r.get::<_, String>(2)?,
                "note": r.get::<_, Option<String>>(3)?,
            }))
        })?.collect::<SqlResult<_>>()?;
        Ok(json!({
            "id": hid, "state": state, "from_id": from, "to_id": to, "reason": reason,
            "package": pkg.and_then(|p| serde_json::from_str::<Value>(&p).ok()),
            "timeline": timeline,
        }))
    }
}

// ------------------------------------------------------------------ git probe

/// 在本机执行 git 探测；失败时记录 error 而不是让 API 挂掉
fn probe_git(path: &str, actor: &str, now: &str) -> Value {
    use std::process::Command;
    let base = || {
        let mut c = Command::new("git");
        c.arg("-C").arg(path);
        c
    };
    let status = base().args(["status", "--porcelain=v1", "-b"]).output();
    let Ok(status) = status else {
        return json!({"error": "git not available or path invalid", "snapshot_at": now, "snapshot_by": actor});
    };
    if !status.status.success() {
        return json!({"error": String::from_utf8_lossy(&status.stderr).trim(), "snapshot_at": now, "snapshot_by": actor});
    }
    let text = String::from_utf8_lossy(&status.stdout);
    let (mut staged, mut unstaged, mut untracked, mut ahead, mut behind) = (0i64, 0i64, 0i64, 0i64, 0i64);
    for line in text.lines() {
        if line.starts_with("##") {
            if let Some(m) = line.split("[ahead ").nth(1) {
                ahead = m.split([',', ']']).next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            }
            if let Some(m) = line.split("behind ").nth(1) {
                behind = m.trim_end_matches(']').trim().parse().ok().unwrap_or(0);
            }
            continue;
        }
        let b = line.as_bytes();
        if b.len() < 2 { continue; }
        if &line[..2] == "??" { untracked += 1; continue; }
        if b[0] != b' ' { staged += 1; }
        if b[1] != b' ' { unstaged += 1; }
    }
    let last = base().args(["log", "-1", "--format=%H%x00%s%x00%aI"]).output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let mut parts = s.splitn(3, '\0');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(sha), Some(msg), Some(at)) if !sha.is_empty() =>
                    Some(json!({"sha": sha, "message": msg, "at": at})),
                _ => None,
            }
        });
    json!({
        "staged": staged, "unstaged": unstaged, "untracked": untracked,
        "clean": staged == 0 && unstaged == 0 && untracked == 0,
        "ahead": ahead, "behind": behind,
        "last_commit": last,
        "snapshot_at": now, "snapshot_by": actor,
    })
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
