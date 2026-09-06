// ============================================================================
// Baton — TypeScript 类型定义（开发契约 v0.1）
// 对齐 PRD §4.6 卡片数据模型；与 schema.sql 一一对应
// ============================================================================

/** ISO 8601 时间戳 */
export type ISODateTime = string;

/** ULID 主键 */
export type Id = string;

// ---------------------------------------------------------------------------
// 成员（PRD §2.2）：人 与 Agent 统一抽象
// ---------------------------------------------------------------------------

export type MemberKind = 'human' | 'agent';

export type MemberRole =
  | 'owner'        // 人：全部权限
  | 'member'       // 人：建卡/移动/评论/审批
  | 'worker'       // Agent：认领执行
  | 'coordinator'  // Agent：额外可创建/分解卡片、指派、调优先级（开放角色，不内置实现）
  | 'observer';    // Agent：只读 + 评论

export interface MemberRef {
  kind: MemberKind;
  id: Id;
  name: string;
}

export interface Member extends MemberRef {
  role: MemberRole;
  avatar?: string;
  /** kind === 'agent' 时存在 */
  agent?: AgentProfile;
  revoked_at?: ISODateTime;
  created_at: ISODateTime;
}

export interface AgentProfile {
  capabilities: string[];          // 能力标签：coding / writing / research ...
  token_hash: string;              // 本地 Token（仅存 SHA-256 哈希；F-212）
  last_heartbeat?: ISODateTime;    // 最近心跳（F-213，120s 内视为在线）
  rate_limit_per_min?: number;     // F-308，默认 60
  concurrency_quota?: number;      // 同时持有卡片上限
}

// ---------------------------------------------------------------------------
// Agent Session（会话实例）：任务分配的真实对象
// ---------------------------------------------------------------------------
//
// Agent Profile（members 表）是"编制"：持久身份、Token、能力标签。
// Session 是"出勤"：某工具的一次对话/某进程，进板时声明 scope 与工作现场。
// 注意与 MCP 协议层的连接无关 —— 2026-07-28 无状态核心要求跨请求状态由
// 客户端显式携带标识符，session_id 即业务层的显式标识。

export type SessionStatus = 'active' | 'ended';
// 'stale' 是计算状态（active 但 180s 无心跳），不落库

export interface AgentSession {
  id: Id;
  agent_id: Id;                  // → Agent Profile
  project_id?: Id;               // 声明的 scope
  board_id?: Id;
  cwd?: string;                  // 进程工作目录
  repo_path?: string;            // 自动探测的 git 仓库根
  branch?: string;               // 自动探测的分支
  status: SessionStatus;
  parent_session_id?: Id;        // resume 接力链
  meta: Record<string, unknown>; // clientInfo / pid / 工具厂商等自报信息
  started_at: ISODateTime;
  last_heartbeat?: ISODateTime;  // 心跳 = 续命 + 自动续期本 session 持有的租约
  ended_at?: ISODateTime;
}

// ---------------------------------------------------------------------------
// 项目 / 看板 / 列
// ---------------------------------------------------------------------------

export interface Project {
  id: Id;
  name: string;
  description: string;
  created_at: ISODateTime;
  archived_at?: ISODateTime;
}

export interface Board {
  id: Id;
  project_id: Id;
  name: string;
  position: number;
  created_at: ISODateTime;
  archived_at?: ISODateTime;
}

/** 列策略（F-304） */
export interface ColumnPolicy {
  enter_roles?: MemberRole[];                  // 允许谁进入（空 = 不限）
  require_approval?: 'human' | 'coordinator' | null; // 进入需审批
  require_progress_summary?: boolean;          // 移入本列时强制更新 progress.summary
  block_if_open_threads?: boolean;             // 存在未结话题时禁止进入
  is_done?: boolean;                           // 完成列：进入时校验 blocked_by 依赖全部完成（F-106）
  on_enter?: ColumnAction[];                   // 自动触发动作
}

