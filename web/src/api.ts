// Baton Web — API 客户端（对齐 core HTTP API / contract/types.ts）

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
}

export interface ColumnPolicy {
  require_approval?: string | null;
  require_progress_summary?: boolean;
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

export interface Comment {
  id: string;
  author_id: string;
  kind: string;
  body: string;
  created_at: string;
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
  threads: Thread[];
  links: Link[];
  work_nodes: WorkNode[];
  handoff: HandoffInfo | null;
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

async function req<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  const r = await fetch(`/api/v1${path}`, {
    method,
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await r.json();
  if (!r.ok) throw Object.assign(new Error(data.error ?? r.statusText), { status: r.status, data });
  return data as T;
}

export const api = {
  board: () => req<BoardState>("/board"),
  card: (id: string) => req<CardDetail>(`/cards/${id}`),
  createCard: (title: string, description = "") =>
    req<CardDetail>("/cards", "POST", { title, description, actor: "u-owner" }),
  claim: (id: string, holder = "a-code") => req(`/cards/${id}/claim`, "POST", { holder }),
  release: (id: string) => req(`/cards/${id}/release`, "POST", { actor: "u-owner" }),
  comment: (id: string, body: string, author = "u-owner", kind = "chat") =>
    req<CardDetail>(`/cards/${id}/comments`, "POST", { author, body, kind }),
  progress: (id: string, percent: number, summary: string, actor = "a-code") =>
    req<CardDetail>(`/cards/${id}/progress`, "POST", { actor, percent, summary }),
  move: (id: string, listId: string, rev: number, actor = "u-owner") =>
    req<CardDetail & { approval_pending?: string }>(`/cards/${id}/move`, "POST", { actor, list_id: listId, rev }),
  approvals: (status = "pending") => req<Approval[]>(`/approvals?status=${status}`),
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
