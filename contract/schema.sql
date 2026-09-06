-- ============================================================================
-- Baton — SQLite Schema (开发契约 v0.1)
-- 对齐 PRD §4.6 卡片数据模型 / §4.6.5 存储映射
-- 要求 SQLite >= 3.31（生成列）; 建议 3.35+（RETURNING）
-- ============================================================================

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ----------------------------------------------------------------------------
-- 成员：人 与 Agent 统一抽象（PRD §2.2）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS members (
    id            TEXT PRIMARY KEY,                -- ULID
    kind          TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
    name          TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'member'
                  CHECK (role IN ('owner', 'member', 'worker', 'coordinator', 'observer')),
    avatar        TEXT,
    -- Agent 专属（kind='agent' 时使用）
    agent_json    TEXT,                            -- {capabilities:[], token_hash, rate_limit, concurrency_quota}
    revoked_at    TEXT,                            -- 吊销时间（Agent token / 成员停用）
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- ----------------------------------------------------------------------------
-- 项目 / 看板 / 列（PRD §2.1, F-101~F-104）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS projects (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    archived_at   TEXT
);

CREATE TABLE IF NOT EXISTS boards (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    position      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    archived_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_boards_project ON boards(project_id);

CREATE TABLE IF NOT EXISTS lists (
    id            TEXT PRIMARY KEY,
    board_id      TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    position      INTEGER NOT NULL DEFAULT 0,
    wip_limit     INTEGER,                         -- WIP 上限（F-102）
    policy_json   TEXT NOT NULL DEFAULT '{}',      -- 列策略（F-304）:
                                                   --   {enter_roles:[], require_approval:'human'|'coordinator'|null,
                                                   --    on_enter:[], require_progress_summary:true, block_if_open_threads:false}
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    archived_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_lists_board ON lists(board_id);

-- ----------------------------------------------------------------------------
-- 卡片（PRD §4.6.2）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cards (
    id            TEXT PRIMARY KEY,                -- ULID
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    board_id      TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    list_id       TEXT NOT NULL REFERENCES lists(id),
    position      INTEGER NOT NULL DEFAULT 0,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',        -- Markdown + YAML frontmatter
    rev           INTEGER NOT NULL DEFAULT 1,      -- 乐观并发版本号（F-302）
    priority      TEXT NOT NULL DEFAULT 'medium'
                  CHECK (priority IN ('urgent', 'high', 'medium', 'low')),
    due_at        TEXT,                            -- ISO 8601
    assignee_id   TEXT REFERENCES members(id),     -- NULL = 抢单池
    parent_id     TEXT REFERENCES cards(id),       -- 父子卡片树（F-107）
    ext_json      TEXT NOT NULL DEFAULT '{}',      -- CardExt（PRD §4.6.4），含 schema_rev
    created_by    TEXT NOT NULL REFERENCES members(id),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    archived_at   TEXT,
    -- 高频查询字段从 ext_json 提取为生成列（PRD §4.6.5）
    progress_percent INTEGER GENERATED ALWAYS AS (json_extract(ext_json, '$.progress.percent')) VIRTUAL,
    handoff_state    TEXT    GENERATED ALWAYS AS (json_extract(ext_json, '$.handoff.state')) VIRTUAL
);
CREATE INDEX IF NOT EXISTS idx_cards_list     ON cards(list_id);
CREATE INDEX IF NOT EXISTS idx_cards_board    ON cards(board_id);
CREATE INDEX IF NOT EXISTS idx_cards_assignee ON cards(assignee_id);
CREATE INDEX IF NOT EXISTS idx_cards_parent   ON cards(parent_id);
CREATE INDEX IF NOT EXISTS idx_cards_handoff  ON cards(handoff_state) WHERE handoff_state IS NOT NULL;

-- 标签（F-103）
CREATE TABLE IF NOT EXISTS labels (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    color         TEXT,
    UNIQUE (project_id, name)
);
CREATE TABLE IF NOT EXISTS card_labels (
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    label_id      TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (card_id, label_id)
);

-- 卡片依赖（F-106）：blocks / blocked_by / relates_to
CREATE TABLE IF NOT EXISTS card_deps (
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    other_card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    relation      TEXT NOT NULL CHECK (relation IN ('blocks', 'blocked_by', 'relates_to')),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (card_id, other_card_id, relation)
);
CREATE INDEX IF NOT EXISTS idx_card_deps_other ON card_deps(other_card_id);

-- ----------------------------------------------------------------------------
-- 认领租约（PRD §4.3 F-301，易变数据独立成表，不进 event 物化状态）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS claims (
    card_id       TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
    holder_id     TEXT NOT NULL REFERENCES members(id),
    run_id        TEXT,                            -- 关联执行记录
    session_id    TEXT,                            -- 认领时的 Agent Session（旧库由迁移补列）
    lease_until   TEXT NOT NULL,                   -- 到期自动释放
    acquired_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_claims_holder ON claims(holder_id);
CREATE INDEX IF NOT EXISTS idx_claims_lease  ON claims(lease_until);

-- ----------------------------------------------------------------------------
-- 协同参与（多 Agent 共同完成同一任务）：租约（claims）只有一条，是"主驾"，
-- 负责状态机推进；参与者表记录"副驾"——协同者可以评论/汇报进度/上传产物/移列
-- （乐观锁 rev 仍兜底并发冲突），但不承担主责。
-- 到场/离场是显式动作（join/leave），让"谁在协同"在板上可见。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS card_participants (
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    member_id     TEXT NOT NULL REFERENCES members(id),
    session_id    TEXT,                            -- 参与时的 Agent Session
    joined_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    left_at       TEXT,                            -- NULL = 协同中
    PRIMARY KEY (card_id, member_id)
);
CREATE INDEX IF NOT EXISTS idx_card_participants_member ON card_participants(member_id);

-- ----------------------------------------------------------------------------
-- Agent Session（会话实例）：任务分配的真实对象。
-- members 表里的 Agent 是"编制"（持久身份 + Token + 能力）；Session 是某次具体出勤
-- （某工具的一次对话 / 某进程），进板时声明 scope 与工作现场，离开或超时判死。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL REFERENCES members(id),
    project_id    TEXT REFERENCES projects(id),    -- 声明的 scope（可空 = 全部）
    board_id      TEXT REFERENCES boards(id),
    cwd           TEXT,                            -- 进程工作目录
    repo_path     TEXT,                            -- 自动探测的 git 仓库根
    branch        TEXT,                            -- 自动探测的分支
    status        TEXT NOT NULL DEFAULT 'active'
                  CHECK (status IN ('active','ended')),   -- stale 由心跳超时计算得出，不落库
    parent_session_id TEXT REFERENCES sessions(id),       -- resume 接力链
    meta_json     TEXT NOT NULL DEFAULT '{}',      -- clientInfo/pid/工具厂商等自报信息
    started_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_heartbeat TEXT,
    ended_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_id, status);
CREATE INDEX IF NOT EXISTS idx_sessions_hb ON sessions(last_heartbeat);

-- 执行记录（PRD §2.1 Run）
CREATE TABLE IF NOT EXISTS runs (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    agent_id      TEXT NOT NULL REFERENCES members(id),
    status        TEXT NOT NULL DEFAULT 'running'
                  CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
    summary       TEXT NOT NULL DEFAULT '',
    started_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ended_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_card ON runs(card_id);

-- ----------------------------------------------------------------------------
-- 讨论区：多话题 × 多轮（PRD §4.6.3）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS threads (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    title         TEXT,                            -- 话题名
    status        TEXT NOT NULL DEFAULT 'open'
                  CHECK (status IN ('open', 'resolved', 'wontfix')),
    anchor_json   TEXT,                            -- {kind, ref_id, quote?}
    created_by    TEXT NOT NULL REFERENCES members(id),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_by   TEXT REFERENCES members(id),
    resolved_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_threads_card ON threads(card_id, status);

CREATE TABLE IF NOT EXISTS comments (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    reply_to      TEXT REFERENCES comments(id),
    author_id     TEXT NOT NULL REFERENCES members(id),
    kind          TEXT NOT NULL DEFAULT 'chat'
                  CHECK (kind IN ('chat', 'progress', 'system', 'handoff', 'approval')),
    body          TEXT NOT NULL,                   -- Markdown
    mentions_json TEXT NOT NULL DEFAULT '[]',      -- MemberRef[]
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    edited_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_comments_card   ON comments(card_id, created_at);
CREATE INDEX IF NOT EXISTS idx_comments_thread ON comments(thread_id, created_at);

-- ----------------------------------------------------------------------------
-- 链接：需求来源 + 需求文档 统一存表（PRD §4.6.4a / §4.6.5）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS links (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    category      TEXT NOT NULL CHECK (category IN ('source', 'doc')),
    -- source: jira/meego/github_issue/url/file + key/title/relation/synced_at
    system        TEXT CHECK (system IN ('jira', 'meego', 'github_issue', 'url', 'file')),
    key           TEXT,                            -- 如 "PROJ-1234"
    relation      TEXT CHECK (relation IN ('origin', 'related')),
    synced_at     TEXT,
    -- doc: url/local_file/artifact + title/path/artifact_id
    kind          TEXT CHECK (kind IN ('url', 'local_file', 'artifact')),
    url           TEXT,
    path          TEXT,                            -- 本机路径
    artifact_id   TEXT,
    title         TEXT NOT NULL DEFAULT '',
    position      INTEGER NOT NULL DEFAULT 0,
    created_by    TEXT NOT NULL REFERENCES members(id),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_links_card   ON links(card_id, category);
CREATE INDEX IF NOT EXISTS idx_links_system ON links(system, key);   -- "找出所有关联 Jira 的卡"

-- ----------------------------------------------------------------------------
-- 产物（PRD §2.1 Artifact；文件本体存 artifacts/<card_id>/）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS artifacts (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    run_id        TEXT REFERENCES runs(id),        -- 由哪次执行产出（可空）
    kind          TEXT NOT NULL DEFAULT 'file'
                  CHECK (kind IN ('file', 'diff', 'doc', 'image', 'log')),
    name          TEXT NOT NULL,
    path          TEXT NOT NULL,                   -- 相对工作区路径
    mime          TEXT,
    size_bytes    INTEGER,
    sha256        TEXT,
    uploaded_by   TEXT NOT NULL REFERENCES members(id),
    uploaded_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_artifacts_card ON artifacts(card_id);

-- ----------------------------------------------------------------------------
-- 工作现场节点（PRD §4.6.4d；root 指针存于 cards.ext_json.worksites.root）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS work_nodes (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('main', 'worktree')),
    path          TEXT NOT NULL,                   -- 本机绝对路径
    branch        TEXT NOT NULL,
    purpose       TEXT,
    owner_id      TEXT REFERENCES members(id),     -- 当前在该现场干活的 Agent/人
    bound_card_id TEXT REFERENCES cards(id),       -- 绑定的（子）卡片，跨卡拓扑
    observed_json TEXT,                            -- GitStatusSnapshot（含 snapshot_at）
    created_by    TEXT NOT NULL REFERENCES members(id),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    removed_at    TEXT                             -- worktree 清理后保留记录
);
CREATE INDEX IF NOT EXISTS idx_work_nodes_card  ON work_nodes(card_id);
CREATE INDEX IF NOT EXISTS idx_work_nodes_bound ON work_nodes(bound_card_id);

-- ----------------------------------------------------------------------------
-- 移交（PRD §4.6.4e）：当前状态镜像到 cards.ext_json.handoff，此处存全量
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS handoffs (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    state         TEXT NOT NULL DEFAULT 'preparing'
                  CHECK (state IN ('preparing', 'ready', 'accepted', 'cancelled')),
    from_id       TEXT NOT NULL REFERENCES members(id),
    to_id         TEXT REFERENCES members(id),     -- NULL = 公开可认领
    reason        TEXT,
    package_json  TEXT,                            -- HandoffPackage
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_handoffs_card  ON handoffs(card_id);
CREATE INDEX IF NOT EXISTS idx_handoffs_state ON handoffs(state);

CREATE TABLE IF NOT EXISTS handoff_timeline (
    id            TEXT PRIMARY KEY,
    handoff_id    TEXT NOT NULL REFERENCES handoffs(id) ON DELETE CASCADE,
    at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    by_id         TEXT NOT NULL REFERENCES members(id),
    action        TEXT NOT NULL,                   -- prepare/ready/accept/cancel/force_takeover...
    note          TEXT
);
CREATE INDEX IF NOT EXISTS idx_handoff_tl ON handoff_timeline(handoff_id, at);

-- ----------------------------------------------------------------------------
-- 审批（PRD F-304 / F-402）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS approvals (
    id            TEXT PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    list_id       TEXT REFERENCES lists(id),       -- 目标列（列策略触发的审批）
    requested_by  TEXT NOT NULL REFERENCES members(id),
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'approved', 'rejected')),
    note          TEXT,
    decided_by    TEXT REFERENCES members(id),
    decided_at    TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);

-- ----------------------------------------------------------------------------
-- 幂等键（F-307）与事件日志（F-502，append-only）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS idempotency_keys (
    key           TEXT PRIMARY KEY,
    actor_id      TEXT NOT NULL REFERENCES members(id),
    request_hash  TEXT NOT NULL,
    response_json TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS events (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    actor_id      TEXT REFERENCES members(id),     -- NULL = 系统
    entity        TEXT NOT NULL,                   -- card/thread/comment/handoff/...
    entity_id     TEXT NOT NULL,
    action        TEXT NOT NULL,                   -- create/update/move/claim/...
    payload_json  TEXT NOT NULL DEFAULT '{}'       -- 变更前后差异
);
CREATE INDEX IF NOT EXISTS idx_events_entity ON events(entity, entity_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_time   ON events(at);

-- ----------------------------------------------------------------------------
-- 常用视图
-- ----------------------------------------------------------------------------
-- 活跃卡片（未归档）及其租约快照
CREATE VIEW IF NOT EXISTS v_active_cards AS
SELECT c.*,
       cl.holder_id   AS claim_holder_id,
       cl.lease_until AS claim_lease_until
FROM cards c
LEFT JOIN claims cl ON cl.card_id = c.id
WHERE c.archived_at IS NULL;

-- 每卡未结话题数
CREATE VIEW IF NOT EXISTS v_card_open_threads AS
SELECT card_id, COUNT(*) AS open_thread_count
FROM threads
WHERE status = 'open'
GROUP BY card_id;