export interface ColumnAction {
  type: 'assign' | 'notify' | 'request_approval' | 'run_script';
  params: Record<string, unknown>;
}

export interface List {
  id: Id;
  board_id: Id;
  name: string;
  position: number;
  wip_limit?: number;
  policy: ColumnPolicy;
  created_at: ISODateTime;
  archived_at?: ISODateTime;
}

// ---------------------------------------------------------------------------
// 卡片（PRD §4.6.2）
// ---------------------------------------------------------------------------

export type Priority = 'urgent' | 'high' | 'medium' | 'low';

export interface Claim {
  holder: MemberRef;
  lease_until: ISODateTime;        // 到期自动释放（F-301）
  run_id?: Id;
  acquired_at: ISODateTime;
}

// 协同参与（多 Agent 同卡协作）：租约是"主驾"，参与者是"副驾"
export interface CardParticipant {
  card_id: Id;
  member_id: Id;
  session_id?: Id;
  joined_at: ISODateTime;
  left_at?: ISODateTime;           // NULL = 协同中
}

export type DepRelation = 'blocks' | 'blocked_by' | 'relates_to';

export interface Card {
  id: Id;
  project_id: Id;
  board_id: Id;
  list_id: Id;                     // 当前所在列 = 状态机当前状态
  position: number;
  title: string;
  description: string;             // Markdown + YAML frontmatter（验收标准等）
  rev: number;                     // 乐观并发版本号（F-302），写操作必须携带
  priority: Priority;
  labels: string[];                // label id 列表
  due_at?: ISODateTime;
  assignee?: MemberRef;            // 缺省 = 抢单池
  claim?: Claim;                   // 当前租约（claims 表物化）
  parent_id?: Id;
  blocked_by: Id[];                // 依赖（card_deps 表，relation='blocked_by'）
  created_by: MemberRef;
  created_at: ISODateTime;
  updated_at: ISODateTime;
  archived_at?: ISODateTime;
  ext: CardExt;                    // §4.6.4
}

// ---------------------------------------------------------------------------
// 讨论区（PRD §4.6.3）：多话题 × 多轮
// ---------------------------------------------------------------------------

export type ThreadStatus = 'open' | 'resolved' | 'wontfix';

export interface ThreadAnchor {
  kind: 'description' | 'comment' | 'checklist_item' | 'artifact';
  ref_id: Id;
  quote?: string;
}

export interface Thread {
  id: Id;
  card_id: Id;
  title?: string;                  // 话题名，如 "验收标准歧义"
  status: ThreadStatus;
  anchor?: ThreadAnchor;
  created_by: MemberRef;
  created_at: ISODateTime;
  resolved_by?: MemberRef;
  resolved_at?: ISODateTime;
}

export type CommentKind =
  | 'chat'       // 普通对话
  | 'progress'   // 进度日志（聚合进 ext.progress 历史）
  | 'system'     // 系统事件（移动卡片、租约变更…）
  | 'handoff'    // 移交相关
  | 'approval';  // 审批记录

export interface Comment {
  id: Id;
  card_id: Id;
  thread_id: Id;
  reply_to?: Id;                   // 话题内的直接回复
  author: MemberRef;
  kind: CommentKind;
  body: string;                    // Markdown
  mentions: MemberRef[];
  created_at: ISODateTime;
  edited_at?: ISODateTime;
}

// ---------------------------------------------------------------------------
// ext：结构化扩展信息（PRD §4.6.4）
// ---------------------------------------------------------------------------

export interface CardExt {
  schema_rev: 1;                   // ext 结构版本，单调递增，启动时迁移
  requirements?: RequirementsExt;
  progress?: ProgressExt;
  git?: GitExt;
  worksite?: WorksiteExt;
  handoff?: HandoffExt;
  /** 插件自定义字段，键必须以 `x-<vendor>.` 前缀命名，核心升级永不触碰 */
  custom?: Record<string, unknown>;
}

// ---------- (a) requirements — 需求来源与需求文档 ----------

