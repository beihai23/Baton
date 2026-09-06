//! Baton Core — 数据层（SQLite）
//! 对齐 contract/schema.sql 与 PRD §4.6。
//! 范围：卡片 CRUD、claim 租约、评论（多话题）、乐观锁移列、列策略引擎、审批流、
//! links / git / worksite / handoff、Agent 注册与 Token（F-211/212）、心跳在线状态（F-213）、
//! 幂等写（F-307）、产物（F-108）、强制接管（F-405）、多项目/看板（F-101）、导出导入（F-503）、
//! 事件日志 + 事件总线（长轮询推送）。

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

/// SHA-256（hex），用于 Token、请求体与产物文件的散列
pub(crate) fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(data.as_ref());
    format!("{:x}", h.finalize())
}

/// 生成 Agent Token：`bt-` + 32 位随机 hex（/dev/urandom；失败时退化为时间戳）
fn gen_token() -> String {
    use std::io::Read;
    let rand = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            let mut b = [0u8; 16];
            f.read_exact(&mut b).map(|_| b)
        })
        .map(|b| b.iter().map(|x| format!("{:02x}", x)).collect::<String>())
        .unwrap_or_else(|_| format!("{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    format!("bt-{}", rand)
}

/// agent_json 中是否已签发 Token（不暴露 hash）
fn agent_json_has_token(agent_json: Option<&str>) -> bool {
    agent_json
        .and_then(|j| serde_json::from_str::<Value>(j).ok())
        .and_then(|v| v.get("token_hash").and_then(Value::as_str).map(String::from))
        .is_some()
}

/// 看板模板（F-112）：列名 + 列策略；未知模板回退 software
fn board_template(template: &str) -> Vec<(&'static str, &'static str)> {
    match template {
        "content" => vec![
            ("选题", "{}"),
            ("写作中", "{\"require_progress_summary\":true}"),
            ("待审核", "{\"require_approval\":\"human\"}"),
            ("已发布", "{\"is_done\":true}"),
        ],
        "gtd" => vec![
            ("Inbox", "{}"),
            ("Next", "{}"),
            ("Doing", "{\"require_progress_summary\":true}"),
            ("Done", "{\"is_done\":true}"),
        ],
        _ => vec![
            ("Ready", "{}"),
            ("In Progress", "{\"require_progress_summary\":true}"),
            ("Review", "{\"require_approval\":\"human\"}"),
            ("Done", "{\"is_done\":true}"),
        ],
    }
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
    db_path: String,
    /// 是否允许 Agent 自注册（BATON_AGENT_SELF_REGISTER，本机服务默认放开）
    allow_agent_self_register: bool,
}

impl Db {
    pub fn open(path: &str) -> SqlResult<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        let allow_agent_self_register = std::env::var("BATON_AGENT_SELF_REGISTER")
            .map(|v| v != "0" && v != "false")
            .unwrap_or(true);
        let db = Db {
            conn,
            bus: Arc::new(EventBus::new()),
            db_path: path.to_string(),
            allow_agent_self_register,
        };
        db.migrate()?;
        db.seed()?;
        Ok(db)
    }

    /// 轻量迁移：schema 幂等建表不处理已有表的新列，这里补上
    fn migrate(&self) -> SqlResult<()> {
        let has_col = |table: &str, col: &str| -> SqlResult<bool> {
            let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({})", table))?;
            let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<SqlResult<_>>()?;
            Ok(cols.iter().any(|c| c == col))
        };
        if !has_col("claims", "session_id")? {
            self.conn.execute("ALTER TABLE claims ADD COLUMN session_id TEXT", [])?;
        }
        Ok(())
    }

    pub fn bus(&self) -> Arc<EventBus> {
        self.bus.clone()
    }

    /// 工作区目录（库文件所在目录）：产物文件存于 `<dir>/artifacts/<card_id>/`
    pub fn workspace_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.db_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
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
                ('l-done','b-main','Done',3,'{\"is_done\":true}');",
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

    pub(crate) fn member_kind_role(&self, id: &str) -> SqlResult<(String, String)> {
        self.conn.query_row(
            "SELECT kind, role FROM members WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    // ------------------------------------------------------------------ agents
    // F-211/212/213：Agent 注册、Token 签发/吊销、心跳在线状态。
    // 信任模型（本地单机工具）：
    // - 人类成员（GUI 操作）不校验 Token；
    // - Agent 成员签发了 Token（agent_json.token_hash 非空）后，HTTP 写操作必须
    //   携带匹配的 `X-Baton-Token`；未签发 Token 的 Agent（如演示种子数据）不校验，
    //   便于本地开发调试。吊销（revoked_at）后一律拒绝。

    /// HTTP 写操作的身份校验：agent 且已签发 token → 必须匹配；已吊销 → 403。
    pub fn auth_check(&self, actor: &str, token: Option<&str>) -> Result<(), ApiErr> {
        let row = self.conn.query_row(
            "SELECT kind, revoked_at, json_extract(agent_json,'$.token_hash')
             FROM members WHERE id=?1",
            params![actor],
            |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            )),
        );
        let (kind, revoked, token_hash) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(ApiErr::bad_request(&format!("unknown member: {}", actor)))
            }
            Err(e) => return Err(ApiErr::from(e)),
        };
        if revoked.is_some() {
            return Err(ApiErr {
                status: 403,
                body: json!({"error": format!("member revoked: {}", actor)}),
            });
        }
        if kind != "agent" {
            return Ok(()); // 人类成员走本地信任模型
        }
        match token_hash {
            None => Ok(()), // 未签发 Token 的 Agent：开发/演示模式，不校验
            Some(hash) => {
                let ok = token.map(sha256_hex).as_deref() == Some(hash.as_str());
                if ok {
                    Ok(())
                } else {
                    Err(ApiErr {
                        status: 401,
                        body: json!({"error": "invalid or missing agent token (X-Baton-Token)"}),
                    })
                }
            }
        }
    }

    /// 注册 Agent 并签发一次性明文 Token。
    /// 人类成员总是允许；本机信任模型下 Agent 自注册默认放开
    /// （`BATON_AGENT_SELF_REGISTER=0` 可关闭，退回仅人类可注册）。
    pub fn create_agent(&self, actor: &str, name: &str, role: &str, capabilities: Vec<String>) -> Result<Value, ApiErr> {
        if let Err(e) = self.require_human(actor) {
            if !self.allow_agent_self_register {
                return Err(e);
            }
            // 自注册：actor 即新 Agent 自报的身份（本机信任模型）
        }
        let id = new_id("a");
        let token = gen_token();
        let agent_json = json!({
            "capabilities": capabilities,
            "token_hash": sha256_hex(&token),
        });
        let role = if role.is_empty() { "worker" } else { role };
        self.conn.execute(
            "INSERT INTO members(id,kind,name,role,agent_json) VALUES(?1,'agent',?2,?3,?4)",
            params![id, name, role, agent_json.to_string()],
        )?;
        self.log_event(actor, "member", &id, "agent_register", json!({"name": name, "role": role}));
        Ok(json!({"id": id, "name": name, "role": role, "token": token,
                  "note": "token 仅此一次返回，请妥善保存；后续只能轮换（rotate）"}))
    }

    /// 轮换 Token（仅人类）：旧 Token 立即失效，返回新明文 Token
    pub fn rotate_agent_token(&self, actor: &str, agent_id: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        self.require_agent(agent_id)?;
        let token = gen_token();
        self.conn.execute(
            "UPDATE members SET agent_json=json_set(COALESCE(agent_json,'{}'),'$.token_hash',?2)
             WHERE id=?1",
            params![agent_id, sha256_hex(&token)],
        )?;
        self.log_event(actor, "member", agent_id, "token_rotate", json!({}));
        Ok(json!({"id": agent_id, "token": token,
                  "note": "token 仅此一次返回，请妥善保存"}))
    }

    /// 吊销 Agent（仅人类）：revoked_at 非空后一切操作被拒绝
    pub fn revoke_agent(&self, actor: &str, agent_id: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        self.require_agent(agent_id)?;
        self.conn.execute(
            "UPDATE members SET revoked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            params![agent_id],
        )?;
        self.log_event(actor, "member", agent_id, "revoke", json!({}));
        Ok(json!({"ok": true, "id": agent_id}))
    }

    /// 心跳（F-213）：Agent 自报在线，写 agent_json.last_heartbeat
    pub fn heartbeat(&self, agent_id: &str) -> Result<Value, ApiErr> {
        self.require_agent(agent_id)?;
        self.conn.execute(
            "UPDATE members SET agent_json=json_set(COALESCE(agent_json,'{}'),'$.last_heartbeat',
                strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE id=?1",
            params![agent_id],
        )?;
        Ok(json!({"ok": true, "agent_id": agent_id}))
    }

    /// Agent 面板（F-213）：在线状态（2 分钟内心跳）、当前持有卡片、最近心跳
    pub fn list_agents(&self) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.name, m.role, m.agent_json, m.revoked_at,
                    json_extract(m.agent_json,'$.last_heartbeat') AS last_hb,
                    (SELECT json_group_array(c2.card_id) FROM claims c2
                     WHERE c2.holder_id=m.id AND c2.lease_until > datetime('now')) AS holding
             FROM members m WHERE m.kind='agent' ORDER BY m.created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            let agent_json: Option<String> = r.get(3)?;
            let last_hb: Option<String> = r.get(5)?;
            let holding: Option<String> = r.get(6)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "role": r.get::<_, String>(2)?,
                "capabilities": agent_json
                    .and_then(|j| serde_json::from_str::<Value>(&j).ok())
                    .and_then(|v| v.get("capabilities").cloned())
                    .unwrap_or(json!([])),
                "revoked": r.get::<_, Option<String>>(4)?.is_some(),
                // 是否已签发 Token（不暴露 hash 本身）
                "token_set": agent_json_has_token(r.get::<_, Option<String>>(3)?.as_deref()),
                "last_heartbeat": last_hb,
                "holding_cards": holding
                    .and_then(|h| serde_json::from_str::<Value>(&h).ok())
                    .unwrap_or(json!([])),
            }))
        })?;
        let mut agents: Vec<Value> = rows.collect::<SqlResult<_>>()?;
        // 在线判定：心跳距今 < 120 秒（SQLite 计算，避免时区歧义）
        for a in agents.iter_mut() {
            let id = a.get("id").and_then(Value::as_str).unwrap_or("");
            let online: bool = self.conn.query_row(
                "SELECT COALESCE(json_extract(agent_json,'$.last_heartbeat'),'') != ''
                    AND (julianday('now') - julianday(json_extract(agent_json,'$.last_heartbeat'))) * 86400 < 120
                 FROM members WHERE id=?1",
                params![id],
                |r| r.get(0),
            ).unwrap_or(false);
            a.as_object_mut().unwrap().insert("online".into(), json!(online));
        }
        Ok(json!(agents))
    }

    fn require_human(&self, actor: &str) -> Result<(), ApiErr> {
        match self.member_kind_role(actor) {
            Ok((kind, _)) if kind == "human" => Ok(()),
            Ok(_) => Err(ApiErr {
                status: 403,
                body: json!({"error": "agent management is human-only"}),
            }),
            Err(_) => Err(ApiErr {
                status: 403,
                body: json!({"error": format!(
                    "unknown member: {}（该操作仅人类可执行；如需 Agent 自注册，设 BATON_AGENT_SELF_REGISTER=1）", actor)}),
            }),
        }
    }

    fn require_agent(&self, agent_id: &str) -> Result<(), ApiErr> {
        match self.member_kind_role(agent_id) {
            Ok((kind, _)) if kind == "agent" => Ok(()),
            Ok(_) => Err(ApiErr::bad_request(&format!("not an agent: {}", agent_id))),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(ApiErr::bad_request(&format!("unknown agent: {}", agent_id)))
            }
            Err(e) => Err(ApiErr::from(e)),
        }
    }

    /// 成员列表（指派用）
    pub fn list_members(&self) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,kind,name,role,revoked_at FROM members ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "name": r.get::<_, String>(2)?,
                "role": r.get::<_, String>(3)?,
                "revoked": r.get::<_, Option<String>>(4)?.is_some(),
            }))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    // ------------------------------------------------------------------ sessions
    // Agent Session（PRD 资源管理视角）：任务分配的真实对象。
    // 进板（session_start）声明 scope + 工作现场（cwd/git 自动探测），返回进板简报；
    // 心跳续命并自动续租约；离场（session_end）或心跳超时（180s → 展示为 stale）。

    /// 进板：创建 Session 并返回简报（在手卡片 / 待接手移交 / 未读 @提及）
    pub fn session_start(&self, agent_id: &str, args: &Value) -> Result<Value, ApiErr> {
        self.require_agent(agent_id)?;
        // 已吊销的 Agent 不能开新会话
        let revoked: Option<String> = self.conn.query_row(
            "SELECT revoked_at FROM members WHERE id=?1", params![agent_id], |r| r.get(0),
        ).map_err(ApiErr::from)?;
        if revoked.is_some() {
            return Err(ApiErr { status: 403, body: json!({"error": format!("agent revoked: {}", agent_id)}) });
        }
        let id = new_id("s");
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        let cwd = s("cwd").map(String::from)
            .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()));
        // 工作现场自动探测：cwd 所在 git 仓库的根与分支（失败不阻塞进板）
        let (repo_path, branch) = match (s("repo_path"), s("branch")) {
            (Some(r), Some(b)) => (Some(r.to_string()), Some(b.to_string())),
            _ => match &cwd {
                Some(c) => probe_repo(c),
                None => (None, None),
            },
        };
        self.conn.execute(
            "INSERT INTO sessions(id,agent_id,project_id,board_id,cwd,repo_path,branch,parent_session_id,meta_json,last_heartbeat)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![
                id, agent_id, s("project_id"), s("board_id"),
                cwd, repo_path, branch,
                s("parent_session_id"),
                args.get("meta").cloned().unwrap_or(json!({})).to_string(),
            ],
        )?;
        // 心跳同步到 profile 在线状态
        self.heartbeat(agent_id)?;
        self.log_event(agent_id, "session", &id, "start",
            json!({"cwd": cwd, "repo_path": repo_path, "branch": branch}));
        let mut out = json!({
            "session_id": id, "agent_id": agent_id,
            "cwd": cwd, "repo_path": repo_path, "branch": branch,
        });
        out.as_object_mut().unwrap().insert("briefing".into(), self.session_briefing(agent_id)?);
        Ok(out)
    }

    /// 进板简报：本 profile 的在手卡片、可接手移交、最近 @提及
    pub fn session_briefing(&self, agent_id: &str) -> Result<Value, ApiErr> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.title, c.list_id, cl.lease_until FROM claims cl
             JOIN cards c ON c.id=cl.card_id
             WHERE cl.holder_id=?1 AND cl.lease_until > datetime('now')")?;
        let my_claims: Vec<Value> = stmt.query_map(params![agent_id], |r| {
            Ok(json!({"card_id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?,
                      "list_id": r.get::<_, String>(2)?, "lease_until": r.get::<_, String>(3)?}))
        })?.collect::<SqlResult<_>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT h.id, h.card_id, c.title, h.from_id, h.reason FROM handoffs h
             JOIN cards c ON c.id=h.card_id
             WHERE h.state='ready' AND (h.to_id=?1 OR h.to_id IS NULL)")?;
        let handoffs: Vec<Value> = stmt.query_map(params![agent_id], |r| {
            Ok(json!({"handoff_id": r.get::<_, String>(0)?, "card_id": r.get::<_, String>(1)?,
                      "title": r.get::<_, String>(2)?, "from": r.get::<_, String>(3)?,
                      "reason": r.get::<_, Option<String>>(4)?}))
        })?.collect::<SqlResult<_>>()?;
        let notifs = self.notifications(agent_id, 0, 50)?;
        let mentions: Vec<Value> = notifs.as_array().cloned().unwrap_or_default()
            .into_iter().filter(|n| n.get("kind").and_then(Value::as_str) == Some("@提及"))
            .take(10).collect();
        Ok(json!({
            "my_claims": my_claims,
            "pending_handoffs": handoffs,
            "mentions": mentions,
        }))
    }

    /// Session 心跳：续命 + 自动续期本 session 持有的全部租约（PRD：心跳用于租约续期）
    pub fn session_heartbeat(&self, session_id: &str) -> Result<Value, ApiErr> {
        let agent_id: String = self.conn.query_row(
            "SELECT agent_id FROM sessions WHERE id=?1 AND status='active'",
            params![session_id], |r| r.get(0),
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows =>
                ApiErr::conflict(json!({"error": "session not active or unknown", "session_id": session_id})),
            other => ApiErr::from(other),
        })?;
        self.conn.execute(
            "UPDATE sessions SET last_heartbeat=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            params![session_id],
        )?;
        let renewed = self.conn.execute(
            "UPDATE claims SET lease_until=datetime('now','+30 minutes') WHERE session_id=?1",
            params![session_id],
        )?;
        self.heartbeat(&agent_id)?;
        Ok(json!({"ok": true, "session_id": session_id, "leases_renewed": renewed}))
    }

    /// 离场：显式结束 Session（租约不强制回收，进入自然到期倒计时，可被接管）
    pub fn session_end(&self, session_id: &str) -> Result<Value, ApiErr> {
        let agent_id: String = self.conn.query_row(
            "SELECT agent_id FROM sessions WHERE id=?1", params![session_id], |r| r.get(0),
        ).map_err(ApiErr::from)?;
        self.conn.execute(
            "UPDATE sessions SET status='ended', ended_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?1 AND status='active'",
            params![session_id],
        )?;
        self.log_event(&agent_id, "session", session_id, "end", json!({}));
        Ok(json!({"ok": true, "session_id": session_id}))
    }

    /// Session 的所属 Agent（鉴权用）
    pub fn session_agent(&self, session_id: &str) -> SqlResult<Option<String>> {
        match self.conn.query_row(
            "SELECT agent_id FROM sessions WHERE id=?1", params![session_id], |r| r.get(0),
        ) {
            Ok(a) => Ok(Some(a)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Session 列表（资源视图）：stale 为计算状态（active 但 180s 无心跳）
    pub fn list_sessions(&self, agent_id: Option<&str>) -> SqlResult<Value> {
        let sql = "SELECT s.id, s.agent_id, s.project_id, s.board_id, s.cwd, s.repo_path,
                    s.branch, s.status, s.parent_session_id, s.started_at, s.last_heartbeat,
                    s.ended_at, s.meta_json,
                    (SELECT json_group_array(c2.card_id) FROM claims c2
                     WHERE c2.session_id=s.id AND c2.lease_until > datetime('now')) AS holding
             FROM sessions s
             WHERE (?1 IS NULL OR s.agent_id=?1)
             ORDER BY s.started_at DESC LIMIT 100";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![agent_id], |r| {
            let status: String = r.get(7)?;
            let last_hb: Option<String> = r.get(10)?;
            let meta: String = r.get(12)?;
            let holding: Option<String> = r.get(13)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "agent_id": r.get::<_, String>(1)?,
                "project_id": r.get::<_, Option<String>>(2)?,
                "board_id": r.get::<_, Option<String>>(3)?,
                "cwd": r.get::<_, Option<String>>(4)?,
                "repo_path": r.get::<_, Option<String>>(5)?,
                "branch": r.get::<_, Option<String>>(6)?,
                "status": status,
                "parent_session_id": r.get::<_, Option<String>>(8)?,
                "started_at": r.get::<_, String>(9)?,
                "last_heartbeat": last_hb,
                "ended_at": r.get::<_, Option<String>>(11)?,
                "meta": serde_json::from_str::<Value>(&meta).unwrap_or(json!({})),
                "holding_cards": holding
                    .and_then(|h| serde_json::from_str::<Value>(&h).ok())
                    .unwrap_or(json!([])),
            }))
        })?;
        let mut out: Vec<Value> = rows.collect::<SqlResult<_>>()?;
        for s in out.iter_mut() {
            // stale = active 但心跳超 180s（session 心跳；兼容无心跳的老数据用 started_at 不算）
            let stale: bool = match s.get("last_heartbeat").and_then(Value::as_str) {
                Some(hb) => self.conn.query_row(
                    "SELECT (julianday('now') - julianday(?1)) * 86400 > 180",
                    params![hb], |r| r.get(0),
                ).unwrap_or(false),
                None => true,
            };
            if s.get("status").and_then(Value::as_str) == Some("active") && stale {
                s.as_object_mut().unwrap().insert("status".into(), json!("stale"));
            }
        }
        Ok(Value::Array(out))
    }

    // ------------------------------------------------------------------ idempotency
    // F-307：写 API 支持 Idempotency-Key；同 key 同请求体重放直接返回首个响应，
    // 同 key 不同请求体 → 409。由 HTTP 层（server.rs）接线。

    /// 查询幂等键：命中返回 (request_hash, response_json)
    pub fn idempotency_lookup(&self, key: &str) -> SqlResult<Option<(String, Option<String>)>> {
        let row = self.conn.query_row(
            "SELECT request_hash, response_json FROM idempotency_keys WHERE key=?1",
            params![key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn idempotency_store(&self, key: &str, actor: &str, request_hash: &str, response_json: &str) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO idempotency_keys(key,actor_id,request_hash,response_json)
             VALUES(?1,?2,?3,?4)",
            params![key, actor, request_hash, response_json],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------ board
    /// 整板状态：列 + 卡（含租约快照、未结话题数、进度）；board_id 缺省取第一个看板
    pub fn board_state(&self, board_id: Option<&str>) -> SqlResult<Value> {
        let bid = match board_id {
            Some(b) => b.to_string(),
            None => self.default_board_id()?,
        };
        let mut lists = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT id,name,position,wip_limit,policy_json FROM lists
             WHERE board_id=?1 AND archived_at IS NULL ORDER BY position",
        )?;
        let list_rows = stmt.query_map(params![bid], |r| {
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
        Ok(json!({ "board_id": bid, "lists": lists }))
    }

    fn default_board_id(&self) -> SqlResult<String> {
        self.conn.query_row(
            "SELECT id FROM boards WHERE archived_at IS NULL ORDER BY created_at, position LIMIT 1",
            [], |r| r.get(0),
        )
    }

    /// 项目列表（F-101）：项目 + 各自看板
    pub fn list_projects(&self) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,name,description FROM projects
             WHERE archived_at IS NULL ORDER BY created_at",
        )?;
        let projects: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<SqlResult<_>>()?;
        let mut out = Vec::new();
        for (pid, name, desc) in projects {
            let mut bs = self.conn.prepare(
                "SELECT id,name FROM boards WHERE project_id=?1 AND archived_at IS NULL
                 ORDER BY position, created_at",
            )?;
            let boards: Vec<Value> = bs
                .query_map(params![pid], |r| {
                    Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?}))
                })?
                .collect::<SqlResult<_>>()?;
            out.push(json!({"id": pid, "name": name, "description": desc, "boards": boards}));
        }
        Ok(Value::Array(out))
    }

    /// 新建项目（仅人类）：自动带一个默认看板；template 见 board_template()
    pub fn create_project(&self, actor: &str, name: &str, description: &str, template: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        let pid = new_id("p");
        self.conn.execute(
            "INSERT INTO projects(id,name,description) VALUES(?1,?2,?3)",
            params![pid, name, description],
        )?;
        let board = self.create_board(actor, &pid, "开发板", template)?;
        self.log_event(actor, "project", &pid, "create",
            json!({"name": name, "template": template}));
        Ok(json!({"id": pid, "name": name, "description": description, "board": board}))
    }

    /// 重命名项目（仅人类）
    pub fn rename_project(&self, actor: &str, project_id: &str, name: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        let n = self.conn.execute(
            "UPDATE projects SET name=?2 WHERE id=?1",
            params![project_id, name],
        )?;
        if n == 0 {
            return Err(ApiErr::bad_request(&format!("project not found: {}", project_id)));
        }
        self.log_event(actor, "project", project_id, "rename", json!({"name": name}));
        Ok(json!({"ok": true}))
    }

    /// 删除项目（仅人类）：boards/lists/cards 及卡片级数据（评论/依赖/租约/审批等）
    /// 由外键 ON DELETE CASCADE 级联清除；sessions 的 project_id 可空，置空保留会话史
    pub fn delete_project(&self, actor: &str, project_id: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        let n = self.conn.execute("DELETE FROM projects WHERE id=?1", params![project_id])?;
        if n == 0 {
            return Err(ApiErr::bad_request(&format!("project not found: {}", project_id)));
        }
        self.conn.execute(
            "UPDATE sessions SET project_id=NULL, board_id=NULL WHERE project_id=?1",
            params![project_id],
        )?;
        self.log_event(actor, "project", project_id, "delete", json!({}));
        Ok(json!({"ok": true}))
    }

    /// 新建看板（仅人类）：按模板建列（F-112：software / content / gtd）
    pub fn create_board(&self, actor: &str, project_id: &str, name: &str, template: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        let bid = new_id("b");
        self.conn.execute(
            "INSERT INTO boards(id,project_id,name) VALUES(?1,?2,?3)",
            params![bid, project_id, name],
        )?;
        for (i, (lname, policy)) in board_template(template).iter().enumerate() {
            self.conn.execute(
                "INSERT INTO lists(id,board_id,name,position,policy_json) VALUES(?1,?2,?3,?4,?5)",
                params![new_id("l"), bid, lname, i as i64, policy],
            )?;
        }
        self.log_event(actor, "board", &bid, "create",
            json!({"project_id": project_id, "name": name, "template": template}));
        Ok(json!({"id": bid, "project_id": project_id, "name": name, "template": template}))
    }

    fn cards_in_list(&self, list_id: &str) -> SqlResult<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id,c.title,c.priority,c.assignee_id,c.rev,c.progress_percent,
                    c.handoff_state, cl.holder_id, cl.lease_until,
                    (SELECT COUNT(*) FROM threads t WHERE t.card_id=c.id AND t.status='open'),
                    c.parent_id,
                    (SELECT json_group_array(member_id) FROM card_participants cp
                     WHERE cp.card_id=c.id AND cp.left_at IS NULL)
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
                "parent_id": r.get::<_, Option<String>>(10)?,
                "participants": serde_json::from_str::<Value>(
                    &r.get::<_, Option<String>>(11)?.unwrap_or_else(|| "[]".into()))
                    .unwrap_or(json!([])),
            }))
        })?;
        rows.collect()
    }

    /// 列出卡片：可选按列过滤；board_id 限定看板（缺省所有看板）
    pub fn list_cards(&self, list_id: Option<&str>, board_id: Option<&str>) -> SqlResult<Value> {
        match list_id {
            Some(l) => Ok(Value::Array(self.cards_in_list(l)?)),
            None => {
                let mut all = Vec::new();
                let mut stmt = match board_id {
                    Some(_) => self.conn.prepare(
                        "SELECT id FROM lists WHERE board_id=?1 AND archived_at IS NULL ORDER BY position",
                    )?,
                    None => self.conn.prepare(
                        "SELECT id FROM lists WHERE archived_at IS NULL ORDER BY position",
                    )?,
                };
                let ids: Vec<String> = match board_id {
                    Some(b) => stmt.query_map(params![b], |r| r.get(0))?.collect::<SqlResult<_>>()?,
                    None => stmt.query_map([], |r| r.get(0))?.collect::<SqlResult<_>>()?,
                };
                for lid in ids {
                    all.extend(self.cards_in_list(&lid)?);
                }
                Ok(Value::Array(all))
            }
        }
    }

    // ------------------------------------------------------------------ card

    /// 建卡：board_id 缺省取第一个看板；进入该看板第一列
    pub fn create_card(&self, actor: &str, title: &str, description: &str, board_id: Option<&str>,
        parent_id: Option<&str>) -> SqlResult<Value> {
        // 子任务（F-107）：继承父卡的看板；父卡必须存在。子卡进看板第一列
        let (bid, parent) = match parent_id {
            Some(p) => {
                let pb: String = self.conn.query_row(
                    "SELECT board_id FROM cards WHERE id=?1", params![p], |r| r.get(0),
                )?;
                (pb, Some(p.to_string()))
            }
            None => (match board_id {
                Some(b) => b.to_string(),
                None => self.default_board_id()?,
            }, None),
        };
        let (pid, lid): (String, String) = self.conn.query_row(
            "SELECT b.project_id,
                    (SELECT id FROM lists WHERE board_id=b.id AND archived_at IS NULL
                     ORDER BY position LIMIT 1)
             FROM boards b WHERE b.id=?1",
            params![bid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let id = new_id("c");
        let ext = json!({"schema_rev":1});
        self.conn.execute(
            "INSERT INTO cards(id,project_id,board_id,list_id,title,description,created_by,ext_json,parent_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![id, pid, bid, lid, title, description, actor, ext.to_string(), parent],
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
                    created_by,created_at,updated_at,parent_id
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
                    "parent_id": r.get::<_, Option<String>>(11)?,
                }))
            },
        )?;
        let mut obj = card.as_object().unwrap().clone();
        obj.insert("claim".into(), self.active_claim(id)?);
        obj.insert("threads".into(), self.threads_with_comments(id)?);
        obj.insert("links".into(), self.card_links(id)?);
        obj.insert("work_nodes".into(), self.card_work_nodes(id)?);
        obj.insert("handoff".into(), self.card_handoff(id)?);
        obj.insert("artifacts".into(), self.card_artifacts(id)?);
        obj.insert("deps".into(), self.card_deps_view(id)?);
        obj.insert("participants".into(),
            Value::Array(self.active_participants(id)?.into_iter().map(Value::String).collect()));
        // 父子卡（F-107）：父卡引用 + 子任务列表（带完成态，供验收视图）
        if let Some(pid) = obj.get("parent_id").and_then(Value::as_str) {
            let parent: Option<(String, String)> = self.conn.query_row(
                "SELECT id, title FROM cards WHERE id=?1", params![pid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).ok();
            obj.insert("parent".into(), parent.map(|(i, t)| json!({"id": i, "title": t})).into());
        }
        let mut cs = self.conn.prepare(
            "SELECT c.id, c.title, c.list_id, c.progress_percent,
                    COALESCE((SELECT policy_json FROM lists l WHERE l.id=c.list_id
                              AND json_extract(l.policy_json,'$.is_done')=1), '') != '' AS done
             FROM cards c WHERE c.parent_id=?1 ORDER BY c.created_at",
        )?;
        let children: Vec<Value> = cs.query_map(params![id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "list_id": r.get::<_, String>(2)?,
                "progress_percent": r.get::<_, Option<i64>>(3)?,
                "done": r.get::<_, bool>(4)?,
            }))
        })?.collect::<SqlResult<_>>()?;
        obj.insert("children".into(), Value::Array(children));
        Ok(Value::Object(obj))
    }

    fn active_claim(&self, card_id: &str) -> SqlResult<Value> {

        let mut stmt = self.conn.prepare(
            "SELECT holder_id, lease_until, session_id FROM claims
             WHERE card_id=?1 AND lease_until > datetime('now')",
        )?;
        let mut rows = stmt.query(params![card_id])?;
        if let Some(r) = rows.next()? {
            Ok(json!({"holder_id": r.get::<_, String>(0)?, "lease_until": r.get::<_, String>(1)?,
                      "session_id": r.get::<_, Option<String>>(2)?}))
        } else {
            Ok(Value::Null)
        }
    }

    /// 在场协同者（left_at IS NULL）
    fn active_participants(&self, card_id: &str) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT member_id FROM card_participants WHERE card_id=?1 AND left_at IS NULL
             ORDER BY joined_at",
        )?;
        let rows = stmt.query_map(params![card_id], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// 参与协同（多 Agent 共同完成同一任务，区别于移交/拆子任务）：
    /// 租约只有一条（主驾负责状态机），协同者是"副驾"——可评论/汇报/传产物/移列，
    /// 但不改变"谁在主责"。重复 join 视为重新到场（复活 left_at）。
    pub fn join_card(&self, card_id: &str, member: &str, session_id: Option<&str>) -> Result<Value, ApiErr> {
        self.conn.query_row("SELECT 1 FROM cards WHERE id=?1", params![card_id], |r| r.get::<_, i64>(0))
            .map_err(|_| ApiErr::bad_request(&format!("card not found: {}", card_id)))?;
        self.conn.execute(
            "INSERT INTO card_participants(card_id,member_id,session_id) VALUES(?1,?2,?3)
             ON CONFLICT(card_id,member_id) DO UPDATE SET left_at=NULL,
                 session_id=excluded.session_id,
                 joined_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![card_id, member, session_id],
        )?;
        self.sys_comment(card_id, member, &format!("🤝 {} 加入协同", member))?;
        self.log_event(member, "card", card_id, "join", json!({}));
        Ok(self.card_detail(card_id)?)
    }

    /// 退出协同：不在场时 409
    pub fn leave_card(&self, card_id: &str, member: &str) -> Result<Value, ApiErr> {
        let n = self.conn.execute(
            "UPDATE card_participants SET left_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE card_id=?1 AND member_id=?2 AND left_at IS NULL",
            params![card_id, member],
        )?;
        if n == 0 {
            return Err(ApiErr::conflict(json!({"error": "not an active participant"})));
        }
        self.sys_comment(card_id, member, &format!("👋 {} 退出协同", member))?;
        self.log_event(member, "card", card_id, "leave", json!({}));
        Ok(self.card_detail(card_id)?)
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
                "SELECT id,author_id,kind,body,created_at,mentions_json,reply_to FROM comments
                 WHERE thread_id=?1 ORDER BY created_at",
            )?;
            let comments: Vec<Value> = cs
                .query_map(params![tid], |r| {
                    let mentions: String = r.get(5)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "author_id": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "body": r.get::<_, String>(3)?,
                        "created_at": r.get::<_, String>(4)?,
                        "mentions": serde_json::from_str::<Value>(&mentions).unwrap_or(json!([])),
                        "reply_to": r.get::<_, Option<String>>(6)?,
                    }))
                })?
                .collect::<SqlResult<_>>()?;
            out.push(json!({"id": tid, "title": title, "status": status, "comments": comments}));
        }
        Ok(Value::Array(out))
    }

    // ------------------------------------------------------------------ claim

    /// 认领（F-303 抢单原子性）：单条 SQL 完成"无活跃租约才插入"，
    /// 多进程并发抢单时由 SQLite 写锁保证先到先得，后到者 409。
    /// session_id 可选：记录认领发生在哪个会话（归属与简报用）。
    pub fn claim_card(&self, card_id: &str, holder: &str, session_id: Option<&str>) -> Result<Value, ApiErr> {
        if let Some(sid) = session_id {
            // session 必须存在、活跃、且属于 holder
            let owner: String = self.conn.query_row(
                "SELECT agent_id FROM sessions WHERE id=?1 AND status='active'",
                params![sid], |r| r.get(0),
            ).map_err(|_| ApiErr::bad_request(&format!("session not active or unknown: {}", sid)))?;
            if owner != holder {
                return Err(ApiErr::bad_request("session does not belong to this agent"));
            }
        }
        // 原子抢单：无行直接插入；有过期租约则替换；有活跃租约则 WHERE 不命中（n=0 → 409）。
        // 单条 upsert 保证并发安全（此前是 INSERT...WHERE NOT EXISTS，过期租约的残留行
        // 会触发 claims.card_id 的 UNIQUE 约束，导致租约过期后永远无法重新认领）。
        let n = self.conn.execute(
            "INSERT INTO claims(card_id,holder_id,session_id,lease_until)
             VALUES (?1,?2,?3,datetime('now','+30 minutes'))
             ON CONFLICT(card_id) DO UPDATE SET
                 holder_id=excluded.holder_id,
                 session_id=excluded.session_id,
                 lease_until=excluded.lease_until
             WHERE claims.lease_until <= datetime('now')",
            params![card_id, holder, session_id],
        )?;
        if n == 0 {
            let existing = self.active_claim(card_id)?;
            return Err(ApiErr::conflict(json!({
                "error": "card already claimed",
                "claim": existing,
            })));
        }
        self.sys_comment(card_id, holder, &format!("{} 认领了此卡片（租约 30 分钟{}）",
            holder,
            session_id.map(|s| format!("，session {}", s)).unwrap_or_default()))?;
        self.log_event(holder, "card", card_id, "claim",
            json!({"holder": holder, "session_id": session_id}));
        Ok(json!({"ok": true, "claim": self.active_claim(card_id)?}))
    }

    /// 指派（F-105）：assignee 为 NULL 即放入抢单池
    pub fn assign_card(&self, card_id: &str, actor: &str, assignee: Option<&str>) -> Result<Value, ApiErr> {
        self.conn.execute(
            "UPDATE cards SET assignee_id=?2, rev=rev+1,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            params![card_id, assignee],
        )?;
        self.sys_comment(card_id, actor, &match assignee {
            Some(a) => format!("👤 指派给 {}", a),
            None => "👤 放入抢单池（任何空闲 Agent 可认领）".to_string(),
        })?;
        self.log_event(actor, "card", card_id, "assign", json!({"assignee": assignee}));
        Ok(self.card_detail(card_id)?)
    }

    pub fn release_card(&self, card_id: &str, actor: &str) -> Result<Value, ApiErr> {
        // 释放租约：仅持有者本人或人类（协调者）可操作；无活跃租约时 409。
        // 此前无任何校验，任何 Agent 都能释放他人租约（等同于强制接管却没有审计痕迹）。
        let prev = self.active_claim(card_id)?;
        let Value::Object(c) = &prev else {
            return Err(ApiErr::conflict(json!({"error": "card has no active claim"})));
        };
        let holder = c.get("holder_id").and_then(Value::as_str).unwrap_or("");
        if actor != holder {
            let (kind, _) = self.member_kind_role(actor)?;
            if kind != "human" {
                return Err(ApiErr {
                    status: 403,
                    body: json!({"error": format!("only the holder or a human can release this claim (holder: {})", holder)}),
                });
            }
        }
        self.conn
            .execute("DELETE FROM claims WHERE card_id=?1", params![card_id])?;
        self.log_event(actor, "card", card_id, "release", json!({}));
        Ok(json!({"ok": true}))
    }

    /// 强制接管（F-405）：人类随时强制释放 Agent 租约，把卡拿回自己手里
    pub fn takeover_card(&self, card_id: &str, actor: &str) -> Result<Value, ApiErr> {
        self.require_human(actor)?;
        let prev = self.active_claim(card_id)?;
        let Value::Object(c) = &prev else {
            return Err(ApiErr::conflict(json!({"error": "card has no active claim"})));
        };
        let prev_holder = c.get("holder_id").and_then(Value::as_str).unwrap_or("").to_string();
        self.conn.execute("DELETE FROM claims WHERE card_id=?1", params![card_id])?;
        self.sys_comment(card_id, actor,
            &format!("⚑ {} 强制接管了卡片（原持有者 {}，租约被强制释放）", actor, prev_holder))?;
        self.log_event(actor, "card", card_id, "takeover", json!({"prev_holder": prev_holder}));
        Ok(self.card_detail(card_id)?)
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

    /// 解析 @提及：body 中含 "@名字" 或 "@id" 的成员 id 列表（写入 mentions_json）
    fn parse_mentions(&self, body: &str) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM members")?;
        let members: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<SqlResult<_>>()?;
        Ok(members.into_iter()
            .filter(|(id, name)| body.contains(&format!("@{}", id)) || body.contains(&format!("@{}", name)))
            .map(|(id, _)| id)
            .collect())
    }

    /// 新建话题（thread）：卡片讨论区分话题组织；返回卡片详情
    pub fn create_thread(&self, card_id: &str, actor: &str, title: &str) -> Result<Value, ApiErr> {
        let id = new_id("t");
        self.conn.execute(
            "INSERT INTO threads(id,card_id,title,created_by) VALUES (?1,?2,?3,?4)",
            params![id, card_id, title, actor],
        )?;
        self.log_event(actor, "thread", &id, "create", json!({"card_id": card_id, "title": title}));
        Ok(self.card_detail(card_id)?)
    }

    /// 写评论。thread 归属优先级：reply_to（父评论所在 thread）> thread_id（指定话题，
    /// 须属于本卡）> 卡片的第一个 thread（主讨论）。
    pub fn add_comment(&self, card_id: &str, author: &str, body: &str, kind: &str,
        reply_to: Option<&str>, thread_id: Option<&str>) -> Result<Value, ApiErr> {
        let id = new_id("cm");
        let mentions = self.parse_mentions(body)?;
        let mentions_json = json!(mentions).to_string();
        if let Some(rt) = reply_to {
            // 直接回复：落到父评论所在的 thread 并记录 reply_to；
            // 父评论必须存在且属于同一张卡
            let (parent_card, parent_thread): (String, String) = self.conn.query_row(
                "SELECT card_id, thread_id FROM comments WHERE id=?1",
                params![rt], |r| Ok((r.get(0)?, r.get(1)?)),
            ).map_err(|_| ApiErr::bad_request(&format!("reply target not found: {}", rt)))?;
            if parent_card != card_id {
                return Err(ApiErr::bad_request("reply target belongs to another card"));
            }
            self.conn.execute(
                "INSERT INTO comments(id,card_id,thread_id,reply_to,author_id,kind,body,mentions_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![id, card_id, parent_thread, rt, author, kind, body, mentions_json],
            )?;
        } else if let Some(tid) = thread_id {
            // 指定话题：thread 必须属于本卡
            let thread_card: String = self.conn.query_row(
                "SELECT card_id FROM threads WHERE id=?1",
                params![tid], |r| r.get(0),
            ).map_err(|_| ApiErr::bad_request(&format!("thread not found: {}", tid)))?;
            if thread_card != card_id {
                return Err(ApiErr::bad_request("thread belongs to another card"));
            }
            self.conn.execute(
                "INSERT INTO comments(id,card_id,thread_id,author_id,kind,body,mentions_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![id, card_id, tid, author, kind, body, mentions_json],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO comments(id,card_id,thread_id,author_id,kind,body,mentions_json)
                 SELECT ?1,?2,t.id,?3,?4,?5,?6 FROM threads t
                 WHERE t.card_id=?2 ORDER BY t.created_at LIMIT 1",
                params![id, card_id, author, kind, body, mentions_json],
            )?;
        }
        self.log_event(author, "comment", &id, "create",
            json!({"card_id": card_id, "kind": kind, "mentions": mentions}));
        Ok(self.card_detail(card_id)?)
    }

    // ------------------------------------------------------------------ notifications
    // F-404：通知中心 —— 从事件日志派生（审批请求、@提及、依赖解除、接管、移交就绪/被接）。
    // 不另建表：events 即事实源；已读状态由客户端记录 last_read_seq。

    pub fn notifications(&self, member_id: &str, since_seq: i64, limit: i64) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, actor_id, entity, entity_id, action, payload_json
             FROM events WHERE seq > ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since_seq, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?, r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?,
                r.get::<_, String>(4)?, r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?.collect::<SqlResult<Vec<_>>>()?;
        let mut out = Vec::new();
        for (seq, at, actor, entity, entity_id, action, payload) in rows {
            let payload: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
            // 本人自己产生的事件不通知自己
            if actor.as_deref() == Some(member_id) { continue; }
            let kind = match (entity.as_str(), action.as_str()) {
                ("approval", "request") => Some("审批请求"),
                ("comment", "create") => {
                    let mentioned = payload.get("mentions").and_then(Value::as_array)
                        .map(|m| m.iter().any(|x| x.as_str() == Some(member_id)))
                        .unwrap_or(false);
                    if mentioned { Some("@提及") } else { None }
                }
                ("card", "dep_resolved") => Some("依赖解除"),
                ("card", "takeover") => Some("强制接管"),
                ("handoff", "ready") => Some("移交待接手"),
                ("handoff", "accept") => Some("移交已接受"),
                ("member", "revoke") => Some("Agent 吊销"),
                _ => None,
            };
            if let Some(k) = kind {
                out.push(json!({
                    "seq": seq, "at": at, "actor_id": actor, "kind": k,
                    "entity": entity, "entity_id": entity_id,
                    "card_id": payload.get("card_id").cloned()
                        .unwrap_or(if entity == "card" { json!(entity_id) } else { Value::Null }),
                }));
            }
        }
        Ok(Value::Array(out))
    }

    // ------------------------------------------------------------------ move

    /// 乐观锁移列 + 列策略引擎：
    /// - rev 不匹配 → 409
    /// - 有活跃租约时仅 holder 可移动 → 409
    /// - 目标列 require_progress_summary → ext.progress.summary 必填，否则 400（只约束 Agent，人类豁免）
    /// - 目标列 require_approval=human 且操作者非人类 Owner → 创建审批单，不移动（202 语义）
    pub fn move_card(&self, card_id: &str, actor: &str, list_id: &str, rev: i64) -> Result<Value, ApiErr> {
        if let Value::Object(c) = self.active_claim(card_id)? {
            // 移动权限：租约持有者（主驾）或在场协同者（副驾）皆可；rev 乐观锁兜底并发冲突
            let holder = c.get("holder_id").and_then(Value::as_str);
            if holder != Some(actor) && !self.active_participants(card_id)?.iter().any(|m| m == actor) {
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
            // 只约束 Agent：进度摘要是"干活的先汇报再动卡片"的行为规范；
            // 人是协调者/管理者，移列不强制。
            let is_human = self.member_kind_role(actor).map(|(k, _)| k == "human").unwrap_or(false);
            if !is_human {
                let summary: Option<String> = self.conn.query_row(
                    "SELECT json_extract(ext_json,'$.progress.summary') FROM cards WHERE id=?1",
                    params![card_id], |r| r.get(0),
                )?;
                if summary.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(ApiErr::bad_request(
                        "列策略：进入该列前需要先上报进度摘要。请先调用 progress_update（percent + 一句话进展说明），再移动卡片",
                    ));
                }
            }
        }

        // F-106：目标列是完成列（policy.is_done）时，blocked_by 依赖必须全部完成
        if policy.get("is_done").and_then(Value::as_bool) == Some(true) {
            let unmet = self.unmet_deps(card_id)?;
            if !unmet.is_empty() {
                return Err(ApiErr::conflict(json!({
                    "error": "blocked by unfinished dependencies",
                    "blocking": unmet,
                })));
            }
            // F-107：有子任务时必须全部完成（父卡不能先于子卡关闭）
            let mut cs = self.conn.prepare(
                "SELECT c.id, c.title FROM cards c
                 WHERE c.parent_id=?1 AND c.archived_at IS NULL
                   AND NOT COALESCE((SELECT json_extract(l.policy_json,'$.is_done')
                                     FROM lists l WHERE l.id=c.list_id), 0)",
            )?;
            let unfinished: Vec<Value> = cs.query_map(params![card_id], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?}))
            })?.collect::<SqlResult<_>>()?;
            if !unfinished.is_empty() {
                return Err(ApiErr::conflict(json!({
                    "error": "有未完成的子任务，父任务不能进入完成列",
                    "unfinished_children": unfinished,
                })));
            }
        }

        match policy.get("require_approval").and_then(Value::as_str) {
            // human 模式：仅人类 Owner 直接移动，其他成员创建审批单
            Some("human") => {
                let (kind, role) = self.member_kind_role(actor)?;
                if !(kind == "human" && role == "owner") {
                    return self.request_approval(card_id, actor, list_id, "请求进入需人工审批的列，已提交审批单");
                }
            }
            // peer 模式（职责分离）：任何人移入都要审批，且审批人不能是申请者自己
            // —— 验收必须由同伴（另一个 Agent 或人）做出，执行者不能自审
            Some("peer") => {
                return self.request_approval(card_id, actor, list_id, "请求进入需同伴验收的列，已提交审批单");
            }
            _ => {}
        }

        self.do_move(card_id, actor, list_id, Some(rev))
    }

    fn request_approval(&self, card_id: &str, actor: &str, list_id: &str, comment: &str) -> Result<Value, ApiErr> {
        let aid = new_id("ap");
        self.conn.execute(
            "INSERT INTO approvals(id,card_id,list_id,requested_by) VALUES(?1,?2,?3,?4)",
            params![aid, card_id, list_id, actor],
        )?;
        self.sys_comment(card_id, actor, comment)?;
        self.log_event(actor, "approval", &aid, "request",
            json!({"card_id": card_id, "list_id": list_id}));
        Ok(json!({"approval_pending": aid, "card_id": card_id}))
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
        // F-305：进入完成列后通知被本卡阻塞的下游卡片
        if self.list_is_done(list_id)? {
            self.notify_dependents(card_id, actor)?;
        }
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
        let (card_id, list_id, status, requested_by): (String, String, String, String) = self.conn.query_row(
            "SELECT card_id,list_id,status,requested_by FROM approvals WHERE id=?1",
            params![approval_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if status != "pending" {
            return Err(ApiErr::conflict(json!({"error": "approval already decided", "status": status})));
        }
        // 职责分离：任何模式下申请者都不能自审
        if actor == requested_by {
            return Err(ApiErr {
                status: 403,
                body: json!({"error": "不能审批自己提交的申请（需要同伴验收）"}),
            });
        }
        // human 模式的审批单只能由人类裁决；peer 模式任何其他成员（含 Agent）均可
        let policy: String = self.conn.query_row(
            "SELECT policy_json FROM lists WHERE id=?1", params![list_id], |r| r.get(0),
        ).unwrap_or_else(|_| "{}".into());
        let policy: Value = serde_json::from_str(&policy).unwrap_or(json!({}));
        if policy.get("require_approval").and_then(Value::as_str) == Some("human") {
            self.require_human(actor)?;
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
        self.add_comment(card_id, actor, summary, "progress", None, None)?;
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

    // ---- artifacts（F-108：产物文件存 <工作区>/artifacts/<card_id>/，元数据进表） ----

    /// 上传产物：content（文本内容）或 path（本机文件路径，复制进工作区）二选一
    pub fn upload_artifact(&self, card_id: &str, actor: &str, args: &Value) -> Result<Value, ApiErr> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if name.is_empty() {
            return Err(ApiErr::bad_request("artifact name is required"));
        }
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("file");
        let mime = args.get("mime").and_then(Value::as_str);
        let data: Vec<u8> = if let Some(p) = args.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()) {
            std::fs::read(p).map_err(|e| ApiErr::bad_request(&format!("cannot read path {}: {}", p, e)))?
        } else if let Some(c) = args.get("content").and_then(Value::as_str) {
            c.as_bytes().to_vec()
        } else {
            return Err(ApiErr::bad_request("artifact requires content or path"));
        };
        let id = new_id("art");
        // 文件名清洗：只保留文件名片段，防路径穿越
        let fname = std::path::Path::new(&name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("artifact");
        let rel = format!("artifacts/{}/{}-{}", card_id, id, fname);
        let abs = self.workspace_dir().join(&rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&abs, &data)
            .map_err(|e| ApiErr { status: 500, body: json!({"error": e.to_string()}) })?;
        let sha = sha256_hex(&data);
        self.conn.execute(
            "INSERT INTO artifacts(id,card_id,kind,name,path,mime,size_bytes,sha256,uploaded_by)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![id, card_id, kind, name, rel, mime, data.len() as i64, sha, actor],
        )?;
        self.sys_comment(card_id, actor, &format!("📎 上传产物 {}（{} 字节）", name, data.len()))?;
        self.log_event(actor, "artifact", &id, "upload", json!({"card_id": card_id, "name": name}));
        Ok(self.card_detail(card_id)?)
    }

    /// 产物元数据 + 文本预览（≤256KB 且能按 UTF-8 解码时内联 content）
    pub fn get_artifact(&self, id: &str) -> Result<Value, ApiErr> {
        let row = self.conn.query_row(
            "SELECT id,card_id,kind,name,path,mime,size_bytes,sha256,uploaded_by,uploaded_at
             FROM artifacts WHERE id=?1",
            params![id],
            |r| Ok(json!({
                "id": r.get::<_, String>(0)?,
                "card_id": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "name": r.get::<_, String>(3)?,
                "path": r.get::<_, String>(4)?,
                "mime": r.get::<_, Option<String>>(5)?,
                "size_bytes": r.get::<_, i64>(6)?,
                "sha256": r.get::<_, Option<String>>(7)?,
                "uploaded_by": r.get::<_, String>(8)?,
                "uploaded_at": r.get::<_, String>(9)?,
            })),
        ).map_err(ApiErr::from)?;
        let mut obj = row.as_object().unwrap().clone();
        let path = obj.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let size = obj.get("size_bytes").and_then(Value::as_i64).unwrap_or(0);
        if size <= 256 * 1024 {
            if let Ok(bytes) = std::fs::read(self.workspace_dir().join(&path)) {
                if let Ok(text) = String::from_utf8(bytes) {
                    obj.insert("content".into(), json!(text));
                }
            }
        }
        Ok(Value::Object(obj))
    }

    fn card_artifacts(&self, card_id: &str) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,kind,name,path,mime,size_bytes,uploaded_by,uploaded_at
             FROM artifacts WHERE card_id=?1 ORDER BY uploaded_at",
        )?;
        let rows = stmt.query_map(params![card_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "name": r.get::<_, String>(2)?,
                "path": r.get::<_, String>(3)?,
                "mime": r.get::<_, Option<String>>(4)?,
                "size_bytes": r.get::<_, i64>(5)?,
                "uploaded_by": r.get::<_, String>(6)?,
                "uploaded_at": r.get::<_, String>(7)?,
            }))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    // ---- deps（F-106/305：blocked_by 未完成的卡片禁入 Done 列；依赖完成时通知下游） ----

    /// 添加依赖：relation 从 card_id 视角出发（blocked_by / blocks / relates_to）
    pub fn add_dep(&self, card_id: &str, other_id: &str, relation: &str, actor: &str) -> Result<Value, ApiErr> {
        if card_id == other_id {
            return Err(ApiErr::bad_request("card cannot depend on itself"));
        }
        if !matches!(relation, "blocked_by" | "blocks" | "relates_to") {
            return Err(ApiErr::bad_request("relation must be blocked_by|blocks|relates_to"));
        }
        for id in [card_id, other_id] {
            let exists: bool = self.conn.query_row(
                "SELECT COUNT(*) FROM cards WHERE id=?1", params![id], |r| r.get(0),
            )?;
            if !exists {
                return Err(ApiErr::bad_request(&format!("unknown card: {}", id)));
            }
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO card_deps(card_id,other_card_id,relation) VALUES(?1,?2,?3)",
            params![card_id, other_id, relation],
        )?;
        self.log_event(actor, "card", card_id, "dep_add",
            json!({"other": other_id, "relation": relation}));
        Ok(self.card_detail(card_id)?)
    }

    pub fn remove_dep(&self, card_id: &str, other_id: &str, relation: &str, actor: &str) -> Result<Value, ApiErr> {
        self.conn.execute(
            "DELETE FROM card_deps WHERE card_id=?1 AND other_card_id=?2 AND relation=?3",
            params![card_id, other_id, relation],
        )?;
        self.log_event(actor, "card", card_id, "dep_remove",
            json!({"other": other_id, "relation": relation}));
        Ok(self.card_detail(card_id)?)
    }

    /// 卡片的依赖视图：含对方卡片标题、所在列、是否已在"完成列"
    fn card_deps_view(&self, card_id: &str) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT d.relation, d.other_card_id, c.title, c.list_id
             FROM card_deps d JOIN cards c ON c.id=d.other_card_id
             WHERE d.card_id=?1",
        )?;
        let rows = stmt.query_map(params![card_id], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, String>(2)?, r.get::<_, String>(3)?,
            ))
        })?.collect::<SqlResult<Vec<_>>>()?;
        let mut out = Vec::new();
        for (relation, oid, title, list_id) in rows {
            out.push(json!({
                "relation": relation, "other_id": oid, "other_title": title,
                "other_list_id": list_id, "other_done": self.list_is_done(&list_id)?,
            }));
        }
        Ok(Value::Array(out))
    }

    /// 该列是否为"完成列"（policy.is_done）
    fn list_is_done(&self, list_id: &str) -> SqlResult<bool> {
        let policy: Option<String> = self.conn.query_row(
            "SELECT policy_json FROM lists WHERE id=?1", params![list_id], |r| r.get(0),
        ).ok();
        Ok(policy
            .and_then(|p| serde_json::from_str::<Value>(&p).ok())
            .and_then(|p| p.get("is_done").and_then(Value::as_bool))
            .unwrap_or(false))
    }

    /// F-106 门禁：进入 Done 列前，所有 blocked_by 依赖必须已在完成列
    fn unmet_deps(&self, card_id: &str) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT other_card_id FROM card_deps WHERE card_id=?1 AND relation='blocked_by'")?;
        let ids: Vec<String> = stmt.query_map(params![card_id], |r| r.get(0))?
            .collect::<SqlResult<_>>()?;
        let mut unmet = Vec::new();
        for oid in ids {
            let list_id: String = self.conn.query_row(
                "SELECT list_id FROM cards WHERE id=?1", params![oid], |r| r.get(0),
            )?;
            if !self.list_is_done(&list_id)? {
                unmet.push(oid);
            }
        }
        Ok(unmet)
    }

    /// F-305：卡片进入完成列后，通知所有被它阻塞的下游卡片（系统评论 + 事件）
    fn notify_dependents(&self, done_card_id: &str, actor: &str) -> SqlResult<()> {
        let mut stmt = self.conn.prepare(
            "SELECT card_id FROM card_deps WHERE other_card_id=?1 AND relation='blocked_by'")?;
        let ids: Vec<String> = stmt.query_map(params![done_card_id], |r| r.get(0))?
            .collect::<SqlResult<_>>()?;
        for cid in ids {
            // 仅当该下游卡片的全部依赖都已满足时才通知"解除阻塞"
            if self.unmet_deps(&cid)?.is_empty() {
                self.sys_comment(&cid, actor,
                    &format!("✅ 依赖 {} 已完成，本卡片解除阻塞，可以开始了", done_card_id))?;
                self.log_event(actor, "card", &cid, "dep_resolved",
                    json!({"by": done_card_id}));
            }
        }
        Ok(())
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
                self.claim_card(card_id, actor, None)?;  // 接手即认领
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

    /// Agent 的速率上限（F-308，agent_json.rate_limit_per_min，默认 60 次/分钟）
    pub fn agent_rate_limit(&self, agent_id: &str) -> SqlResult<i64> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT json_extract(agent_json,'$.rate_limit_per_min') FROM members
             WHERE id=?1 AND kind='agent'",
            params![agent_id], |r| r.get(0),
        ).unwrap_or(None);
        Ok(v.unwrap_or(60))
    }

    // ------------------------------------------------------------------ backup
    // F-504：快照备份 —— VACUUM INTO 在线快照到 <工作区>/backups/，保留最近 N 份。

    pub fn backup(&self, keep: usize) -> Result<Value, ApiErr> {
        let dir = self.workspace_dir().join("backups");
        std::fs::create_dir_all(&dir)
            .map_err(|e| ApiErr { status: 500, body: json!({"error": e.to_string()}) })?;
        let ts = self.now_iso().replace([':', '.'], "-");
        let path = dir.join(format!("baton-{}.db", ts));
        // 路径由时间戳生成，无外部输入，无注入风险
        self.conn.execute(
            &format!("VACUUM INTO '{}'", path.display()),
            [],
        ).map_err(ApiErr::from)?;
        // 清理旧快照，保留最近 keep 份
        let mut snaps: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| ApiErr { status: 500, body: json!({"error": e.to_string()}) })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "db").unwrap_or(false))
            .collect();
        snaps.sort();
        let mut pruned = 0usize;
        while snaps.len() > keep {
            if let Some(p) = snaps.first() {
                std::fs::remove_file(p).ok();
                snaps.remove(0);
                pruned += 1;
            }
        }
        Ok(json!({
            "ok": true,
            "snapshot": path.display().to_string(),
            "kept": snaps.len(), "pruned": pruned,
        }))
    }

    // ------------------------------------------------------------------ export / import
    // F-503：项目一键导出（JSON 全量 + Markdown 卡片 + 附件目录）与导入。
    // 导出/导入由 CLI 驱动（baton export / baton import）；JSON 为事实源，
    // Markdown 仅供人阅读。导入用 INSERT OR REPLACE（按原 id 幂等落库）。

    /// 导出项目全量数据为 JSON（claims 为易变状态，不导出）
    pub fn export_project(&self, project_id: Option<&str>) -> SqlResult<Value> {
        let pid = match project_id {
            Some(p) => p.to_string(),
            None => self.conn.query_row(
                "SELECT id FROM projects WHERE archived_at IS NULL ORDER BY created_at LIMIT 1",
                [], |r| r.get(0),
            )?,
        };
        let card_ids = "SELECT id FROM cards WHERE project_id=?1";
        let board_ids = "SELECT id FROM boards WHERE project_id=?1";
        let mut out = Map::new();
        out.insert("format".into(), json!("baton-export/v1"));
        out.insert("exported_at".into(), json!(self.now_iso()));
        out.insert("members".into(), self.dump_rows("SELECT * FROM members", &[])?);
        out.insert("projects".into(), self.dump_rows("SELECT * FROM projects WHERE id=?1", &[&pid])?);
        out.insert("boards".into(), self.dump_rows("SELECT * FROM boards WHERE project_id=?1", &[&pid])?);
        out.insert("lists".into(), self.dump_rows(
            &format!("SELECT * FROM lists WHERE board_id IN ({})", board_ids), &[&pid])?);
        out.insert("cards".into(), self.dump_rows("SELECT * FROM cards WHERE project_id=?1", &[&pid])?);
        out.insert("threads".into(), self.dump_rows(
            &format!("SELECT * FROM threads WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("comments".into(), self.dump_rows(
            &format!("SELECT * FROM comments WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("links".into(), self.dump_rows(
            &format!("SELECT * FROM links WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("artifacts".into(), self.dump_rows(
            &format!("SELECT * FROM artifacts WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("work_nodes".into(), self.dump_rows(
            &format!("SELECT * FROM work_nodes WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("handoffs".into(), self.dump_rows(
            &format!("SELECT * FROM handoffs WHERE card_id IN ({})", card_ids), &[&pid])?);
        out.insert("handoff_timeline".into(), self.dump_rows(
            &format!("SELECT * FROM handoff_timeline WHERE handoff_id IN
                (SELECT id FROM handoffs WHERE card_id IN ({}))", card_ids), &[&pid])?);
        out.insert("approvals".into(), self.dump_rows(
            &format!("SELECT * FROM approvals WHERE card_id IN ({})", card_ids), &[&pid])?);
        Ok(Value::Object(out))
    }

    /// 把一张表（或子查询结果）导出为 JSON 数组（列名 → 值）
    fn dump_rows(&self, sql: &str, args: &[&str]) -> SqlResult<Value> {
        let mut stmt = self.conn.prepare(sql)?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let params: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |r| {
            let mut m = Map::new();
            for (i, c) in cols.iter().enumerate() {
                let v: rusqlite::types::Value = r.get(i)?;
                m.insert(c.clone(), sqlval_to_json(v));
            }
            Ok(Value::Object(m))
        })?;
        Ok(Value::Array(rows.collect::<SqlResult<_>>()?))
    }

    /// 导入项目 JSON（INSERT OR REPLACE，幂等）；返回各表行数统计
    pub fn import_project(&self, data: &Value) -> Result<Value, ApiErr> {
        if data.get("format").and_then(Value::as_str) != Some("baton-export/v1") {
            return Err(ApiErr::bad_request("not a baton-export/v1 file"));
        }
        // 外键安全顺序：成员 → 项目 → 看板 → 列 → 卡片 → 其余依附表
        let order = [
            "members", "projects", "boards", "lists", "cards", "threads", "comments",
            "links", "artifacts", "work_nodes", "handoffs", "handoff_timeline", "approvals",
        ];
        let mut stats = Map::new();
        for table in order {
            let rows = data.get(table).and_then(Value::as_array).cloned().unwrap_or_default();
            // 生成列（如 cards.progress_percent/handoff_state）不可写入，导入时剔除
            let skip = self.generated_cols(table)?;
            let mut n = 0usize;
            for row in &rows {
                let Some(obj) = row.as_object() else { continue };
                let cols: Vec<&str> = obj.keys().map(String::as_str)
                    .filter(|c| !skip.contains(*c)).collect();
                let sql = format!(
                    "INSERT OR REPLACE INTO {}({}) VALUES({})",
                    table,
                    cols.join(","),
                    cols.iter().map(|_| "?").collect::<Vec<_>>().join(",")
                );
                let vals: Vec<rusqlite::types::Value> =
                    cols.iter().map(|c| json_to_sqlval(&obj[*c])).collect();
                self.conn.execute(&sql, rusqlite::params_from_iter(vals))?;
                n += 1;
            }
            stats.insert(table.into(), json!(n));
        }
        self.log_event("u-owner", "project",
            data.pointer("/projects/0/id").and_then(Value::as_str).unwrap_or(""),
            "import", json!({}));
        Ok(json!({"ok": true, "imported": Value::Object(stats)}))
    }

    /// 单卡 Markdown 渲染（导出用，仅供人阅读）
    pub fn card_markdown(&self, card_id: &str) -> SqlResult<String> {
        let card = self.card_detail(card_id)?;
        let mut md = format!("# {}\n\n", card["title"].as_str().unwrap_or(""));
        md.push_str(&format!("- id: {}\n- rev: {}\n- 创建: {} by {}\n\n",
            card_id, card["rev"], card["created_at"].as_str().unwrap_or(""),
            card["created_by"].as_str().unwrap_or("")));
        let desc = card["description"].as_str().unwrap_or("");
        if !desc.is_empty() {
            md.push_str("## 描述\n\n");
            md.push_str(desc);
            md.push_str("\n\n");
        }
        if let Some(ts) = card["threads"].as_array() {
            md.push_str("## 讨论\n\n");
            for t in ts {
                md.push_str(&format!("### {}（{}）\n\n",
                    t["title"].as_str().unwrap_or("话题"), t["status"].as_str().unwrap_or("")));
                if let Some(cs) = t["comments"].as_array() {
                    for c in cs {
                        md.push_str(&format!("- **{}** [{}] ({}): {}\n",
                            c["author_id"].as_str().unwrap_or(""),
                            c["kind"].as_str().unwrap_or(""),
                            c["created_at"].as_str().unwrap_or(""),
                            c["body"].as_str().unwrap_or("")));
                    }
                }
                md.push('\n');
            }
        }
        Ok(md)
    }

    /// 项目内所有卡片 id（导出 Markdown 用）
    pub fn project_card_ids(&self, project_id: &str) -> SqlResult<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title FROM cards WHERE project_id=?1 ORDER BY created_at")?;
        let rows = stmt.query_map(params![project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// 表的生成列（PRAGMA table_xinfo 的 hidden>0），导入时需剔除
    fn generated_cols(&self, table: &str) -> SqlResult<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_xinfo({})", table))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, i64>(6)?))
        })?;
        Ok(rows.collect::<SqlResult<Vec<_>>>()?.into_iter()
            .filter(|(_, hidden)| *hidden > 0)
            .map(|(name, _)| name)
            .collect())
    }
}

fn sqlval_to_json(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(i) => json!(i),
        rusqlite::types::Value::Real(f) => json!(f),
        rusqlite::types::Value::Text(s) => json!(s),
        rusqlite::types::Value::Blob(b) => json!(format!("<blob {} bytes>", b.len())),
    }
}

fn json_to_sqlval(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rusqlite::types::Value::Integer(i)
            } else {
                rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        other => rusqlite::types::Value::Text(other.to_string()),
    }
}

// ------------------------------------------------------------------ git probe

/// 探测目录所在 git 仓库的根路径与当前分支（供 Session 进板自动声明工作现场）
fn probe_repo(cwd: &str) -> (Option<String>, Option<String>) {
    use std::process::Command;
    let get = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
        if !out.status.success() { return None; }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    let root = get(&["rev-parse", "--show-toplevel"]);
    let branch = get(&["rev-parse", "--abbrev-ref", "HEAD"]);
    (root, branch)
}

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
