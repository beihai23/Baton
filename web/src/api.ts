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

export interface ListWithCards {
  id: string;
  name: string;
  position: number;
  wip_limit: number | null;
  policy: Record<string, unknown>;
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

export interface CardDetail {
  id: string;
  list_id: string;
  title: string;
  description: string;
  rev: number;
  priority: string;
  assignee_id: string | null;
  ext: Record<string, unknown>;
  created_by: string;
  created_at: string;
  updated_at: string;
  claim: Claim | null;
  threads: Thread[];
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
    req<CardDetail>(`/cards/${id}/move`, "POST", { actor, list_id: listId, rev }),
};

export const MEMBER_NAMES: Record<string, string> = {
  "u-owner": "Lance",
  "a-code": "code-agent",
  "a-review": "review-agent",
};