export type SourceSystem = 'jira' | 'meego' | 'github_issue' | 'url' | 'file';

export interface SourceLink {
  system: SourceSystem;
  url: string;                     // 如 https://jira.corp/browse/PROJ-1234
  key?: string;                    // 结构化键，如 "PROJ-1234"
  title?: string;                  // 抓取或手填的标题快照
  relation: 'origin' | 'related';  // origin = 需求来源
  synced_at?: ISODateTime;         // v0.3 双向同步插件预留
}

export interface DocLink {
  kind: 'url' | 'local_file' | 'artifact';
  url?: string;
  path?: string;                   // kind=local_file：本机路径
  artifact_id?: Id;                // kind=artifact：卡内产物
  title: string;
}

export interface RequirementsExt {
  source_links: SourceLink[];      // Jira / MeeGo 等任务关联（MVP 只存链接，不同步）
  doc_links: DocLink[];
  attachment_refs: Id[];           // 需求附件 → artifact id
}

// ---------- (b) progress — 当前工作进度 ----------

export interface ProgressMilestone {
  title: string;
  done: boolean;
  done_at?: ISODateTime;
}

export interface ProgressExt {
  percent: number;                 // 0-100，持有者自评
  summary: string;                 // 一句话现状（移列时按列策略强制更新）
  milestones: ProgressMilestone[];
  blockers?: string;               // 当前卡点
  updated_by: MemberRef;
  updated_at: ISODateTime;
  // 历史轨迹不冗余存储：由 kind='progress' 的 Comment 聚合
}

// ---------- (c) git — 仓库上下文（declared 与 observed 分离） ----------

export interface GitStatusSnapshot {
  staged: number;                  // 已 stage 文件数
  unstaged: number;                // 已修改未 stage 文件数
  untracked: number;
  clean: boolean;                  // 三者全 0
  ahead: number;                   // 领先 remote/base 的提交数
  behind: number;
  last_commit?: { sha: string; message: string; at: ISODateTime };
  /** ⚠️ 快照时间，UI 必须显示"截至 xx:xx"；卡片永不声称实时 git 状态 */
  snapshot_at: ISODateTime;
  snapshot_by: MemberRef;
}

export interface RepoCtx {
  repo_path: string;               // 主工作目录（本机绝对路径）
  branch: string;                  // 主工作分支
  declared: {                      // 声明层：Agent 打算/应该在哪干活
    base_branch?: string;
    pr_url?: string;
  };
  observed?: GitStatusSnapshot;    // 探测层：最近一次真实探测（git.refresh 触发）
}

export interface GitExt {
  repos: RepoCtx[];                // 一张卡可关联多个仓库
}

// ---------- (d) worksite — 工作现场拓扑 ----------

export interface WorkNode {
  id: Id;
  kind: 'main' | 'worktree';
  path: string;                    // 本机绝对路径
  branch: string;
  purpose?: string;                // "并行开发支付模块"
  owner?: MemberRef;               // 当前在该现场干活的 Agent/人
  bound_card_id?: Id;              // 绑定的（子）卡片，形成跨卡拓扑
  observed?: GitStatusSnapshot;    // 该节点独立脏状态快照
  created_by: MemberRef;
  created_at: ISODateTime;
  removed_at?: ISODateTime;        // worktree 清理后保留记录
}

export interface WorksiteExt {
  root: Id;                        // → WorkNode.id，主工作目录节点
  nodes: WorkNode[];
}

// ---------- (e) handoff — 工作现场移交 ----------

export type HandoffState = 'none' | 'preparing' | 'ready' | 'accepted' | 'cancelled';

export interface HandoffPackage {
  context_note: string;            // Markdown：已完成什么、卡在哪、坑、建议下一步
  worksite_snapshot: WorksiteExt;  // 移交时的工作现场快照
  env_notes?: string;              // 环境说明：依赖、如何跑起来
  open_threads: Id[];              // 未 resolved 话题（接手方必读）
  artifact_refs: Id[];
  prepared_at: ISODateTime;
}

