// Baton Web — API 客户端（对齐 core HTTP API / contract/types.ts）
// dev 环境走 vite 代理（/api → 7700）；WebUI 模式页面由 core:7700 直接 serve（同源，空串）；
// 仅 Tauri webview（tauri.localhost 等 origin）需要显式直连内嵌 core
export const API_BASE =
  import.meta.env.DEV || window.location.port === "7700" ? "" : "http://127.0.0.1:7700";

export interface Claim {
  holder_id: string;
  lease_until: string;
}

export interface CardSummary {
  id: string;
  title: string;
  priority: string;
  assignee_id: string | null;
  rev: number;
  progress_percent: number | null;
  handoff_state: string | null;
  claim: Claim | null;
  open_threads: number;
  parent_id?: string | null;
  participants?: string[]; // 在场协同者（多 Agent 同卡协作）
}

export interface ChildCard {
  id: string;
  title: string;
  list_id: string;
  progress_percent: number | null;
  done: boolean;
}

export interface ColumnPolicy {
  require_approval?: string | null;
  require_progress_summary?: boolean;
  is_done?: boolean;
}

export interface ListWithCards {
  id: string;
  name: string;
  position: number;
  wip_limit: number | null;
  policy: ColumnPolicy;
  cards: CardSummary[];
}

export interface BoardState {
  board_id: string;
  lists: ListWithCards[];
}

export interface BoardRef {
  id: string;
  name: string;
}

export interface Project {
  id: string;
  name: string;
  description: string;
  boards: BoardRef[];
}

export interface Comment {
  id: string;
  author_id: string;
  kind: string;
  body: string;
  created_at: string;
  mentions?: string[];
  reply_to?: string | null; // 直接回复的评论 id
}

export interface Thread {
  id: string;
  title: string | null;
  status: string;
  comments: Comment[];
}

export interface Link {
  id: string;
  category: "source" | "doc";
  system: string | null;
  key: string | null;
  relation: string | null;
  kind: string | null;
  url: string | null;
  path: string | null;
  title: string;
}

export interface WorkNode {
  id: string;
  kind: "main" | "worktree";
  path: string;
  branch: string;
  purpose: string | null;
  owner_id: string | null;
  bound_card_id: string | null;
  created_at: string;
}

export interface GitRepo {
  repo_path: string;
  branch: string;
  declared: { base_branch?: string | null };
  observed?: {
    staged?: number; unstaged?: number; untracked?: number; clean?: boolean;
    ahead?: number; behind?: number;
    last_commit?: { sha: string; message: string; at: string } | null;
    snapshot_at: string; snapshot_by: string; error?: string;
  };
}

export interface HandoffInfo {
  id: string;
  state: string;
  from_id: string | null;
  to_id: string | null;
  reason: string | null;
  package: { context_note?: string; env_notes?: string | null; open_threads?: string[] } | null;
  timeline: { at: string; by_id: string; action: string; note: string | null }[];
}

export interface Member {
  id: string;
  kind: "human" | "agent";
  name: string;
  role: string;
  revoked: boolean;
}

export interface Dep {
  relation: "blocked_by" | "blocks" | "relates_to";
  other_id: string;
  other_title: string;
  other_list_id: string;
  other_done: boolean;
}

export interface Artifact {
  id: string;
  kind: string;
  name: string;
  path: string;
  mime: string | null;
  size_bytes: number;
  uploaded_by: string;
  uploaded_at: string;
  content?: string; // GET /artifacts/{id} 时内联（≤256KB 文本）
}

export interface CardDetail {
  id: string;
  list_id: string;
  title: string;
  description: string;
  rev: number;
  priority: string;
  assignee_id: string | null;
  ext: {
    progress?: { percent: number; summary: string; blockers?: string };
    git?: { repos: GitRepo[] };
    worksite?: { root?: string };
    handoff?: { state: string };
  };
  created_by: string;
  created_at: string;
  updated_at: string;
  claim: Claim | null;
  parent?: { id: string; title: string } | null;
  children: ChildCard[];
  participants: string[]; // 在场协同者
  threads: Thread[];
  links: Link[];
  work_nodes: WorkNode[];
  handoff: HandoffInfo | null;
  artifacts: Artifact[];
  deps: Dep[];
}

export interface AgentSession {
  id: string;
  agent_id: string;
  project_id: string | null;
  board_id: string | null;
  cwd: string | null;
  repo_path: string | null;
  branch: string | null;
  status: "active" | "stale" | "ended";
  parent_session_id: string | null;
  started_at: string;
  last_heartbeat: string | null;
  ended_at: string | null;
  holding_cards: string[];
}

export interface AppNotification {
  seq: number;
  at: string;
  actor_id: string | null;
  kind: string;
  entity: string;
  entity_id: string;
  card_id: string | null;
}

export interface Approval {
  id: string;
  card_id: string;
  card_title: string;
  list_id: string;
  requested_by: string;
  status: string;
  note: string | null;
  created_at: string;
}

export interface AgentInfo {
  id: string;
  name: string;
  role: string;
  capabilities: string[];
  revoked: boolean;
  token_set: boolean;
  last_heartbeat: string | null;
  online: boolean;
  holding_cards: string[];
}

