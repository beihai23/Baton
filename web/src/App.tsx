import { useCallback, useEffect, useRef, useState } from "react";
import {
  api, Approval, BoardState, CardDetail, CardSummary,
  ListWithCards, MEMBER_NAMES,
} from "./api";

const memberName = (id?: string | null) => (id ? MEMBER_NAMES[id] ?? id : "—");

type Tab = "讨论" | "需求" | "Git" | "现场" | "移交";

export default function App() {
  const [board, setBoard] = useState<BoardState | null>(null);
  const [selected, setSelected] = useState<CardDetail | null>(null);
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [showApprovals, setShowApprovals] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  const [toast, setToast] = useState<string | null>(null);
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selected?.id ?? null;

  const refresh = useCallback(async () => {
    try {
      setBoard(await api.board());
      setApprovals(await api.approvals());
      if (selectedIdRef.current) {
        setSelected(await api.card(selectedIdRef.current));
      }
    } catch { /* core 未启动时静默 */ }
  }, []);

  useEffect(() => {
    refresh();
    // 长轮询实时推送（F-401）：有新事件立即返回，否则服务端挂起 25s
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    (async function poll(since: number) {
      while (alive) {
        try {
          const r = await fetch(`/api/v1/events?since=${since}`);
          const d = await r.json();
          since = d.last_seq ?? since;
          if (d.events?.length) {
            clearTimeout(timer);
            timer = setTimeout(refresh, 100); // 防抖合并密集事件
          }
        } catch {
          await new Promise((r) => setTimeout(r, 3000)); // core 未启动时退避重试
        }
      }
    })(0);
    return () => { alive = false; clearTimeout(timer); };
  }, [refresh]);

  const showError = (e: unknown) => {
    const err = e as { data?: { error?: string }; message?: string };
    setToast(err?.data?.error ?? err?.message ?? "操作失败");
    setTimeout(() => setToast(null), 4000);
  };
  const info = (msg: string) => { setToast(msg); setTimeout(() => setToast(null), 3000); };

  const openCard = async (id: string) => {
    try { setSelected(await api.card(id)); } catch (e) { showError(e); }
  };

  const createCard = async () => {
    if (!newTitle.trim()) return;
    try { await api.createCard(newTitle.trim()); setNewTitle(""); }
    catch (e) { showError(e); }
  };

  const dropCard = async (card: CardSummary, list: ListWithCards) => {
    try {
      const r = await api.move(card.id, list.id, card.rev, card.claim ? card.claim.holder_id : "u-owner");
      if (r.approval_pending) info("已提交审批，等待人类批准");
    } catch (e) { showError(e); }
  };

  const decide = async (id: string, decision: "approved" | "rejected") => {
    try { await api.decide(id, decision); refresh(); }
    catch (e) { showError(e); }
  };

  const pendingCount = approvals.filter((a) => a.status === "pending").length;

  return (
    <div className="app">
      <header className="topbar">
        <h1>Baton</h1>
        <span className="badge">演示项目 / 开发板</span>
        <button className="approval-btn" onClick={() => setShowApprovals(!showApprovals)}>
          🔔 审批{pendingCount > 0 && <span className="pill">{pendingCount}</span>}
        </button>
        <div className="spacer" />
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && createCard()}
          placeholder="新卡片标题，回车创建 →"
        />
        <button onClick={createCard}>+ 建卡</button>
      </header>

      {toast && <div className="toast">{toast}</div>}

      {showApprovals && (
        <section className="approvals">
          <h3>审批中心</h3>
          {pendingCount === 0 && <div className="muted">没有待审批事项</div>}
          {approvals.filter((a) => a.status === "pending").map((a) => (
            <div key={a.id} className="approval-row">
              <span className="a-title">{a.card_title}</span>
              <span className="muted">→ {a.list_id} · 由 {memberName(a.requested_by)} 申请</span>
              <button onClick={() => decide(a.id, "approved")}>✓ 通过</button>
              <button className="danger" onClick={() => decide(a.id, "rejected")}>✗ 打回</button>
            </div>
          ))}
        </section>
      )}

      <main className="board">
        {board?.lists.map((l) => (
          <Column key={l.id} list={l} onOpen={openCard} onDrop={dropCard} />
        ))}
        {!board && <div className="loading">正在连接 core server（127.0.0.1:7700）…</div>}
      </main>

      {selected && (
        <CardDrawer
          card={selected}
          board={board}
          onClose={() => setSelected(null)}
          onError={showError}
          onInfo={info}
          onDone={async (next) => { if (next) setSelected(next); }}
        />
      )}
    </div>
  );
}