export interface HandoffEvent {
  at: ISODateTime;
  by: MemberRef;
  action: 'prepare' | 'ready' | 'accept' | 'cancel' | 'force_takeover';
  note?: string;
}

/**
 * 状态机：
 *   none ──prepare──▶ preparing ──ready──▶ ready ──accept──▶ accepted ─▶ none（新一轮）
 *                        ▲                    │                  │
 *                        └────── cancel ──────┴──── cancel ──────┘
 */
export interface HandoffExt {
  state: HandoffState;
  from?: MemberRef;                // 移交方
  to?: MemberRef;                  // 指定接手方；缺省 = 公开可认领
  reason?: string;                 // "超出能力范围，需要前端专家"
  package?: HandoffPackage;
  timeline: HandoffEvent[];        // 永久留痕（handoff_timeline 表存全量）
}

// ---------------------------------------------------------------------------
// 产物 / 执行记录 / 审批
// ---------------------------------------------------------------------------

export type ArtifactKind = 'file' | 'diff' | 'doc' | 'image' | 'log';

export interface Artifact {
  id: Id;
  card_id: Id;
  run_id?: Id;
  kind: ArtifactKind;
  name: string;
  path: string;                    // 相对工作区路径，本体存 artifacts/<card_id>/
  mime?: string;
  size_bytes?: number;
  sha256?: string;
  uploaded_by: MemberRef;
  uploaded_at: ISODateTime;
}

export type RunStatus = 'running' | 'completed' | 'failed' | 'cancelled';

export interface Run {
  id: Id;
  card_id: Id;
  agent: MemberRef;
  status: RunStatus;
  summary: string;
  started_at: ISODateTime;
  ended_at?: ISODateTime;
}

export type ApprovalStatus = 'pending' | 'approved' | 'rejected';

export interface Approval {
  id: Id;
  card_id: Id;
  list_id?: Id;                    // 目标列（列策略触发的审批）
  requested_by: MemberRef;
  status: ApprovalStatus;
  note?: string;
  decided_by?: MemberRef;
  decided_at?: ISODateTime;
  created_at: ISODateTime;
}

// ---------------------------------------------------------------------------
// 事件日志（F-502，append-only，统一事实源）
// ---------------------------------------------------------------------------

export interface Event {
  seq: number;
  at: ISODateTime;
  actor?: MemberRef;               // 缺省 = 系统
  entity: 'card' | 'thread' | 'comment' | 'link' | 'artifact'
        | 'work_node' | 'handoff' | 'approval' | 'claim' | 'run'
        | 'board' | 'list' | 'project' | 'member';
  entity_id: Id;
  action: string;                  // create/update/move/claim/release/takeover/import/...
  payload: Record<string, unknown>; // 变更前后差异
}

// ---------------------------------------------------------------------------
// 幂等写（F-307）与导出（F-503）
// ---------------------------------------------------------------------------

/** 幂等键记录（idempotency_keys 表）：HTTP 写请求携带 Idempotency-Key 头时生效 */
export interface IdempotencyKey {
  key: string;
  actor_id: Id;
  request_hash: string;            // sha256(method + path + body)
  response_json?: string;          // 首个成功响应，重放时原样返回
  created_at: ISODateTime;
}

/** 项目导出包（baton export）：project.json 的顶层结构 */
export interface ProjectExport {
  format: 'baton-export/v1';
  exported_at: ISODateTime;
  members: Record<string, unknown>[];
  projects: Record<string, unknown>[];
  boards: Record<string, unknown>[];
  lists: Record<string, unknown>[];
  cards: Record<string, unknown>[];
  threads: Record<string, unknown>[];
  comments: Record<string, unknown>[];
  links: Record<string, unknown>[];
  artifacts: Record<string, unknown>[];
  work_nodes: Record<string, unknown>[];
  handoffs: Record<string, unknown>[];
  handoff_timeline: Record<string, unknown>[];
  approvals: Record<string, unknown>[];
}