async function req<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  const r = await fetch(`${API_BASE}/api/v1${path}`, {
    method,
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await r.json();
  if (!r.ok) throw Object.assign(new Error(data.error ?? r.statusText), { status: r.status, data });
  return data as T;
}

export const api = {
  board: (boardId?: string) =>
    req<BoardState>(`/board${boardId ? `?board_id=${boardId}` : ""}`),
  projects: () => req<Project[]>("/projects"),
  createProject: (name: string, description = "", template = "software") =>
    req<Project>("/projects", "POST", { actor: "u-owner", name, description, template }),
  createBoard: (projectId: string, name: string) =>
    req<BoardRef>(`/projects/${projectId}/boards`, "POST", { actor: "u-owner", name }),
  renameProject: (projectId: string, name: string) =>
    req(`/projects/${projectId}/rename`, "POST", { actor: "u-owner", name }),
  deleteProject: (projectId: string) =>
    req(`/projects/${projectId}/delete`, "POST", { actor: "u-owner" }),
  agents: () => req<AgentInfo[]>("/agents"),
  installInfo: () => req<{ mcp_bin: string }>("/install-info"),
  sessions: () => req<AgentSession[]>("/sessions"),
  members: () => req<Member[]>("/members"),
  createAgent: (name: string, role: string, capabilities: string[]) =>
    req<{ id: string; token: string }>("/agents", "POST", { actor: "u-owner", name, role, capabilities }),
  rotateToken: (id: string) =>
    req<{ id: string; token: string }>(`/agents/${id}/token`, "POST", { actor: "u-owner" }),
  revokeAgent: (id: string) =>
    req(`/agents/${id}/revoke`, "POST", { actor: "u-owner" }),
  card: (id: string) => req<CardDetail>(`/cards/${id}`),
  createCard: (title: string, description = "", boardId?: string, parentId?: string) =>
    req<CardDetail>("/cards", "POST", {
      title, description, actor: "u-owner",
      ...(boardId ? { board_id: boardId } : {}),
      ...(parentId ? { parent_id: parentId } : {}),
    }),
  claim: (id: string, holder = "a-code") => req(`/cards/${id}/claim`, "POST", { holder }),
  release: (id: string) => req(`/cards/${id}/release`, "POST", { actor: "u-owner" }),
  comment: (id: string, body: string, author = "u-owner", kind = "chat", replyTo?: string, threadId?: string) =>
    req<CardDetail>(`/cards/${id}/comments`, "POST", {
      author, body, kind,
      ...(replyTo ? { reply_to: replyTo } : {}),
      ...(threadId ? { thread_id: threadId } : {}),
    }),
  createThread: (id: string, title: string) =>
    req<CardDetail>(`/cards/${id}/threads`, "POST", { actor: "u-owner", title }),
  progress: (id: string, percent: number, summary: string, actor = "a-code") =>
    req<CardDetail>(`/cards/${id}/progress`, "POST", { actor, percent, summary }),
  move: (id: string, listId: string, rev: number, actor = "u-owner") =>
    req<CardDetail & { approval_pending?: string }>(`/cards/${id}/move`, "POST", { actor, list_id: listId, rev }),
  approvals: (status?: string) =>
    req<Approval[]>(`/approvals${status ? `?status=${status}` : ""}`),
  notifications: (member = "u-owner", since = 0) =>
    req<AppNotification[]>(`/notifications?member=${member}&since=${since}`),
  decide: (id: string, decision: "approved" | "rejected", note = "") =>
    req(`/approvals/${id}/decide`, "POST", { actor: "u-owner", decision, note }),
  addLink: (cardId: string, link: Partial<Link> & { category: string; title: string }) =>
    req<CardDetail>(`/cards/${cardId}/links`, "POST", { actor: "u-owner", ...link }),
  gitAttach: (cardId: string, repoPath: string, branch: string, baseBranch?: string) =>
    req<CardDetail>(`/cards/${cardId}/git/attach`, "POST", { actor: "a-code", repo_path: repoPath, branch, base_branch: baseBranch }),
  gitRefresh: (cardId: string) =>
    req<CardDetail>(`/cards/${cardId}/git/refresh`, "POST", { actor: "a-code" }),
  worksiteAdd: (cardId: string, node: { kind: string; path: string; branch: string; purpose?: string; owner?: string }) =>
    req<CardDetail>(`/cards/${cardId}/worksite/nodes`, "POST", { actor: "a-code", ...node }),
  handoff: (cardId: string, action: "prepare" | "ready" | "accept" | "cancel", extra?: Record<string, unknown>) =>
    req<CardDetail>(`/cards/${cardId}/handoff/${action}`, "POST", { actor: "a-code", ...extra }),
  takeover: (cardId: string) =>
    req<CardDetail>(`/cards/${cardId}/takeover`, "POST", { actor: "u-owner" }),
  assign: (cardId: string, assignee: string | null) =>
    req<CardDetail>(`/cards/${cardId}/assign`, "POST", { actor: "u-owner", assignee }),
  addDep: (cardId: string, otherId: string, relation = "blocked_by") =>
    req<CardDetail>(`/cards/${cardId}/deps`, "POST", { actor: "u-owner", other_card_id: otherId, relation }),
  removeDep: (cardId: string, otherId: string, relation = "blocked_by") =>
    req<CardDetail>(`/cards/${cardId}/deps/remove`, "POST", { actor: "u-owner", other_card_id: otherId, relation }),
  uploadArtifact: (cardId: string, a: { name: string; content?: string; path?: string; kind?: string }) =>
    req<CardDetail>(`/cards/${cardId}/artifacts`, "POST", { actor: "u-owner", ...a }),
  artifact: (id: string) => req<Artifact>(`/artifacts/${id}`),
};

export const MEMBER_NAMES: Record<string, string> = {
  "u-owner": "Lance",
  "a-code": "code-agent",
  "a-review": "review-agent",
};

export const LIST_NAMES: Record<string, string> = {
  "l-ready": "Ready",
  "l-doing": "In Progress",
  "l-review": "Review",
  "l-done": "Done",
};
