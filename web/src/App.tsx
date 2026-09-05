import { useCallback, useEffect, useState } from "react";
import {
  api, BoardState, CardDetail, CardSummary, ListWithCards, MEMBER_NAMES,
} from "./api";

const memberName = (id?: string | null) => (id ? MEMBER_NAMES[id] ?? id : "—");

export default function App() {
  const [board, setBoard] = useState<BoardState | null>(null);
  const [selected, setSelected] = useState<CardDetail | null>(null);
  const [newTitle, setNewTitle] = useState("");
  const [toast, setToast] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setBoard(await api.board());
    } catch {
      /* core 未启动时静默 */
    }
  }, []);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 2000); // 骨架阶段用轮询代替 WebSocket
    return () => clearInterval(t);
  }, [refresh]);

  const showError = (e: unknown) => {
    const err = e as { data?: { error?: string }; message?: string };
    setToast(err?.data?.error ?? err?.message ?? "操作失败");
    setTimeout(() => setToast(null), 4000);
  };

  const openCard = async (id: string) => {
    try { setSelected(await api.card(id)); } catch (e) { showError(e); }
  };

  const createCard = async () => {
    if (!newTitle.trim()) return;
    try {
      await api.createCard(newTitle.trim());
      setNewTitle("");
      refresh();
    } catch (e) { showError(e); }
  };

  return (
    <div className="app">
      <header className="topbar">
        <h1>Baton</h1>
        <span className="badge">演示项目 / 开发板</span>
        <div className="spacer" />
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && createCard()}
          placeholder="新卡片标题，回车创建 →"
        />
        <button onClick={createCard}>+ 建卡</button>
      </header>

      {toast && <div className="toast">⚠️ {toast}</div>}

      <main className="board">
        {board?.lists.map((l) => (
          <Column key={l.id} list={l} onOpen={openCard} />
        ))}
        {!board && <div className="loading">正在连接 core server（127.0.0.1:7700）…</div>}
      </main>

      {selected && (
        <CardDrawer
          card={selected}
          board={board}
          onClose={() => setSelected(null)}
          onAction={async (fn) => {
            try {
              const next = await fn();
              if (next) setSelected(next);
              refresh();
            } catch (e) { showError(e); }
          }}
        />
      )}
    </div>
  );
}

function Column({ list, onOpen }: { list: ListWithCards; onOpen: (id: string) => void }) {
  const overWip = list.wip_limit != null && list.cards.length > list.wip_limit;
  return (
    <section className={`column ${overWip ? "over-wip" : ""}`}>
      <h2>
        {list.name}
        <span className="count">
          {list.cards.length}
          {list.wip_limit != null && ` / ${list.wip_limit}`}
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
    <div className={`card p-${card.priority}`} onClick={() => onOpen(card.id)}>
      <div className="card-title">{card.title}</div>
      <div className="card-meta">
        {card.progress_percent != null && (
          <span className="progress">{card.progress_percent}%</span>
        )}
        {card.claim && (
          <span className="claimed">🔒 {memberName(card.claim.holder_id)}</span>
        )}
        {card.open_threads > 0 && <span>💬 {card.open_threads}</span>}
        {card.handoff_state && card.handoff_state !== "none" && (
          <span className="handoff">⇄ {card.handoff_state}</span>
        )}
      </div>
    </div>
  );
}

function CardDrawer({
  card, board, onClose, onAction,
}: {
  card: CardDetail;
  board: BoardState | null;
  onClose: () => void;
  onAction: (fn: () => Promise<CardDetail | null>) => Promise<void>;
}) {
  const [comment, setComment] = useState("");
  const claimed = !!card.claim;
  const lists = board?.lists ?? [];
  const idx = lists.findIndex((l) => l.id === card.list_id);
  const prev = lists[idx - 1];
  const next = lists[idx + 1];

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
      {card.description && <p className="desc">{card.description}</p>}

      <div className="actions">
        {!claimed ? (
          <button onClick={() => onAction(async () => { await api.claim(card.id, "a-code"); return api.card(card.id); })}>
            🤖 code-agent 认领
          </button>
        ) : (
          <button onClick={() => onAction(async () => { await api.release(card.id); return api.card(card.id); })}>
            🔓 释放租约
          </button>
        )}
        {prev && (
          <button onClick={() => onAction(() => api.move(card.id, prev.id, card.rev, "u-owner"))}>
            ← {prev.name}
          </button>
        )}
        {next && (
          <button onClick={() => onAction(() => api.move(card.id, next.id, card.rev, claimed ? "a-code" : "u-owner"))}>
            → {next.name}
          </button>
        )}
        {claimed && (
          <button onClick={() => onAction(() => api.progress(card.id, 50, "进展更新：已完成一半", "a-code"))}>
            📈 模拟进度 50%
          </button>
        )}
      </div>

      <h3>讨论</h3>
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
              onAction(() => api.comment(card.id, comment.trim()));
              setComment("");
            }
          }}
          placeholder="评论，回车发送"
        />
      </div>
    </aside>
  );
}