function Column({
  list, onOpen, onDrop,
}: {
  list: ListWithCards;
  onOpen: (id: string) => void;
  onDrop: (card: CardSummary, list: ListWithCards) => void;
}) {
  const [dragOver, setDragOver] = useState(false);
  const overWip = list.wip_limit != null && list.cards.length > list.wip_limit;
  return (
    <section
      className={`column ${overWip ? "over-wip" : ""} ${dragOver ? "drag-over" : ""}`}
      onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
      onDragLeave={() => setDragOver(false)}
      onDrop={(e) => {
        e.preventDefault(); setDragOver(false);
        const raw = e.dataTransfer.getData("application/x-baton-card");
        if (!raw) return;
        const card = JSON.parse(raw) as CardSummary;
        if (card.id && !list.cards.some((c) => c.id === card.id)) onDrop(card, list);
      }}
    >
      <h2>
        {list.name}
        {list.policy.require_approval && <span className="lock-hint" title="进入需审批">🛂</span>}
        <span className="count">
          {list.cards.length}{list.wip_limit != null && ` / ${list.wip_limit}`}
        </span>
      </h2>
      <div className="cards">
        {list.cards.map((c) => (
          <CardChip key={c.id} card={c} onOpen={onOpen} />
        ))}
      </div>
    </section>
  );
}

function CardChip({ card, onOpen }: { card: CardSummary; onOpen: (id: string) => void }) {
  return (
    <div
      className={`card p-${card.priority}`}
      draggable
      onDragStart={(e) => e.dataTransfer.setData("application/x-baton-card", JSON.stringify(card))}
      onClick={() => onOpen(card.id)}
    >
      <div className="card-title">{card.title}</div>
      <div className="card-meta">
        {card.progress_percent != null && <span className="progress">{card.progress_percent}%</span>}
        {card.claim && <span className="claimed">🔒 {memberName(card.claim.holder_id)}</span>}
        {card.open_threads > 0 && <span>💬 {card.open_threads}</span>}
        {card.handoff_state && card.handoff_state !== "none" && (
          <span className="handoff">⇄ {card.handoff_state}</span>
        )}
      </div>
    </div>
  );
}

function CardDrawer({
  card, board, onClose, onError, onInfo, onDone,
}: {
  card: CardDetail;
  board: BoardState | null;
  onClose: () => void;
  onError: (e: unknown) => void;
  onInfo: (msg: string) => void;
  onDone: (next: CardDetail | null) => Promise<void>;
}) {
  const [tab, setTab] = useState<Tab>("讨论");
  const [comment, setComment] = useState("");
  const claimed = !!card.claim;
  const lists = board?.lists ?? [];
  const idx = lists.findIndex((l) => l.id === card.list_id);
  const prev = lists[idx - 1];
  const next = lists[idx + 1];
  const handoffState = card.ext.handoff?.state ?? "none";

  const run = async (fn: () => Promise<CardDetail | { approval_pending?: string } | unknown>) => {
    try {
      const r = (await fn()) as CardDetail & { approval_pending?: string };
      if (r?.approval_pending) onInfo("已提交审批，等待人类批准");
      else if (r?.id) await onDone(r);
    } catch (e) { onError(e); }
  };

  const moveActor = claimed ? card.claim!.holder_id : "u-owner";

  return (
    <aside className="drawer">
      <div className="drawer-head">
        <h2>{card.title}</h2>
        <button className="close" onClick={onClose}>×</button>
      </div>
      <div className="meta-line">
        <span>rev {card.rev}</span>
        <span>创建者 {memberName(card.created_by)}</span>
        {card.claim && <span>🔒 {memberName(card.claim.holder_id)} 持有至 {card.claim.lease_until}</span>}
      </div>
      {card.ext.progress && (
        <div className="progress-line">
          <div className="bar"><div style={{ width: `${card.ext.progress.percent}%` }} /></div>
          <span>{card.ext.progress.percent}% · {card.ext.progress.summary}</span>
        </div>
      )}
      {card.description && <p className="desc">{card.description}</p>}

      <div className="actions">
        {!claimed ? (
          <button onClick={() => run(async () => { await api.claim(card.id, "a-code"); return api.card(card.id); })}>
            🤖 code-agent 认领
          </button>
        ) : (
          <button onClick={() => run(async () => { await api.release(card.id); return api.card(card.id); })}>
            🔓 释放租约
          </button>
        )}
        {prev && <button onClick={() => run(() => api.move(card.id, prev.id, card.rev, moveActor))}>← {prev.name}</button>}
        {next && <button onClick={() => run(() => api.move(card.id, next.id, card.rev, moveActor))}>→ {next.name}</button>}
        {claimed && (
          <button onClick={() => run(() => api.progress(card.id, 50, "进展更新：已完成一半", "a-code"))}>
            📈 模拟进度 50%
          </button>
        )}
      </div>

      <nav className="tabs">
        {(["讨论", "需求", "Git", "现场", "移交"] as Tab[]).map((t) => (
          <button key={t} className={tab === t ? "tab active" : "tab"} onClick={() => setTab(t)}>{t}</button>
        ))}
      </nav>

      {tab === "讨论" && (
        <>
          {card.threads.map((t) => (
            <div key={t.id} className="thread">
              <div className="thread-title">{t.title ?? "话题"} · {t.status}</div>
              {t.comments.map((c) => (
                <div key={c.id} className={`comment kind-${c.kind}`}>
                  <span className="author">{memberName(c.author_id)}</span>
                  <span className="kind">{c.kind}</span>
                  <div>{c.body}</div>
                </div>
              ))}
            </div>
          ))}
          <div className="comment-box">
            <input
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && comment.trim()) {
                  run(() => api.comment(card.id, comment.trim()));
                  setComment("");
                }
              }}
              placeholder="评论，回车发送"
            />
          </div>
        </>
      )}

      {tab === "需求" && <LinksTab card={card} run={run} />}
      {tab === "Git" && <GitTab card={card} run={run} />}
      {tab === "现场" && <WorksiteTab card={card} run={run} />}
      {tab === "移交" && (
        <HandoffTab card={card} state={handoffState} run={run} />
      )}
    </aside>
  );
}

function LinksTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [title, setTitle] = useState("");
  const [url, setUrl] = useState("");
  const [system, setSystem] = useState("jira");
  return (
    <div>
      <h4>需求来源</h4>
      {card.links.filter((l) => l.category === "source").map((l) => (
        <div key={l.id} className="kv-row">
          <span className="tag">{l.system}</span>
          {l.key && <span className="mono">{l.key}</span>}
          <a href={l.url ?? "#"} target="_blank" rel="noreferrer">{l.title || l.url}</a>
          <span className="muted">{l.relation === "origin" ? "来源" : "关联"}</span>
        </div>
      ))}
      <h4>需求文档</h4>
      {card.links.filter((l) => l.category === "doc").map((l) => (
        <div key={l.id} className="kv-row">
          <span className="tag">{l.kind}</span>
          {l.url ? <a href={l.url} target="_blank" rel="noreferrer">{l.title}</a>
                 : <span className="mono">{l.path ?? l.title}</span>}
        </div>
      ))}
      {card.links.length === 0 && <div className="muted">暂无链接</div>}
      <div className="form-row">
        <select value={system} onChange={(e) => setSystem(e.target.value)}>
          <option value="jira">Jira</option><option value="meego">MeeGo</option>
          <option value="url">URL</option><option value="file">本地文件</option>
        </select>
        <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="标题 / PROJ-123" />
        <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://… 或 /path" />
        <button onClick={() => run(() => api.addLink(card.id, {
          category: "source", system, title, url,
          relation: "origin", kind: system === "file" ? "local_file" : "url",
        }))}>+ 添加</button>
      </div>
    </div>
  );
}

function GitTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [path, setPath] = useState("");
  const [branch, setBranch] = useState("");
  const repos = card.ext.git?.repos ?? [];
  return (
    <div>
      {repos.map((r, i) => (
        <div key={i} className="repo">
          <div className="mono">{r.repo_path}</div>
          <div className="muted">分支 {r.branch}{r.declared.base_branch && ` ← ${r.declared.base_branch}`}</div>
          {r.observed && (
            r.observed.error
              ? <div className="error-text">探测失败：{r.observed.error}</div>
              : <div className="git-status">
                  {r.observed.clean ? "✅ 干净" : (
                    <>
                      {r.observed.staged ? <span>●{r.observed.staged} staged</span> : null}
                      {r.observed.unstaged ? <span>{r.observed.unstaged} 未暂存</span> : null}
                      {r.observed.untracked ? <span>{r.observed.untracked} 未跟踪</span> : null}
                    </>
                  )}
                  {r.observed.ahead ? <span>↑{r.observed.ahead}</span> : null}
                  {r.observed.behind ? <span>↓{r.observed.behind}</span> : null}
                  <span className="muted">截至 {r.observed.snapshot_at}</span>
                </div>
          )}
        </div>
      ))}
      {repos.length === 0 && <div className="muted">未关联仓库</div>}
      <div className="form-row">
        <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="仓库绝对路径" />
        <input value={branch} onChange={(e) => setBranch(e.target.value)} placeholder="分支" />
        <button onClick={() => run(() => api.gitAttach(card.id, path, branch))}>关联</button>
      </div>
      {repos.length > 0 && (
        <button onClick={() => run(() => api.gitRefresh(card.id))}>🔄 探测 git 状态</button>
      )}
    </div>
  );
}

function WorksiteTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [path, setPath] = useState("");
  const [branch, setBranch] = useState("");
  const [purpose, setPurpose] = useState("");
  const main = card.work_nodes.filter((n) => n.kind === "main");
  const trees = card.work_nodes.filter((n) => n.kind === "worktree");
  return (
    <div>
      <h4>主工作目录</h4>
      {main.map((n) => (
        <div key={n.id} className="node main-node">
          <span className="mono">{n.path}</span>
          <span className="tag">{n.branch}</span>
        </div>
      ))}
      {main.length === 0 && <div className="muted">未登记</div>}
      <h4>Worktrees（{trees.length}）</h4>
      {trees.map((n) => (
        <div key={n.id} className="node">
          <span className="mono">{n.path}</span>
          <span className="tag">{n.branch}</span>
          {n.purpose && <span className="muted">{n.purpose}</span>}
          {n.owner_id && <span>👤 {memberName(n.owner_id)}</span>}
          {n.bound_card_id && <span className="muted">→ {n.bound_card_id}</span>}
        </div>
      ))}
      <div className="form-row">
        <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="路径" />
        <input value={branch} onChange={(e) => setBranch(e.target.value)} placeholder="分支" />
        <input value={purpose} onChange={(e) => setPurpose(e.target.value)} placeholder="用途" />
        <button onClick={() => run(() => api.worksiteAdd(card.id, {
          kind: main.length === 0 ? "main" : "worktree", path, branch, purpose, owner: "a-code",
        }))}>+ 登记</button>
      </div>
    </div>
  );
}

function HandoffTab({
  card, state, run,
}: {
  card: CardDetail;
  state: string;
  run: (fn: () => Promise<unknown>) => void;
}) {
  const [note, setNote] = useState("");
  const h = card.handoff;
  return (
    <div>
      <div className="handoff-state">当前状态：<b>{state}</b></div>
      {state === "none" && (
        <div className="form-row">
          <input value={note} onChange={(e) => setNote(e.target.value)}
            placeholder="移交说明：进展/卡点/下一步（Markdown）" />
          <button onClick={() => run(() => api.handoff(card.id, "prepare", { context_note: note, reason: note }))}>
            📦 准备移交
          </button>
        </div>
      )}
      {state === "preparing" && (
        <div className="actions">
          <button onClick={() => run(() => api.handoff(card.id, "ready"))}>✅ 移交就绪（释放租约）</button>
          <button onClick={() => run(() => api.handoff(card.id, "cancel"))}>取消</button>
        </div>
      )}
      {state === "ready" && (
        <div className="actions">
          <button onClick={() => run(() => api.handoff(card.id, "accept"))}>🤝 接受移交（code-agent）</button>
          <button onClick={() => run(() => api.handoff(card.id, "cancel"))}>取消</button>
        </div>
      )}
      {h?.package && (
        <div className="handoff-pkg">
          <h4>移交包</h4>
          <p>{h.package.context_note}</p>
          {h.package.env_notes && <p className="muted">环境：{h.package.env_notes}</p>}
          <div className="muted">未结话题 {h.package.open_threads?.length ?? 0} 个</div>
        </div>
      )}
      {h && h.timeline.length > 0 && (
        <>
          <h4>移交时间线</h4>
          {h.timeline.map((t, i) => (
            <div key={i} className="kv-row">
              <span className="tag">{t.action}</span>
              <span>{memberName(t.by_id)}</span>
              <span className="muted">{t.at}</span>
            </div>
          ))}
        </>
      )}
    </div>
  );
}
